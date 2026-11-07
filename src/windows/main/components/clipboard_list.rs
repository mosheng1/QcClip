use crate::ui::scale::ui_rem;

use unicode_segmentation::UnicodeSegmentation;

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    ops::Range,
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    time::Duration,
};

#[cfg(target_os = "windows")]
use gpui::Bounds;
use gpui::prelude::*;
use gpui::{
    Animation, AnimationExt, AnyElement, AnyWindowHandle, App, ClickEvent, Context, DragMoveEvent,
    DurationWithEasing, Element, ElementId, Entity, GlobalElementId, InspectorElementId,
    IntoElement, LayoutId, MouseButton, ObjectFit, Pixels, Point, PromptLevel, Render, RenderImage,
    ScrollWheelEvent, SharedString, Size, StatefulInteractiveElement, Window, canvas, div, img,
    linear_color_stop, linear_gradient, point, px, size,
};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{
    ActiveTheme as _, ReorderableVirtualListSizes, ThemeColor, VirtualListScrollHandle,
    tooltip::Tooltip, v_flex, v_virtual_list_reorderable,
};

use super::tags_panel::TagDrag;
use crate::windows::list_order::nearest_survivor;

#[cfg(target_os = "windows")]
use crate::platform::drag_preview::DragPreviewService;
use crate::{
    config::{
        ADAPTIVE_MAX_LINES_MAX, ADAPTIVE_MAX_LINES_MIN, CardItemGap, ClipboardDisplayPriority,
        ClipboardMouseAction, ClipboardMouseBehavior, ItemDisplayField, ItemRegionVisibility,
        ListItemHeight, ListItemStyle, SearchEngine,
    },
    config_runtime,
    domain::{
        capture::{FormatKind, FormatRole},
        clipboard::{
            ClipboardItemId, ClipboardListItem, ContentKind, FormatPreview, HistoryItemId,
            ImageDimensions, ImagePreviewSource, Tag, is_empty_textual_preview,
            select_user_visible_previews_with_priority,
        },
    },
    i18n::{self, Translator},
    platform::window::logical_to_physical_desktop,
    services::{capture::extract_links, image_preview::LoadedImagePreview},
    ui::{hover_icon, html as html_preview, icon},
    windows::main::state::{
        FileListKey, ListDropTarget, ListItemLoadAnimation, ListLayoutCache, ListLayoutCacheKey,
        ListReorderAnimation, ListStatus, MainWindow, PendingListAnchor, display_priority_roles,
    },
    windows::main::{
        MainWindowService,
        file_list::{FileListView, file_list_preview_height},
        tabs::TabId,
    },
    windows::menu::{
        DropdownAnchor, DropdownItem, DropdownItemId, DropdownModel, MenuActionId, MenuIcon,
        MenuItem, MenuModel, MenuService,
    },
    windows::preview::{PreviewAnchor, PreviewRect, PreviewSource, PreviewWindowService},
};

#[cfg(target_os = "windows")]
use crate::platform::external_drag::ExternalDragService;

mod context_menu;
mod heat_scrollbar;
mod metrics;

pub(crate) use context_menu::confirm_remove_from_favorites;
use context_menu::{
    OPEN_ALL_LINKS_ACTION, OPEN_LINK_ACTION_PREFIX, OPEN_WITH_DEFAULT_ACTION, PIN_TO_SCREEN_ACTION,
    SPLIT_WORDS_ACTION, favorite_item_context_menu, history_item_context_menu,
    parse_toggle_tag_action, tag_color_value, truncate_link_label,
};
use metrics::*;

const MIN_PREFETCH_ROWS: usize = 4;
const DEFAULT_ADAPTIVE_PREVIEW_LINES: usize = 3;
const ADAPTIVE_IMAGE_PREVIEW_LINES: usize = 8;
const PREVIEW_FONT_SCALE: f32 = 0.875;
const PREVIEW_LINE_HEIGHT_SCALE: f32 = 1.25;
const META_ROW_HEIGHT_SCALE: f32 = 1.25;
const ROW_VERTICAL_PADDING_SCALE: f32 = 0.5;
const MAX_ROW_BORDER_WIDTH: f32 = 2.0;
const CARD_HORIZONTAL_INSET: f32 = 8.0;
const CARD_RADIUS: f32 = 8.0;
const ROW_HORIZONTAL_PADDING: f32 = 10.0;
const IMAGE_PREVIEW_OVERSAMPLE: u32 = 2;
const IMAGE_PREVIEW_MAX_WIDTH: u32 = 4096;
const LIST_REORDER_ANIMATION_DURATION: Duration = Duration::from_millis(260);
const LIST_ITEM_LOAD_ANIMATION_DURATION: Duration = Duration::from_millis(180);
const LIST_ITEM_LOAD_ANIMATION_DELAY: Duration = Duration::from_millis(10);
const LIST_ITEM_LOAD_ANIMATION_MAX_DELAY: Duration = Duration::from_millis(20);
const DRAG_PREVIEW_BACKGROUND_OPACITY: f32 = 0.8;
/// 多选拖拽预览中每层堆叠卡片相对上一层的错位距离。
const DRAG_STACK_STEP: f32 = 6.0;
const DRAG_STACK_MAX_LAYERS: usize = 2;
const LIST_BOTTOM_BAR_HEIGHT: f32 = 6.0;
const ADD_TO_FAVORITES_ACTION: &str = "add-to-favorites";
const EDIT_ITEM_ACTION: &str = "edit-item";
const EDIT_NOTE_ACTION: &str = "edit-note";

#[derive(Clone)]
struct ListItemDrag {
    source_tab: TabId,
    history_id: HistoryItemId,
    is_pinned: bool,
    selected: bool,
    selection_number: Option<usize>,
    item: ClipboardListItem,
    metrics: ListItemMetrics,
    list_item_style: ListItemStyle,
    display_preview: Option<FormatPreview>,
    image: Option<LoadedImagePreview>,
    source_app_icon: Option<Arc<RenderImage>>,
    file_list: Option<Entity<FileListView>>,
    preview_size: Rc<Cell<Option<Size<Pixels>>>>,
    /// 拖拽开始时写入：大于 1 表示拖拽整个多选集合，预览显示为堆叠样式且不参与列表重排。
    stack_count: Rc<Cell<usize>>,
}

impl ListItemDrag {
    fn is_multi(&self) -> bool {
        self.stack_count.get() > 1
    }
}

fn drag_stack_offset(stack_count: usize) -> f32 {
    stack_count.saturating_sub(1).min(DRAG_STACK_MAX_LAYERS) as f32 * DRAG_STACK_STEP
}

struct ListItemDragPreview {
    drag: ListItemDrag,
    html_images: HashMap<PathBuf, Arc<RenderImage>>,
    render_preview: bool,
}

impl Render for ListItemDragPreview {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.render_preview {
            return div().into_any_element();
        }

        render_drag_preview(&self.drag, &self.html_images, &self.drag.item.shortcut, cx)
    }
}

#[derive(PartialEq)]
struct ClipboardRowContent {
    item: ClipboardListItem,
    can_reorder: bool,
    focused: bool,
    selection_number: Option<usize>,
    metrics: ListItemMetrics,
    list_item_style: ListItemStyle,
    display_priority: ClipboardDisplayPriority,
    preview_priority: ClipboardDisplayPriority,
    display_preview: Option<FormatPreview>,
    html_images: HashMap<PathBuf, Arc<RenderImage>>,
    image: Option<LoadedImagePreview>,
    source_app_icon: Option<Arc<RenderImage>>,
    file_list: Option<Entity<FileListView>>,
    active_tab: TabId,
    tags: Vec<Tag>,
    reorder_animation: Option<(u64, Pixels)>,
    load_animation: Option<(u64, usize)>,
    load_offset: Pixels,
    hover_allowed: bool,
    appearance: crate::config::AppearanceConfig,
    clipboard: crate::config::ClipboardConfig,
    minute: u64,
}

struct ClipboardRowView {
    content: ClipboardRowContent,
    main_window: gpui::WeakEntity<MainWindow>,
}

impl Render for ClipboardRowView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = &self.content;
        self.main_window
            .update(cx, |_, cx| {
                render_row(
                    content.item.clone(),
                    content.can_reorder,
                    content.focused.then_some(content.item.history_id),
                    content.selection_number,
                    content.metrics,
                    content.list_item_style,
                    content.display_priority,
                    content.preview_priority,
                    content.display_preview.clone(),
                    &content.html_images,
                    content.image.clone(),
                    content.source_app_icon.clone(),
                    content.file_list.clone(),
                    content.active_tab,
                    &content.tags,
                    content.reorder_animation,
                    content
                        .load_animation
                        .map(|(generation, order)| ListItemLoadAnimation { generation, order }),
                    content.load_offset,
                    content.hover_allowed,
                    window,
                    cx,
                )
            })
            .unwrap_or_else(|_| div().into_any_element())
    }
}

fn render_cached_row(
    content: ClipboardRowContent,
    window: &mut Window,
    cx: &mut Context<MainWindow>,
) -> AnyElement {
    let main_window = cx.entity().downgrade();
    let row_height = content.metrics.row_height;
    let mut content = Some(content);
    let row = window.use_keyed_state(
        (
            "clipboard-row-view",
            content.as_ref().unwrap().item.history_id as u64,
        ),
        cx,
        |_, _| ClipboardRowView {
            content: content.take().unwrap(),
            main_window,
        },
    );
    // 内容变化当帧走普通视图，丢弃旧的绘制缓存；后续稳定帧才复用。
    let changed = content.is_some_and(|content| {
        row.update(cx, |row, _| {
            if row.content == content {
                false
            } else {
                row.content = content;
                true
            }
        })
    });
    if changed {
        row.into_any_element()
    } else {
        let mut slot = div().w_full().h(row_height);
        row.cached(slot.style().clone()).into_any_element()
    }
}

pub(crate) fn quick_paste_row_metrics(
    window: &Window,
    cx: &App,
) -> (Pixels, Pixels, Pixels, Pixels) {
    let appearance = config_runtime::appearance_config(cx);
    let metrics = ListItemMetrics::from_rem_size(
        window.rem_size(),
        ListItemStyle::Card,
        appearance.card_item_gap,
        ListItemHeight::ExtraSmall,
        appearance.adaptive_max_lines,
    )
    .with_font_metrics(crate::ui::fonts::AppFontMetrics::from_window(window, cx));
    (
        metrics.row_height,
        metrics.content_height,
        metrics.preview_font_size,
        metrics.preview_height,
    )
}

pub fn render(
    active_tab: TabId,
    can_reorder: bool,
    selected_item_id: Option<HistoryItemId>,
    multi_selected_item_ids: &[HistoryItemId],
    list_status: &ListStatus,
    scroll_handle: &VirtualListScrollHandle,
    heatmap: &mut crate::windows::main::state::ListHeatmap,
    heat_scrollbar: &crate::windows::main::state::HeatScrollbarState,
    adaptive_item_heights: &mut HashMap<HistoryItemId, Pixels>,
    list_layout_cache: &mut Option<ListLayoutCache>,
    pending_list_anchor: &mut Option<PendingListAnchor>,
    last_rendered_list_horizontal: &mut bool,
    last_rendered_list_leading_padding: &mut Pixels,
    list_layout_generation: u64,
    query_generation: u64,
    total_item_count: usize,
    list_order_ids: &[HistoryItemId],
    loaded_items: &HashMap<HistoryItemId, ClipboardListItem>,
    html_images: &HashMap<PathBuf, Arc<RenderImage>>,
    file_list_views: &HashMap<FileListKey, Entity<FileListView>>,
    toolbar_bottom: Pixels,
    toolbar_dragging: bool,
    window: &Window,
    cx: &mut Context<MainWindow>,
) -> AnyElement {
    let appearance = crate::config_runtime::appearance_config(cx);
    let list_item_style = appearance.list_item_style;
    let metrics = ListItemMetrics::from_rem_size(
        window.rem_size(),
        list_item_style,
        appearance.card_item_gap,
        appearance.list_item_height,
        appearance.adaptive_max_lines,
    )
    .with_info_row(appearance.item_display.shows_info_row())
    .with_font_metrics(crate::ui::fonts::AppFontMetrics::from_window(window, cx))
    .with_note_display(appearance.only_show_notes);
    let clipboard = crate::config_runtime::clipboard_config(cx);
    let display_priority = clipboard.display_priority;
    let preview_priority = clipboard.preview_priority.resolve(display_priority);
    match list_status {
        ListStatus::Ready => render_rows(
            active_tab,
            can_reorder,
            selected_item_id,
            multi_selected_item_ids,
            scroll_handle,
            heatmap,
            heat_scrollbar,
            adaptive_item_heights,
            list_layout_cache,
            pending_list_anchor,
            last_rendered_list_horizontal,
            last_rendered_list_leading_padding,
            list_layout_generation,
            query_generation,
            total_item_count,
            list_order_ids,
            loaded_items,
            html_images,
            file_list_views,
            metrics,
            list_item_style,
            appearance.card_item_gap,
            display_priority,
            preview_priority,
            toolbar_bottom,
            toolbar_dragging,
            window,
            cx,
        ),
        ListStatus::Loading => {
            let translator = i18n::translator(cx);
            render_status_with_toolbar(
                &translator.translate("main.list.loading"),
                active_tab == TabId::Favorites,
                toolbar_bottom,
                toolbar_dragging,
                scroll_handle,
                cx.theme().colors,
                cx,
            )
        }
        ListStatus::Empty => {
            let translator = i18n::translator(cx);
            render_status_with_toolbar(
                &translator.translate("main.list.empty"),
                active_tab == TabId::Favorites,
                toolbar_bottom,
                toolbar_dragging,
                scroll_handle,
                cx.theme().colors,
                cx,
            )
        }
        ListStatus::Error => render_status_element_with_toolbar(
            error_message(cx),
            active_tab == TabId::Favorites,
            toolbar_bottom,
            toolbar_dragging,
            scroll_handle,
            cx,
        ),
    }
}

fn render_status_with_toolbar(
    message: &str,
    show_new_favorite: bool,
    toolbar_bottom: Pixels,
    toolbar_dragging: bool,
    scroll_handle: &VirtualListScrollHandle,
    theme: ThemeColor,
    cx: &mut Context<MainWindow>,
) -> AnyElement {
    render_status_element_with_toolbar(
        status_message(message, theme),
        show_new_favorite,
        toolbar_bottom,
        toolbar_dragging,
        scroll_handle,
        cx,
    )
}

fn render_status_element_with_toolbar(
    status: AnyElement,
    show_new_favorite: bool,
    toolbar_bottom: Pixels,
    toolbar_dragging: bool,
    scroll_handle: &VirtualListScrollHandle,
    cx: &mut Context<MainWindow>,
) -> AnyElement {
    if !show_new_favorite {
        return status;
    }
    let entity = cx.entity().clone();
    v_flex()
        .relative()
        .size_full()
        .overflow_hidden()
        .child(
            canvas(
                move |bounds, _, cx| {
                    entity.update(cx, |this, _| {
                        this.floating_toolbar_container_bounds = bounds;
                    });
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full(),
        )
        .child(status)
        .child(super::floating_toolbar::render(
            toolbar_bottom,
            toolbar_dragging,
            true,
            scroll_handle,
            cx,
        ))
        .into_any_element()
}

fn status_message(message: &str, theme: ThemeColor) -> AnyElement {
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(theme.muted_foreground)
        .child(message.to_owned())
        .into_any_element()
}

fn error_message(cx: &mut Context<MainWindow>) -> AnyElement {
    let theme = cx.theme().colors;
    let translator = i18n::translator(cx);
    div()
        .flex_1()
        .min_h_0()
        .px_4()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .text_sm()
        .text_color(theme.muted_foreground)
        .child(translator.translate("main.list.error"))
        .child(
            div()
                .id("retry-history-query")
                .px_3()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .bg(theme.primary)
                .text_color(theme.primary_foreground)
                .child(translator.translate("common.retry"))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.refresh_history(cx);
                })),
        )
        .into_any_element()
}

fn horizontal_header_enabled(display: &crate::config::ItemDisplayConfig) -> bool {
    let toolbar_info_enabled = display.toolbar_visibility != ItemRegionVisibility::Hidden
        && (display.enabled(ItemDisplayField::Source)
            || display.enabled(ItemDisplayField::Shortcut));
    let action_enabled = display.actions_visibility != ItemRegionVisibility::Hidden
        && [
            ItemDisplayField::Edit,
            ItemDisplayField::PinToScreen,
            ItemDisplayField::SplitWords,
            ItemDisplayField::Note,
            ItemDisplayField::PastePlain,
            ItemDisplayField::OpenDefault,
            ItemDisplayField::OpenBrowser,
            ItemDisplayField::Favorite,
            ItemDisplayField::Pin,
            ItemDisplayField::Delete,
        ]
        .into_iter()
        .any(|field| display.enabled(field));
    toolbar_info_enabled || action_enabled
}

fn render_rows(
    active_tab: TabId,
    can_reorder: bool,
    selected_item_id: Option<HistoryItemId>,
    multi_selected_item_ids: &[HistoryItemId],
    scroll_handle: &VirtualListScrollHandle,
    heatmap: &mut crate::windows::main::state::ListHeatmap,
    heat_scrollbar: &crate::windows::main::state::HeatScrollbarState,
    adaptive_item_heights: &mut HashMap<HistoryItemId, Pixels>,
    list_layout_cache: &mut Option<ListLayoutCache>,
    pending_list_anchor: &mut Option<PendingListAnchor>,
    last_rendered_list_horizontal: &mut bool,
    last_rendered_list_leading_padding: &mut Pixels,
    list_layout_generation: u64,
    query_generation: u64,
    total_item_count: usize,
    list_order_ids: &[HistoryItemId],
    loaded_items: &HashMap<HistoryItemId, ClipboardListItem>,
    html_images: &HashMap<PathBuf, Arc<RenderImage>>,
    file_list_views: &HashMap<FileListKey, Entity<FileListView>>,
    metrics: ListItemMetrics,
    list_item_style: ListItemStyle,
    card_item_gap: CardItemGap,
    display_priority: ClipboardDisplayPriority,
    preview_priority: ClipboardDisplayPriority,
    toolbar_bottom: Pixels,
    toolbar_dragging: bool,
    window: &Window,
    cx: &mut Context<MainWindow>,
) -> AnyElement {
    let translator = i18n::translator(cx);
    let hover_allowed = window.is_window_hovered() && MenuService::hover_animation_allowed(cx);
    let clipboard_config = config_runtime::clipboard_config(cx);
    let horizontal = clipboard_config.window_position.uses_horizontal_list();
    *last_rendered_list_horizontal = horizontal;
    let item_display = config_runtime::appearance_config(cx).item_display;
    let show_horizontal_header = horizontal && horizontal_header_enabled(&item_display);
    let viewport_height = scroll_handle.bounds().size.height;
    let metrics = if horizontal {
        metrics.with_horizontal_card(
            if viewport_height > px(0.0) {
                viewport_height
            } else {
                (window.viewport_size().height - metrics.rem_size * 5.25)
                    .max(metrics.rem_size * 5.0)
            },
            show_horizontal_header,
        )
    } else {
        metrics
    };
    *last_rendered_list_leading_padding = if list_item_style == ListItemStyle::Card && !horizontal {
        metrics.card_vertical_inset
    } else {
        px(0.0)
    };
    let measured_width = scroll_handle.bounds().size.width;
    let viewport_width = if measured_width > px(0.0) {
        measured_width
    } else {
        let clipboard = config_runtime::clipboard_config(cx);
        if clipboard.effective_list_layout() == crate::config::ClipboardListLayout::SideBySide {
            window.viewport_size().width / 2.0
        } else {
            window.viewport_size().width
        }
    };
    let list_width = if horizontal {
        metrics.horizontal_item_width()
    } else {
        viewport_width
    };
    let preview_width = if horizontal {
        available_preview_width_with_card_inset(
            list_width,
            list_item_style,
            window.rem_size(),
            metrics.horizontal_card_inset,
        )
    } else {
        available_preview_width(list_width, list_item_style, window.rem_size())
    };
    let layout_key = ListLayoutCacheKey {
        viewport_width,
        viewport_height: if horizontal { viewport_height } else { px(0.0) },
        horizontal,
        rem_size: window.rem_size(),
        font_metrics: crate::ui::fonts::AppFontMetrics::from_window(window, cx),
        list_item_style,
        card_item_gap,
        list_item_height: metrics.height,
        adaptive_max_lines: metrics.adaptive_max_lines as u8,
        only_show_notes: metrics.only_show_notes,
        show_info_row: metrics.show_meta,
        show_horizontal_header,
        display_priority,
        query_generation,
        // 固定行高与已加载条目的内容无关，分页时无需重建全部行的布局数组。
        // 自适应行高仍需在新条目或资源尺寸变化后重新估算。
        layout_generation: (metrics.height == ListItemHeight::Adaptive)
            .then_some(list_layout_generation)
            .unwrap_or_default(),
        total_item_count,
    };
    // 风格/行高切换会扩大虚拟列表的预取范围。此时补齐的条目只是布局变化后
    // 首次进入视口，不属于用户主动加载的新内容，不应再启动整批滑入动画。
    let list_layout_settings_changed = list_layout_cache.as_ref().is_some_and(|cache| {
        cache.key.viewport_width == layout_key.viewport_width
            && cache.key.rem_size == layout_key.rem_size
            && cache.key.query_generation == layout_key.query_generation
            && cache.key.total_item_count == layout_key.total_item_count
            && (cache.key.list_item_style != layout_key.list_item_style
                || cache.key.card_item_gap != layout_key.card_item_gap
                || cache.key.list_item_height != layout_key.list_item_height
                || cache.key.adaptive_max_lines != layout_key.adaptive_max_lines
                || cache.key.font_metrics != layout_key.font_metrics
                || cache.key.only_show_notes != layout_key.only_show_notes
                || cache.key.show_info_row != layout_key.show_info_row
                || cache.key.show_horizontal_header != layout_key.show_horizontal_header)
    });
    let anchor_index = pending_list_anchor.as_ref().and_then(|anchor| {
        if anchor.horizontal != horizontal {
            return None;
        }
        list_order_ids
            .iter()
            .position(|id| *id == anchor.history_id)
            .or_else(|| {
                let old_index = anchor
                    .layout_order_ids
                    .iter()
                    .position(|id| *id == anchor.history_id)?;
                let visible_ids: HashSet<_> = list_order_ids.iter().copied().collect();
                nearest_survivor(&anchor.layout_order_ids, old_index, &visible_ids)
                    .and_then(|id| list_order_ids.iter().position(|candidate| *candidate == id))
            })
    });
    if pending_list_anchor
        .as_ref()
        .is_some_and(|anchor| anchor.horizontal == horizontal)
        && anchor_index.is_none()
    {
        *pending_list_anchor = None;
    }
    let needs_anchor = pending_list_anchor
        .as_ref()
        .is_some_and(|anchor| anchor.horizontal == horizontal);
    let rebuild_for_anchor = needs_anchor && anchor_index.is_some();
    let mut anchor_origin = None;
    let mut build_layout = || {
        let mut origin = px(0.0);
        let sizes = (0..total_item_count)
            .map(|index| {
                if Some(index) == anchor_index {
                    anchor_origin = Some(origin);
                }
                let history_id = list_order_ids.get(index).copied();
                let measured_row_height = history_id
                    .and_then(|history_id| loaded_items.get(&history_id))
                    .map(|item| {
                        let file_count =
                            file_list_view_for_item(item, display_priority, file_list_views)
                                .map(|view| view.read(cx).path_count());
                        metrics
                            .for_item(
                                item,
                                display_priority,
                                html_images,
                                file_count,
                                preview_width,
                                window,
                            )
                            .row_height
                    });
                let row_height = resolve_row_height(
                    history_id,
                    metrics.height,
                    measured_row_height,
                    metrics.row_height,
                    adaptive_item_heights,
                );
                origin += if horizontal { list_width } else { row_height };
                size(list_width, row_height)
            })
            .collect::<Vec<_>>();
        ListLayoutCache {
            key: layout_key,
            sizes: ReorderableVirtualListSizes::new(sizes),
        }
    };
    if list_layout_cache
        .as_ref()
        .is_none_or(|cache| cache.key != layout_key)
        || rebuild_for_anchor
    {
        *list_layout_cache = Some(build_layout());
    }
    drop(build_layout);
    if let Some(anchor) = pending_list_anchor.as_ref()
        && anchor.horizontal == horizontal
        && let Some(origin) = anchor_origin
    {
        let anchor = pending_list_anchor.take().unwrap();
        scroll_handle.set_offset(anchor.restored_offset(scroll_handle.offset(), origin));
    }
    let layout_cache = list_layout_cache
        .as_ref()
        .expect("列表布局缓存应该已初始化");
    let item_sizes = layout_cache.sizes.clone();
    let multi_selected_item_ids = multi_selected_item_ids.to_vec();
    let mut list = v_virtual_list_reorderable(
        cx.entity(),
        "clipboard-history-list",
        item_sizes.clone(),
        move |this, range: Range<usize>, window, cx| {
            let reorder_animation = this
                .list_reorder_animation
                .as_ref()
                .map(|animation| (animation.generation, animation.start_offsets.clone()));
            let rows = range
                .clone()
                .map(|index| {
                    let history_id = this.list_order_ids.get(index).copied();
                    history_id
                        .and_then(|history_id| this.loaded_items.get(&history_id))
                        .cloned()
                        .map(|item| {
                            let item_history_id = item.history_id;
                            let is_drag_placeholder = this.is_list_drag_source(item.history_id);
                            let preferred_roles = display_priority_roles(display_priority);
                            let display_preview = select_user_visible_previews_with_priority(
                                &item.preview_options,
                                &preferred_roles,
                            )
                            .into_iter()
                            .next();
                            let image = display_preview.as_ref().and_then(|preview| {
                                if preview.content_kind != ContentKind::Image {
                                    return None;
                                }
                                preview.image_source.as_ref().and_then(|_| {
                                    this.image_for(
                                        item.item_id,
                                        &preview.format_namespace,
                                        &preview.format_key,
                                    )
                                })
                            });
                            let file_list = display_preview.as_ref().and_then(|preview| {
                                if preview.format_kind != FormatKind::FileList
                                    || preview.content_kind != ContentKind::FileList
                                {
                                    return None;
                                }
                                preview.image_source.as_ref().and_then(|_| {
                                    this.file_list_for(
                                        item.item_id,
                                        &preview.format_namespace,
                                        &preview.format_key,
                                    )
                                })
                            });
                            let file_count = file_list
                                .as_ref()
                                .map(|file_list| file_list.read(cx).path_count());
                            let source_app_icon = item
                                .source_app_icon
                                .as_ref()
                                .and_then(|source| this.application_icon_for(source));
                            let row_metrics = metrics.for_item(
                                &item,
                                display_priority,
                                &this.loaded_html_images,
                                file_count,
                                available_preview_width(
                                    list_width,
                                    list_item_style,
                                    window.rem_size(),
                                ),
                                window,
                            );
                            let load_animation =
                                if matches!(active_tab, TabId::Clipboard | TabId::Favorites) {
                                    this.list_item_load_animations
                                        .get(&item.history_id)
                                        .copied()
                                } else {
                                    None
                                };
                            if is_drag_placeholder {
                                render_placeholder_row(row_metrics, list_item_style, cx)
                            } else {
                                let selection_number = multi_selected_item_ids
                                    .iter()
                                    .position(|id| *id == item.history_id)
                                    .map(|index| index + 1);
                                render_cached_row(
                                    ClipboardRowContent {
                                        focused: selected_item_id == Some(item_history_id),
                                        item,
                                        can_reorder,
                                        selection_number,
                                        metrics: row_metrics,
                                        list_item_style,
                                        display_priority,
                                        preview_priority,
                                        display_preview,
                                        html_images: this.loaded_html_images.clone(),
                                        image,
                                        source_app_icon,
                                        file_list,
                                        active_tab,
                                        tags: this.tags.clone(),
                                        reorder_animation: reorder_animation.as_ref().and_then(
                                            |(generation, offsets)| {
                                                offsets
                                                    .get(&item_history_id)
                                                    .copied()
                                                    .map(|offset| (*generation, offset))
                                            },
                                        ),
                                        load_animation: load_animation.map(|animation| {
                                            (animation.generation, animation.order)
                                        }),
                                        load_offset: list_width,
                                        hover_allowed,
                                        appearance: config_runtime::appearance_config(cx),
                                        clipboard: config_runtime::clipboard_config(cx),
                                        minute: std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .map(|duration| duration.as_secs() / 60)
                                            .unwrap_or_default(),
                                    },
                                    window,
                                    cx,
                                )
                            }
                        })
                        .unwrap_or_else(|| {
                            render_loading_row(
                                index,
                                metrics,
                                list_item_style,
                                cx.theme().colors,
                                &translator,
                            )
                        })
                })
                .collect::<Vec<_>>();

            if metrics.height == ListItemHeight::Adaptive
                && range.clone().any(|index| {
                    this.list_order_ids.get(index).is_some_and(|history_id| {
                        this.loaded_items.contains_key(history_id)
                            && !this.adaptive_item_heights.contains_key(history_id)
                    })
                })
            {
                // 新加载的自适应行先使用估算高度；进入可见范围后再刷新一次真实高度。
                this.invalidate_list_layout_cache();
            }

            // 先完成当前帧的行构造，再清理下一帧不需要的内容，避免行高与行内容来自不同缓存状态。
            if !range.is_empty() {
                let visible_rows = range.end.saturating_sub(range.start).max(1);
                let prefetch_rows = (visible_rows / 2).max(MIN_PREFETCH_ROWS);
                let cache_start = range.start.saturating_sub(visible_rows / 2);
                let cache_end = range
                    .end
                    .saturating_add(prefetch_rows)
                    .min(this.total_item_count);
                let cache_range = cache_start..cache_end;
                this.trim_loaded_items(cache_range.clone());
                this.trim_image_cache(cache_range.clone(), Some(window));
                this.trim_html_image_cache(cache_range.clone(), display_priority, Some(window));
                this.trim_application_icon_cache(cache_range.clone(), Some(window));
                this.trim_file_list_cache(cache_range.clone(), window, cx);
                this.ensure_images_for_range(
                    cache_range.clone(),
                    (
                        IMAGE_PREVIEW_MAX_WIDTH,
                        u32::from(metrics.resource_preview_height())
                            .max(1)
                            .saturating_mul(IMAGE_PREVIEW_OVERSAMPLE),
                    ),
                    display_priority,
                    cx,
                );
                this.ensure_html_images_for_range(
                    cache_range.clone(),
                    (
                        IMAGE_PREVIEW_MAX_WIDTH,
                        u32::from(metrics.resource_preview_height())
                            .max(1)
                            .saturating_mul(IMAGE_PREVIEW_OVERSAMPLE),
                    ),
                    display_priority,
                    cx,
                );
                this.ensure_application_icons_for_range(cache_range.clone(), (32, 32), cx);
                this.ensure_file_lists_for_range(cache_range.clone(), cx);
                // 缓存范围包含可见区域前后的预取区；向上滚动时也要检查前方缺失项，
                // 否则数据会等到进入视口后才开始加载。
                let missing_index = (cache_start..cache_end).find(|index| {
                    this.list_order_ids
                        .get(*index)
                        .is_some_and(|history_id| !this.loaded_items.contains_key(history_id))
                });
                if let Some(offset) = missing_index {
                    let page_size = cache_end
                        .saturating_sub(offset)
                        .max(visible_rows.saturating_mul(2))
                        .max(MIN_PREFETCH_ROWS.saturating_mul(2));
                    if list_layout_settings_changed {
                        this.load_page_without_animation(cx, offset, page_size);
                    } else {
                        this.load_page(cx, offset, page_size);
                    }
                }
            }

            rows
        },
    )
    .with_axis(if horizontal {
        gpui::Axis::Horizontal
    } else {
        gpui::Axis::Vertical
    })
    .track_scroll(scroll_handle)
    .with_cross_axis_measurement(false)
    .overflow_hidden()
    .h_full()
    .w_full();
    if list_item_style == ListItemStyle::Card && !horizontal {
        list = list.py(metrics.card_vertical_inset);
    }

    let heat_scrollbar_enabled = config_runtime::clipboard_config(cx).heat_scrollbar_enabled;
    let list_container = div()
        .id("clipboard-list-drop-surface")
        .relative()
        .flex_grow_1()
        .min_h_0()
        .w_full()
        .on_drop(cx.listener(|this, drag: &ListItemDrag, _, cx| {
            if drag.source_tab != this.active_tab {
                return;
            }
            #[cfg(target_os = "windows")]
            DragPreviewService::hide(cx);
            #[cfg(target_os = "windows")]
            ExternalDragService::cancel(cx);
            if this
                .list_drop_target
                .is_some_and(|target| target.source_id == drag.history_id)
            {
                cx.stop_propagation();
                this.commit_reorder_preview(cx);
            } else if this.cancel_reorder_preview() {
                cx.notify();
            }
        }))
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| {
                #[cfg(target_os = "windows")]
                DragPreviewService::hide(cx);
                #[cfg(target_os = "windows")]
                ExternalDragService::cancel(cx);
                if this.cancel_reorder_preview() {
                    cx.notify();
                }
            }),
        )
        .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, cx| {
            if !event.modifiers.control {
                PreviewWindowService::hide(cx);
            }
            this.handle_list_scroll_wheel(event, window, cx);
        }))
        .child(list)
        .when(heat_scrollbar_enabled, |this| {
            this.child(heat_scrollbar::render(
                heatmap.buckets(),
                heat_scrollbar,
                scroll_handle,
                viewport_width,
                if horizontal {
                    gpui::Axis::Horizontal
                } else {
                    gpui::Axis::Vertical
                },
                window,
                cx,
            ))
        })
        .when(!heat_scrollbar_enabled, |this| {
            if horizontal {
                this.horizontal_scrollbar(scroll_handle)
            } else {
                this.vertical_scrollbar(scroll_handle)
            }
        });
    let list_container = if horizontal
        || clipboard_config.effective_list_layout() != crate::config::ClipboardListLayout::Single
    {
        let entity = cx.entity();
        gpui_component::ElementExt::on_prepaint(list_container, move |bounds, _, cx| {
            if (horizontal && bounds.size.height != viewport_height)
                || (!horizontal && bounds.size.width != viewport_width)
            {
                entity.update(cx, |_, cx| cx.notify());
            }
        })
    } else {
        list_container
    };

    v_flex()
        .flex_grow_1()
        .relative()
        .size_full()
        .overflow_hidden()
        .child(
            canvas(
                {
                    let entity = cx.entity().clone();
                    move |bounds, _, cx| {
                        entity.update(cx, |this, _| {
                            this.floating_toolbar_container_bounds = bounds;
                        });
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full(),
        )
        .child(list_container)
        .when(!horizontal, |this| {
            this.child(div().h(px(LIST_BOTTOM_BAR_HEIGHT)).w_full().flex_shrink_0())
        })
        .child(super::floating_toolbar::render(
            toolbar_bottom,
            toolbar_dragging,
            active_tab == TabId::Favorites,
            scroll_handle,
            cx,
        ))
        .into_any_element()
}

fn current_reorder_animation_offsets(
    animation: &ListReorderAnimation,
) -> HashMap<HistoryItemId, Pixels> {
    let progress = (animation.started_at.elapsed().as_secs_f32()
        / LIST_REORDER_ANIMATION_DURATION.as_secs_f32())
    .clamp(0.0, 1.0);
    let remaining = 1.0 - gpui::ease_in_out(progress);
    animation
        .start_offsets
        .iter()
        .filter_map(|(history_id, offset)| {
            let offset = px(f32::from(*offset) * remaining);
            (offset != px(0.0)).then_some((*history_id, offset))
        })
        .collect()
}

fn source_application_icon(
    item: &ClipboardListItem,
    image: Option<Arc<RenderImage>>,
    theme: ThemeColor,
    translator: &Translator,
    horizontal: bool,
) -> AnyElement {
    let icon = if item.source_device_name.is_some() {
        img("assets/icon32.png")
            .size(ui_rem(16.0))
            .object_fit(ObjectFit::Contain)
            .into_any_element()
    } else {
        image
            .map(|image| {
                img(image)
                    .size(ui_rem(16.0))
                    .object_fit(ObjectFit::Contain)
                    .into_any_element()
            })
            .unwrap_or_else(|| {
                icon("square-x-filled", 14.0, theme.muted_foreground).into_any_element()
            })
    };
    let tooltip = i18n::clipboard_source(
        translator,
        item.source_app_name.as_deref(),
        item.source_device_name.as_deref(),
    );

    let mut icon_container = div()
        .id(("source-application-icon", item.history_id as u64))
        .size(ui_rem(20.0))
        .flex()
        .items_center()
        .justify_center();
    if horizontal {
        icon_container = icon_container.relative().top(px(-1.0));
    }
    icon_container
        .child(icon)
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .into_any_element()
}

fn render_tag_indicators(
    item: &ClipboardListItem,
    tags: &[Tag],
    theme: ThemeColor,
    animations_enabled: bool,
    hover_allowed: bool,
) -> AnyElement {
    div()
        .absolute()
        .right(ui_rem(6.0))
        .bottom(ui_rem(1.5))
        .flex()
        .items_end()
        .gap(ui_rem(2.0))
        .children(item.tag_ids.iter().filter_map(|tag_id| {
            tags.iter().find(|tag| tag.id == *tag_id).map(|tag| {
                render_tag_indicator(
                    item.history_id,
                    tag,
                    theme,
                    animations_enabled,
                    hover_allowed,
                )
            })
        }))
        .into_any_element()
}

fn render_tag_indicator(
    row_id: HistoryItemId,
    tag: &Tag,
    theme: ThemeColor,
    animations_enabled: bool,
    hover_allowed: bool,
) -> AnyElement {
    let group_name = format!("clipboard-tag-{row_id}-{}", tag.id);
    let tag_name = tag.name.clone();
    let tag_color = gpui::rgb_to_hsla(gpui::rgb(tag_color_value(&tag.color)));
    let mut name_label = div()
        .absolute()
        .right_0()
        .bottom(ui_rem(20.0))
        .max_w(ui_rem(140.0))
        .h(ui_rem(18.0))
        .px(ui_rem(4.0))
        .flex()
        .items_center()
        .rounded_sm()
        .border_1()
        .border_color(theme.border)
        .bg(theme.popover)
        .text_size(ui_rem(10.0))
        .text_color(theme.foreground)
        .whitespace_nowrap()
        .truncate()
        .invisible();
    if hover_allowed {
        name_label = name_label.group_hover(group_name.clone(), |style| style.visible());
    }
    let name_label = name_label.child(tag_name);

    div()
        .id(format!("clipboard-tag-indicator-{row_id}-{}", tag.id))
        .group(group_name)
        .relative()
        .w(ui_rem(6.0))
        .h(ui_rem(10.0))
        .flex_none()
        .rounded_t_sm()
        .cursor_pointer()
        .bg(tag_color)
        .when(hover_allowed && animations_enabled, |this| {
            this.transitions(|transitions| {
                transitions
                    .w(Duration::from_millis(160).with_easing(gpui::ease_in_out))
                    .h(Duration::from_millis(160).with_easing(gpui::ease_in_out))
            })
        })
        .when(hover_allowed, |this| {
            this.hover(|style| style.w(ui_rem(10.0)).h(ui_rem(20.0)))
        })
        .child(name_label)
        .into_any_element()
}

fn hover_action_fields(
    item: &ClipboardListItem,
    active_tab: TabId,
    display: &crate::config::ItemDisplayConfig,
) -> Vec<ItemDisplayField> {
    use ItemDisplayField::*;
    [
        Edit,
        PinToScreen,
        SplitWords,
        Note,
        PastePlain,
        OpenDefault,
        OpenBrowser,
        Favorite,
        Pin,
        Delete,
    ]
    .into_iter()
    .filter(|field| display.enabled(*field))
    .filter(|field| match field {
        Edit | SplitWords => has_editable_text(item),
        PastePlain => {
            context_menu::has_file_list(item)
                || item.all_format_options.iter().any(|preview| {
                    preview.role() == FormatRole::PlainText
                        && preview.is_restorable
                        && !is_empty_textual_preview(preview)
                })
        }
        OpenDefault => can_open_with_default(item),
        OpenBrowser => has_plain_text_links(item),
        Delete => active_tab != TabId::Favorites,
        _ => true,
    })
    .collect()
}

fn render_hover_actions(
    item: &ClipboardListItem,
    active_tab: TabId,
    selected: bool,
    hover_allowed: bool,
    cx: &mut Context<MainWindow>,
) -> Option<(AnyElement, usize)> {
    let display = config_runtime::appearance_config(cx).item_display;
    if display.actions_visibility == ItemRegionVisibility::Hidden {
        return None;
    }
    let fields = hover_action_fields(item, active_tab, &display);
    let button_count = fields.len();
    if button_count == 0 {
        return None;
    }
    let expanded_width =
        gpui_component::Theme::global(cx).font_size * (button_count as f32 * 20.0 / 16.0);
    let visible = display.actions_visibility.is_visible(selected);
    let animations_enabled =
        crate::config_runtime::animation_enabled(cx, crate::config::AnimationKind::ListActions);
    let motion = Duration::from_millis(160).with_easing(gpui::ease_in_out);
    let row_id = item.history_id;
    let item = item.clone();
    let main_window = cx.entity();
    let buttons = canvas(
        move |bounds, window, cx| {
            let visible_bounds = bounds.intersect(&window.content_mask().bounds);
            if visible_bounds.size.width <= px(0.0) || visible_bounds.size.height <= px(0.0) {
                return None;
            }
            // 按动画后的实际裁剪范围构造按钮，收起后的行不再布局整棵按钮树。
            let mut buttons = main_window.update(cx, |_, cx| {
                render_hover_action_buttons(&item, active_tab, &fields, cx)
            });
            buttons.layout_as_root(bounds.size.into(), window, cx);
            buttons.prepaint_at(bounds.origin, window, cx);
            Some(buttons)
        },
        |_, buttons, window, cx| {
            if let Some(mut buttons) = buttons {
                buttons.paint(window, cx);
            }
        },
    )
    .w(expanded_width)
    .h_full()
    .flex_none();
    Some((
        div()
            .id(("hover-actions", row_id as u64))
            .h_full()
            .w(if visible { expanded_width } else { px(0.0) })
            .opacity(if visible { 1.0 } else { 0.0 })
            .flex()
            .items_center()
            .flex_none()
            .overflow_hidden()
            .when(hover_allowed && animations_enabled, |this| {
                this.transitions(|transitions| transitions.w(motion.clone()).opacity(motion))
            })
            .when(
                hover_allowed && display.actions_visibility == ItemRegionVisibility::Active,
                |this| {
                    this.group_hover(format!("clipboard-row-hover-{row_id}"), |style| {
                        style.w(expanded_width).opacity(1.0)
                    })
                },
            )
            .child(buttons)
            .into_any_element(),
        button_count,
    ))
}

fn render_hover_action_buttons(
    item: &ClipboardListItem,
    active_tab: TabId,
    fields: &[ItemDisplayField],
    cx: &mut Context<MainWindow>,
) -> AnyElement {
    let translator = i18n::translator(cx);
    let theme = cx.theme().colors;
    let row_id = item.history_id;
    let item_id = item.item_id;
    let favorite_id = item.favorite_id;
    let is_pinned = item.is_pinned;
    let is_favorites_tab = active_tab == TabId::Favorites;
    let is_favorite = is_favorites_tab || favorite_id.is_some();
    let main_window = cx.entity();

    let favorite_tooltip = if is_favorite {
        translator.translate("main.menu.remove_from_favorites")
    } else {
        translator.translate("main.menu.add_to_favorites")
    };
    let delete_tooltip = translator.translate("main.menu.delete");

    // 文件条目的纯文本粘贴优先使用文件路径。
    let plain_text_format = if context_menu::has_file_list(item) {
        Some((
            "windows".to_owned(),
            context_menu::FILE_PATH_FORMAT_KEY.to_owned(),
        ))
    } else {
        item.all_format_options
            .iter()
            .filter(|preview| {
                preview.role() == FormatRole::PlainText
                    && preview.is_restorable
                    && !is_empty_textual_preview(preview)
            })
            .min_by_key(|preview| preview.format_order)
            .map(|preview| (preview.format_namespace.clone(), preview.format_key.clone()))
    };

    let paste_plain_button = plain_text_format.map(|(format_namespace, format_key)| {
        let paste_window = main_window.clone();
        hover_action_button(
            ("hover-action-paste-plain", row_id as u64),
            "clipboard-typography",
            translator.translate("main.menu.paste_as_plain_text"),
            theme.muted_foreground,
            theme,
            move |_, _, cx| {
                cx.stop_propagation();
                paste_window.update(cx, |this, cx| {
                    this.paste_item_format(
                        item_id,
                        format_namespace.clone(),
                        format_key.clone(),
                        cx,
                    );
                });
            },
        )
    });

    let default_open_button = can_open_with_default(item).then(|| {
        let open_window = main_window.clone();
        hover_action_button(
            ("hover-action-open-default", row_id as u64),
            "external-link",
            translator.translate("main.menu.open_with_default"),
            theme.muted_foreground,
            theme,
            move |_, _, cx| {
                cx.stop_propagation();
                PreviewWindowService::hide(cx);
                MainWindowService::cancel_hover_preview(cx);
                open_window.update(cx, |this, cx| {
                    this.open_item_with_default(item_id, cx);
                });
            },
        )
    });

    let browser_button = has_plain_text_links(item).then(|| {
        let browser_window = main_window.clone();
        hover_action_button(
            ("hover-action-open-browser", row_id as u64),
            "external-link",
            translator.translate("main.menu.open_in_browser"),
            theme.muted_foreground,
            theme,
            move |event, window, cx| {
                cx.stop_propagation();
                PreviewWindowService::hide(cx);
                MainWindowService::cancel_hover_preview(cx);
                open_browser_links_from_button(
                    browser_window.clone(),
                    item_id,
                    event.position(),
                    window,
                    cx,
                );
            },
        )
    });

    let edit_button = has_editable_text(item).then(|| {
        let edit_window = main_window.clone();
        hover_action_button(
            ("hover-action-edit", row_id as u64),
            "edit",
            translator.translate("main.menu.edit"),
            theme.muted_foreground,
            theme,
            move |_, _, cx| {
                cx.stop_propagation();
                PreviewWindowService::hide(cx);
                MainWindowService::cancel_hover_preview(cx);
                edit_window.update(cx, |this, cx| this.edit_item(item_id, cx));
            },
        )
    });

    let pin_to_screen_button = hover_action_button(
        ("hover-action-pin-to-screen", row_id as u64),
        "sticker",
        translator.translate("main.menu.pin_to_screen"),
        theme.muted_foreground,
        theme,
        move |_, _, cx| {
            cx.stop_propagation();
            PreviewWindowService::hide(cx);
            MainWindowService::cancel_hover_preview(cx);
            crate::windows::sticky::StickyService::open(item_id, cx);
        },
    );

    let split_words_button = has_editable_text(item).then(|| {
        hover_action_button(
            ("hover-action-split-words", row_id as u64),
            "blocks",
            translator.translate("main.menu.split_words"),
            theme.muted_foreground,
            theme,
            move |_, _, cx| {
                cx.stop_propagation();
                PreviewWindowService::hide(cx);
                MainWindowService::cancel_hover_preview(cx);
                crate::windows::sticky::StickyService::open_split(item_id, cx);
            },
        )
    });

    let note_window = main_window.clone();
    let note_button = hover_action_button(
        ("hover-action-note", row_id as u64),
        "note",
        translator.translate(if item.note_text.is_empty() {
            "main.note.add"
        } else {
            "main.note.edit"
        }),
        if item.note_text.is_empty() {
            theme.muted_foreground
        } else {
            theme.primary
        },
        theme,
        move |_, _, cx| {
            cx.stop_propagation();
            PreviewWindowService::hide(cx);
            MainWindowService::cancel_hover_preview(cx);
            note_window.update(cx, |this, cx| this.edit_note(item_id, cx));
        },
    );

    let favorite_window = main_window.clone();
    let favorite_button = hover_action_button(
        ("hover-action-favorite", row_id as u64),
        if is_favorite { "star-filled" } else { "star" },
        favorite_tooltip,
        if is_favorite {
            theme.yellow
        } else {
            theme.muted_foreground
        },
        theme,
        move |_, window, cx| {
            cx.stop_propagation();
            if is_favorite {
                let owner_window = window.window_handle();
                if let Some(favorite_id) = favorite_id {
                    let favorite_window = favorite_window.clone();
                    cx.defer(move |cx| {
                        confirm_remove_from_favorites(
                            favorite_window,
                            owner_window,
                            favorite_id,
                            row_id,
                            cx,
                        );
                    });
                }
            } else {
                favorite_window.update(cx, |this, cx| this.add_to_favorites(item_id, cx));
            }
        },
    );

    let pin_window = main_window.clone();
    let pin_button = hover_action_button(
        ("hover-action-pin", row_id as u64),
        if is_pinned { "pinned-filled" } else { "pin" },
        if is_pinned {
            translator.translate("main.menu.unpin")
        } else {
            translator.translate("main.menu.pin")
        },
        if is_pinned {
            theme.blue
        } else {
            theme.muted_foreground
        },
        theme,
        move |_, _, cx| {
            cx.stop_propagation();
            pin_window.update(cx, |this, cx| {
                this.set_item_pinned(row_id, favorite_id, !is_pinned, cx)
            });
        },
    );

    // 收藏列表中的项没有删除操作，只保留收藏按钮。
    let delete_button = (!is_favorites_tab).then(|| {
        let delete_window = main_window.clone();
        hover_action_button(
            ("hover-action-delete", row_id as u64),
            "trash",
            delete_tooltip,
            theme.danger,
            theme,
            move |_, _, cx| {
                cx.stop_propagation();
                delete_window.update(cx, |this, cx| this.delete_item(row_id, cx));
            },
        )
    });

    let buttons = [
        (ItemDisplayField::Edit, edit_button),
        (ItemDisplayField::PinToScreen, Some(pin_to_screen_button)),
        (ItemDisplayField::SplitWords, split_words_button),
        (ItemDisplayField::Note, Some(note_button)),
        (ItemDisplayField::PastePlain, paste_plain_button),
        (ItemDisplayField::OpenDefault, default_open_button),
        (ItemDisplayField::OpenBrowser, browser_button),
        (ItemDisplayField::Favorite, Some(favorite_button)),
        (ItemDisplayField::Pin, Some(pin_button)),
        (ItemDisplayField::Delete, delete_button),
    ]
    .into_iter()
    .filter(|(field, _)| fields.contains(field))
    .filter_map(|(_, button)| button)
    .collect::<Vec<_>>();
    div()
        .size_full()
        .flex()
        .items_center()
        .children(buttons)
        .into_any_element()
}

fn hover_action_button(
    id: impl Into<ElementId>,
    icon_name: &'static str,
    tooltip: impl Into<SharedString>,
    icon_color: gpui::Hsla,
    theme: ThemeColor,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let tooltip = tooltip.into();
    let id = id.into();
    let icon_group = format!("clipboard-hover-action-icon-{id:?}-{icon_name}");
    div()
        .id(id)
        .group(icon_group.clone())
        .size(ui_rem(20.0))
        .flex()
        .items_center()
        .justify_center()
        .flex_none()
        .cursor_pointer()
        .rounded_sm()
        .border_1()
        .border_color(gpui::transparent_black())
        .hover(|style| style.bg(theme.secondary_active).border_color(theme.border))
        .child(hover_icon(icon_group, icon_name, 13.0, icon_color))
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .on_click(on_click)
        .into_any_element()
}

fn can_open_with_default(item: &ClipboardListItem) -> bool {
    item.all_format_options.iter().any(|preview| {
        preview.is_restorable && matches!(preview.role(), FormatRole::FileList | FormatRole::Image)
    })
}

fn has_plain_text_links(item: &ClipboardListItem) -> bool {
    item.all_format_options.iter().any(|preview| {
        preview.is_restorable
            && preview.role() == FormatRole::PlainText
            && preview.content_kind == ContentKind::Link
    })
}

fn has_editable_text(item: &ClipboardListItem) -> bool {
    item.all_format_options
        .iter()
        .any(|preview| preview.is_restorable && preview.role() == FormatRole::PlainText)
}

fn open_browser_links_from_button(
    main_window: Entity<MainWindow>,
    item_id: i64,
    click_position: Point<Pixels>,
    window: &Window,
    cx: &mut App,
) {
    let links = main_window
        .update(cx, |this, _| {
            this.item_plain_text(item_id)
                .ok()
                .flatten()
                .map(|text| extract_links(&text))
        })
        .unwrap_or_default();
    let Some(first_link) = links.first() else {
        return;
    };
    if links.len() == 1 {
        cx.open_url(first_link);
        return;
    }

    let Some(origin) = logical_to_physical_desktop(window, click_position) else {
        tracing::error!("无法确定链接下拉菜单位置，未打开链接选项");
        return;
    };
    let scale_factor = window.scale_factor().max(1.0);
    let converter = crate::platform::window::CoordinateConverter::new(scale_factor);
    let dropdown_items = links
        .iter()
        .enumerate()
        .map(|(index, link)| {
            DropdownItem::option(
                DropdownItemId::new(format!("{OPEN_LINK_ACTION_PREFIX}{index}")),
                truncate_link_label(link),
            )
        })
        .chain(std::iter::once(DropdownItem::option(
            DropdownItemId::new(OPEN_ALL_LINKS_ACTION),
            i18n::translator(cx).translate("main.menu.open_all_links"),
        )))
        .collect();
    let menu = DropdownModel::new(dropdown_items);
    let links = Rc::new(links);
    let anchor = DropdownAnchor::new(
        origin.x,
        origin.y,
        converter.logical_to_physical(20.0),
        converter.logical_to_physical(20.0),
        scale_factor,
    );
    MenuService::open_dropdown_menu(
        menu,
        anchor,
        move |action, cx| {
            if action.as_str() == OPEN_ALL_LINKS_ACTION {
                for link in links.iter() {
                    cx.open_url(link);
                }
                return;
            }
            if let Some(index) = action
                .as_str()
                .strip_prefix(OPEN_LINK_ACTION_PREFIX)
                .and_then(|value| value.parse::<usize>().ok())
                && let Some(link) = links.get(index)
            {
                cx.open_url(link);
            }
        },
        cx,
    );
}

fn render_selection_indicator(
    row_id: HistoryItemId,
    selection_number: Option<usize>,
    list_item_style: ListItemStyle,
    theme: ThemeColor,
    selection_window: Option<Entity<MainWindow>>,
    hover_allowed: bool,
) -> AnyElement {
    let group_name = format!("clipboard-row-hover-{row_id}");
    let mut indicator = div()
        .absolute()
        .top(px(0.0))
        .left(px(0.0))
        .w_auto()
        .min_w(ui_rem(13.0))
        .h(ui_rem(13.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_br(px(6.0))
        .border_r_1()
        .border_b_1()
        .border_color(theme.border)
        .bg(theme.secondary);
    if list_item_style == ListItemStyle::Card {
        indicator = indicator.rounded_tl(px(6.0));
    }
    let marker = if let Some(number) = selection_number {
        div()
            .min_w(ui_rem(11.0))
            .h(ui_rem(11.0))
            .px(px(1.0))
            .flex()
            .items_center()
            .justify_center()
            .flex_none()
            .rounded_full()
            .border_1()
            .border_color(theme.border)
            .bg(theme.primary)
            .text_size(ui_rem(9.0))
            .text_color(theme.primary_foreground)
            .child(number.to_string())
            .into_any_element()
    } else {
        indicator = indicator.invisible().border_color(theme.border);
        if hover_allowed {
            indicator = indicator.group_hover(group_name, |style| style.visible());
        }
        div()
            .size(ui_rem(11.0))
            .rounded_full()
            .border_1()
            .border_color(theme.muted_foreground)
            .bg(theme.background)
            .into_any_element()
    };
    indicator = indicator.child(marker);
    if let Some(selection_window) = selection_window {
        let mut indicator = indicator
            .id(("clipboard-selection", row_id as u64))
            .cursor_pointer()
            .on_click(move |event: &ClickEvent, _, cx| {
                if !event.standard_click() {
                    return;
                }
                cx.stop_propagation();
                selection_window.update(cx, |this, cx| {
                    this.select_list_item_with_modifiers(row_id, event.modifiers());
                    cx.notify();
                });
            });
        if selection_number.is_none() {
            indicator = indicator.tooltip(|window, cx| Tooltip::new("点击选中").build(window, cx));
        }
        return indicator.into_any_element();
    }
    indicator.into_any_element()
}

fn image_meta_byte_length(
    format_kind: FormatKind,
    source: Option<&ImagePreviewSource>,
    loaded_byte_length: Option<u64>,
) -> Option<u64> {
    if format_kind == FormatKind::FileList {
        // 文件列表原始数据是路径，体积必须使用实际解码的图片文件大小。
        return loaded_byte_length;
    }
    loaded_byte_length.or_else(|| match source? {
        ImagePreviewSource::Inline { bytes } => Some(bytes.len() as u64),
        ImagePreviewSource::External { byte_length, .. } => Some(*byte_length),
    })
}

fn render_item_surface<T>(
    item: &ClipboardListItem,
    selected: bool,
    selection_number: Option<usize>,
    metrics: ListItemMetrics,
    list_item_style: ListItemStyle,
    display_preview: Option<&FormatPreview>,
    html_images: &HashMap<PathBuf, Arc<RenderImage>>,
    image: Option<LoadedImagePreview>,
    source_app_icon: Option<Arc<RenderImage>>,
    file_list: Option<Entity<FileListView>>,
    width: Option<Pixels>,
    drag_preview: bool,
    hover_actions: Option<(AnyElement, usize)>,
    selection_window: Option<Entity<MainWindow>>,
    hover_allowed: bool,
    cx: &mut Context<T>,
) -> gpui::Div {
    let translator = i18n::translator(cx);
    let display = config_runtime::appearance_config(cx).item_display;
    let row_hover_group = format!("clipboard-row-hover-{}", item.history_id);
    let note_only = metrics.shows_only_note(&item.note_text);
    let show_note_header = !item.note_text.is_empty() && !note_only;
    let content_metrics = if show_note_header {
        metrics.with_preview_height(
            (metrics.preview_height - metrics.preview_line_height).max(metrics.preview_line_height),
            metrics.preview_line_count.saturating_sub(1).max(1),
        )
    } else {
        metrics
    };
    let shortcut_number = item
        .shortcut
        .strip_prefix("Ctrl+")
        .unwrap_or_default()
        .to_owned();
    let length = if let Some(file_list) = file_list.as_ref() {
        file_list_meta_label(&translator, file_list.read(cx).path_count())
    } else if display_preview.is_some_and(|preview| preview.content_kind == ContentKind::Image) {
        image_meta_label(
            &translator,
            display_preview.and_then(|preview| preview.image_dimensions),
        )
    } else {
        translator.translate_with_args(
            "main.preview.char_count",
            &[(
                "count",
                display_preview
                    .map(|preview| preview.char_count)
                    .unwrap_or(item.preview_char_count)
                    .to_string(),
            )],
        )
    };
    let image_size = display_preview
        .filter(|preview| {
            preview.content_kind == ContentKind::Image
                && matches!(preview.role(), FormatRole::Image | FormatRole::FileList)
        })
        .filter(|_| display.enabled(ItemDisplayField::ImageSize))
        .map(|preview| {
            let bytes = image_meta_byte_length(
                preview.format_kind,
                preview.image_source.as_ref(),
                image.as_ref().and_then(|loaded| loaded.byte_length),
            );
            i18n::file_size(&translator, bytes, false)
        });
    let theme = cx.theme().colors;
    let info_bar_background = if drag_preview {
        gpui::transparent_black()
    } else if !metrics.horizontal {
        let mut background = theme.popover;
        background.alpha = 0.5;
        crate::window_material::overlay_surface_color(cx, background)
    } else {
        theme.popover
    };
    let (hover_actions, action_count) = hover_actions
        .map(|(actions, count)| (Some(actions), count))
        .unwrap_or((None, 0));
    let show_source = display.toolbar_visibility != ItemRegionVisibility::Hidden
        && display.enabled(ItemDisplayField::Source);
    let show_shortcut = display.toolbar_visibility != ItemRegionVisibility::Hidden
        && display.enabled(ItemDisplayField::Shortcut)
        && !shortcut_number.is_empty();
    let identity_count = usize::from(show_source) + usize::from(show_shortcut);
    let has_toolbar = action_count + identity_count > 0;
    let mut identity = div()
        .h_full()
        .when(!display.toolbar_visibility.is_visible(selected), |this| {
            this.w_0()
        })
        .flex_none()
        .flex()
        .items_center()
        .overflow_hidden()
        .when(show_source, |this| {
            this.child(source_application_icon(
                item,
                source_app_icon,
                theme,
                &translator,
                metrics.horizontal,
            ))
        })
        .when(show_shortcut, |this| {
            this.child(
                div()
                    .h_full()
                    .min_w(ui_rem(20.0))
                    .px(ui_rem(4.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .whitespace_nowrap()
                    .text_xs()
                    .text_color(theme.primary)
                    .child(shortcut_number),
            )
        });
    if hover_allowed && display.toolbar_visibility == ItemRegionVisibility::Active {
        identity = identity.group_hover(row_hover_group.clone(), |style| style.w_auto());
    }
    let mut info_bar = div()
        .absolute()
        .top_0()
        .right_0()
        .h(ui_rem(20.0))
        .flex_none()
        .max_w_full()
        .flex()
        .items_center()
        .overflow_hidden()
        .border_l_1()
        .border_b_1()
        .border_color(theme.border)
        .bg(info_bar_background)
        .rounded_bl(px(6.0))
        .when(!metrics.horizontal, |this| {
            crate::window_material::with_backdrop_blur(this, cx)
        })
        .children(hover_actions)
        .when(identity_count > 0, |this| this.child(identity));
    let toolbar_visible = (identity_count > 0 && display.toolbar_visibility.is_visible(selected))
        || (action_count > 0 && display.actions_visibility.is_visible(selected));
    if !toolbar_visible {
        info_bar = info_bar.invisible();
        if hover_allowed
            && ((identity_count > 0 && display.toolbar_visibility == ItemRegionVisibility::Active)
                || (action_count > 0 && display.actions_visibility == ItemRegionVisibility::Active))
        {
            info_bar = info_bar.group_hover(row_hover_group.clone(), |style| style.visible());
        }
    }
    if list_item_style == ListItemStyle::Card {
        info_bar = info_bar.rounded_tr(px(CARD_RADIUS));
    }
    let inline_info_bar = !metrics.horizontal && !item.note_text.is_empty() && !metrics.show_meta;
    if inline_info_bar {
        info_bar = info_bar.relative().h(metrics.preview_line_height);
    }
    let mut info_bar = has_toolbar.then_some(info_bar);

    let mut preview = div()
        .flex_none()
        .min_h_0()
        .min_w_0()
        .h(content_metrics.preview_height)
        .w_full()
        .overflow_hidden()
        .text_color(cx.theme().foreground);
    if note_only {
        preview = preview.flex().items_start().child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_size(metrics.preview_font_size)
                .line_height(metrics.preview_line_height)
                .line_clamp(metrics.preview_line_count)
                .child(item.note_text.clone()),
        );
        if inline_info_bar {
            preview = preview.children(info_bar.take());
        }
    } else if let Some(file_list) = file_list {
        if show_note_header && metrics.height != ListItemHeight::Adaptive {
            preview = preview
                .text_size(metrics.preview_font_size)
                .line_height(metrics.preview_line_height)
                .line_clamp(content_metrics.preview_line_count)
                .text_color(theme.muted_foreground)
                .child(file_list.read(cx).preview_summary());
        } else {
            preview = preview.child(file_list);
        }
    } else if let Some(loaded) = image {
        preview = preview.child(
            div()
                .w_full()
                .h(content_metrics.preview_height)
                .min_w_0()
                .min_h_0()
                .overflow_hidden()
                .when(metrics.horizontal, |view| {
                    view.flex()
                        .items_center()
                        .justify_center()
                        .rounded_md()
                        .bg(theme.background)
                })
                .child(
                    img(loaded.image)
                        .h(content_metrics.preview_height)
                        .max_w_full()
                        .max_h(content_metrics.preview_height)
                        .min_w_0()
                        .min_h_0()
                        .object_fit(ObjectFit::Contain),
                ),
        );
    } else if let Some(html_preview) = display_preview
        && html_preview.role() == FormatRole::Html
        && let Some(html) = html_preview.html_content.as_deref()
    {
        let html_background = if drag_preview {
            gpui::transparent_black()
        } else {
            theme.background
        };
        preview =
            preview
                .bg(html_background)
                .child(html_preview::render_with_images_and_max_lines(
                    html,
                    metrics.preview_font_size,
                    metrics.preview_line_height,
                    translator.translate("main.format.image_html"),
                    html_images,
                    None,
                    // 自适应行高按固定的图片上限估算高度，渲染时必须使用同一上限。
                    if metrics.height == ListItemHeight::Adaptive {
                        ADAPTIVE_IMAGE_PREVIEW_LINES
                    } else {
                        content_metrics.preview_line_count
                    },
                ));
    } else {
        preview = preview
            .text_size(metrics.preview_font_size)
            .line_height(metrics.preview_line_height)
            .line_clamp(content_metrics.preview_line_count)
            .text_color(if show_note_header {
                theme.muted_foreground
            } else {
                theme.foreground
            })
            .child(
                display_preview
                    .map(|preview| preview.text.clone())
                    .unwrap_or_else(|| item.preview_text.clone()),
            );
    }

    let mut meta_row = div()
        .h(metrics.meta_row_height)
        .w_full()
        .min_w_0()
        .flex_none()
        .flex()
        .items_center()
        .gap(ui_rem(6.0))
        .text_size(metrics.meta_font_size)
        .line_height(metrics.meta_row_height)
        .text_color(cx.theme().muted_foreground)
        .overflow_hidden()
        .whitespace_nowrap()
        .when(display.enabled(ItemDisplayField::Time), |this| {
            this.child(
                div()
                    .flex_shrink_0()
                    .child(i18n::relative_time(&translator, item.last_seen_at)),
            )
        })
        .when(display.enabled(ItemDisplayField::Length), |this| {
            this.child(div().min_w_0().truncate().child(length))
        })
        .children(image_size.map(|label| div().flex_shrink_0().child(label)));
    if display.info_visibility == ItemRegionVisibility::Active && !selected {
        meta_row = meta_row.invisible();
        if hover_allowed {
            meta_row = meta_row.group_hover(row_hover_group.clone(), |style| style.visible());
        }
    }
    let mut meta_row = Some(meta_row);
    let mut row = div()
        .h(metrics.content_height)
        .flex_none()
        .relative()
        .flex()
        .flex_col()
        .group(row_hover_group)
        .cursor_pointer();
    row = if let Some(width) = width {
        row.w(width)
    } else {
        row.w_full()
    };
    row = row.px(ui_rem(10.0)).py(metrics.vertical_padding);
    if metrics.horizontal {
        let surface_theme = crate::window_material::excluded_window_theme(cx);
        let mut background = surface_theme.background;
        background.alpha = 0.5;
        row = crate::window_material::with_backdrop_blur(row.bg(background), cx);
        if has_toolbar {
            row = row.child(
                div()
                    .w_full()
                    .h(metrics.rem_size * 1.5)
                    .mb(metrics.rem_size * 0.5)
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px(ui_rem(6.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(if show_source {
                                if item.source_device_name.is_some() {
                                    i18n::clipboard_source(
                                        &translator,
                                        item.source_app_name.as_deref(),
                                        item.source_device_name.as_deref(),
                                    )
                                } else {
                                    item.source_app_name.clone().unwrap_or_default()
                                }
                            } else {
                                String::new()
                            }),
                    )
                    .children(info_bar.take().map(|bar| {
                        bar.relative()
                            .h_full()
                            .border_0()
                            .rounded_none()
                            .bg(gpui::transparent_black())
                    })),
            );
        }
    }
    if metrics.show_meta && !metrics.horizontal {
        row = row.children(meta_row.take());
    }
    if show_note_header {
        let mut header = div()
            .w_full()
            .h(metrics.preview_line_height)
            .flex_none()
            .flex()
            .items_start()
            .text_size(metrics.preview_font_size)
            .line_height(metrics.preview_line_height)
            .font_weight(gpui::FontWeight::MEDIUM)
            .child(
                div().flex_1().min_w_0().truncate().child(
                    item.note_text
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" "),
                ),
            );
        if inline_info_bar {
            header = header.children(info_bar.take());
        }
        row = row.child(header);
    }
    row = row.child(preview);
    if metrics.horizontal && metrics.show_meta {
        row = row.children(meta_row.take().map(|mut meta| {
            if item.paste_count > 0 {
                meta = meta.child(div().flex_shrink_0().child(translator.translate_with_args(
                    "main.preview.info.paste_count_value",
                    &[("count", item.paste_count.to_string())],
                )));
            }
            meta.mt(metrics.rem_size * 0.25)
        }));
    }
    row = row
        .children(info_bar)
        .child(
            // 用于在预绘制阶段取得整行边界，滚动时鼠标不移动也能同步选中项。
            div().absolute().top_0().left_0().right_0().bottom_0(),
        )
        .child(render_selection_indicator(
            item.history_id,
            selection_number,
            list_item_style,
            theme,
            selection_window.clone(),
            hover_allowed,
        ));

    let item_border_color = if selected && list_item_style == ListItemStyle::Card {
        cx.theme().list_active_border
    } else {
        cx.theme().border
    };
    row = match list_item_style {
        ListItemStyle::Seamless if metrics.horizontal => {
            row.border_r_1().border_color(item_border_color)
        }
        ListItemStyle::Seamless => row.border_b_1().border_color(item_border_color),
        ListItemStyle::Card => row
            .overflow_hidden()
            .rounded(px(CARD_RADIUS))
            .border(px(1.5))
            .border_color(item_border_color),
    };
    if !drag_preview && hover_allowed {
        let hover_color = if metrics.horizontal {
            let surface_theme = crate::window_material::excluded_window_theme(cx);
            let mut color = surface_theme.background;
            color.alpha = 1.0;
            color
        } else {
            theme.list_hover
        };
        if crate::config_runtime::animation_enabled(cx, crate::config::AnimationKind::ListItemHover)
        {
            row = row.group_hover(
                format!("clipboard-item-hover-{}", item.history_id),
                move |this| this.bg(hover_color),
            );
        } else {
            row = row.hover(move |this| this.bg(hover_color));
        }
    }
    if selected && list_item_style == ListItemStyle::Seamless {
        row = row.child(
            div()
                .absolute()
                .top_0()
                .left(px(1.5))
                .right(px(1.5))
                .bottom_0()
                .border(px(1.5))
                .border_color(cx.theme().list_active_border),
        );
    }
    if selected && !drag_preview {
        if let Some(main_window) = selection_window {
            row = row.child(render_tag_item_drop_overlay(
                item.item_id,
                &item.tag_ids,
                theme,
                main_window,
            ));
        }
    }
    row
}

fn render_item_border(
    list_item_style: ListItemStyle,
    color: Option<gpui::Background>,
    horizontal: bool,
) -> Option<AnyElement> {
    let color = color?;

    let mut border = div().absolute().inset_0().border_color(color);
    border = if horizontal {
        border.border_t_3()
    } else {
        border.border_l_3()
    };
    if list_item_style == ListItemStyle::Seamless {
        border = if horizontal {
            border.top(px(1.0))
        } else {
            border.left(px(1.0))
        };
    }
    if list_item_style == ListItemStyle::Card {
        border = border.rounded(px(CARD_RADIUS));
    }
    Some(border.into_any_element())
}

fn item_border_color(
    is_pinned: bool,
    paste_count: u64,
    (pinned_border, pasted_border): (bool, bool),
    theme: ThemeColor,
    horizontal: bool,
) -> Option<gpui::Background> {
    match (is_pinned && pinned_border, paste_count > 0 && pasted_border) {
        (true, true) => Some(linear_gradient(
            if horizontal { 90.0 } else { 0.0 },
            linear_color_stop(theme.yellow, 0.49),
            linear_color_stop(theme.blue, 0.51),
        )),
        (true, false) => Some(theme.blue.into()),
        (false, true) => Some(theme.yellow.into()),
        (false, false) => None,
    }
}

fn render_drag_preview(
    drag: &ListItemDrag,
    html_images: &HashMap<PathBuf, Arc<RenderImage>>,
    shortcut: &str,
    cx: &mut Context<ListItemDragPreview>,
) -> AnyElement {
    let Some(preview_size) = drag.preview_size.get() else {
        return div().into_any_element();
    };
    let theme = crate::window_material::excluded_window_theme(cx);
    let preview_background =
        gpui::ColorExt::opacity(&theme.background, DRAG_PREVIEW_BACKGROUND_OPACITY);
    let mut item = drag.item.clone();
    item.shortcut = shortcut.to_owned();
    let row_width = if drag.list_item_style == ListItemStyle::Card {
        let inset = if drag.metrics.horizontal {
            f32::from(drag.metrics.horizontal_card_inset)
        } else {
            CARD_HORIZONTAL_INSET
        };
        px((f32::from(preview_size.width) - inset * 2.0).max(1.0))
    } else {
        preview_size.width
    };
    let surface = render_item_surface(
        &item,
        drag.selected,
        drag.selection_number,
        drag.metrics,
        drag.list_item_style,
        drag.display_preview.as_ref(),
        html_images,
        drag.image.clone(),
        drag.source_app_icon.clone(),
        drag.file_list.clone(),
        Some(row_width),
        true,
        None,
        None,
        false,
        cx,
    );
    let surface = if drag.list_item_style == ListItemStyle::Card {
        surface.bg(preview_background)
    } else {
        surface
    };
    let row = div()
        .relative()
        .h(drag.metrics.content_height)
        .w(row_width)
        .flex_none()
        .child(surface)
        .children(render_item_border(
            drag.list_item_style,
            item_border_color(
                item.is_pinned,
                item.paste_count,
                crate::config_runtime::item_status_borders(cx),
                theme,
                drag.metrics.horizontal,
            ),
            drag.metrics.horizontal,
        ));
    let card_inset_x = if drag.metrics.horizontal {
        drag.metrics.horizontal_card_inset
    } else {
        px(CARD_HORIZONTAL_INSET)
    };
    let front = if drag.list_item_style == ListItemStyle::Card {
        div()
            .h(preview_size.height)
            .w(preview_size.width)
            .flex_none()
            .px(card_inset_x)
            .py(drag.metrics.card_vertical_inset)
            .child(row)
    } else {
        div()
            .relative()
            .h(preview_size.height)
            .w(preview_size.width)
            .bg(preview_background)
            .child(row)
    };

    let stack_count = drag.stack_count.get();
    if stack_count <= 1 {
        return front.into_any_element();
    }

    // 多选时在当前项后方叠放错位卡片并显示数量，避免平铺多项干扰视线。
    let (card_origin, card_size, radius) = if drag.list_item_style == ListItemStyle::Card {
        (
            point(card_inset_x, drag.metrics.card_vertical_inset),
            size(row_width, drag.metrics.content_height),
            px(CARD_RADIUS),
        )
    } else {
        (point(px(0.0), px(0.0)), preview_size, px(0.0))
    };
    let layer_count = (stack_count - 1).min(DRAG_STACK_MAX_LAYERS);
    let stack_offset = px(drag_stack_offset(stack_count));
    let layer = |offset: Pixels| {
        div()
            .absolute()
            .left(card_origin.x + offset)
            .top(card_origin.y + offset)
            .w(card_size.width)
            .h(card_size.height)
            .rounded(radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
    };
    let badge = div()
        .absolute()
        .top(card_origin.y + px(4.0))
        .right(stack_offset + preview_size.width - card_origin.x - card_size.width + px(4.0))
        .min_w(ui_rem(18.0))
        .h(ui_rem(18.0))
        .px(ui_rem(5.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(theme.primary)
        .text_size(ui_rem(11.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme.primary_foreground)
        .child(stack_count.to_string());
    div()
        .relative()
        .w(preview_size.width + stack_offset)
        .h(preview_size.height + stack_offset)
        .children(
            (1..=layer_count)
                .rev()
                .map(|depth| layer(px(depth as f32 * DRAG_STACK_STEP))),
        )
        // 当前项的背景是半透明的，垫一层不透明底色，避免透出后方卡片的边框。
        .child(layer(px(0.0)).border_0())
        .child(div().absolute().top_0().left_0().child(front))
        .child(badge)
        .into_any_element()
}

#[cfg(target_os = "windows")]
fn drag_preview_bounds(
    window: &Window,
    cx: &mut App,
    cursor_offset: Point<Pixels>,
    preview_size: Size<Pixels>,
) -> Option<(Bounds<Pixels>, Option<gpui::DisplayId>, Point<f32>)> {
    let mouse_position = window.mouse_position();
    let window_origin = crate::platform::window::logical_client_origin_on_desktop(window)?;
    let scale_factor = window.scale_factor().max(1.0);
    let converter = crate::platform::window::CoordinateConverter::new(scale_factor);
    let cursor_offset_physical = point(
        converter.logical_to_physical(f32::from(cursor_offset.x)),
        converter.logical_to_physical(f32::from(cursor_offset.y)),
    );
    let bounds = Bounds::new(
        point(
            px(f32::from(window_origin.x) + f32::from(mouse_position.x)
                - f32::from(cursor_offset.x)),
            px(f32::from(window_origin.y) + f32::from(mouse_position.y)
                - f32::from(cursor_offset.y)),
        ),
        preview_size,
    );
    let display_id = window.display(cx).map(|display| display.id());
    Some((bounds, display_id, cursor_offset_physical))
}

fn handle_row_hover(
    this: &mut MainWindow,
    hovered: bool,
    row_id: HistoryItemId,
    preview_anchor: &Cell<Option<PreviewAnchor>>,
    preview_item: &ClipboardListItem,
    preview_source: PreviewSource,
    preview_priority: ClipboardDisplayPriority,
    window: &mut Window,
    cx: &mut Context<MainWindow>,
) {
    if hovered {
        if this.can_follow_mouse_hover() && this.selected_item_id != Some(row_id) {
            this.select_item(row_id);
            cx.notify();
        }
        if !config_runtime::hover_preview_enabled(cx) {
            this.cancel_hover_preview(cx);
            return;
        }
        if this.preview_hover_source == Some(preview_source) {
            return;
        }
        if let Some(anchor) = preview_anchor.get() {
            this.show_hover_preview(
                preview_item.clone(),
                preview_source,
                anchor,
                PreviewRect::from_window(window),
                preview_priority,
                cx,
            );
        } else {
            this.begin_hover_preview(preview_source, cx);
        }
    } else {
        this.hide_hover_preview(preview_source, cx);
    }
}

fn with_row_hover(
    row: gpui::Stateful<gpui::Div>,
    row_id: HistoryItemId,
    preview_anchor: Rc<Cell<Option<PreviewAnchor>>>,
    preview_item: ClipboardListItem,
    preview_source: PreviewSource,
    preview_priority: ClipboardDisplayPriority,
    cx: &mut Context<MainWindow>,
) -> gpui::Stateful<gpui::Div> {
    let hover_anchor = preview_anchor.clone();
    let hover_item = preview_item.clone();
    let main_window = cx.entity().downgrade();
    row.on_hover(cx.listener(move |this, hovered: &bool, window, cx| {
        if !*hovered || (window.is_window_hovered() && MenuService::hover_animation_allowed(cx)) {
            handle_row_hover(
                this,
                *hovered,
                row_id,
                &hover_anchor,
                &hover_item,
                preview_source,
                preview_priority,
                window,
                cx,
            );
        }
    }))
    .on_mouse_move_all(move |event, phase, hitbox, window, cx| {
        if phase != gpui::DispatchPhase::Capture
            || event.pressed_button.is_some()
            || cx.has_active_drag()
            || !window.is_window_hovered()
        {
            return;
        }
        let hovered = hitbox.is_hovered(window);
        let Some(main_window) = main_window.upgrade() else {
            return;
        };
        let is_current_source = main_window.read(cx).preview_hover_source == Some(preview_source);
        if hovered == is_current_source
            || !MenuService::hover_animation_allowed(cx)
            || !config_runtime::hover_preview_enabled(cx)
        {
            return;
        }
        let _ = main_window.update(cx, |this, cx| {
            // 按当前命中结果恢复预览，不依赖 GPUI 是否再次发出悬停状态变化。
            if hovered {
                handle_row_hover(
                    this,
                    true,
                    row_id,
                    &preview_anchor,
                    &preview_item,
                    preview_source,
                    preview_priority,
                    window,
                    cx,
                );
            } else {
                this.hide_hover_preview(preview_source, cx);
            }
        });
    })
}

/// 鼠标点击行时执行操作所需的条目信息，左键与中键共用。
#[derive(Clone)]
struct RowMouseTarget {
    main_window: gpui::Entity<MainWindow>,
    row_id: HistoryItemId,
    item_id: ClipboardItemId,
    favorite_id: Option<i64>,
    is_pinned: bool,
}

fn run_row_mouse_behavior(
    this: &mut MainWindow,
    behavior: ClipboardMouseBehavior,
    click_count: usize,
    target: &RowMouseTarget,
    window: &mut Window,
    cx: &mut Context<MainWindow>,
) {
    if !behavior.matches_click_count(click_count) {
        return;
    }
    let action = behavior.action();
    // 切换选中状态本身就是修改选中集合，不能先单选当前项。
    if action != ClipboardMouseAction::SelectItem {
        this.select_item(target.row_id);
    }
    match action {
        ClipboardMouseAction::Paste => this.paste_item(target.item_id, cx),
        ClipboardMouseAction::PlainTextPaste => {
            this.paste_selected_item_as_plain_text(false, cx);
        }
        ClipboardMouseAction::Copy => this.copy_item(target.item_id, cx),
        ClipboardMouseAction::SelectItem => {
            this.select_list_item_with_modifiers(target.row_id, Default::default());
        }
        ClipboardMouseAction::PinItem => {
            this.set_item_pinned(target.row_id, target.favorite_id, !target.is_pinned, cx);
        }
        ClipboardMouseAction::FavoriteItem => {
            if let Some(favorite_id) = target.favorite_id {
                let main_window = target.main_window.clone();
                let owner_window = window.window_handle();
                let row_id = target.row_id;
                cx.defer(move |cx| {
                    confirm_remove_from_favorites(
                        main_window,
                        owner_window,
                        favorite_id,
                        row_id,
                        cx,
                    );
                });
            } else {
                this.add_to_favorites(target.item_id, cx);
            }
        }
    }
    cx.notify();
}

fn render_row(
    item: ClipboardListItem,
    can_reorder: bool,
    selected_item_id: Option<HistoryItemId>,
    selection_number: Option<usize>,
    metrics: ListItemMetrics,
    list_item_style: ListItemStyle,
    display_priority: ClipboardDisplayPriority,
    preview_priority: ClipboardDisplayPriority,
    display_preview: Option<FormatPreview>,
    html_images: &HashMap<PathBuf, Arc<RenderImage>>,
    image: Option<LoadedImagePreview>,
    source_app_icon: Option<Arc<RenderImage>>,
    file_list: Option<gpui::Entity<FileListView>>,
    active_tab: TabId,
    tags: &[Tag],
    reorder_animation: Option<(u64, Pixels)>,
    load_animation: Option<ListItemLoadAnimation>,
    load_offset: Pixels,
    hover_allowed: bool,
    window: &Window,
    cx: &mut Context<MainWindow>,
) -> AnyElement {
    let list_item_load_enabled =
        crate::config_runtime::animation_enabled(cx, crate::config::AnimationKind::ListItemLoad);
    let list_item_reorder_enabled =
        crate::config_runtime::animation_enabled(cx, crate::config::AnimationKind::ListItemReorder);
    let tag_indicator_hover_enabled = crate::config_runtime::animation_enabled(
        cx,
        crate::config::AnimationKind::TagIndicatorHover,
    );
    let focused = Some(item.history_id) == selected_item_id;
    let selected = focused || selection_number.is_some();
    let drag_image = image.clone();
    let drag_source_app_icon = source_app_icon.clone();
    let drag_file_list = file_list.clone();
    let drag_display_preview = display_preview.clone();
    let row_id = item.history_id;
    let hover_group = format!("clipboard-item-hover-{row_id}");
    let row_group = if list_item_style == ListItemStyle::Card {
        format!("clipboard-row-hit-area-{row_id}")
    } else {
        hover_group.clone()
    };
    let main_window = cx.entity();
    let hover_actions = render_hover_actions(&item, active_tab, selected, hover_allowed, cx);
    let mut row = render_item_surface(
        &item,
        selected,
        selection_number,
        metrics,
        list_item_style,
        display_preview.as_ref(),
        html_images,
        image,
        source_app_icon,
        file_list,
        None,
        false,
        hover_actions,
        Some(main_window.clone()),
        hover_allowed,
        cx,
    );
    let target_is_pinned = item.is_pinned;
    let preview_anchor = Rc::new(Cell::new(None));
    let preview_item = item.clone();
    let preview_source = if active_tab == TabId::Favorites {
        item.favorite_id
            .map(PreviewSource::Favorite)
            .unwrap_or(PreviewSource::History(item.history_id))
    } else {
        PreviewSource::History(item.history_id)
    };

    if !focused && hover_allowed {
        let selection_window = main_window.clone();
        row = row.on_children_prepainted(move |children_bounds, window, _| {
            if !window.is_window_hovered() {
                return;
            }
            if !children_bounds
                .iter()
                .any(|bounds| bounds.contains(&window.mouse_position()))
            {
                return;
            }

            let hitboxes = children_bounds
                .into_iter()
                .map(|bounds| window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal))
                .collect::<Vec<_>>();
            let selection_window = selection_window.clone();
            window.on_next_frame(move |window, cx| {
                // 完整帧生成后再检查命中框，让标签面板等遮挡层的 occlude 生效。
                if !window.is_window_hovered()
                    || !hitboxes.iter().any(|hitbox| hitbox.is_hovered(window))
                    || !MenuService::hover_animation_allowed(cx)
                {
                    return;
                }
                let _ = selection_window.update(cx, |this, cx| {
                    if this.selected_item_id != Some(row_id) && this.can_follow_mouse_hover() {
                        this.select_item(row_id);
                        cx.notify();
                    }
                });
            });
        });
    }

    let item_id = item.item_id;
    let favorite_id = item.favorite_id;
    let item_tag_ids = Rc::new(RefCell::new(item.tag_ids.clone()));
    // 右键菜单在 on_mouse_down 中现场构建：此时才能取到完整纯文本来解析链接。
    let menu_item = item.clone();
    let menu_tags = tags.to_vec();
    let mut row = row.id(("clipboard-row", row_id as u64));
    let tag_ids_for_drop_style = item_tag_ids.borrow().clone();
    let tag_ids_for_drop_check = item_tag_ids.borrow().clone();
    row = row
        .drag_over::<TagDrag>(move |this, drag, _, cx| {
            if !tag_ids_for_drop_style.contains(&drag.tag_id) {
                this.bg(cx.theme().drop_target)
            } else {
                this
            }
        })
        .can_drop(move |dragged, _, _| {
            if let Some(drag) = dragged.downcast_ref::<ListItemDrag>() {
                return can_reorder
                    && drag.source_tab == active_tab
                    && !drag.is_multi()
                    && drag.is_pinned == target_is_pinned;
            }
            dragged
                .downcast_ref::<TagDrag>()
                .is_some_and(|drag| !tag_ids_for_drop_check.contains(&drag.tag_id))
        })
        .on_drop(cx.listener(move |this, drag: &TagDrag, _, cx| {
            #[cfg(target_os = "windows")]
            DragPreviewService::hide(cx);
            #[cfg(target_os = "windows")]
            ExternalDragService::cancel(cx);
            cx.stop_propagation();
            if this.cancel_tag_reorder_preview() {
                cx.notify();
            }
            this.add_to_tag(item_id, drag.tag_id, cx);
        }));
    let preview_size = Rc::new(Cell::new(None));
    if can_reorder {
        let drag_data = ListItemDrag {
            source_tab: active_tab,
            history_id: row_id,
            is_pinned: target_is_pinned,
            selected,
            selection_number,
            item: item.clone(),
            metrics,
            list_item_style,
            display_preview: drag_display_preview,
            image: drag_image,
            source_app_icon: drag_source_app_icon,
            file_list: drag_file_list,
            preview_size: preview_size.clone(),
            stack_count: Rc::new(Cell::new(1)),
        };
        let drag_window = main_window.clone();
        if list_item_style == ListItemStyle::Seamless {
            let preview_size = preview_size.clone();
            row = row.on_prepaint(move |prepaint, _, _| {
                preview_size.set(Some(prepaint.bounds.size));
            });
        }
        row = row
            .on_drag(
                drag_data,
                move |drag: &ListItemDrag, cursor_offset, window, cx| {
                    cx.stop_propagation();
                    PreviewWindowService::hide(cx);
                    let html_images = drag_window.read(cx).loaded_html_images.clone();
                    // 从多选集合中的一项开始拖拽时，整组作为一次外部拖拽，不做列表内重排。
                    let multi_selected = &drag_window.read(cx).multi_selected_item_ids;
                    let stack_count =
                        if multi_selected.len() > 1 && multi_selected.contains(&drag.history_id) {
                            multi_selected.len()
                        } else {
                            1
                        };
                    drag.stack_count.set(stack_count);
                    if !drag.is_multi() {
                        drag_window.update(cx, |this, cx| {
                            this.begin_reorder_drag(drag.history_id);
                            cx.notify();
                        });
                    }
                    #[cfg(target_os = "windows")]
                    let Some(preview_size) = drag.preview_size.get() else {
                        return cx.new(|_| ListItemDragPreview {
                            drag: drag.clone(),
                            html_images,
                            render_preview: true,
                        });
                    };
                    #[cfg(target_os = "windows")]
                    let Some((initial_bounds, display_id, cursor_offset_physical)) =
                        drag_preview_bounds(window, cx, cursor_offset, {
                            let stack_offset = px(drag_stack_offset(stack_count));
                            size(
                                preview_size.width + stack_offset,
                                preview_size.height + stack_offset,
                            )
                        })
                    else {
                        return cx.new(|_| ListItemDragPreview {
                            drag: drag.clone(),
                            html_images,
                            render_preview: true,
                        });
                    };
                    #[cfg(not(target_os = "windows"))]
                    let cursor_offset_physical = {
                        let _ = cursor_offset;
                        point(0.0, 0.0)
                    };
                    let source_preview = cx.new(|_| ListItemDragPreview {
                        drag: drag.clone(),
                        html_images: html_images.clone(),
                        render_preview: cfg!(not(target_os = "windows")),
                    });
                    #[cfg(target_os = "windows")]
                    {
                        let popup_preview = cx.new(|_| ListItemDragPreview {
                            drag: drag.clone(),
                            html_images,
                            render_preview: true,
                        });
                        let source_window = window.window_handle();
                        if let Err(error) = DragPreviewService::start(
                            source_window,
                            popup_preview,
                            initial_bounds,
                            display_id,
                            cursor_offset_physical,
                            drag.metrics.rem_size,
                            {
                                let drag_window = drag_window.clone();
                                move |cx| {
                                    let _ = drag_window.update(cx, |this, cx| {
                                        this.finish_list_drag_preview(cx);
                                    });
                                }
                            },
                            cx,
                        ) {
                            tracing::error!("启动拖拽预览失败：{error}");
                        }
                        if drag.is_multi() {
                            match drag_window.read(cx).load_multi_drag_payloads() {
                                Ok((payloads, image_files)) => {
                                    if let Err(error) = ExternalDragService::begin_with_files(
                                        source_window,
                                        payloads,
                                        image_files,
                                        cx,
                                    ) {
                                        tracing::error!("准备多选外部拖拽失败：{error}");
                                    }
                                }
                                Err(error) => {
                                    tracing::error!("读取多选外部拖拽内容失败：{error}");
                                    let translator = i18n::translator(cx);
                                    crate::windows::notification::NotificationService::show_message(
                                        translator.translate("main.multi_selection.drag"),
                                        format!(
                                            "{}：{error}",
                                            translator
                                                .translate("main.multi_selection.drag_failed")
                                        ),
                                        crate::windows::notification::NotificationLevel::Error,
                                        cx,
                                    );
                                }
                            }
                        } else {
                            match drag_window.read(cx).load_drag_payloads(drag.item.item_id) {
                                Ok(payloads) => {
                                    if let Err(error) =
                                        ExternalDragService::begin(source_window, payloads, cx)
                                    {
                                        tracing::error!("准备外部拖拽失败：{error}");
                                    }
                                }
                                Err(error) => tracing::error!("读取外部拖拽内容失败：{error}"),
                            }
                        }
                    }
                    #[cfg(target_os = "windows")]
                    return source_preview;
                    #[cfg(not(target_os = "windows"))]
                    source_preview
                },
            )
            .on_drag_move(cx.listener(
                move |this, event: &DragMoveEvent<ListItemDrag>, window, cx| {
                    let Some(drag) = event.dragged_item().downcast_ref::<ListItemDrag>() else {
                        return;
                    };
                    if drag.source_tab != this.active_tab || drag.is_multi() {
                        return;
                    }
                    if !event.bounds.contains(&window.mouse_position()) {
                        this.clear_list_drag_hover(row_id);
                        return;
                    }
                    if drag.history_id == row_id || drag.is_pinned != target_is_pinned {
                        return;
                    }
                    let target = ListDropTarget {
                        source_id: drag.history_id,
                        target_id: row_id,
                    };
                    let current_visual_offsets = this
                        .list_reorder_animation
                        .as_ref()
                        .map(current_reorder_animation_offsets)
                        .unwrap_or_default();
                    if this.set_reorder_target(
                        target,
                        if metrics.horizontal {
                            metrics.horizontal_item_width()
                        } else {
                            metrics.row_height
                        },
                        &current_visual_offsets,
                    ) {
                        cx.notify();
                    }
                },
            ))
            .on_drop(cx.listener(move |this, drag: &ListItemDrag, _, cx| {
                if drag.source_tab != this.active_tab {
                    return;
                }
                #[cfg(target_os = "windows")]
                DragPreviewService::hide(cx);
                #[cfg(target_os = "windows")]
                ExternalDragService::cancel(cx);
                if drag.is_multi() {
                    return;
                }
                if drag.history_id != row_id && drag.is_pinned == target_is_pinned {
                    let target = ListDropTarget {
                        source_id: drag.history_id,
                        target_id: row_id,
                    };
                    let current_visual_offsets = this
                        .list_reorder_animation
                        .as_ref()
                        .map(current_reorder_animation_offsets)
                        .unwrap_or_default();
                    if this.set_reorder_target(target, metrics.row_height, &current_visual_offsets)
                    {
                        cx.notify();
                    }
                }
                if this
                    .list_drop_target
                    .is_some_and(|target| target.source_id == drag.history_id)
                {
                    cx.stop_propagation();
                    this.commit_reorder_preview(cx);
                } else if this.cancel_reorder_preview() {
                    cx.notify();
                }
            }));
    }
    let menu_window = main_window.clone();
    let middle_click_target = RowMouseTarget {
        main_window: main_window.clone(),
        row_id,
        item_id,
        favorite_id,
        is_pinned: target_is_pinned,
    };
    let left_click_target = middle_click_target.clone();
    let row = row
        .on_scroll_wheel(cx.listener(move |_, event, window, cx| {
            PreviewWindowService::handle_wheel(preview_source, event, window, cx);
        }))
        .on_aux_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
            if !event.is_middle_click() {
                return;
            }
            cx.stop_propagation();
            let behavior = config_runtime::clipboard_config(cx).mouse_middle_behavior;
            run_row_mouse_behavior(
                this,
                behavior,
                event.click_count(),
                &middle_click_target,
                window,
                cx,
            );
        }))
        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
            let behavior = config_runtime::clipboard_config(cx).mouse_left_behavior;
            if !event.standard_click() {
                return;
            }
            let modifiers = event.modifiers();
            if modifiers.control || modifiers.shift {
                this.select_list_item_with_modifiers(row_id, modifiers);
                cx.notify();
                return;
            }
            run_row_mouse_behavior(
                this,
                behavior,
                event.click_count(),
                &left_click_target,
                window,
                cx,
            );
        }))
        .on_mouse_down(MouseButton::Right, move |event, window, cx| {
            window.prevent_default();
            cx.stop_propagation();
            PreviewWindowService::hide(cx);
            let Some(anchor) = logical_to_physical_desktop(window, event.position) else {
                tracing::error!("无法确定菜单位置，未打开剪贴板菜单");
                return;
            };

            menu_window.update(cx, |this, _| this.select_item(row_id));
            let position =
                crate::windows::menu::MenuPosition::new(anchor.x, anchor.y, window.scale_factor());
            let owner_window = window.window_handle();
            // 只有捕获时已标记为链接的纯文本内容，才读取完整原文解析链接。
            // 普通文本不触发原始大文本读取，避免右键菜单出现延迟。
            let can_open_in_browser = menu_item.all_format_options.iter().any(|preview| {
                preview.is_restorable
                    && preview.role() == FormatRole::PlainText
                    && preview.content_kind == ContentKind::Link
            });
            let links = if can_open_in_browser {
                menu_window
                    .update(cx, |this, _| {
                        this.item_plain_text(item_id)
                            .ok()
                            .flatten()
                            .map(|text| extract_links(&text))
                    })
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let translator = i18n::translator(cx);
            let search_engines = config_runtime::clipboard_config(cx).search_engines_or_default();
            let last_search_engine_template = crate::state_runtime::state(cx)
                .main_window
                .last_search_engine_template;
            let (mut context_menu, format_actions) = if active_tab == TabId::Favorites {
                favorite_item_context_menu(
                    &menu_item,
                    &menu_tags,
                    &translator,
                    display_priority,
                    &search_engines,
                    last_search_engine_template.as_deref(),
                    &links,
                )
            } else {
                history_item_context_menu(
                    &menu_item,
                    &menu_tags,
                    &translator,
                    display_priority,
                    &search_engines,
                    last_search_engine_template.as_deref(),
                    &links,
                )
            };
            {
                let mut items = context_menu.items().to_vec();
                items.push(crate::services::sync::ui::menu(cx));
                context_menu = MenuModel::new(items);
            }
            let browser_links = Rc::new(links);
            if active_tab == TabId::Favorites {
                let format_actions = format_actions.clone();
                let item_tag_ids = item_tag_ids.clone();
                let main_window = menu_window.clone();
                let browser_links = browser_links.clone();
                MenuService::open_context_menu(
                    context_menu.clone(),
                    position,
                    move |action, cx| {
                        if crate::services::sync::ui::perform(action.as_str(), item_id, true, cx) {
                            return;
                        }
                        if let Some(index) = action
                            .as_str()
                            .strip_prefix(OPEN_LINK_ACTION_PREFIX)
                            .and_then(|value| value.parse::<usize>().ok())
                        {
                            if let Some(link) = browser_links.get(index) {
                                cx.open_url(link);
                            }
                            return;
                        }
                        if action.as_str() == OPEN_ALL_LINKS_ACTION {
                            for link in browser_links.iter() {
                                cx.open_url(link);
                            }
                            return;
                        }
                        if action.as_str() == PIN_TO_SCREEN_ACTION {
                            crate::windows::sticky::StickyService::open(item_id, cx);
                            return;
                        }
                        if action.as_str() == SPLIT_WORDS_ACTION {
                            crate::windows::sticky::StickyService::open_split(item_id, cx);
                            return;
                        }
                        if action.as_str() == "remove-from-favorites" {
                            if let Some(favorite_id) = favorite_id {
                                confirm_remove_from_favorites(
                                    main_window.clone(),
                                    owner_window,
                                    favorite_id,
                                    row_id,
                                    cx,
                                );
                            }
                            return;
                        }
                        main_window.update(cx, |this, cx| {
                            if action.as_str() == "toggle-pin" {
                                this.set_item_pinned(row_id, favorite_id, !menu_item.is_pinned, cx);
                            } else if action.as_str() == EDIT_ITEM_ACTION {
                                this.edit_item(item_id, cx);
                            } else if action.as_str() == EDIT_NOTE_ACTION {
                                this.edit_note(item_id, cx);
                            } else if action.as_str() == "paste" {
                                this.paste_item(item_id, cx);
                            } else if action.as_str() == "copy" {
                                this.copy_item(item_id, cx);
                            } else if action.as_str() == OPEN_WITH_DEFAULT_ACTION {
                                this.open_item_with_default(item_id, cx);
                            } else if let Some(index) = action
                                .as_str()
                                .strip_prefix("search-engine-")
                                .and_then(|value| value.parse::<usize>().ok())
                            {
                                this.search_item(item_id, index, cx);
                            } else if let Some(tag_id) = parse_toggle_tag_action(action.as_str()) {
                                let is_selected = item_tag_ids.borrow().contains(&tag_id);
                                if is_selected {
                                    this.remove_from_tag(item_id, tag_id, row_id, cx);
                                    item_tag_ids.borrow_mut().retain(|id| *id != tag_id);
                                } else {
                                    this.add_to_tag(item_id, tag_id, cx);
                                    item_tag_ids.borrow_mut().push(tag_id);
                                }
                            } else if let Some(index) = action
                                .as_str()
                                .strip_prefix("paste-format-")
                                .and_then(|value| value.parse::<usize>().ok())
                                && let Some(format) = format_actions.get(index)
                            {
                                this.paste_item_format(
                                    item_id,
                                    format.format_namespace.clone(),
                                    format.format_key.clone(),
                                    cx,
                                );
                            }
                        });
                    },
                    cx,
                );
            } else {
                let format_actions = format_actions.clone();
                let item_tag_ids = item_tag_ids.clone();
                let main_window = menu_window.clone();
                let browser_links = browser_links.clone();
                MenuService::open_context_menu(
                    context_menu.clone(),
                    position,
                    move |action, cx| {
                        if crate::services::sync::ui::perform(action.as_str(), item_id, false, cx) {
                            return;
                        }
                        if let Some(index) = action
                            .as_str()
                            .strip_prefix(OPEN_LINK_ACTION_PREFIX)
                            .and_then(|value| value.parse::<usize>().ok())
                        {
                            if let Some(link) = browser_links.get(index) {
                                cx.open_url(link);
                            }
                            return;
                        }
                        if action.as_str() == OPEN_ALL_LINKS_ACTION {
                            for link in browser_links.iter() {
                                cx.open_url(link);
                            }
                            return;
                        }
                        if action.as_str() == PIN_TO_SCREEN_ACTION {
                            crate::windows::sticky::StickyService::open(item_id, cx);
                            return;
                        }
                        if action.as_str() == SPLIT_WORDS_ACTION {
                            crate::windows::sticky::StickyService::open_split(item_id, cx);
                            return;
                        }
                        if action.as_str() == "remove-from-favorites" {
                            if let Some(favorite_id) = favorite_id {
                                confirm_remove_from_favorites(
                                    main_window.clone(),
                                    owner_window,
                                    favorite_id,
                                    row_id,
                                    cx,
                                );
                            }
                            return;
                        }
                        main_window.update(cx, |this, cx| {
                            if action.as_str() == "toggle-pin" {
                                this.set_item_pinned(row_id, favorite_id, !menu_item.is_pinned, cx);
                            } else if action.as_str() == ADD_TO_FAVORITES_ACTION {
                                this.add_to_favorites(item_id, cx);
                            } else if action.as_str() == EDIT_ITEM_ACTION {
                                this.edit_item(item_id, cx);
                            } else if action.as_str() == EDIT_NOTE_ACTION {
                                this.edit_note(item_id, cx);
                            } else if let Some(tag_id) = parse_toggle_tag_action(action.as_str()) {
                                let is_selected = item_tag_ids.borrow().contains(&tag_id);
                                if is_selected {
                                    this.remove_from_tag(item_id, tag_id, row_id, cx);
                                    item_tag_ids.borrow_mut().retain(|id| *id != tag_id);
                                } else {
                                    this.add_to_tag(item_id, tag_id, cx);
                                    item_tag_ids.borrow_mut().push(tag_id);
                                }
                            } else if action.as_str() == "paste" {
                                this.paste_item(item_id, cx);
                            } else if action.as_str() == "copy" {
                                this.copy_item(item_id, cx);
                            } else if action.as_str() == OPEN_WITH_DEFAULT_ACTION {
                                this.open_item_with_default(item_id, cx);
                            } else if let Some(index) = action
                                .as_str()
                                .strip_prefix("search-engine-")
                                .and_then(|value| value.parse::<usize>().ok())
                            {
                                this.search_item(item_id, index, cx);
                            } else if action.as_str() == "delete" {
                                this.delete_item(row_id, cx);
                            } else if let Some(index) = action
                                .as_str()
                                .strip_prefix("paste-format-")
                                .and_then(|value| value.parse::<usize>().ok())
                                && let Some(format) = format_actions.get(index)
                            {
                                this.paste_item_format(
                                    item_id,
                                    format.format_namespace.clone(),
                                    format.format_key.clone(),
                                    cx,
                                );
                            }
                        });
                    },
                    cx,
                );
            }
        });

    let mut row = div()
        .relative()
        .h(metrics.content_height)
        .w_full()
        .flex_none()
        .child(row)
        .children(render_item_border(
            list_item_style,
            item_border_color(
                item.is_pinned,
                item.paste_count,
                crate::config_runtime::item_status_borders(cx),
                cx.theme().colors,
                metrics.horizontal,
            ),
            metrics.horizontal,
        ))
        .child(render_tag_indicators(
            &item,
            tags,
            cx.theme().colors,
            tag_indicator_hover_enabled,
            hover_allowed,
        ));
    if hover_allowed
        && crate::config_runtime::animation_enabled(cx, crate::config::AnimationKind::ListItemHover)
    {
        let hover_motion = Duration::from_millis(160).with_easing(gpui::ease_in_out);
        row = row
            .transitions(|transitions| transitions.top(hover_motion))
            .group_hover(hover_group.clone(), |this| this.top(ui_rem(-2.0)));
    }

    let mut row = div()
        .id(("clipboard-row-hit-area", row_id as u64))
        .relative()
        .h(metrics.content_height)
        .w_full()
        .flex_none()
        .group(row_group)
        .when(list_item_style == ListItemStyle::Seamless, |row| {
            with_row_hover(
                row,
                row_id,
                preview_anchor.clone(),
                preview_item.clone(),
                preview_source,
                preview_priority,
                cx,
            )
        })
        .child(row);
    if can_reorder {
        row = crate::windows::main::window_drag::exclude_from_window_drag(row, main_window.clone());
    }
    let preview_anchor_for_prepaint = preview_anchor.clone();
    let row = row.child(
        canvas(
            move |bounds, window, cx| {
                if let Some(anchor) = PreviewAnchor::from_bounds(window, bounds) {
                    preview_anchor_for_prepaint.set(Some(anchor));
                    PreviewWindowService::update_anchor(preview_source, anchor, cx);
                }
            },
            |_bounds, _state, _window, _cx| {},
        )
        .absolute()
        .inset_0()
        .size_full(),
    );
    let row = with_reorder_animation(
        row.into_any_element(),
        row_id,
        reorder_animation,
        list_item_reorder_enabled,
        metrics.horizontal,
    );
    if list_item_style == ListItemStyle::Card || metrics.horizontal {
        let mut card = div()
            .id(("clipboard-card", row_id as u64))
            .h(metrics.row_height)
            .w_full()
            .flex_none()
            .when(list_item_style == ListItemStyle::Card, |this| {
                this.px(if metrics.horizontal {
                    metrics.horizontal_card_inset
                } else {
                    px(CARD_HORIZONTAL_INSET)
                })
            })
            .py(metrics.card_vertical_inset)
            .group(hover_group)
            .map(|card| {
                with_row_hover(
                    card,
                    row_id,
                    preview_anchor,
                    preview_item,
                    preview_source,
                    preview_priority,
                    cx,
                )
            })
            .child(row);
        if can_reorder {
            let preview_size = preview_size.clone();
            card = gpui_component::ElementExt::on_prepaint(card, move |bounds, _, _| {
                preview_size.set(Some(bounds.size));
            });
        }
        with_load_animation(
            card.into_any_element(),
            row_id,
            load_animation,
            load_offset,
            list_item_load_enabled,
        )
    } else {
        with_load_animation(
            row.into_any_element(),
            row_id,
            load_animation,
            load_offset,
            list_item_load_enabled,
        )
    }
}

fn render_tag_item_drop_overlay(
    item_id: ClipboardItemId,
    item_tag_ids: &[i64],
    theme: ThemeColor,
    main_window: Entity<MainWindow>,
) -> AnyElement {
    let mut overlay = div()
        .absolute()
        .inset_0()
        .invisible()
        .flex()
        .items_center()
        .justify_center()
        .px(ui_rem(8.0))
        .bg(gpui::ColorExt::opacity(&theme.popover, 0.78))
        .text_size(ui_rem(13.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme.foreground)
        .child("松开以添加标签");
    let item_tag_ids_for_style = item_tag_ids.to_vec();
    let item_tag_ids_for_drop = item_tag_ids.to_vec();
    overlay = overlay
        .drag_over::<TagDrag>(move |this, drag, _, _| {
            if !item_tag_ids_for_style.contains(&drag.tag_id) {
                this.visible()
            } else {
                this
            }
        })
        .can_drop(move |dragged, _, _| {
            dragged
                .downcast_ref::<TagDrag>()
                .is_some_and(|drag| !item_tag_ids_for_drop.contains(&drag.tag_id))
        })
        .on_drop(move |drag: &TagDrag, _, cx| {
            #[cfg(target_os = "windows")]
            crate::platform::drag_preview::DragPreviewService::hide(cx);
            cx.stop_propagation();
            let _ = main_window.update(cx, |this, cx| {
                if this.cancel_tag_reorder_preview() {
                    cx.notify();
                }
                this.add_to_tag(item_id, drag.tag_id, cx);
            });
        });
    overlay.into_any_element()
}

fn with_reorder_animation(
    row: AnyElement,
    row_id: HistoryItemId,
    reorder_animation: Option<(u64, Pixels)>,
    animations_enabled: bool,
    horizontal: bool,
) -> AnyElement {
    let Some((generation, offset)) = reorder_animation.filter(|_| animations_enabled) else {
        return row;
    };

    let animation_id = ElementId::NamedChild(
        Arc::new(ElementId::NamedInteger(
            "clipboard-row-reorder".into(),
            row_id as u64,
        )),
        SharedString::from(generation.to_string()),
    );
    OffsetElement {
        element: Some(row),
        offset: point(px(0.0), px(0.0)),
    }
    .with_animation(
        animation_id,
        Animation::new(LIST_REORDER_ANIMATION_DURATION).with_easing(gpui::ease_in_out),
        move |mut this, delta| {
            let offset = px(f32::from(offset) * (1.0 - delta));
            this.offset = if horizontal {
                point(offset, px(0.0))
            } else {
                point(px(0.0), offset)
            };
            this
        },
    )
    .into_any_element()
}

fn with_load_animation(
    row: AnyElement,
    row_id: HistoryItemId,
    load_animation: Option<ListItemLoadAnimation>,
    load_offset: Pixels,
    animations_enabled: bool,
) -> AnyElement {
    let Some(load_animation) = load_animation.filter(|_| animations_enabled) else {
        return row;
    };

    let animation_id = ElementId::NamedChild(
        Arc::new(ElementId::NamedInteger(
            "clipboard-row-load".into(),
            row_id as u64,
        )),
        SharedString::from(format!(
            "{}-{}",
            load_animation.generation, load_animation.order
        )),
    );
    let initial_offset = -load_offset;
    let element = OffsetElement {
        element: Some(row),
        offset: point(initial_offset, px(0.0)),
    };
    let delay = (LIST_ITEM_LOAD_ANIMATION_DELAY * load_animation.order as u32)
        .min(LIST_ITEM_LOAD_ANIMATION_MAX_DELAY);
    let slide_animation =
        Animation::new(LIST_ITEM_LOAD_ANIMATION_DURATION).with_easing(gpui::ease_out_quint());

    if delay.is_zero() {
        element
            .with_animation(animation_id, slide_animation, move |mut this, delta| {
                this.offset = point(px(f32::from(initial_offset) * (1.0 - delta)), px(0.0));
                this
            })
            .into_any_element()
    } else {
        element
            .with_animations(
                animation_id,
                vec![Animation::new(delay), slide_animation],
                move |mut this, animation_index, delta| {
                    if animation_index > 0 {
                        this.offset = point(px(f32::from(initial_offset) * (1.0 - delta)), px(0.0));
                    }
                    this
                },
            )
            .into_any_element()
    }
}

struct OffsetElement {
    element: Option<AnyElement>,
    offset: Point<Pixels>,
}

impl IntoElement for OffsetElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for OffsetElement {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut element = self.element.take().expect("列表动画元素只能布局一次");
        let layout_id = element.request_layout(window, cx);
        (layout_id, element)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: gpui::Bounds<Pixels>,
        element: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.with_element_offset(self.offset, |window| {
            element.prepaint(window, cx);
        });
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: gpui::Bounds<Pixels>,
        element: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        element.paint(window, cx);
    }
}

fn render_loading_row(
    index: usize,
    metrics: ListItemMetrics,
    list_item_style: ListItemStyle,
    theme: ThemeColor,
    translator: &Translator,
) -> AnyElement {
    let mut row = div()
        .id(("clipboard-loading-row", index as u64))
        .h(metrics.content_height)
        .w_full()
        .flex_none()
        .flex()
        .items_center()
        .px(ui_rem(10.0))
        .text_size(metrics.preview_font_size)
        .line_height(metrics.preview_line_height)
        .text_color(theme.muted_foreground)
        .child(translator.translate("main.list.loading_row"));
    if list_item_style == ListItemStyle::Card {
        row = row
            .overflow_hidden()
            .rounded(px(CARD_RADIUS))
            .border_1()
            .border_color(theme.border);
    }

    if list_item_style == ListItemStyle::Card || metrics.horizontal {
        div()
            .h(metrics.row_height)
            .w_full()
            .flex_none()
            .when(list_item_style == ListItemStyle::Card, |this| {
                this.px(if metrics.horizontal {
                    metrics.horizontal_card_inset
                } else {
                    px(CARD_HORIZONTAL_INSET)
                })
            })
            .py(metrics.card_vertical_inset)
            .child(row)
            .into_any_element()
    } else {
        row.into_any_element()
    }
}

fn render_placeholder_row(
    metrics: ListItemMetrics,
    list_item_style: ListItemStyle,
    cx: &mut Context<MainWindow>,
) -> AnyElement {
    let surface = div()
        .on_drop(cx.listener(|this, drag: &ListItemDrag, _, cx| {
            if drag.source_tab != this.active_tab {
                return;
            }
            cx.stop_propagation();
            #[cfg(target_os = "windows")]
            DragPreviewService::hide(cx);
            #[cfg(target_os = "windows")]
            ExternalDragService::cancel(cx);
            if this
                .list_drop_target
                .is_some_and(|target| target.source_id == drag.history_id)
            {
                this.commit_reorder_preview(cx);
            } else if this.cancel_reorder_preview() {
                cx.notify();
            }
        }))
        .h(metrics.content_height)
        .w_full()
        .flex_none();
    if list_item_style == ListItemStyle::Card || metrics.horizontal {
        div()
            .h(metrics.row_height)
            .w_full()
            .flex_none()
            .when(list_item_style == ListItemStyle::Card, |this| {
                this.px(if metrics.horizontal {
                    metrics.horizontal_card_inset
                } else {
                    px(CARD_HORIZONTAL_INSET)
                })
            })
            .py(metrics.card_vertical_inset)
            .child(surface)
            .into_any_element()
    } else {
        surface.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ADAPTIVE_IMAGE_PREVIEW_LINES, ListItemMetrics, estimate_text_lines, file_list_meta_label,
        image_meta_byte_length, image_meta_label, resolve_row_height,
    };
    use crate::config::{CardItemGap, ListItemHeight, ListItemStyle};
    use crate::domain::clipboard::ImageDimensions;
    use crate::i18n::{Locale, Translator};
    use gpui::px;
    use std::collections::HashMap;

    #[test]
    fn lazy_action_width_matches_available_buttons_and_display_settings() {
        use crate::config::{ItemDisplayConfig, ItemDisplayField::*};
        use crate::domain::capture::FormatKind;
        use crate::domain::clipboard::{ClipboardListItem, ContentKind, FormatPreview};
        use crate::windows::main::tabs::TabId;
        let mut item = ClipboardListItem {
            history_id: 1,
            item_id: 1,
            favorite_id: None,
            tag_ids: Vec::new(),
            note_text: String::new(),
            preview_text: "链接".into(),
            preview_char_count: 2,
            preview_options: Vec::new(),
            all_format_options: vec![FormatPreview {
                format_namespace: "mime".into(),
                format_key: "text/plain".into(),
                format_name: "纯文本".into(),
                format_order: 0,
                format_kind: FormatKind::Text,
                text: "https://example.com".into(),
                char_count: 19,
                content_kind: ContentKind::Link,
                html_content: None,
                image_source: None,
                image_dimensions: None,
                is_restorable: true,
            }],
            content_kind: ContentKind::Link,
            paste_count: 0,
            is_pinned: false,
            sort_order: 1,
            last_seen_at: 0,
            shortcut: String::new(),
            source_app_name: None,
            source_device_name: None,
            source_app_icon: None,
            is_selected: false,
        };
        let mut display = ItemDisplayConfig::default();
        assert_eq!(
            super::hover_action_fields(&item, TabId::Clipboard, &display),
            vec![Edit, Note, PastePlain, OpenBrowser, Favorite, Pin, Delete]
        );
        display.set_enabled(Note, false);
        assert_eq!(
            super::hover_action_fields(&item, TabId::Favorites, &display),
            vec![Edit, PastePlain, OpenBrowser, Favorite, Pin]
        );
        item.all_format_options[0].is_restorable = false;
        assert_eq!(
            super::hover_action_fields(&item, TabId::Clipboard, &display),
            vec![Favorite, Pin, Delete]
        );
        display.fields.clear();
        assert!(super::hover_action_fields(&item, TabId::Clipboard, &display).is_empty());
    }

    #[test]
    fn file_list_meta_label_includes_file_count() {
        let translator = Translator::builtin(Locale::zh_cn());

        assert_eq!(file_list_meta_label(&translator, 5), "5 文件");
    }

    #[test]
    fn image_meta_label_includes_dimensions() {
        let translator = Translator::builtin(Locale::zh_cn());

        assert_eq!(
            image_meta_label(
                &translator,
                Some(ImageDimensions {
                    width: 1024,
                    height: 1024,
                }),
            ),
            "1024 x 1024"
        );
        assert_eq!(image_meta_label(&translator, None), "图片");
    }

    #[test]
    fn image_size_uses_raw_payload_or_actual_image_file_bytes() {
        use crate::domain::{capture::FormatKind, clipboard::ImagePreviewSource};

        let inline = ImagePreviewSource::Inline {
            bytes: vec![0; 1234].into(),
        };
        let external = ImagePreviewSource::External {
            sha256: [0; 32],
            byte_length: 5678,
            relative_path: "图片.raw".into(),
        };
        for (source, expected) in [(&inline, 1234), (&external, 5678)] {
            assert_eq!(
                image_meta_byte_length(FormatKind::Image, Some(source), None),
                Some(expected)
            );
            assert_eq!(
                image_meta_byte_length(FormatKind::FileList, Some(source), None),
                None
            );
            assert_eq!(
                image_meta_byte_length(FormatKind::FileList, Some(source), Some(8192)),
                Some(8192)
            );
        }
        assert_eq!(image_meta_byte_length(FormatKind::Image, None, None), None);
        let translator = Translator::builtin(Locale::zh_cn());
        assert_eq!(
            crate::i18n::file_size(&translator, Some(8192), false),
            "8.0 KB"
        );
    }

    #[test]
    fn fixed_height_presets_use_the_requested_line_count() {
        let large = ListItemMetrics::from_rem_size(
            px(16.0),
            ListItemStyle::Seamless,
            CardItemGap::Eight,
            ListItemHeight::Large,
            8,
        );
        let small = ListItemMetrics::from_rem_size(
            px(16.0),
            ListItemStyle::Seamless,
            CardItemGap::Eight,
            ListItemHeight::Small,
            8,
        );

        assert_eq!(large.preview_line_count, 4);
        assert!(large.show_meta);
        assert_eq!(small.preview_line_count, 2);
        assert!(!small.show_meta);
    }

    #[test]
    fn adaptive_text_lines_are_limited_by_the_configured_maximum() {
        let lines = estimate_text_lines(
            "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
            px(100.0),
            px(10.0),
            2,
        );

        assert_eq!(lines, 2);
    }

    #[test]
    fn shaped_text_line_count_includes_automatic_wraps() {
        assert_eq!(super::shaped_line_count([2, 0], 8), 4);
        assert_eq!(super::shaped_line_count([20], 8), 8);
    }

    #[test]
    fn adaptive_row_height_survives_content_cache_cleanup() {
        let mut heights = HashMap::new();
        assert_eq!(
            resolve_row_height(
                Some(12),
                ListItemHeight::Adaptive,
                Some(px(72.0)),
                px(240.0),
                &mut heights,
            ),
            px(72.0)
        );
        assert_eq!(
            resolve_row_height(
                Some(12),
                ListItemHeight::Adaptive,
                None,
                px(240.0),
                &mut heights,
            ),
            px(72.0)
        );
    }

    #[test]
    fn fixed_row_height_does_not_use_adaptive_cache() {
        let mut heights = HashMap::from([(12, px(72.0))]);
        assert_eq!(
            resolve_row_height(
                Some(12),
                ListItemHeight::Medium,
                None,
                px(160.0),
                &mut heights,
            ),
            px(160.0)
        );
        assert_eq!(heights.get(&12), Some(&px(72.0)));
    }

    #[test]
    fn adaptive_row_height_cache_follows_history_id_after_prepend() {
        let mut heights = HashMap::new();
        resolve_row_height(
            Some(12),
            ListItemHeight::Adaptive,
            Some(px(72.0)),
            px(240.0),
            &mut heights,
        );

        assert_eq!(
            resolve_row_height(
                Some(12),
                ListItemHeight::Adaptive,
                None,
                px(240.0),
                &mut heights,
            ),
            px(72.0)
        );
        assert_eq!(
            resolve_row_height(
                Some(99),
                ListItemHeight::Adaptive,
                None,
                px(240.0),
                &mut heights,
            ),
            px(240.0)
        );
    }

    #[test]
    fn adaptive_metrics_use_a_stable_placeholder_line_count() {
        let metrics = ListItemMetrics::from_rem_size(
            px(16.0),
            ListItemStyle::Card,
            CardItemGap::Eight,
            ListItemHeight::Adaptive,
            8,
        );

        assert_eq!(metrics.preview_line_count, 3);
        assert_eq!(metrics.card_vertical_inset, px(4.0));
    }

    #[test]
    fn adaptive_resource_height_uses_the_large_row_preview_height() {
        let metrics = ListItemMetrics::from_rem_size(
            px(16.0),
            ListItemStyle::Seamless,
            CardItemGap::Eight,
            ListItemHeight::Adaptive,
            20,
        );

        assert_eq!(metrics.resource_preview_height(), px(70.0));
        assert_eq!(metrics.preview_line_count, 3);
    }

    #[test]
    fn adaptive_image_preview_uses_its_intrinsic_aspect_ratio() {
        let metrics = ListItemMetrics::from_rem_size(
            px(16.0),
            ListItemStyle::Seamless,
            CardItemGap::Eight,
            ListItemHeight::Adaptive,
            20,
        );
        let image_metrics = metrics.with_adaptive_image_dimensions(
            ImageDimensions {
                width: 4,
                height: 1,
            },
            px(400.0),
        );

        assert_eq!(image_metrics.preview_height, px(100.0));
        assert_eq!(image_metrics.preview_line_count, 6);
    }

    #[test]
    fn adaptive_image_preview_caps_tall_images_without_changing_their_width() {
        let metrics = ListItemMetrics::from_rem_size(
            px(16.0),
            ListItemStyle::Seamless,
            CardItemGap::Eight,
            ListItemHeight::Adaptive,
            20,
        );
        let image_metrics = metrics.with_adaptive_image_dimensions(
            ImageDimensions {
                width: 1,
                height: 4,
            },
            px(400.0),
        );

        assert_eq!(image_metrics.preview_height, px(140.0));
        assert_eq!(
            image_metrics.preview_line_count,
            ADAPTIVE_IMAGE_PREVIEW_LINES
        );
    }
}
