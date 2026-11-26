use crate::ui::scale::ui_rem;
use gpui_component::tooltip::ManagedTooltipExt as _;

use std::{cell::Cell, ops::Range, path::PathBuf, rc::Rc, sync::Arc};

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Context, DispatchPhase, DragMoveEvent, ExternalPaths, FileDropEvent,
    IntoElement, MouseButton, ObjectFit, Pixels, Point, Render, RenderImage, Size, Window, canvas,
    div, img, point, px, size, transparent_black,
};
use gpui_component::{ActiveTheme as _, scroll::ScrollableElement, tooltip::Tooltip};

use crate::{
    i18n,
    platform::window::logical_to_physical_desktop,
    services::image_library::{ImageLibraryGroup, ImageLibraryImage, ImageLibraryService},
    state_runtime::{GallerySortField, GallerySortOrder, PersistedGallerySort},
    ui::{hover_icon, icon},
    windows::main::state::MainWindow,
    windows::menu::{
        DropdownAnchor, DropdownItem, DropdownItemId, DropdownModel, MenuActionId, MenuIcon,
        MenuItem, MenuModel, MenuService,
    },
};

#[cfg(target_os = "windows")]
use crate::platform::drag_preview::DragPreviewService;

use super::{
    new_gallery_group_form::NewGalleryGroupForm, rename_gallery_image_form::RenameGalleryImageForm,
    symbol_layout, symbol_list, symbol_search, symbol_sidebar,
};

const GALLERY_CELL_MAX_WIDTH: f32 = 84.0;
const GALLERY_MAX_COLUMNS: u16 = 20;
const GALLERY_GRID_GAP: f32 = 8.0;
const GALLERY_GRID_HORIZONTAL_PADDING: f32 = 8.0;
const GALLERY_GRID_VERTICAL_PADDING: f32 = 4.0;
const GALLERY_DRAG_PREVIEW_OPACITY: f32 = 0.7;
const GALLERY_DRAG_PREVIEW_ACCEPT_SCALE: f32 = 0.72;

#[derive(Default)]
pub(crate) struct GalleryImagesCache {
    key: Option<(u64, PersistedGallerySort, String)>,
    images: Rc<Vec<ImageLibraryImage>>,
}

impl GalleryImagesCache {
    fn get(
        &mut self,
        generation: u64,
        images: &[ImageLibraryImage],
        sort: PersistedGallerySort,
        query: &str,
    ) -> Rc<Vec<ImageLibraryImage>> {
        let key = (generation, sort, query.trim().to_lowercase());
        if self.key.as_ref() != Some(&key) {
            let mut filtered = images
                .iter()
                .filter(|image| key.2.is_empty() || image.name.to_lowercase().contains(&key.2))
                .cloned()
                .collect::<Vec<_>>();
            sort_gallery_images(&mut filtered, sort);
            self.images = Rc::new(filtered);
            self.key = Some(key);
        }
        self.images.clone()
    }
}

#[derive(Clone)]
struct GalleryImageDrag {
    path: PathBuf,
    image: Option<Arc<RenderImage>>,
    size: Rc<Cell<Option<Size<Pixels>>>>,
    preview_scale: Rc<Cell<f32>>,
    source_group_id: Option<String>,
}

struct GalleryImageDragPreview {
    drag: GalleryImageDrag,
    render_preview: bool,
}

impl Render for GalleryImageDragPreview {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        if !self.render_preview {
            return div().into_any_element();
        }
        let Some(size) = self.drag.size.get() else {
            return div().into_any_element();
        };
        let scale = self
            .drag
            .preview_scale
            .get()
            .clamp(GALLERY_DRAG_PREVIEW_ACCEPT_SCALE, 1.0);
        let image = self
            .drag
            .image
            .as_ref()
            .map(|image| {
                img(image.clone())
                    .id("gallery-drag-preview-image")
                    .size_full()
                    .aspect_square()
                    .object_fit(ObjectFit::Cover)
                    .rounded_lg()
                    .into_any_element()
            })
            .unwrap_or_else(|| {
                div()
                    .size_full()
                    .bg(gpui::transparent_black())
                    .into_any_element()
            });
        let preview = div()
            .w(px(f32::from(size.width) * scale))
            .h(px(f32::from(size.height) * scale))
            .rounded_lg()
            .overflow_hidden()
            .opacity(GALLERY_DRAG_PREVIEW_OPACITY)
            .child(image);
        div()
            .w(size.width)
            .h(size.height)
            .flex()
            .items_center()
            .justify_center()
            .child(preview)
            .into_any_element()
    }
}

pub(crate) fn render(
    main: &mut MainWindow,
    window: &mut Window,
    cx: &mut Context<MainWindow>,
) -> AnyElement {
    main.ensure_image_library_loaded(cx);
    main.ensure_gallery_group_icons(cx);

    let translator = i18n::translator(cx);
    let horizontal_categories = crate::config_runtime::clipboard_config(cx)
        .window_position
        .uses_horizontal_list();
    let add_group_label = translator.translate("main.gallery.add_group");
    let category_area_width =
        f32::from(window.viewport_size().width) / crate::ui::scale::window_scale(window) / 2.0;
    let show_group_labels = horizontal_categories
        && symbol_sidebar::horizontal_labels_fit(
            main.gallery_groups
                .iter()
                .map(|group| group.name.as_str())
                .chain(std::iter::once(add_group_label.as_str())),
            category_area_width,
            24.0,
        );
    let columns = gallery_grid_columns(main, window, horizontal_categories);
    let cell_size = gallery_cell_size(main, window, columns, horizontal_categories);
    let sidebar = render_sidebar(
        main,
        cx,
        &translator,
        horizontal_categories,
        show_group_labels,
    );
    let add_images_button = render_add_images_button(&translator, cx);
    let selected_group = main
        .selected_gallery_group_id
        .as_ref()
        .and_then(|id| main.gallery_groups.iter().find(|group| group.id == *id));
    let mut search_actions = div()
        .flex()
        .items_center()
        .gap_1()
        .child(render_gallery_sort_button(main, cx, &translator));
    if let Some(group) = selected_group {
        search_actions = search_actions.child(render_edit_group_button(
            group,
            main.image_library.clone(),
            &translator,
            cx,
        ));
    }
    let search_actions = search_actions.child(add_images_button).into_any_element();
    let search_box = symbol_search::render(main, window, cx, Some(search_actions));
    let content = render_group_content(main, window, columns, cell_size, cx, &translator);

    symbol_layout::render(sidebar, search_box, content, horizontal_categories, cx)
}

pub(crate) fn render_gallery_import_overlay(
    main: &MainWindow,
    cx: &mut Context<MainWindow>,
) -> AnyElement {
    let Some(progress) = main.gallery_import_progress else {
        return div().into_any_element();
    };
    let theme = cx.theme().colors;
    let translator = i18n::translator(cx);
    let completed = progress.completed.min(progress.total);
    let ratio = if progress.total == 0 {
        0.0
    } else {
        completed as f32 / progress.total as f32
    };
    let title = translator.translate_with_args(
        "main.gallery.importing",
        &[
            ("completed", completed.to_string()),
            ("total", progress.total.to_string()),
        ],
    );
    let progress_width = 240.0 * ratio;

    div()
        .id("gallery-import-overlay")
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui::ColorExt::opacity(&theme.popover, 0.58))
        .child(
            div()
                .w(ui_rem(320.0))
                .px(ui_rem(36.0))
                .py(ui_rem(28.0))
                .flex()
                .flex_col()
                .items_center()
                .gap(ui_rem(18.0))
                .rounded_xl()
                .bg(theme.popover)
                .shadow_lg()
                .child(icon("refresh", 44.0, theme.primary))
                .child(
                    div()
                        .text_size(ui_rem(18.0))
                        .text_color(theme.foreground)
                        .child(title),
                )
                .child(
                    div()
                        .h(ui_rem(8.0))
                        .w(ui_rem(240.0))
                        .rounded_full()
                        .bg(theme.border)
                        .overflow_hidden()
                        .child(
                            div()
                                .h_full()
                                .w(ui_rem(progress_width))
                                .rounded_full()
                                .bg(theme.primary),
                        ),
                ),
        )
        .into_any_element()
}

fn gallery_grid_columns(main: &MainWindow, window: &Window, horizontal_categories: bool) -> u16 {
    let viewport_width =
        f32::from(window.viewport_size().width) / crate::ui::scale::window_scale(window);
    let main_sidebar_width =
        if !horizontal_categories && crate::windows::main::uses_sidebar_layout(viewport_width) {
            if main.sidebar_collapsed { 56.0 } else { 132.0 }
        } else {
            0.0
        };
    let category_sidebar_width = if horizontal_categories {
        0.0
    } else {
        symbol_sidebar::TAB_BAR_HEIGHT
    };
    let content_width = (viewport_width
        - main_sidebar_width
        - category_sidebar_width
        - GALLERY_GRID_HORIZONTAL_PADDING * 2.0)
        .max(0.0);
    let columns_for_max_size =
        ((content_width + GALLERY_GRID_GAP) / (GALLERY_CELL_MAX_WIDTH + GALLERY_GRID_GAP)).ceil();
    (columns_for_max_size as u16).clamp(1, GALLERY_MAX_COLUMNS)
}

fn gallery_cell_size(
    main: &MainWindow,
    window: &Window,
    columns: u16,
    horizontal_categories: bool,
) -> f32 {
    let viewport_width =
        f32::from(window.viewport_size().width) / crate::ui::scale::window_scale(window);
    let main_sidebar_width =
        if !horizontal_categories && crate::windows::main::uses_sidebar_layout(viewport_width) {
            if main.sidebar_collapsed { 56.0 } else { 132.0 }
        } else {
            0.0
        };
    let category_sidebar_width = if horizontal_categories {
        0.0
    } else {
        symbol_sidebar::TAB_BAR_HEIGHT
    };
    let content_width = (viewport_width
        - main_sidebar_width
        - category_sidebar_width
        - GALLERY_GRID_HORIZONTAL_PADDING * 2.0)
        .max(0.0);
    let gaps = GALLERY_GRID_GAP * f32::from(columns.saturating_sub(1));
    ((content_width - gaps) / f32::from(columns)).max(1.0) * crate::ui::scale::window_scale(window)
}

fn render_add_images_button(
    translator: &i18n::Translator,
    cx: &mut Context<MainWindow>,
) -> AnyElement {
    let theme = cx.theme().colors;
    let main_window = cx.entity().clone();
    let translator_for_click = translator.clone();
    let label = translator.translate("main.gallery.add_images");
    div()
        .id("gallery-add-images")
        .size(ui_rem(symbol_sidebar::TAB_BUTTON_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_lg()
        .cursor_pointer()
        .managed_tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
        .hover(|this| this.bg(theme.secondary_hover))
        .on_click(cx.listener(move |_, _, window, cx| {
            choose_gallery_images(main_window.clone(), window, cx, &translator_for_click);
        }))
        .child(hover_icon(
            "gallery-add-images",
            "photo-plus",
            18.0,
            theme.primary,
        ))
        .into_any_element()
}

fn render_sidebar(
    main: &MainWindow,
    cx: &mut Context<MainWindow>,
    translator: &i18n::Translator,
    horizontal: bool,
    show_labels: bool,
) -> AnyElement {
    let theme = cx.theme().colors;
    let main_window = cx.entity().clone();
    let image_library = main.image_library.clone();
    let animations_enabled = crate::config_runtime::animation_enabled(
        cx,
        crate::config::AnimationKind::SingleSelectIndicator,
    );
    let active_index = main
        .selected_gallery_group_id
        .as_ref()
        .and_then(|active_id| {
            main.gallery_groups
                .iter()
                .position(|group| &group.id == active_id)
        });
    let group_buttons = main
        .gallery_groups
        .iter()
        .map(|group| {
            render_group_button(
                group,
                main.selected_gallery_group_id.as_deref() == Some(group.id.as_str()),
                horizontal,
                show_labels,
                main,
                cx,
            )
        })
        .collect::<Vec<_>>();
    let group_list = if horizontal {
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_row()
            .items_center()
            .relative()
            .gap_1()
            .overflow_x_scrollbar()
            .children(group_buttons)
            .child(render_add_group_button(
                &main_window,
                image_library,
                translator,
                theme,
                true,
                show_labels,
                cx,
            ))
            .into_any_element()
    } else {
        div()
            .w_full()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .relative()
            .gap_1()
            .overflow_y_scrollbar()
            .when_some(active_index, |this, index| {
                this.child(symbol_sidebar::active_indicator_at(
                    "gallery-sidebar-active-indicator",
                    index,
                    0.0,
                    theme.primary,
                    animations_enabled,
                ))
            })
            .children(group_buttons)
            .into_any_element()
    };
    let mut sidebar = div()
        .bg(theme.tab_bar)
        .overflow_hidden()
        .on_drag_move(|event: &DragMoveEvent<GalleryImageDrag>, _, cx| {
            let Some(drag) = event.dragged_item().downcast_ref::<GalleryImageDrag>() else {
                return;
            };
            if drag.preview_scale.replace(1.0) != 1.0 {
                #[cfg(target_os = "windows")]
                DragPreviewService::refresh(cx);
            }
            #[cfg(not(target_os = "windows"))]
            let _ = cx;
        })
        .on_drop(cx.listener(|_, drag: &GalleryImageDrag, _, cx| {
            drag.preview_scale.set(1.0);
            #[cfg(target_os = "windows")]
            {
                DragPreviewService::hide(cx);
                crate::platform::external_drag::ExternalDragService::cancel(cx);
            }
        }));
    if horizontal {
        sidebar = sidebar
            .w_full()
            .min_w_0()
            .h(px(symbol_sidebar::TAB_BAR_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .border_l(px(0.5))
            .border_color(theme.border)
            .child(group_list);
    } else {
        sidebar = sidebar
            .w(ui_rem(symbol_sidebar::TAB_BAR_HEIGHT))
            .min_w(ui_rem(symbol_sidebar::TAB_BAR_HEIGHT))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .py_1()
            .child(group_list)
            .child(render_add_group_button(
                &main_window,
                main.image_library.clone(),
                translator,
                theme,
                false,
                false,
                cx,
            ))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .bottom_0()
                    .w(px(0.5))
                    .bg(theme.border),
            );
    }
    sidebar.into_any_element()
}

fn render_group_button(
    group: &ImageLibraryGroup,
    active: bool,
    horizontal: bool,
    show_label: bool,
    main: &MainWindow,
    cx: &mut Context<MainWindow>,
) -> AnyElement {
    let theme = cx.theme().colors;
    let group_id = group.id.clone();
    let target_group_id_for_style = group.id.clone();
    let target_group_id_for_check = group.id.clone();
    let target_group_id_for_scale = group.id.clone();
    let target_group_id_for_drop = group.id.clone();
    let label = group.name.clone();
    let tooltip_label = label.clone();
    let icon_path = main.image_library.group_icon_path(group);
    let icon_image = icon_path
        .as_ref()
        .and_then(|path| main.loaded_gallery_group_icons.get(path))
        .cloned();
    let mut group_button = div()
        .id(format!("gallery-group-{}", group.id))
        .h(ui_rem(symbol_sidebar::TAB_BUTTON_HEIGHT))
        .flex()
        .items_center()
        .rounded_lg()
        .cursor_pointer()
        .on_click(cx.listener(move |this, _, window, cx| {
            if this.selected_gallery_group_id.as_deref() == Some(group_id.as_str()) {
                this.refresh_gallery_images(cx);
            } else {
                this.select_gallery_group(&group_id, cx);
            }
            let _ = window;
        }));
    if horizontal && show_label {
        group_button = group_button
            .min_w_auto()
            .flex_1()
            .justify_center()
            .gap_1()
            .px_2();
    } else if horizontal {
        group_button = group_button
            .w(ui_rem(symbol_sidebar::TAB_BUTTON_HEIGHT))
            .min_w(ui_rem(symbol_sidebar::TAB_BUTTON_HEIGHT))
            .flex_1()
            .justify_center();
    } else {
        group_button = group_button
            .size(ui_rem(symbol_sidebar::TAB_BUTTON_HEIGHT))
            .flex_none()
            .justify_center();
    }
    group_button = group_button
        .managed_tooltip(move |window, cx| Tooltip::new(tooltip_label.clone()).build(window, cx));
    if horizontal && active {
        group_button = group_button.bg(theme.primary).shadow_sm();
    } else if !active {
        group_button = group_button.hover(|this| this.bg(theme.secondary_hover));
    }
    let icon = match icon_image {
        Some(image) => div()
            .size(ui_rem(symbol_sidebar::TAB_BUTTON_HEIGHT - 4.0))
            .flex_none()
            .aspect_square()
            .rounded_lg()
            .overflow_hidden()
            .child(
                img(image)
                    .size_full()
                    .aspect_square()
                    .object_fit(ObjectFit::Cover)
                    .rounded_lg(),
            )
            .into_any_element(),
        None => hover_icon(
            format!("gallery-group-icon-{}", group.id),
            "photo",
            18.0,
            if active {
                theme.primary_foreground
            } else {
                theme.foreground
            },
        )
        .into_any_element(),
    };
    let group_button = group_button
        .drag_over::<GalleryImageDrag>(move |this, drag, _, cx| {
            if drag.source_group_id.as_deref() != Some(target_group_id_for_style.as_str()) {
                this.bg(cx.theme().drop_target)
            } else {
                this
            }
        })
        .can_drop(move |dragged, _, _| {
            dragged
                .downcast_ref::<GalleryImageDrag>()
                .is_some_and(|drag| {
                    drag.source_group_id.as_deref() != Some(target_group_id_for_check.as_str())
                })
        })
        .on_drag_move(cx.listener(
            move |_, event: &DragMoveEvent<GalleryImageDrag>, window, cx| {
                let Some(drag) = event.dragged_item().downcast_ref::<GalleryImageDrag>() else {
                    return;
                };
                if drag.source_group_id.as_deref() == Some(target_group_id_for_scale.as_str())
                    || !event.bounds.contains(&window.mouse_position())
                {
                    return;
                }
                if drag
                    .preview_scale
                    .replace(GALLERY_DRAG_PREVIEW_ACCEPT_SCALE)
                    != GALLERY_DRAG_PREVIEW_ACCEPT_SCALE
                {
                    #[cfg(target_os = "windows")]
                    DragPreviewService::refresh(cx);
                }
                #[cfg(not(target_os = "windows"))]
                let _ = cx;
            },
        ))
        .on_drop(cx.listener(move |this, drag: &GalleryImageDrag, _, cx| {
            if drag.source_group_id.as_deref() == Some(target_group_id_for_drop.as_str()) {
                return;
            }
            #[cfg(target_os = "windows")]
            {
                DragPreviewService::hide(cx);
                crate::platform::external_drag::ExternalDragService::cancel(cx);
            }
            cx.stop_propagation();
            this.move_gallery_image(drag.path.clone(), target_group_id_for_drop.clone(), cx);
        }))
        .child(icon)
        .when(horizontal && show_label, |this| {
            this.child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(ui_rem(12.0))
                    .text_color(if active {
                        theme.primary_foreground
                    } else {
                        theme.foreground
                    })
                    .child(group.name.clone()),
            )
        });
    group_button.into_any_element()
}

fn render_edit_group_button(
    group: &ImageLibraryGroup,
    image_library: ImageLibraryService,
    translator: &i18n::Translator,
    cx: &mut Context<MainWindow>,
) -> AnyElement {
    let theme = cx.theme().colors;
    let main_window = cx.entity().clone();
    let group = group.clone();
    let label = translator.translate("main.gallery.edit_current_group");
    div()
        .id("gallery-edit-group")
        .size(ui_rem(symbol_sidebar::TAB_BUTTON_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_lg()
        .cursor_pointer()
        .managed_tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
        .hover(|this| this.bg(theme.secondary_hover))
        .on_click(cx.listener(move |_, _, window, cx| {
            open_edit_group_form(
                main_window.clone(),
                image_library.clone(),
                group.clone(),
                window,
                cx,
            );
        }))
        .child(hover_icon(
            "gallery-edit-group",
            "edit",
            18.0,
            theme.foreground,
        ))
        .into_any_element()
}

fn render_add_group_button(
    main_window: &gpui::Entity<MainWindow>,
    image_library: ImageLibraryService,
    translator: &i18n::Translator,
    theme: gpui_component::ThemeColor,
    horizontal: bool,
    show_label: bool,
    cx: &mut Context<MainWindow>,
) -> AnyElement {
    let label = translator.translate("main.gallery.add_group");
    let tooltip_label = label.clone();
    let main_window = main_window.clone();
    let mut button = div()
        .id("gallery-add-group")
        .h(ui_rem(symbol_sidebar::TAB_BUTTON_HEIGHT))
        .flex()
        .items_center()
        .rounded_lg()
        .cursor_pointer()
        .on_click(cx.listener(move |_, _, window, cx| {
            open_new_group_form(main_window.clone(), image_library.clone(), window, cx);
        }));
    if horizontal && show_label {
        button = button.min_w_auto().flex_1().justify_center().gap_1().px_2();
    } else if horizontal {
        button = button
            .w(ui_rem(symbol_sidebar::TAB_BUTTON_HEIGHT))
            .min_w(ui_rem(symbol_sidebar::TAB_BUTTON_HEIGHT))
            .flex_1()
            .justify_center();
    } else {
        button = button
            .size(ui_rem(symbol_sidebar::TAB_BUTTON_HEIGHT))
            .flex_none()
            .justify_center();
    }
    button
        .managed_tooltip(move |window, cx| Tooltip::new(tooltip_label.clone()).build(window, cx))
        .hover(|this| this.bg(theme.secondary_hover))
        .child(hover_icon(
            "gallery-add-group",
            "plus",
            18.0,
            theme.foreground,
        ))
        .when(horizontal && show_label, |this| {
            this.child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(ui_rem(12.0))
                    .text_color(theme.foreground)
                    .child(label),
            )
        })
        .into_any_element()
}

fn render_group_content(
    main: &mut MainWindow,
    window: &mut Window,
    columns: u16,
    cell_size: f32,
    cx: &mut Context<MainWindow>,
    translator: &i18n::Translator,
) -> AnyElement {
    if main.gallery_groups_loading || main.gallery_images_loading || main.gallery_images.is_empty()
    {
        main.ensure_gallery_thumbnails(&[], window, cx);
        main.sync_gallery_animations(&[], window, cx);
    }
    if main.gallery_groups_loading {
        return centered_message(
            translator.translate("main.gallery.loading_groups"),
            cx.theme().colors.muted_foreground,
        );
    }
    if main.gallery_groups.is_empty() {
        return empty_state(
            translator.translate("main.gallery.no_groups"),
            translator.translate("main.gallery.add_group_hint"),
            false,
            cx,
            translator,
        );
    }
    let content = if main.gallery_images_loading {
        centered_message(
            translator.translate("main.gallery.loading_images"),
            cx.theme().colors.muted_foreground,
        )
    } else {
        let images = main.gallery_images_cache.get(
            main.gallery_images_generation,
            &main.gallery_images,
            main.gallery_sort,
            &main.symbol_search_query,
        );
        if images.is_empty() {
            main.ensure_gallery_thumbnails(&[], window, cx);
            main.sync_gallery_animations(&[], window, cx);
            let has_query = !main.symbol_search_query.trim().is_empty();
            let title = if has_query {
                translator.translate("main.gallery.search_empty")
            } else {
                translator.translate("main.gallery.empty_group")
            };
            let detail = if has_query {
                String::new()
            } else {
                translator.translate("main.gallery.drop_hint")
            };
            empty_state(title, detail, !has_query, cx, translator)
        } else {
            render_gallery_grid(main, window, images, columns, cell_size, cx)
        }
    };

    render_gallery_drop_target(content, main.gallery_drop_hovering, cx)
}

fn sort_gallery_images(images: &mut [ImageLibraryImage], sort: PersistedGallerySort) {
    images.sort_by(|left, right| {
        let comparison = match sort.field {
            GallerySortField::Name => left
                .name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then_with(|| left.name.cmp(&right.name)),
            GallerySortField::Type => left
                .file_type
                .cmp(&right.file_type)
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
                .then_with(|| left.name.cmp(&right.name)),
            GallerySortField::ModifiedDate => left
                .modified_at
                .cmp(&right.modified_at)
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
                .then_with(|| left.name.cmp(&right.name)),
            GallerySortField::CreatedDate => left
                .created_at
                .cmp(&right.created_at)
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
                .then_with(|| left.name.cmp(&right.name)),
            GallerySortField::UsageFrequency => left
                .usage_score
                .total_cmp(&right.usage_score)
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
                .then_with(|| left.name.cmp(&right.name)),
        };
        if sort.order == GallerySortOrder::Descending {
            comparison.reverse()
        } else {
            comparison
        }
    });
}

fn render_gallery_drop_target(
    content: AnyElement,
    hovering: bool,
    cx: &mut Context<MainWindow>,
) -> AnyElement {
    let theme = cx.theme().colors;
    let main_window = cx.entity().clone();
    let main_window_for_move = main_window.clone();
    let main_window_for_reset = main_window.downgrade();
    let label = i18n::translator(cx).translate("main.gallery.drop_overlay");
    let mut target = div()
        .relative()
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .flex_col()
        .on_drop(move |paths: &ExternalPaths, _, cx| {
            let paths = paths.paths().to_vec();
            main_window.update(cx, |main, cx| {
                main.gallery_drop_hovering = false;
                main.import_gallery_images(paths, cx);
                cx.notify();
            });
        })
        .on_mouse_move_all(move |_, phase, hitbox, window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            let is_file_drag = cx
                .active_drag()
                .is_some_and(|drag| drag.value.downcast_ref::<ExternalPaths>().is_some());
            let hovering = is_file_drag && hitbox.is_hovered(window);
            main_window_for_move.update(cx, |main, cx| {
                if main.gallery_drop_hovering != hovering {
                    main.gallery_drop_hovering = hovering;
                    cx.notify();
                }
            });
        })
        .child(
            canvas(
                |_, _, _| (),
                move |_, _, window, _| {
                    let main_window = main_window_for_reset.clone();
                    window.on_mouse_event(move |event: &FileDropEvent, phase, _, cx| {
                        if phase != DispatchPhase::Bubble
                            || !matches!(event, FileDropEvent::Exited | FileDropEvent::Ended)
                        {
                            return;
                        }
                        let Some(main_window) = main_window.upgrade() else {
                            return;
                        };
                        let _ = main_window.update(cx, |main, cx| {
                            if main.gallery_drop_hovering {
                                main.gallery_drop_hovering = false;
                                cx.notify();
                            }
                        });
                    });
                },
            )
            .absolute()
            .size(px(0.0)),
        )
        .child(content);
    if hovering {
        target = target.child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .border_2()
                .border_color(theme.primary)
                .bg(gpui::ColorExt::opacity(&theme.primary, 0.1))
                .child(
                    div()
                        .px(ui_rem(28.0))
                        .py(ui_rem(16.0))
                        .rounded_xl()
                        .bg(theme.popover)
                        .shadow_lg()
                        .flex()
                        .items_center()
                        .gap(ui_rem(10.0))
                        .text_size(ui_rem(18.0))
                        .text_color(theme.foreground)
                        .child(hover_icon(
                            "gallery-drop-overlay-icon",
                            "arrow-bar-to-up",
                            24.0,
                            theme.primary,
                        ))
                        .child(label),
                ),
        );
    }
    target.into_any_element()
}

fn gallery_visible_rows(
    scroll_top: f32,
    viewport_height: f32,
    row_height: f32,
    padding: f32,
    row_count: usize,
) -> Range<usize> {
    let first = (((scroll_top + padding) / row_height).floor().max(0.0) as usize).min(row_count);
    if viewport_height <= 0.0 {
        return first..first;
    }
    let end = (((scroll_top + viewport_height - padding) / row_height)
        .ceil()
        .max(0.0) as usize)
        .clamp(first, row_count);
    first..end
}

fn gallery_viewport_paths(
    images: &[ImageLibraryImage],
    columns: usize,
    visible_rows: Range<usize>,
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let start = visible_rows.start.saturating_mul(columns).min(images.len());
    let visible_end = visible_rows.end.saturating_mul(columns).min(images.len());
    let cache_end = visible_rows
        .end
        .saturating_add(2)
        .saturating_mul(columns)
        .min(images.len());
    let thumbnail_paths = images[start..cache_end]
        .iter()
        .map(|image| image.path.clone())
        .collect();
    let visible_gifs = images[start..visible_end]
        .iter()
        .filter(|image| image.is_gif)
        .map(|image| image.path.clone())
        .collect();
    (thumbnail_paths, visible_gifs)
}

fn render_gallery_grid(
    main: &mut MainWindow,
    window: &mut Window,
    images: Rc<Vec<ImageLibraryImage>>,
    columns: u16,
    cell_size: f32,
    cx: &mut Context<MainWindow>,
) -> AnyElement {
    let columns = usize::from(columns.max(1));
    let rows = images.len().div_ceil(columns);
    let row_height =
        cell_size + GALLERY_GRID_VERTICAL_PADDING * 2.0 * crate::ui::scale::window_scale(window);

    let image_sizes = Rc::new(
        (0..rows)
            .map(|_| size(px(0.0), px(row_height)))
            .collect::<Vec<_>>(),
    );
    let scroll_handle = main.gallery_scroll_handle.clone();
    let loading_images = images.clone();
    let main_window = cx.weak_entity();
    let list = symbol_list::render_sized_virtual_list(
        "gallery-image-list",
        image_sizes,
        scroll_handle,
        MainWindow::handle_gallery_scroll_wheel,
        move |this, range: Range<usize>, _, cx| {
            range
                .filter_map(|row_index| {
                    let start = row_index * columns;
                    let end = start.saturating_add(columns).min(images.len());
                    (start < end).then(|| {
                        render_gallery_row(
                            &images[start..end],
                            row_index,
                            columns,
                            row_height,
                            this,
                            cx,
                        )
                    })
                })
                .collect::<Vec<_>>()
        },
        cx,
    );
    div()
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .flex_col()
        .child(list)
        .on_children_prepainted(move |_, window, cx| {
            // 行回调也用于测量首行，资源调度必须等实际列表完成布局后再执行。
            let _ = main_window.update(cx, |this, cx| {
                let scroll_top = -f32::from(this.gallery_scroll_handle.offset().y);
                let viewport_height = f32::from(
                    this.gallery_scroll_handle
                        .base_handle()
                        .bounds()
                        .size
                        .height,
                );
                let visible_rows = gallery_visible_rows(
                    scroll_top,
                    viewport_height,
                    row_height,
                    GALLERY_GRID_VERTICAL_PADDING * crate::ui::scale::window_scale(window),
                    rows,
                );
                let (thumbnail_paths, visible_gifs) =
                    gallery_viewport_paths(&loading_images, columns, visible_rows);
                this.ensure_gallery_thumbnails(&thumbnail_paths, window, cx);
                this.sync_gallery_animations(&visible_gifs, window, cx);
            });
        })
        .into_any_element()
}

fn render_gallery_row(
    images: &[ImageLibraryImage],
    row_index: usize,
    columns: usize,
    row_height: f32,
    main: &MainWindow,
    cx: &mut Context<MainWindow>,
) -> AnyElement {
    div()
        .id(("gallery-image-row", row_index))
        .h(px(row_height))
        .w_full()
        .grid()
        .grid_cols(columns as u16)
        .gap(ui_rem(GALLERY_GRID_GAP))
        .px(ui_rem(GALLERY_GRID_HORIZONTAL_PADDING))
        .py(ui_rem(GALLERY_GRID_VERTICAL_PADDING))
        .children(
            images
                .iter()
                .map(|image| render_gallery_image(image, main, cx)),
        )
        .into_any_element()
}

fn render_gallery_image(
    image: &ImageLibraryImage,
    main: &MainWindow,
    cx: &mut Context<MainWindow>,
) -> AnyElement {
    let theme = cx.theme().colors;
    let path = image.path.clone();
    let click_path = path.clone();
    let hover_path = path.clone();
    let is_gif = image.is_gif;
    let tooltip = image.name.clone();
    let displayed_image = if is_gif {
        main.loaded_gallery_animations
            .get(&path)
            .or_else(|| main.loaded_gallery_images.get(&path))
    } else {
        main.loaded_gallery_images.get(&path)
    }
    .cloned();
    let preview_image = displayed_image.clone();
    let preview_size = Rc::new(Cell::new(None));
    let image_content = displayed_image
        .map(|image| {
            img(image)
                .id(format!("gallery-image-content-{}", path.display()))
                .size_full()
                .object_fit(ObjectFit::Cover)
                .rounded_lg()
                .into_any_element()
        })
        .unwrap_or_else(|| {
            div()
                .size_full()
                .rounded_lg()
                .bg(cx.theme().colors.secondary)
                .into_any_element()
        });
    let mut tile = div()
        .id(format!("gallery-image-{}", image.name))
        .w_full()
        .aspect_square()
        .flex()
        .items_center()
        .justify_center()
        .rounded_lg()
        .overflow_hidden()
        .cursor_pointer()
        .managed_tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .ring_2()
        .ring_color(transparent_black())
        .hover(|this| this.bg(theme.secondary_hover).ring_color(theme.primary))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.paste_gallery_image(click_path.clone(), cx);
        }))
        .child(image_content);
    let preview_size_for_prepaint = preview_size.clone();
    tile = tile.on_prepaint(move |prepaint, _, _| {
        preview_size_for_prepaint.set(Some(prepaint.bounds.size));
    });
    if is_gif {
        tile = tile.on_hover(cx.listener(move |this, hovered, _, cx| {
            if *hovered {
                this.ensure_gallery_animation(hover_path.clone(), cx);
            } else {
                this.clear_gallery_hover(&hover_path);
                cx.notify();
            }
        }));
    }
    tile = crate::windows::main::window_drag::exclude_from_window_drag(tile, cx.entity());
    let drag_data = GalleryImageDrag {
        path: path.clone(),
        image: preview_image,
        size: preview_size.clone(),
        preview_scale: Rc::new(Cell::new(1.0)),
        source_group_id: main.selected_gallery_group_id.clone(),
    };
    tile = tile.on_drag(
        drag_data,
        move |drag: &GalleryImageDrag, cursor_offset, window, cx| {
            cx.stop_propagation();
            let source_preview = cx.new(|_| GalleryImageDragPreview {
                drag: drag.clone(),
                render_preview: !cfg!(target_os = "windows"),
            });
            #[cfg(target_os = "windows")]
            {
                if let Some(preview_dimensions) = drag.size.get()
                    && let Some((initial_bounds, display_id, cursor_offset_physical)) =
                        gallery_drag_preview_bounds(window, cx, cursor_offset, preview_dimensions)
                {
                    let popup_preview = cx.new(|_| GalleryImageDragPreview {
                        drag: drag.clone(),
                        render_preview: true,
                    });
                    if let Err(error) = DragPreviewService::start(
                        window.window_handle(),
                        popup_preview,
                        initial_bounds,
                        display_id,
                        cursor_offset_physical,
                        px(16.0),
                        |_| {},
                        cx,
                    ) {
                        tracing::error!("启动图库图片拖拽预览失败：{error}");
                    }
                }
                begin_external_file_drag(&drag.path, window, cx);
            }
            #[cfg(not(target_os = "windows"))]
            let _ = cursor_offset;
            source_preview
        },
    );
    let main_window = cx.entity().clone();
    let source_group_id = main.selected_gallery_group_id.clone();
    let groups = main.gallery_groups.clone();
    let image_name = image.name.clone();
    tile = tile.on_mouse_down(MouseButton::Right, move |event, window, cx| {
        window.prevent_default();
        cx.stop_propagation();
        let Some(anchor) = logical_to_physical_desktop(window, event.position) else {
            tracing::error!("无法确定图库菜单位置，未打开图片菜单");
            return;
        };
        let source_group_id = source_group_id.clone();
        let move_items = groups
            .iter()
            .filter(|group| Some(group.id.as_str()) != source_group_id.as_deref())
            .map(|group| {
                MenuItem::action(
                    MenuActionId::new(format!("move-gallery-{}", group.id)),
                    group.name.clone(),
                )
            })
            .collect::<Vec<_>>();
        let translator = i18n::translator(cx);
        let move_item = if move_items.is_empty() {
            MenuItem::submenu(
                translator.translate("main.gallery.menu.move_to_group"),
                vec![
                    MenuItem::action(
                        MenuActionId::new("move-gallery-none"),
                        translator.translate("main.gallery.menu.no_other_group"),
                    )
                    .disabled(),
                ],
            )
        } else {
            MenuItem::submenu(
                translator.translate("main.gallery.menu.move_to_group"),
                move_items,
            )
        }
        .with_icon(MenuIcon::svg("arrows-move.svg"));
        let menu = MenuModel::new(vec![
            MenuItem::action(
                MenuActionId::new("copy-gallery"),
                translator.translate("main.menu.copy"),
            )
            .with_icon(MenuIcon::svg("copy.svg")),
            MenuItem::action(
                MenuActionId::new("rename-gallery"),
                translator.translate("main.gallery.menu.rename"),
            )
            .with_icon(MenuIcon::svg("edit.svg")),
            move_item,
            MenuItem::action(
                MenuActionId::new("delete-gallery"),
                translator.translate("main.menu.delete"),
            )
            .with_icon(MenuIcon::svg("trash.svg")),
        ]);
        let path = path.clone();
        let image_name = image_name.clone();
        let main_window = main_window.clone();
        let owner_window = window.window_handle();
        let position =
            crate::windows::menu::MenuPosition::new(anchor.x, anchor.y, window.scale_factor());
        MenuService::open_context_menu(
            menu,
            position,
            move |action, cx| {
                let action = action.as_str();
                if action == "copy-gallery" {
                    main_window.update(cx, |this, cx| this.copy_gallery_image(path.clone(), cx));
                } else if action == "rename-gallery" {
                    open_rename_gallery_image_form(
                        main_window.clone(),
                        path.clone(),
                        image_name.clone(),
                        owner_window.clone(),
                        cx,
                    );
                } else if action == "delete-gallery" {
                    main_window.update(cx, |this, cx| this.delete_gallery_image(path.clone(), cx));
                } else if let Some(group_id) = action.strip_prefix("move-gallery-") {
                    if group_id != "none" {
                        main_window.update(cx, |this, cx| {
                            this.move_gallery_image(path.clone(), group_id.to_owned(), cx)
                        });
                    }
                }
            },
            cx,
        );
    });
    tile.into_any_element()
}

fn render_gallery_sort_button(
    main: &MainWindow,
    cx: &mut Context<MainWindow>,
    translator: &i18n::Translator,
) -> AnyElement {
    let theme = cx.theme().colors;
    let trigger_bounds = Rc::new(Cell::new(None::<Bounds<Pixels>>));
    let trigger_bounds_for_paint = trigger_bounds.clone();
    let trigger_bounds_for_click = trigger_bounds.clone();
    let main_window = cx.entity().clone();
    let sort = main.gallery_sort;
    let menu = DropdownModel::new(vec![
        DropdownItem::option(
            DropdownItemId::new("field:name"),
            translator.translate("main.gallery.sort.name"),
        )
        .selected(sort.field == GallerySortField::Name),
        DropdownItem::option(
            DropdownItemId::new("field:type"),
            translator.translate("main.gallery.sort.type"),
        )
        .selected(sort.field == GallerySortField::Type),
        DropdownItem::option(
            DropdownItemId::new("field:modified"),
            translator.translate("main.gallery.sort.modified_date"),
        )
        .selected(sort.field == GallerySortField::ModifiedDate),
        DropdownItem::option(
            DropdownItemId::new("field:created"),
            translator.translate("main.gallery.sort.created_date"),
        )
        .selected(sort.field == GallerySortField::CreatedDate),
        DropdownItem::option(
            DropdownItemId::new("field:usage"),
            translator.translate("main.gallery.sort.usage_frequency"),
        )
        .selected(sort.field == GallerySortField::UsageFrequency),
        DropdownItem::separator(),
        DropdownItem::option(
            DropdownItemId::new("order:ascending"),
            translator.translate("main.gallery.sort.ascending"),
        )
        .selected(sort.order == GallerySortOrder::Ascending),
        DropdownItem::option(
            DropdownItemId::new("order:descending"),
            translator.translate("main.gallery.sort.descending"),
        )
        .selected(sort.order == GallerySortOrder::Descending),
    ]);
    let label = translator.translate("main.gallery.sort");
    let sort_label = label.clone();
    div()
        .id("gallery-sort")
        .size(ui_rem(symbol_sidebar::TAB_BUTTON_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_lg()
        .cursor_pointer()
        .aria_label(label)
        .hover(|this| this.bg(theme.secondary_hover))
        .managed_tooltip(move |window, cx| Tooltip::new(sort_label.clone()).build(window, cx))
        .on_prepaint(move |prepaint, _, _| {
            trigger_bounds_for_paint.set(Some(prepaint.bounds));
        })
        .on_click(move |_, window, cx| {
            let Some(bounds) = trigger_bounds_for_click.get() else {
                return;
            };
            window.bounds_changed(cx);
            let scale_factor = window.scale_factor().max(1.0);
            let Some(origin) =
                crate::platform::window::logical_to_physical_desktop(window, bounds.origin)
            else {
                tracing::error!("无法确定图库排序菜单位置，未打开菜单");
                return;
            };
            let anchor = DropdownAnchor::new(
                origin.x,
                origin.y,
                f32::from(bounds.size.width) * scale_factor,
                f32::from(bounds.size.height) * scale_factor,
                scale_factor,
            );
            let main_window = main_window.clone();
            MenuService::open_dropdown_menu(
                menu.clone(),
                anchor,
                move |item, cx| {
                    main_window.update(cx, |main, cx| {
                        let mut sort = main.gallery_sort;
                        match item.as_str() {
                            "field:name" => sort.field = GallerySortField::Name,
                            "field:type" => sort.field = GallerySortField::Type,
                            "field:modified" => sort.field = GallerySortField::ModifiedDate,
                            "field:created" => sort.field = GallerySortField::CreatedDate,
                            "field:usage" => sort.field = GallerySortField::UsageFrequency,
                            "order:ascending" => sort.order = GallerySortOrder::Ascending,
                            "order:descending" => sort.order = GallerySortOrder::Descending,
                            _ => return,
                        }
                        main.set_gallery_sort(sort, cx);
                    });
                },
                cx,
            );
        })
        .child(hover_icon(
            "gallery-sort",
            "sort-ascending",
            16.0,
            theme.foreground,
        ))
        .into_any_element()
}

fn empty_state(
    title: String,
    detail: String,
    can_add: bool,
    cx: &mut Context<MainWindow>,
    translator: &i18n::Translator,
) -> AnyElement {
    let theme = cx.theme().colors;
    let main_window = cx.entity().clone();
    let choose_button = can_add.then(|| {
        let translator = translator.clone();
        let translator_for_click = translator.clone();
        div()
            .id("gallery-empty-choose-images")
            .mt(ui_rem(12.0))
            .px(ui_rem(12.0))
            .py(ui_rem(7.0))
            .rounded_md()
            .cursor_pointer()
            .text_size(ui_rem(12.0))
            .text_color(theme.primary_foreground)
            .bg(theme.primary)
            .hover(|this| this.bg(theme.primary_hover))
            .on_click(cx.listener(move |_, _, window, cx| {
                choose_gallery_images(main_window.clone(), window, cx, &translator_for_click);
            }))
            .child(translator.translate("main.gallery.choose_images"))
            .into_any_element()
    });
    div()
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(ui_rem(8.0))
                .child(
                    div()
                        .size(ui_rem(120.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_2xl()
                        .border_2()
                        .border_dashed()
                        .border_color(theme.border)
                        .child(hover_icon("gallery-empty", "photo", 40.0, theme.foreground)),
                )
                .child(
                    div()
                        .text_size(ui_rem(16.0))
                        .text_color(theme.foreground)
                        .child(title),
                )
                .when(!detail.is_empty(), |this| {
                    this.child(
                        div()
                            .text_size(ui_rem(13.0))
                            .text_color(theme.muted_foreground)
                            .child(detail),
                    )
                })
                .when_some(choose_button, |this, button| this.child(button)),
        )
        .into_any_element()
}

fn centered_message(message: String, color: gpui::Hsla) -> AnyElement {
    div()
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(color)
        .child(message)
        .into_any_element()
}

fn open_new_group_form(
    main_window: gpui::Entity<MainWindow>,
    image_library: ImageLibraryService,
    window: &mut Window,
    cx: &mut Context<MainWindow>,
) {
    let result = crate::windows::dialog::open_adaptive_window(
        Some(window),
        cx,
        crate::windows::dialog::DialogWindowPosition::Center,
        crate::windows::dialog::DialogMode::Modal,
        move |window, cx| {
            let on_saved = move |group, cx: &mut App| {
                main_window.update(cx, |this, cx| this.add_image_library_group(group, cx));
            };
            cx.new(|cx| NewGalleryGroupForm::new(window, cx, image_library, on_saved))
        },
    );
    if let Err(error) = result {
        tracing::error!("打开图库组窗口失败：{error}");
    }
}

fn open_edit_group_form(
    main_window: gpui::Entity<MainWindow>,
    image_library: ImageLibraryService,
    group: ImageLibraryGroup,
    window: &mut Window,
    cx: &mut Context<MainWindow>,
) {
    let result = crate::windows::dialog::open_adaptive_window(
        Some(window),
        cx,
        crate::windows::dialog::DialogWindowPosition::Center,
        crate::windows::dialog::DialogMode::Modal,
        move |window, cx| {
            let delete_main_window = main_window.clone();
            let on_saved = move |group, cx: &mut App| {
                main_window.update(cx, |this, cx| this.update_image_library_group(group, cx));
            };
            let on_deleted = move |group_id: String, cx: &mut App| {
                delete_main_window.update(cx, |this, cx| {
                    this.remove_image_library_group(&group_id, cx)
                });
            };
            cx.new(|cx| {
                NewGalleryGroupForm::edit(window, cx, image_library, group, on_saved, on_deleted)
            })
        },
    );
    if let Err(error) = result {
        tracing::error!("打开图库组编辑窗口失败：{error}");
    }
}

fn open_rename_gallery_image_form(
    main_window: gpui::Entity<MainWindow>,
    path: PathBuf,
    name: String,
    owner_window: gpui::AnyWindowHandle,
    cx: &mut App,
) {
    let result = owner_window.update(cx, |_, window, cx| {
        crate::windows::dialog::open_adaptive_window(
            Some(window),
            cx,
            crate::windows::dialog::DialogWindowPosition::Center,
            crate::windows::dialog::DialogMode::Modal,
            move |window, cx| {
                let on_saved = move |new_name, cx: &mut App| {
                    main_window
                        .update(cx, |this, cx| this.rename_gallery_image(path, new_name, cx));
                };
                cx.new(|cx| RenameGalleryImageForm::new(window, cx, name, on_saved))
            },
        )
    });
    if let Err(error) = result {
        tracing::error!("打开图库图片重命名窗口失败：{error}");
    }
}

fn choose_gallery_images(
    main_window: gpui::Entity<MainWindow>,
    window: &mut Window,
    cx: &mut Context<MainWindow>,
    translator: &i18n::Translator,
) {
    let dialog = rfd::AsyncFileDialog::new()
        .set_title(translator.translate("main.gallery.choose_images"))
        .add_filter("图片文件", &["png", "jpg", "jpeg", "gif", "webp"]);
    let _ = window;
    cx.spawn(async move |_, cx| {
        let files = dialog.pick_files().await.unwrap_or_default();
        if files.is_empty() {
            return;
        }
        let paths = files
            .iter()
            .map(|file| file.path().to_path_buf())
            .collect::<Vec<_>>();
        cx.update(|cx| {
            main_window.update(cx, |main, cx| main.import_gallery_images(paths, cx));
        });
    })
    .detach();
}

#[cfg(target_os = "windows")]
fn gallery_drag_preview_bounds(
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

fn begin_external_file_drag(path: &PathBuf, window: &mut Window, cx: &mut App) {
    #[cfg(target_os = "windows")]
    {
        use crate::platform::{
            clipboard::ClipboardPayload, external_drag::ExternalDragService,
            image_temp_file::hdrop_bytes_for_paths,
        };

        let payload = ClipboardPayload {
            format_kind: "file_list".to_owned(),
            format_namespace: "windows".to_owned(),
            format_key: "CF_HDROP".to_owned(),
            format_name: "CF_HDROP".to_owned(),
            native_format_id: Some(15),
            bytes: hdrop_bytes_for_paths(std::slice::from_ref(path)),
        };
        if let Err(error) = ExternalDragService::begin(window.window_handle(), vec![payload], cx) {
            tracing::error!("准备拖出图库图片失败：{error}");
        }
    }
    #[cfg(not(target_os = "windows"))]
    let _ = (path, window, cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gif_visibility_excludes_overscan_and_counts_partial_tiles() {
        assert_eq!(gallery_visible_rows(0.0, 210.0, 100.0, 10.0, 10), 0..2);
        assert_eq!(gallery_visible_rows(90.0, 20.0, 100.0, 10.0, 10), 1..1);
        assert_eq!(gallery_visible_rows(89.0, 22.0, 100.0, 10.0, 10), 0..2);
        assert_eq!(gallery_visible_rows(0.0, 1000.0, 100.0, 10.0, 3), 0..3);
        assert_eq!(gallery_visible_rows(100.0, 0.0, 100.0, 10.0, 10), 1..1);
    }

    fn image(name: &str) -> ImageLibraryImage {
        ImageLibraryImage {
            path: PathBuf::from(name),
            name: name.to_owned(),
            is_gif: true,
            file_type: "gif".to_owned(),
            modified_at: std::time::UNIX_EPOCH,
            created_at: std::time::UNIX_EPOCH,
            usage_score: 0.0,
        }
    }

    #[test]
    fn scrolled_viewport_loads_static_images_and_gifs_without_requesting_the_first_row() {
        let images = (0..20)
            .map(|index| {
                let mut entry = image(&format!("{index}.png"));
                entry.is_gif = index % 3 == 1;
                entry
            })
            .collect::<Vec<_>>();
        let visible_rows = gallery_visible_rows(300.0, 200.0, 100.0, 10.0, 10);
        let (thumbnails, animations) = gallery_viewport_paths(&images, 2, visible_rows);
        assert_eq!(
            thumbnails,
            images[6..14]
                .iter()
                .map(|image| image.path.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(animations, vec![images[7].path.clone()]);
        assert!(thumbnails.contains(&images[6].path));
        assert!(thumbnails.contains(&images[7].path));
        assert!(!thumbnails.contains(&images[0].path));
        assert!(!animations.contains(&images[10].path));
    }

    #[test]
    fn initial_viewport_loads_all_static_rows_without_any_animated_images() {
        let images = (0..12)
            .map(|index| {
                let mut entry = image(&format!("{index}.png"));
                entry.is_gif = false;
                entry
            })
            .collect::<Vec<_>>();
        let visible_rows = gallery_visible_rows(0.0, 300.0, 100.0, 10.0, 6);
        let (thumbnails, animations) = gallery_viewport_paths(&images, 2, visible_rows);
        assert_eq!(
            thumbnails,
            images[..10]
                .iter()
                .map(|image| image.path.clone())
                .collect::<Vec<_>>()
        );
        assert!(animations.is_empty());
    }

    #[test]
    fn gallery_cache_reuses_frames_and_invalidates_for_search_sort_and_reload() {
        let mut cache = GalleryImagesCache::default();
        let mut images = vec![image("B.gif"), image("A.gif")];
        let sort = PersistedGallerySort::default();
        let first = cache.get(1, &images, sort, "");
        assert_eq!(first[0].name, "A.gif");
        assert!(Rc::ptr_eq(&first, &cache.get(1, &images, sort, "  ")));
        let searched = cache.get(1, &images, sort, " b ");
        assert_eq!(searched.len(), 1);
        assert_eq!(searched[0].name, "B.gif");
        assert!(Rc::ptr_eq(&searched, &cache.get(1, &images, sort, "B")));
        let descending = PersistedGallerySort {
            order: GallerySortOrder::Descending,
            ..sort
        };
        let reordered = cache.get(1, &images, descending, "");
        assert_eq!(reordered[0].name, "B.gif");
        images.push(image("C.gif"));
        let reloaded = cache.get(2, &images, descending, "");
        assert_eq!(reloaded.len(), 3);
        assert_eq!(reloaded[0].name, "C.gif");
    }
}
