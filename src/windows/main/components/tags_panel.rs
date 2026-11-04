use crate::ui::scale::ui_rem;

use std::{cell::Cell, rc::Rc, sync::Arc, time::Duration};

use gpui::Bounds;
use gpui::prelude::*;
use gpui::{
    Animation, AnimationExt, AnyElement, App, Context, DragMoveEvent, Element, ElementId, Entity,
    Focusable, FontWeight, GlobalElementId, Hsla, InspectorElementId, InteractiveElement,
    IntoElement, LayoutId, MouseButton, MouseDownEvent, Pixels, Point, Render, SharedString, Size,
    StatefulInteractiveElement, Styled, Window, div, point, px, relative, rgb,
};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{ActiveTheme as _, Sizable, h_flex, input::Input, tooltip::Tooltip, v_flex};

#[cfg(target_os = "windows")]
use crate::platform::drag_preview::DragPreviewService;
use crate::{
    config::{ClipboardWindowPosition, TitlebarPosition, WindowMaterial},
    config_runtime,
    domain::clipboard::Tag,
    i18n,
    services::database::TagRepository,
    ui::hover_icon,
    windows::main::state::{MainWindow, TagDropTarget, TagReorderAnimation},
};

const TAG_REORDER_ANIMATION_DURATION: Duration = Duration::from_millis(260);
const TAG_DRAG_PREVIEW_BACKGROUND_OPACITY: f32 = 0.8;

fn sidebar_panel_width(preferred_width: f32, viewport_width: f32) -> f32 {
    use crate::state_runtime::PersistedMainWindowState as State;
    let max_width =
        (viewport_width * 0.45).clamp(State::MIN_TAGS_PANEL_WIDTH, State::MAX_TAGS_PANEL_WIDTH);
    preferred_width.clamp(State::MIN_TAGS_PANEL_WIDTH, max_width)
}

impl MainWindow {
    pub(crate) fn finish_tags_panel_resize(&mut self, cx: &mut Context<Self>) {
        if self.tags_panel_resize_start.take().is_none() {
            return;
        }
        let mut state = crate::state_runtime::state(cx);
        state.main_window.tags_panel_width = self.tags_panel_width;
        if let Err(error) = crate::state_runtime::set_state(state, cx) {
            tracing::error!("保存标签面板宽度失败：{error}");
        }
        cx.notify();
    }
}

#[derive(Clone)]
pub(crate) struct TagDrag {
    pub(crate) tag_id: i64,
    tag: Tag,
    selected: bool,
    preview_size: Rc<Cell<Option<Size<Pixels>>>>,
}

struct TagDragPreview {
    tag: Tag,
    selected: bool,
    preview_size: Rc<Cell<Option<Size<Pixels>>>>,
    render_preview: bool,
}

impl Render for TagDragPreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.render_preview {
            return div().into_any_element();
        }
        let Some(preview_size) = self.preview_size.get() else {
            return div().into_any_element();
        };
        render_tag_drag_preview(&self.tag, self.selected, preview_size.width, window, cx)
    }
}

fn with_tag_reorder_animation(
    element: AnyElement,
    tag_id: i64,
    animation: Option<&TagReorderAnimation>,
    animations_enabled: bool,
) -> AnyElement {
    let Some(animation) = animation.filter(|_| animations_enabled) else {
        return element;
    };
    let Some(start_origin) = animation.start_positions.get(&tag_id).copied() else {
        return element;
    };

    let animation_id = ElementId::NamedChild(
        Arc::new(ElementId::NamedInteger(
            "tag-chip-reorder".into(),
            tag_id as u64,
        )),
        SharedString::from(animation.generation.to_string()),
    );
    TagReorderOffsetElement {
        element: Some(element),
        start_origin,
        delta: 0.0,
    }
    .with_animation(
        animation_id,
        Animation::new(TAG_REORDER_ANIMATION_DURATION).with_easing(gpui::ease_in_out),
        |mut this, delta| {
            this.delta = delta;
            this
        },
    )
    .into_any_element()
}

struct TagReorderOffsetElement {
    element: Option<AnyElement>,
    start_origin: Point<Pixels>,
    delta: f32,
}

impl IntoElement for TagReorderOffsetElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TagReorderOffsetElement {
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
        let mut element = self.element.take().expect("标签排序动画元素只能布局一次");
        let layout_id = element.request_layout(window, cx);
        (layout_id, element)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        element: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let progress = 1.0 - self.delta;
        let offset = point(
            px(f32::from(self.start_origin.x - bounds.origin.x) * progress),
            px(f32::from(self.start_origin.y - bounds.origin.y) * progress),
        );
        window.with_element_offset(offset, |window| {
            element.prepaint(window, cx);
        });
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        element: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        element.paint(window, cx);
    }
}

fn action_button(
    name: &str,
    label: String,
    active: bool,
    theme: gpui_component::ThemeColor,
    cx: &Context<MainWindow>,
) -> impl IntoElement {
    let action_id = match name {
        "plus" => 0_u32,
        "settings" => 1_u32,
        "filter" => 2_u32,
        "refresh" => 4_u32,
        "versions-off" => 5_u32,
        "checks" => 6_u32,
        _ => 3_u32,
    };
    let color = if active {
        theme.primary_foreground
    } else {
        theme.muted_foreground
    };
    let icon_group = format!("tags-panel-action-{action_id}");
    let mut button = div()
        .id(("tags-panel-action", action_id))
        .group(icon_group.clone())
        .size(ui_rem(24.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .cursor_pointer()
        .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
        .child(hover_icon(icon_group, name, 13.0, color));

    if name == "plus" {
        button = button.on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, window, cx| {
                open_tag_form(this.tag_repository.clone(), cx.entity(), window, cx);
            }),
        );
    } else if name == "settings" {
        button = button.on_click(cx.listener(|this, _, window, cx| {
            super::tag_manager::open(this.tag_repository.clone(), cx.entity(), window, cx);
        }));
    } else if name == "filter" {
        button = button.on_click(cx.listener(|this, _, _, cx| {
            this.toggle_tags_match_mode(cx);
        }));
    } else if name == "checks" {
        button = button.on_click(cx.listener(|this, _, _, cx| {
            this.toggle_tags_multi_select(cx);
        }));
    } else if name == "refresh" {
        button = button.on_click(cx.listener(|this, _, _, cx| {
            this.reset_tag_selection(cx);
        }));
    } else if name == "versions-off" {
        button = button.on_click(cx.listener(|this, _, _, cx| {
            this.toggle_clear_tags_on_tab_switch(cx);
        }));
    } else {
        button = button.on_click(cx.listener(|this, _, _, cx| {
            this.toggle_tags_panel_pin();
            cx.notify();
        }));
    }

    if active {
        button
            .bg(theme.primary)
            .hover(|this| this.bg(theme.primary_hover))
    } else {
        button.hover(|this| this.bg(theme.secondary_hover))
    }
}

fn open_tag_form(
    repository: TagRepository,
    main_window: Entity<MainWindow>,
    window: &mut Window,
    cx: &mut Context<MainWindow>,
) {
    let result = crate::windows::dialog::open_adaptive_window(
        Some(window),
        cx,
        crate::windows::dialog::DialogWindowPosition::Center,
        crate::windows::dialog::DialogMode::Modal,
        move |window, cx| {
            let on_saved = move |tag: Tag, cx: &mut App| {
                main_window.update(cx, |this, cx| this.save_tag(tag, cx));
            };
            cx.new(|cx| super::new_tag_form::NewTagForm::new(window, cx, repository, on_saved))
        },
    );
    if let Err(error) = result {
        tracing::error!("打开标签窗口失败：{error}");
    }
}

fn parse_tag_color(color: &str, fallback: Hsla) -> Hsla {
    let value = color.trim().strip_prefix('#').unwrap_or(color.trim());
    if value.len() == 6
        && value.chars().all(|character| character.is_ascii_hexdigit())
        && let Ok(value) = u32::from_str_radix(value, 16)
    {
        return gpui::rgb_to_hsla(rgb(value));
    }
    fallback
}

fn tag_status(message: &str, theme: gpui_component::ThemeColor) -> AnyElement {
    div()
        .px_2()
        .py_1()
        .text_size(ui_rem(11.0))
        .text_color(theme.muted_foreground)
        .child(message.to_owned())
        .into_any_element()
}

fn truncate_tag_name(tag_name: &str, max_width: Pixels, window: &Window) -> SharedString {
    let mut text_style = window.text_style();
    text_style.font_size = ui_rem(11.0).into();
    text_style.font_weight = FontWeight::MEDIUM;
    let font_size = text_style.font_size.to_pixels(window.rem_size());
    let mut wrapper = window
        .text_system()
        .line_wrapper(text_style.font(), font_size);
    let runs = vec![text_style.to_run(tag_name.len())];
    wrapper
        .truncate_line(
            tag_name.to_owned().into(),
            max_width,
            "…",
            &runs,
            gpui::TruncateFrom::End,
        )
        .0
}

fn tag_chip(
    tag: &Tag,
    selected: bool,
    dragging: bool,
    max_text_width: Pixels,
    window: &Window,
    cx: &Context<MainWindow>,
) -> impl IntoElement {
    let theme = cx.theme().colors;
    let tag_id = tag.id;
    let tag_name = tag.name.clone();
    let display_name = truncate_tag_name(&tag.name, max_text_width, window);
    let color = parse_tag_color(&tag.color, theme.blue);
    let foreground = if selected {
        theme.primary_foreground
    } else {
        theme.foreground
    };
    let tag_label = div()
        .flex_auto()
        .min_w_0()
        .w_full()
        .h_full()
        .flex()
        .items_center()
        .gap(ui_rem(6.0))
        .pl(ui_rem(9.0))
        .pr(ui_rem(11.0))
        .child(div().size(ui_rem(8.0)).flex_none().rounded_full().bg(color))
        .child(div().w_full().min_w_0().truncate().child(display_name));

    let preview_size = Rc::new(Cell::new(None));
    let drag_data = TagDrag {
        tag_id,
        tag: tag.clone(),
        selected,
        preview_size: preview_size.clone(),
    };
    let drag_window = cx.entity();
    let mut chip = div()
        .id(("tag-chip", tag_id as u64))
        .flex_auto()
        .w_auto()
        .min_w_0()
        .max_w_full()
        .h(ui_rem(30.0))
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .rounded_lg()
        .cursor_pointer()
        .text_size(ui_rem(11.0))
        .font_weight(FontWeight::MEDIUM)
        .text_color(foreground)
        .border_1()
        .border_color(theme.border)
        .overflow_hidden()
        .bg(if selected {
            theme.primary
        } else {
            theme.background
        })
        .hover(|this| {
            if selected {
                this.bg(theme.primary_hover)
            } else {
                this.bg(theme.secondary_hover)
            }
        })
        .tooltip(move |window, cx| Tooltip::new(tag_name.clone()).build(window, cx))
        .child(tag_label)
        .on_click(cx.listener(move |this, _, _, cx| {
            this.select_tag(tag_id, cx);
        }));

    let preview_size_for_prepaint = preview_size.clone();
    chip = chip.on_prepaint(move |prepaint, _, _| {
        preview_size_for_prepaint.set(Some(prepaint.bounds.size));
    });
    if dragging {
        chip = chip.invisible();
    }
    chip = crate::windows::main::window_drag::exclude_from_window_drag(chip, drag_window.clone());

    chip.on_drag(
        drag_data,
        move |drag: &TagDrag, cursor_offset, window, cx| {
            cx.stop_propagation();
            drag_window.update(cx, |this, cx| {
                this.begin_tag_reorder_drag(drag.tag_id);
                cx.notify();
            });

            #[cfg(target_os = "windows")]
            let Some(preview_size) = drag.preview_size.get() else {
                return cx.new(|_| TagDragPreview {
                    tag: drag.tag.clone(),
                    selected: drag.selected,
                    preview_size: drag.preview_size.clone(),
                    render_preview: true,
                });
            };
            #[cfg(target_os = "windows")]
            let Some((initial_bounds, display_id, cursor_offset_physical)) =
                drag_preview_bounds(window, cx, cursor_offset, preview_size)
            else {
                return cx.new(|_| TagDragPreview {
                    tag: drag.tag.clone(),
                    selected: drag.selected,
                    preview_size: drag.preview_size.clone(),
                    render_preview: true,
                });
            };
            #[cfg(not(target_os = "windows"))]
            let cursor_offset_physical = {
                let _ = cursor_offset;
                point(0.0, 0.0)
            };

            let source_preview = cx.new(|_| TagDragPreview {
                tag: drag.tag.clone(),
                selected: drag.selected,
                preview_size: drag.preview_size.clone(),
                render_preview: cfg!(not(target_os = "windows")),
            });
            #[cfg(target_os = "windows")]
            {
                let popup_preview = cx.new(|_| TagDragPreview {
                    tag: drag.tag.clone(),
                    selected: drag.selected,
                    preview_size: drag.preview_size.clone(),
                    render_preview: true,
                });
                let source_window = window.window_handle();
                if let Err(error) = DragPreviewService::start(
                    source_window,
                    popup_preview,
                    initial_bounds,
                    display_id,
                    cursor_offset_physical,
                    px(16.0),
                    {
                        let drag_window = drag_window.clone();
                        move |cx| {
                            drag_window.update(cx, |this, cx| {
                                this.finish_tag_drag_preview(cx);
                            });
                        }
                    },
                    cx,
                ) {
                    tracing::error!("启动标签拖拽预览失败：{error}");
                }
            }
            #[cfg(target_os = "windows")]
            return source_preview;
            #[cfg(not(target_os = "windows"))]
            source_preview
        },
    )
    .drag_over::<TagDrag>(move |this, drag, _, cx| {
        if drag.tag_id != tag_id {
            this.bg(cx.theme().drop_target)
        } else {
            this
        }
    })
    .can_drop(move |dragged, _, _| {
        dragged
            .downcast_ref::<TagDrag>()
            .is_some_and(|drag| drag.tag_id != tag_id)
    })
    .on_drag_move(
        cx.listener(move |this, event: &DragMoveEvent<TagDrag>, window, cx| {
            let Some(drag) = event.dragged_item().downcast_ref::<TagDrag>() else {
                return;
            };
            if !event.bounds.contains(&window.mouse_position()) {
                this.clear_tag_drag_hover(tag_id);
                return;
            }
            if drag.tag_id == tag_id {
                return;
            }
            let target = TagDropTarget {
                source_id: drag.tag_id,
                target_id: tag_id,
            };
            if this.preview_reorder_tag(target) {
                cx.notify();
            }
        }),
    )
    .on_drop(cx.listener(move |this, drag: &TagDrag, _, cx| {
        #[cfg(target_os = "windows")]
        DragPreviewService::hide(cx);
        if this
            .tag_drop_target
            .is_some_and(|target| target.source_id == drag.tag_id)
        {
            cx.stop_propagation();
            this.commit_tag_reorder_preview(cx);
        } else if this.cancel_tag_reorder_preview() {
            cx.notify();
        }
    }))
}

fn render_tag_drag_preview(
    tag: &Tag,
    selected: bool,
    width: Pixels,
    window: &Window,
    cx: &mut Context<TagDragPreview>,
) -> AnyElement {
    let theme = cx.theme().colors;
    let color = parse_tag_color(&tag.color, theme.blue);
    let foreground = if selected {
        theme.primary_foreground
    } else {
        theme.foreground
    };
    let max_text_width = (width - px(38.0 * crate::ui::scale::window_scale(window))).max(px(1.0));
    let display_name = truncate_tag_name(&tag.name, max_text_width, window);
    div()
        .h(ui_rem(30.0))
        .w(width)
        .flex_none()
        .flex()
        .items_center()
        .gap(ui_rem(6.0))
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
        .overflow_hidden()
        .text_size(ui_rem(11.0))
        .font_weight(FontWeight::MEDIUM)
        .text_color(foreground)
        .bg(gpui::ColorExt::opacity(
            if selected {
                &theme.primary
            } else {
                &theme.background
            },
            TAG_DRAG_PREVIEW_BACKGROUND_OPACITY,
        ))
        .pl(ui_rem(9.0))
        .pr(ui_rem(11.0))
        .child(div().size(ui_rem(8.0)).flex_none().rounded_full().bg(color))
        .child(div().w_full().min_w_0().truncate().child(display_name))
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

pub fn render(
    main: &MainWindow,
    window: &Window,
    cx: &Context<MainWindow>,
    sidebar_layout: bool,
) -> impl IntoElement {
    if !main.tags_panel_open {
        return div().id("tags-panel");
    }

    let theme = cx.theme().colors;
    let tags_panel_pinned = main.tags_panel_pinned;
    let clipboard = config_runtime::clipboard_config(cx);
    let clear_tags_on_tab_switch = clipboard.clear_tags_on_tab_switch;
    let tags_multi_select = clipboard.tags_multi_select;
    let appearance = config_runtime::appearance_config(cx);
    let titlebar_position = appearance.titlebar_position;
    let material_enabled = appearance.window_material != WindowMaterial::Disabled;
    let translator = i18n::translator(cx);
    let scale = crate::ui::scale::window_scale(window);
    let panel_width = if sidebar_layout {
        px(sidebar_panel_width(
            main.tags_panel_width,
            f32::from(window.viewport_size().width) / scale,
        ) * scale)
    } else {
        window.viewport_size().width * 0.65 + px(1.0)
    };
    let content_width = panel_width - px(24.0 * crate::ui::scale::window_scale(window));
    let max_text_width = content_width - px(38.0 * crate::ui::scale::window_scale(window));
    let tags_search_query = main.tags_search_query.trim().to_lowercase();
    let visible_tags = main
        .tags
        .iter()
        .filter(|tag| tag.name.to_lowercase().contains(&tags_search_query))
        .collect::<Vec<_>>();
    let visible_tag_ids = visible_tags.iter().map(|tag| tag.id).collect::<Vec<_>>();
    let tag_chip_positions = main.tag_chip_positions.clone();
    let tag_reorder_animation = main.tag_reorder_animation.as_ref();
    let status_text = if main.tags_loading {
        Some(translator.translate("main.tags.loading"))
    } else if main.tags_error {
        Some(translator.translate("main.tags.load_error"))
    } else if main.tags.is_empty() {
        Some(translator.translate("main.tags.empty"))
    } else if visible_tags.is_empty() {
        Some(translator.translate("main.tags.search_empty"))
    } else {
        None
    };

    let tags_search_input = main
        .tags_search_input
        .as_ref()
        .expect("标签搜索输入框状态尚未初始化");
    let input_focused = tags_search_input.focus_handle(cx).is_focused(window);
    let input_border = if input_focused {
        theme.ring
    } else {
        theme.input
    };
    let has_search_text = !main.tags_search_query.is_empty();
    let clear_button = if has_search_text {
        let tags_search_input_for_clear = tags_search_input.clone();
        Some(
            div()
                .id("tags-search-clear-button")
                .group("tags-panel-search-clear")
                .size(ui_rem(20.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .cursor_pointer()
                .hover(|this| this.bg(theme.secondary_hover))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.tags_search_query.clear();
                    tags_search_input_for_clear.update(cx, |input, cx| {
                        input.set_value("", window, cx);
                        input.focus(window, cx);
                    });
                    cx.notify();
                }))
                .child(hover_icon(
                    "tags-panel-search-clear",
                    "x",
                    14.0,
                    theme.danger,
                )),
        )
    } else {
        None
    };
    let tags_search_input_for_mouse = tags_search_input.clone();
    let mut tags_search_input_element = Input::new(tags_search_input)
        .small()
        .appearance(false)
        .bordered(false)
        .focus_bordered(false)
        .cleanable(false)
        .w_full();
    if let Some(clear_button) = clear_button {
        tags_search_input_element = tags_search_input_element.suffix(clear_button);
    }
    let tags_search_box = div()
        .h(ui_rem(26.0))
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .rounded_lg()
        .border_1()
        .border_color(input_border)
        .bg(theme.background)
        .capture_any_mouse_down(
            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                this.window_drag_start = None;
                if event.button != MouseButton::Left || window.is_window_active() {
                    return;
                }

                if let Err(error) = crate::services::focus::focus_input_window(window) {
                    tracing::error!("点击标签搜索框时激活窗口失败：{error}");
                    return;
                }

                tags_search_input_for_mouse.update(cx, |input, cx| input.focus(window, cx));
            }),
        )
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(tags_search_input_element);

    let panel_background = crate::window_material::overlay_surface_color(cx, theme.secondary);

    let mut panel = div()
        .id("tags-panel")
        .track_focus(&main.focus_handle)
        .flex()
        .flex_col()
        .relative()
        .overflow_hidden()
        .occlude()
        .bg(panel_background)
        .map(|this| crate::window_material::with_backdrop_blur(this, cx))
        .border(px(0.5))
        .border_color(theme.border)
        .when(!material_enabled, |this| this.shadow_lg())
        .on_prepaint(
            cx.listener(|this, prepaint: &gpui::InteractivityPrepaint, _, _| {
                this.tags_panel_bounds = Some(prepaint.bounds);
            }),
        )
        .on_hover(cx.listener(|this, _, window, cx| {
            this.update_tags_panel_auto_close(window, cx);
        }))
        .on_mouse_move_all({
            let main_window = cx.entity().downgrade();
            move |event: &gpui::MouseMoveEvent, phase, _, window, cx| {
                if phase == gpui::DispatchPhase::Capture {
                    main_window
                        .update(cx, |this, cx| {
                            if let Some((start_x, start_width)) = this.tags_panel_resize_start {
                                if event.dragging() {
                                    let scale = crate::ui::scale::window_scale(window);
                                    let width = sidebar_panel_width(
                                        start_width + f32::from(event.position.x - start_x) / scale,
                                        f32::from(window.viewport_size().width) / scale,
                                    );
                                    if this.tags_panel_width != width {
                                        this.tags_panel_width = width;
                                        this.adaptive_item_heights.clear();
                                        this.invalidate_list_layout_cache();
                                        cx.notify();
                                    }
                                    cx.stop_propagation();
                                } else {
                                    this.finish_tags_panel_resize(cx);
                                }
                            }
                            this.update_tags_panel_auto_close(window, cx);
                        })
                        .ok();
                }
            }
        })
        .on_mouse_up_all({
            let main_window = cx.entity().downgrade();
            move |event, phase, _, window, cx| {
                if phase == gpui::DispatchPhase::Capture && event.button == MouseButton::Left {
                    main_window
                        .update(cx, |this, cx| {
                            this.finish_tags_panel_resize(cx);
                            this.update_tags_panel_auto_close(window, cx);
                        })
                        .ok();
                }
            }
        })
        .on_drag_move(
            cx.listener(|this, event: &DragMoveEvent<TagDrag>, window, cx| {
                if event.bounds.contains(&window.mouse_position()) {
                    this.cancel_tags_panel_close();
                } else if !this.tags_panel_pinned {
                    this.schedule_tags_panel_close(window, cx);
                }
            }),
        )
        .on_drop(cx.listener(|this, drag: &TagDrag, _, cx| {
            #[cfg(target_os = "windows")]
            DragPreviewService::hide(cx);
            if this
                .tag_drop_target
                .is_some_and(|target| target.source_id == drag.tag_id)
            {
                cx.stop_propagation();
                this.commit_tag_reorder_preview(cx);
            } else if this.cancel_tag_reorder_preview() {
                cx.notify();
            }
        }))
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| {
                #[cfg(target_os = "windows")]
                DragPreviewService::hide(cx);
                if this.cancel_tag_reorder_preview() {
                    cx.notify();
                }
            }),
        )
        .child(
            div()
                .min_h(ui_rem(34.0))
                .w_full()
                .flex_none()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap(ui_rem(4.0))
                .px_2()
                .py(ui_rem(4.0))
                .border_b(px(0.5))
                .border_color(theme.border)
                .child(
                    div()
                        .flex_none()
                        .text_size(ui_rem(12.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child(translator.translate("main.tags.title")),
                )
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(ui_rem(2.0))
                        .flex_wrap()
                        .max_w_full()
                        .child(action_button(
                            "plus",
                            translator.translate("main.tags.new"),
                            false,
                            theme,
                            cx,
                        ))
                        .child(action_button(
                            "settings",
                            translator.translate("main.tags.manage"),
                            false,
                            theme,
                            cx,
                        ))
                        .child(action_button(
                            "checks",
                            translator.translate(if tags_multi_select {
                                "main.tags.multi_select_enabled"
                            } else {
                                "main.tags.multi_select_disabled"
                            }),
                            tags_multi_select,
                            theme,
                            cx,
                        ))
                        .child(action_button(
                            "filter",
                            translator.translate(if main.tags_match_all {
                                "main.tags.switch_to_any"
                            } else {
                                "main.tags.switch_to_all"
                            }),
                            main.tags_match_all,
                            theme,
                            cx,
                        ))
                        .child(action_button(
                            "refresh",
                            translator.translate("main.tags.reset_selection"),
                            false,
                            theme,
                            cx,
                        ))
                        .child(action_button(
                            "versions-off",
                            translator.translate(if clear_tags_on_tab_switch {
                                "main.tags.clear_on_tab_switch_enabled"
                            } else {
                                "main.tags.clear_on_tab_switch_disabled"
                            }),
                            clear_tags_on_tab_switch,
                            theme,
                            cx,
                        ))
                        .child(action_button(
                            "pin",
                            translator.translate("main.tags.pin"),
                            tags_panel_pinned,
                            theme,
                            cx,
                        )),
                ),
        )
        .child(
            div()
                .h(ui_rem(32.0))
                .w_full()
                .flex_none()
                .flex()
                .items_center()
                .px_2()
                .child(tags_search_box),
        )
        .child(
            div().flex_1().overflow_hidden().child(
                v_flex().size_full().overflow_y_scrollbar().child(
                    h_flex()
                        .on_children_prepainted(move |children_bounds, _, _| {
                            let mut positions = tag_chip_positions.borrow_mut();
                            positions.retain(|tag_id, _| visible_tag_ids.contains(tag_id));
                            for (tag_id, bounds) in
                                visible_tag_ids.iter().zip(children_bounds.iter())
                            {
                                positions.insert(*tag_id, bounds.origin);
                            }
                        })
                        .id("tags-list-content")
                        .w_full()
                        .min_w_0()
                        .flex_wrap()
                        .items_start()
                        .content_start()
                        .gap(ui_rem(6.0))
                        .px(ui_rem(12.0))
                        .pt(ui_rem(4.0))
                        .pb(ui_rem(10.0))
                        .children(visible_tags.iter().copied().map(|tag| {
                            with_tag_reorder_animation(
                                tag_chip(
                                    tag,
                                    main.selected_tag_ids.contains(&tag.id),
                                    main.is_tag_drag_source(tag.id),
                                    max_text_width,
                                    window,
                                    cx,
                                )
                                .into_any_element(),
                                tag.id,
                                tag_reorder_animation,
                                config_runtime::animation_enabled(
                                    cx,
                                    crate::config::AnimationKind::TagItemReorder,
                                ),
                            )
                        }))
                        .when_some(status_text, |this, message| {
                            this.child(tag_status(message.as_ref(), theme))
                        }),
                ),
            ),
        );

    if sidebar_layout {
        panel = panel
            .w(panel_width)
            .h_full()
            .flex_none()
            .border_r(px(0.5))
            .child(
                div()
                    .id("tags-panel-resize-handle")
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right_0()
                    .w(ui_rem(5.0))
                    .cursor_col_resize()
                    .hover(|this| this.bg(theme.border))
                    .when(main.tags_panel_resize_start.is_some(), |this| {
                        this.bg(theme.ring)
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.window_drag_start = None;
                            this.cancel_tags_panel_close();
                            this.tags_panel_resize_start =
                                Some((event.position.x, f32::from(panel_width) / scale));
                            cx.notify();
                        }),
                    ),
            );
    } else {
        let scale = crate::ui::scale::window_scale(window);
        let content_left = if matches!(titlebar_position, TitlebarPosition::Left) {
            px(36.0 * scale)
        } else {
            px(0.0)
        };
        let content_right = window.viewport_size().width
            - if matches!(titlebar_position, TitlebarPosition::Right) {
                px(36.0 * scale)
            } else {
                px(0.0)
            };
        let panel_width = panel_width.min(content_right - content_left);
        let anchor = main
            .tags_button_bounds
            .map(|bounds| bounds.center().x)
            .unwrap_or(content_right);
        let panel_left = (anchor - panel_width / 2.0)
            .max(content_left)
            .min(content_right - panel_width);
        let tags_button_bounds = main.tags_button_bounds;
        let panel_below_tags_button = tags_button_bounds
            .map(|bounds| bounds.center().y < window.viewport_size().height / 2.0)
            .unwrap_or(clipboard.window_position != ClipboardWindowPosition::ScreenBottomCenter);
        panel = panel
            .absolute()
            .left(panel_left)
            .w(panel_width)
            .max_h(relative(0.70));
        panel = if panel_below_tags_button {
            let top = tags_button_bounds
                .map(|bounds| bounds.origin.y + bounds.size.height)
                .unwrap_or_else(|| {
                    if clipboard.window_position == ClipboardWindowPosition::ScreenTopCenter {
                        px(super::symbol_sidebar::TAB_BAR_HEIGHT * scale)
                    } else if titlebar_position.is_vertical() {
                        px(34.5 * scale)
                    } else {
                        px(70.5 * scale)
                    }
                });
            panel
                .top(top)
                .border_t_0()
                .rounded_bl(px(16.0))
                .rounded_br(px(16.0))
        } else {
            let bottom = tags_button_bounds
                .map(|bounds| window.viewport_size().height - bounds.origin.y)
                .unwrap_or(px(super::symbol_sidebar::TAB_BAR_HEIGHT * scale));
            panel
                .bottom(bottom)
                .border_b_0()
                .rounded_tl(px(16.0))
                .rounded_tr(px(16.0))
        };
    }

    panel
}

#[cfg(test)]
mod tests {
    use super::sidebar_panel_width;

    #[test]
    fn sidebar_panel_width_respects_drag_limits_and_window_space() {
        assert_eq!(sidebar_panel_width(80.0, 800.0), 120.0);
        assert_eq!(sidebar_panel_width(240.0, 800.0), 240.0);
        assert_eq!(sidebar_panel_width(500.0, 1200.0), 360.0);
        assert_eq!(sidebar_panel_width(360.0, 500.0), 225.0);
        assert_eq!(sidebar_panel_width(360.0, 800.0), 360.0);
    }
}
