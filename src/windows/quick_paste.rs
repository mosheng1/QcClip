use crate::ui::scale::ui_rem;

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::{
    config::HistoryConfig,
    domain::clipboard::{ContentKind, HistoryItemId},
    platform::window::{
        self as platform_window, BorderlessPopup, DisplayPlacement, PhysicalDisplayBounds,
    },
    services::{
        database::{FavoriteRepository, HistoryRepository, query::HistoryQuery},
        paste::PasteService,
    },
    state_runtime::PersistedQuickPasteTab,
    windows::list_order::nearest_survivor,
};
use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, DisplayId, Global, IsZero as _, ObjectFit, Pixels, Point, Render,
    RenderImage, ScrollWheelEvent, SharedString, Window, WindowHandle, div, img, point, px,
    relative, size,
};
use gpui_component::{Icon, IconNamed, Sizable as _};

const QUICK_PASTE_TAB_HEIGHT: f32 = 38.0;
const QUICK_PASTE_MAX_VISIBLE_ROWS: usize = 15;
const QUICK_PASTE_PREFETCH_ROWS: usize = 30;
const QUICK_PASTE_REVEAL_DURATION: Duration = Duration::from_millis(250);
const QUICK_PASTE_HIDE_DURATION: Duration = Duration::from_millis(100);

/// 卡片阴影的模糊半径。
const QUICK_PASTE_SHADOW_BLUR: f32 = 3.0;
/// 为阴影在窗口边缘预留的空间，避免被窗口裁剪；需不小于阴影模糊半径。
const QUICK_PASTE_SHADOW_MARGIN: f32 = QUICK_PASTE_SHADOW_BLUR + 1.0;

/// 偏移为 0 的阴影，使四周阴影均匀。
fn quick_paste_shadow(alpha: f32, blur: f32) -> Vec<gpui::BoxShadow> {
    vec![
        gpui::BoxShadow::new(px(0.0), px(0.0), gpui::hsla(0.0, 0.0, 0.0, alpha))
            .blur_radius(px(blur))
            .spread_radius(px(0.0)),
    ]
}
const QUICK_PASTE_REVEAL_FRAME: Duration = Duration::from_millis(16);

#[derive(Default)]
pub(crate) struct QuickPasteWindowService {
    history_repository: Option<HistoryRepository>,
    favorite_repository: Option<FavoriteRepository>,
    paste_service: Option<PasteService>,
    image_preview_service: Option<crate::services::image_preview::ImagePreviewService>,
    window: Option<WindowHandle<QuickPasteWindowView>>,
    active: bool,
    closing: bool,
}

impl Global for QuickPasteWindowService {}

impl QuickPasteWindowService {
    pub(crate) fn refresh_missing_files(cx: &mut App, changed: bool) {
        let window = cx.try_global::<Self>().and_then(|service| service.window);
        if let Some(window) = window {
            let _ = window.update(cx, |view, window, cx| {
                view.missing_visibility_dirty |= changed;
                if view.missing_visibility_dirty {
                    view.reload_source(view.source, true, window, cx);
                }
            });
        }
    }

    pub(crate) fn install(
        history_repository: HistoryRepository,
        favorite_repository: FavoriteRepository,
        paste_service: PasteService,
        image_preview_service: crate::services::image_preview::ImagePreviewService,
        cx: &mut App,
    ) {
        cx.set_global(Self {
            history_repository: Some(history_repository),
            favorite_repository: Some(favorite_repository),
            paste_service: Some(paste_service),
            image_preview_service: Some(image_preview_service),
            ..Default::default()
        });
        cx.on_window_closed(|cx, window_id| {
            let closed = cx.update_global::<Self, _>(|service, _| {
                if service
                    .window
                    .is_some_and(|window| window.window_id() == window_id)
                {
                    service.window = None;
                    service.active = false;
                    service.closing = false;
                    true
                } else {
                    false
                }
            });
            if closed {
                crate::hotkey::deactivate_quick_paste_navigation(cx);
            }
        })
        .detach();
    }

    pub(crate) fn replace_data_sources(
        database: crate::services::database::Database,
        blob_store: crate::services::storage::RawBlobStore,
        clipboard_write_suppressor: crate::platform::clipboard::ClipboardWriteSuppressor,
        cx: &mut App,
    ) {
        crate::hotkey::deactivate_quick_paste_navigation(cx);
        let history_repository = HistoryRepository::new(database.clone(), blob_store.clone());
        let favorite_repository =
            FavoriteRepository::with_blob_store(database.clone(), blob_store.clone());
        let image_preview_service =
            crate::services::image_preview::ImagePreviewService::new(blob_store.clone());
        let paste_service = PasteService::with_clipboard_write_suppressor(
            database,
            blob_store,
            clipboard_write_suppressor,
        );
        // 关闭窗口会同步触发 on_window_closed 回调并再次访问本全局，必须在 update_global 之外执行。
        let window = cx.update_global::<Self, _>(|service, _| {
            service.active = false;
            service.closing = false;
            service.history_repository = Some(history_repository);
            service.favorite_repository = Some(favorite_repository);
            service.paste_service = Some(paste_service);
            service.image_preview_service = Some(image_preview_service);
            service.window.take()
        });
        if let Some(window) = window {
            let _ = window.update(cx, |_, window, _| window.remove_window());
        }
    }

    pub(crate) fn show(cx: &mut App) {
        let should_show = cx.update_global::<Self, _>(|service, _| {
            if service.active || service.closing || service.window.is_some() {
                false
            } else {
                service.active = true;
                true
            }
        });
        if !should_show {
            return;
        }
        crate::windows::main::MainWindowService::request_missing_scan(cx);

        let (repository, favorite_repository, image_preview_service) =
            cx.read_global::<Self, _>(|service, _| {
                (
                    service.history_repository.clone(),
                    service.favorite_repository.clone(),
                    service.image_preview_service.clone(),
                )
            });
        let (Some(repository), Some(favorite_repository), Some(image_preview_service)) =
            (repository, favorite_repository, image_preview_service)
        else {
            cx.update_global::<Self, _>(|service, _| {
                service.active = false;
                service.closing = false;
            });
            return;
        };

        let history_config = crate::config_runtime::history_config(cx);
        let quick_paste_config = crate::config_runtime::quick_paste_config(cx);
        let configured_width = f32::from(quick_paste_config.window_width);
        let query = HistoryQuery {
            preview_char_limit: Some(500),
            load_image_sources: true,
            ..HistoryQuery::default()
        };
        let persisted_state = crate::state_runtime::state(cx);
        let source = persisted_state.main_window.quick_paste_tab;
        let configured_rows = usize::from(quick_paste_config.visible_items);
        let list_ids = match source {
            PersistedQuickPasteTab::Clipboard => {
                repository.ordered_ids_scoped(&query, &history_config)
            }
            PersistedQuickPasteTab::Favorites => {
                favorite_repository.ordered_ids_scoped(&query, &history_config)
            }
        };
        let list_ids = match list_ids {
            Ok(list_ids) => list_ids,
            Err(error) => {
                tracing::error!("查询便捷粘贴列表失败：{error}");
                cx.update_global::<Self, _>(|service, _| {
                    service.active = false;
                    service.closing = false;
                });
                return;
            }
        };
        let initial_index = if quick_paste_config.keep_last_position {
            persisted_state
                .main_window
                .quick_paste_index
                .min(list_ids.len().saturating_sub(1))
        } else {
            0
        };
        let initial_ids = list_ids
            .iter()
            .copied()
            .take(configured_rows)
            .collect::<Vec<_>>();
        let initial_query = HistoryQuery {
            load_image_sources: false,
            ..query.clone()
        };
        let initial_records = match query_list_records(
            source,
            &repository,
            &favorite_repository,
            &initial_query,
            &history_config,
            &initial_ids,
        ) {
            Ok(records) => records,
            Err(error) => {
                tracing::error!("预加载便捷粘贴首屏内容失败：{error}");
                cx.update_global::<Self, _>(|service, _| {
                    service.active = false;
                    service.closing = false;
                });
                return;
            }
        };
        let initial_index_by_id = list_ids
            .iter()
            .enumerate()
            .map(|(index, history_id)| (*history_id, index))
            .collect::<HashMap<_, _>>();
        let initial_records = initial_records
            .into_iter()
            .filter_map(|record| {
                let index = *initial_index_by_id.get(&record.history_id)?;
                Some((index, QuickPasteRecord::from_initial(record)))
            })
            .collect::<HashMap<_, _>>();
        let pointer = platform_window::physical_cursor_position();
        let display =
            pointer.and_then(|pointer| platform_window::work_area_for_physical_point(cx, pointer));
        let (initial_bounds, display_id) =
            initial_popup_bounds(pointer, display, configured_width, cx);
        let repository_for_view = repository.clone();
        let favorite_repository_for_view = favorite_repository.clone();
        let image_preview_service_for_view = image_preview_service.clone();
        let result = platform_window::open_borderless_overlay(
            cx,
            initial_bounds,
            display_id,
            move |window, cx, popup| {
                let view = cx.new(|_| {
                    QuickPasteWindowView::new(
                        list_ids,
                        source,
                        initial_index,
                        configured_rows,
                        configured_width,
                        repository_for_view,
                        favorite_repository_for_view,
                        image_preview_service_for_view,
                        history_config,
                        query,
                        initial_records,
                        pointer,
                        display,
                        popup,
                    )
                });
                view.update(cx, |view, cx| {
                    view.place_window(window, popup, cx);
                    view.load_visible_records(cx);
                });
                view
            },
        );

        match result {
            Ok(window) => {
                cx.update_global::<Self, _>(|service, _| {
                    service.window = Some(window);
                });
                crate::hotkey::activate_quick_paste_navigation(quick_paste_config, cx);
                Self::start_pointer_tracking(cx);
            }
            Err(error) => {
                tracing::error!("打开便捷粘贴窗口失败：{error}");
                cx.update_global::<Self, _>(|service, _| {
                    service.active = false;
                    service.closing = false;
                });
            }
        }
    }

    fn start_pointer_tracking(cx: &mut App) {
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(16))
                    .await;
                let active = cx.update(|cx| {
                    let active = cx.read_global::<Self, _>(|service, _| service.active);
                    if active {
                        Self::sync_pointer_state(cx);
                    }
                    active
                });
                if !active {
                    break;
                }
            }
        })
        .detach();
    }

    fn sync_pointer_state(cx: &mut App) {
        let pointer = platform_window::physical_cursor_position();
        let Some(window) = cx.read_global::<Self, _>(|service, _| service.window) else {
            return;
        };
        let _ = window.update(cx, |view, window, _| {
            let mouse_inside = match (pointer, platform_window::physical_client_bounds(window)) {
                (Some(pointer), Ok(bounds)) => {
                    pointer.x >= bounds.left as f32
                        && pointer.x < bounds.right as f32
                        && pointer.y >= bounds.top as f32
                        && pointer.y < bounds.bottom as f32
                }
                _ => false,
            };
            view.set_mouse_inside(mouse_inside, window);
        });
    }

    pub(crate) fn finish(cx: &mut App) {
        Self::sync_pointer_state(cx);
        crate::hotkey::deactivate_quick_paste_navigation(cx);
        let Some((
            window,
            selected_id,
            selected_index,
            source,
            repository,
            favorite_repository,
            paste_service,
        )) = cx.update_global::<Self, _>(|service, cx| {
            if service.closing || !service.active {
                return None;
            }
            let window = service.window?;
            service.active = false;
            service.closing = true;
            let (selected_id, selected_index, source, mouse_inside) = window
                .update(cx, |view, _, _| {
                    (
                        view.selected_list_id(),
                        view.selected_index,
                        view.source,
                        view.mouse_inside || view.keyboard_navigation_used,
                    )
                })
                .ok()
                .unwrap_or((None, 0, PersistedQuickPasteTab::Clipboard, false));
            Some((
                window,
                mouse_inside.then_some(selected_id).flatten(),
                selected_index,
                source,
                service.history_repository.clone(),
                service.favorite_repository.clone(),
                service.paste_service.clone(),
            ))
        })
        else {
            return;
        };

        let mut persisted_state = crate::state_runtime::state(cx);
        persisted_state.main_window.quick_paste_index = selected_index;
        if let Err(error) = crate::state_runtime::set_state(persisted_state, cx) {
            tracing::error!("保存便捷粘贴位置失败：{error}");
        }

        let animate = window
            .update(cx, |view, window, cx| {
                view.begin_close_animation(window, cx)
            })
            .unwrap_or(false);
        if !animate {
            Self::complete_finish(
                window,
                selected_id,
                source,
                repository,
                favorite_repository,
                paste_service,
                cx,
            );
            return;
        }

        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(QUICK_PASTE_HIDE_DURATION + QUICK_PASTE_REVEAL_FRAME)
                .await;
            cx.update(|cx| {
                Self::complete_finish(
                    window,
                    selected_id,
                    source,
                    repository,
                    favorite_repository,
                    paste_service,
                    cx,
                );
            });
        })
        .detach();
    }

    fn complete_finish(
        window: WindowHandle<QuickPasteWindowView>,
        selected_id: Option<HistoryItemId>,
        source: PersistedQuickPasteTab,
        repository: Option<HistoryRepository>,
        favorite_repository: Option<FavoriteRepository>,
        paste_service: Option<PasteService>,
        cx: &mut App,
    ) {
        let should_finish = cx.update_global::<Self, _>(|service, _| {
            if !service.closing || service.window != Some(window) {
                return false;
            }
            service.window = None;
            service.closing = false;
            true
        });
        if !should_finish {
            return;
        }

        let _ = window.update(cx, |_, window, _| window.remove_window());

        let (Some(selected_id), Some(repository), Some(favorite_repository), Some(paste_service)) =
            (selected_id, repository, favorite_repository, paste_service)
        else {
            return;
        };
        let clipboard_config = crate::config_runtime::clipboard_config(cx);
        let move_to_first = match source {
            PersistedQuickPasteTab::Clipboard
                if clipboard_config.move_pasted_to_first_in_clipboard =>
            {
                Some(crate::services::paste::PasteOrderTarget::Clipboard)
            }
            PersistedQuickPasteTab::Favorites
                if clipboard_config.move_pasted_to_first_in_favorites =>
            {
                Some(crate::services::paste::PasteOrderTarget::Favorites)
            }
            _ => None,
        };
        let paste_task = cx.background_spawn(async move {
            let item_id = match source {
                PersistedQuickPasteTab::Clipboard => repository
                    .item_ids_for_history_ids(&[selected_id])
                    .map_err(|error| error.to_string())?
                    .get(&selected_id)
                    .copied(),
                PersistedQuickPasteTab::Favorites => favorite_repository
                    .item_ids_for_favorite_ids(&[selected_id])
                    .map_err(|error| error.to_string())?
                    .get(&selected_id)
                    .copied(),
            };
            let Some(item_id) = item_id else {
                return Err("便捷粘贴项目已不存在".to_owned());
            };
            paste_service
                .paste_item(item_id, move_to_first)
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |_| {
            if let Err(error) = paste_task.await {
                tracing::error!("便捷粘贴失败：{error}");
            }
        })
        .detach();
    }

    pub(crate) fn cancel(cx: &mut App) {
        crate::hotkey::deactivate_quick_paste_navigation(cx);
        // 关闭窗口会同步触发 on_window_closed 回调并再次访问本全局，必须在 update_global 之外执行。
        let window = cx.update_global::<Self, _>(|service, _| {
            service.active = false;
            service.closing = false;
            service.window.take()
        });
        if let Some(window) = window {
            let _ = window.update(cx, |_, window, _| window.remove_window());
        }
    }

    pub(crate) fn navigate(navigation: crate::hotkey::QuickPasteNavigation, cx: &mut App) {
        let Some(window) = cx.read_global::<Self, _>(|service, _| {
            (service.active && !service.closing)
                .then_some(service.window)
                .flatten()
        }) else {
            return;
        };
        if let Err(error) = window.update(cx, |view, window, cx| {
            view.navigate(navigation, window, cx);
        }) {
            tracing::warn!(%error, "便捷粘贴方向键事件处理失败");
        }
    }
}

struct QuickPasteWindowView {
    list_ids: Vec<HistoryItemId>,
    source: PersistedQuickPasteTab,
    records: HashMap<usize, QuickPasteRecord>,
    history_repository: HistoryRepository,
    favorite_repository: FavoriteRepository,
    image_preview_service: crate::services::image_preview::ImagePreviewService,
    history_config: HistoryConfig,
    query: HistoryQuery,
    pointer: Option<gpui::Point<f32>>,
    display: Option<DisplayPlacement>,
    popup: BorderlessPopup,
    selected_index: usize,
    keyboard_navigation_used: bool,
    visible_rows: usize,
    configured_rows: usize,
    configured_width: f32,
    mouse_inside: bool,
    loading_preview_indices: HashSet<usize>,
    loading_image_indices: HashSet<usize>,
    width: f32,
    image_max_size: (u32, u32),
    tab_height: f32,
    row_height: Pixels,
    content_height: Pixels,
    preview_font_size: Pixels,
    reveal_center: Point<Pixels>,
    reveal_progress: f32,
    animation_generation: u64,
    closing: bool,
    source_generation: u64,
    missing_visibility_dirty: bool,
}

#[derive(Clone)]
struct QuickPasteRecord {
    history_id: HistoryItemId,
    preview_text: String,
    content_kind: ContentKind,
    paste_count: u64,
    is_pinned: bool,
    image: Option<std::sync::Arc<RenderImage>>,
    preview_loaded: bool,
    image_loaded: bool,
}

impl QuickPasteWindowView {
    fn new(
        list_ids: Vec<HistoryItemId>,
        source: PersistedQuickPasteTab,
        selected_index: usize,
        configured_rows: usize,
        configured_width: f32,
        history_repository: HistoryRepository,
        favorite_repository: FavoriteRepository,
        image_preview_service: crate::services::image_preview::ImagePreviewService,
        history_config: HistoryConfig,
        query: HistoryQuery,
        initial_records: HashMap<usize, QuickPasteRecord>,
        pointer: Option<gpui::Point<f32>>,
        display: Option<DisplayPlacement>,
        popup: BorderlessPopup,
    ) -> Self {
        let visible_rows = list_ids.len().min(configured_rows).max(1);
        let selected_index = selected_index.min(list_ids.len().saturating_sub(1));
        Self {
            list_ids,
            source,
            records: initial_records,
            history_repository,
            favorite_repository,
            image_preview_service,
            history_config,
            query,
            pointer,
            display,
            popup,
            selected_index,
            keyboard_navigation_used: false,
            visible_rows,
            configured_rows,
            configured_width,
            mouse_inside: true,
            loading_preview_indices: HashSet::new(),
            loading_image_indices: HashSet::new(),
            width: configured_width,
            image_max_size: (64, 64),
            tab_height: QUICK_PASTE_TAB_HEIGHT,
            row_height: px(0.0),
            content_height: px(0.0),
            preview_font_size: px(0.0),
            reveal_center: point(px(0.0), px(0.0)),
            reveal_progress: 0.0,
            animation_generation: 0,
            closing: false,
            source_generation: 0,
            missing_visibility_dirty: false,
        }
    }

    fn place_window(
        &mut self,
        window: &mut Window,
        popup: BorderlessPopup,
        cx: &mut Context<Self>,
    ) {
        window.set_rem_size(gpui_component::Theme::global(cx).font_size);
        self.tab_height = QUICK_PASTE_TAB_HEIGHT * crate::ui::scale::window_scale(window);
        let (row_height, content_height, preview_font_size, preview_height) =
            crate::windows::main::components::clipboard_list::quick_paste_row_metrics(window, cx);
        self.row_height = row_height;
        self.content_height = content_height;
        self.preview_font_size = preview_font_size;
        self.image_max_size = (4096, u32::from(preview_height).max(1).saturating_mul(2));

        let desired_row_count = self
            .list_ids
            .len()
            .min(self.configured_rows)
            .min(QUICK_PASTE_MAX_VISIBLE_ROWS)
            .max(1);
        let scale = self
            .display
            .map(|display| display.scale_factor)
            .unwrap_or_else(|| window.scale_factor())
            .max(1.0);
        let Some(_pointer) = self.pointer else {
            let bounds = window.bounds();
            self.width = self.configured_width;
            self.visible_rows = desired_row_count;
            let logical_height = self.tab_height
                + f32::from(row_height) * desired_row_count as f32
                + QUICK_PASTE_SHADOW_MARGIN;
            self.reveal_center = point(px(self.width / 2.0), px(logical_height / 2.0));
            if !popup.resize_to_physical_bounds(
                window,
                cx,
                f32::from(bounds.origin.x) * scale,
                f32::from(bounds.origin.y) * scale,
                self.configured_width * scale,
                logical_height * scale,
            ) {
                popup.set_visible(window, true);
            }
            self.start_reveal_animation(window, cx);
            return;
        };
        let margin = 8.0 * scale;
        let available_width = self
            .display
            .map(|display| display.bounds.width)
            .map(|width| (width - margin * 2.0).max(1.0))
            .unwrap_or(self.configured_width * scale);
        let physical_width = (self.configured_width * scale).min(available_width);
        self.width = physical_width / scale;
        self.visible_rows = desired_row_count;
        self.resize_for_visible_rows(window, cx);
        if let Some(display) = self.display {
            let available_height = (display.bounds.height - margin * 2.0).max(1.0);
            while self.visible_rows > 1 {
                let exceeds_screen = platform_window::physical_client_bounds(window)
                    .map(|bounds| (bounds.bottom - bounds.top) as f32 > available_height)
                    .unwrap_or(false);
                if !exceeds_screen {
                    break;
                }
                self.visible_rows -= 1;
                self.resize_for_visible_rows(window, cx);
            }
        }
        self.start_reveal_animation(window, cx);
    }

    fn start_reveal_animation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let animation_enabled = !cx.reduce_motion()
            && crate::config_runtime::animation_enabled(
                cx,
                crate::config::AnimationKind::QuickPaste,
            );
        self.closing = false;
        self.animation_generation = self.animation_generation.wrapping_add(1);
        let generation = self.animation_generation;
        self.reveal_progress = if animation_enabled { 0.0 } else { 1.0 };
        if !animation_enabled {
            self.apply_reveal_clip(window);
            cx.notify();
            return;
        }
        self.apply_reveal_clip(window);

        let view = cx.entity().downgrade();
        let window_handle = window.window_handle();
        cx.spawn(async move |_, cx| {
            let started_at = Instant::now();
            loop {
                cx.background_executor()
                    .timer(QUICK_PASTE_REVEAL_FRAME)
                    .await;
                let elapsed = started_at.elapsed().as_secs_f32();
                let progress =
                    (elapsed / QUICK_PASTE_REVEAL_DURATION.as_secs_f32()).clamp(0.0, 1.0);
                let progress = quick_paste_reveal_easing(progress);
                let updated = cx.update(|cx| {
                    let Some(view) = view.upgrade() else {
                        return false;
                    };
                    let mut clip = None;
                    let updated = view.update(cx, |view, cx| {
                        if view.animation_generation != generation || view.closing {
                            return false;
                        }
                        view.reveal_progress = progress;
                        clip = Some(view.reveal_clip_parameters());
                        cx.notify();
                        true
                    });
                    if let Some(clip) = clip {
                        let _ = window_handle.update(cx, |_, window, _| {
                            apply_reveal_clip(window, clip);
                        });
                    }
                    updated
                });
                if !updated || progress >= 1.0 {
                    break;
                }
            }
        })
        .detach();
    }

    fn begin_close_animation(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.closing = true;
        self.animation_generation = self.animation_generation.wrapping_add(1);
        let generation = self.animation_generation;
        let start_progress = self.reveal_progress.clamp(0.0, 1.0);
        let animation_enabled = !cx.reduce_motion()
            && crate::config_runtime::animation_enabled(
                cx,
                crate::config::AnimationKind::QuickPaste,
            );
        if !animation_enabled || start_progress <= 0.0 {
            self.reveal_progress = 0.0;
            self.apply_reveal_clip(window);
            cx.notify();
            return false;
        }
        self.apply_reveal_clip(window);

        let view = cx.entity().downgrade();
        let window_handle = window.window_handle();
        cx.spawn(async move |_, cx| {
            let started_at = Instant::now();
            loop {
                cx.background_executor()
                    .timer(QUICK_PASTE_REVEAL_FRAME)
                    .await;
                let elapsed = started_at.elapsed().as_secs_f32();
                let progress = (elapsed / QUICK_PASTE_HIDE_DURATION.as_secs_f32()).clamp(0.0, 1.0);
                let progress = start_progress * (1.0 - quick_paste_reveal_easing(progress));
                let updated = cx.update(|cx| {
                    let Some(view) = view.upgrade() else {
                        return false;
                    };
                    let mut clip = None;
                    let updated = view.update(cx, |view, cx| {
                        if view.animation_generation != generation || !view.closing {
                            return false;
                        }
                        view.reveal_progress = progress;
                        clip = Some(view.reveal_clip_parameters());
                        cx.notify();
                        true
                    });
                    if let Some(clip) = clip {
                        let _ = window_handle.update(cx, |_, window, _| {
                            apply_reveal_clip(window, clip);
                        });
                    }
                    updated
                });
                if !updated || progress <= 0.0 {
                    break;
                }
            }
        })
        .detach();
        true
    }

    fn apply_reveal_clip(&self, window: &Window) {
        apply_reveal_clip(window, self.reveal_clip_parameters());
    }

    fn reveal_clip_parameters(&self) -> Option<(f32, f32, f32)> {
        if self.reveal_progress >= 1.0 {
            return None;
        }

        let total_height = self.tab_height
            + f32::from(self.row_height) * self.visible_row_count().max(1) as f32
            + QUICK_PASTE_SHADOW_MARGIN;
        let center_x = f32::from(self.reveal_center.x);
        let center_y = f32::from(self.reveal_center.y);
        let radius = center_x
            .max(self.width - center_x)
            .hypot(center_y.max(total_height - center_y));
        let diameter = (radius * 2.0 * self.reveal_progress).max(1.0);
        Some((center_x, center_y, diameter))
    }

    fn visible_start(&self) -> usize {
        self.selected_index
            .saturating_sub(self.visible_rows / 2)
            .min(self.list_ids.len().saturating_sub(self.visible_rows))
    }

    fn visible_row_count(&self) -> usize {
        self.list_ids.len().min(self.visible_rows)
    }

    fn selected_list_id(&self) -> Option<HistoryItemId> {
        self.list_ids.get(self.selected_index).copied()
    }

    fn select_source(
        &mut self,
        source: PersistedQuickPasteTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.source == source {
            return;
        }
        self.reload_source(source, false, window, cx);
    }

    fn reload_source(
        &mut self,
        source: PersistedQuickPasteTab,
        preserve_selection: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.closing {
            return;
        }
        let history_config = crate::config_runtime::history_config(cx);
        let selected_id = preserve_selection
            .then(|| self.selected_list_id())
            .flatten();
        let selected_index = self.selected_index;

        let list_ids = match source {
            PersistedQuickPasteTab::Clipboard => self
                .history_repository
                .ordered_ids_scoped(&self.query, &history_config),
            PersistedQuickPasteTab::Favorites => self
                .favorite_repository
                .ordered_ids_scoped(&self.query, &history_config),
        };
        let list_ids = match list_ids {
            Ok(list_ids) => list_ids,
            Err(error) => {
                tracing::error!("切换便捷粘贴列表失败：{error}");
                return;
            }
        };
        if preserve_selection && list_ids == self.list_ids {
            self.missing_visibility_dirty = false;
            return;
        }
        let previous_records = preserve_selection
            .then(|| {
                self.records
                    .values()
                    .cloned()
                    .map(|record| (record.history_id, record))
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        let initial_ids = list_ids
            .iter()
            .copied()
            .take(self.configured_rows)
            .filter(|id| !previous_records.contains_key(id))
            .collect::<Vec<_>>();
        let initial_query = HistoryQuery {
            load_image_sources: false,
            ..self.query.clone()
        };
        let initial_records = match query_list_records(
            source,
            &self.history_repository,
            &self.favorite_repository,
            &initial_query,
            &history_config,
            &initial_ids,
        ) {
            Ok(records) => records,
            Err(error) => {
                tracing::error!("预加载便捷粘贴首屏内容失败：{error}");
                return;
            }
        };
        let initial_index_by_id = list_ids
            .iter()
            .enumerate()
            .map(|(index, list_id)| (*list_id, index))
            .collect::<HashMap<_, _>>();
        let mut records = list_ids
            .iter()
            .enumerate()
            .filter_map(|(index, id)| {
                previous_records
                    .get(id)
                    .cloned()
                    .map(|record| (index, record))
            })
            .collect::<HashMap<_, _>>();
        records.extend(
            initial_records
                .into_iter()
                .filter_map(|record| {
                    let index = *initial_index_by_id.get(&record.history_id)?;
                    Some((index, QuickPasteRecord::from_initial(record)))
                })
                .collect::<HashMap<_, _>>(),
        );

        if !preserve_selection {
            let mut persisted_state = crate::state_runtime::state(cx);
            persisted_state.main_window.quick_paste_tab = source;
            if let Err(error) = crate::state_runtime::set_state(persisted_state, cx) {
                tracing::error!("保存便捷粘贴列表选项失败：{error}");
            }
        }

        let previous_list_ids = self.list_ids.clone();
        self.source = source;
        self.missing_visibility_dirty = false;
        self.history_config = history_config;
        self.list_ids = list_ids;
        self.records = records;
        self.loading_preview_indices.clear();
        self.loading_image_indices.clear();
        self.source_generation = self.source_generation.wrapping_add(1);
        self.selected_index = if preserve_selection {
            let anchor_index = selected_id
                .and_then(|id| {
                    previous_list_ids
                        .iter()
                        .position(|candidate| *candidate == id)
                })
                .unwrap_or(selected_index);
            selected_id
                .and_then(|id| self.list_ids.iter().position(|candidate| *candidate == id))
                .or_else(|| {
                    let visible_ids: HashSet<_> = self.list_ids.iter().copied().collect();
                    nearest_survivor(&previous_list_ids, anchor_index, &visible_ids)
                        .and_then(|id| self.list_ids.iter().position(|candidate| *candidate == id))
                })
                .unwrap_or_else(|| selected_index.min(self.list_ids.len().saturating_sub(1)))
        } else {
            0
        };
        self.visible_rows = self
            .list_ids
            .len()
            .min(self.configured_rows)
            .min(QUICK_PASTE_MAX_VISIBLE_ROWS)
            .max(1);
        self.resize_for_visible_rows(window, cx);
        self.load_visible_records(cx);
        cx.notify();
    }

    fn resize_for_visible_rows(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let row_count = self
            .list_ids
            .len()
            .min(self.visible_rows)
            .min(QUICK_PASTE_MAX_VISIBLE_ROWS)
            .max(1);
        self.visible_rows = row_count;
        let scale = self
            .display
            .map(|display| display.scale_factor)
            .unwrap_or_else(|| window.scale_factor())
            .max(1.0);
        let physical_height = (self.tab_height
            + f32::from(self.row_height) * row_count as f32
            + QUICK_PASTE_SHADOW_MARGIN)
            * scale;
        let physical_width = self.width * scale;
        let bounds = self.display.map(|display| display.bounds);
        let margin = 8.0 * scale;
        let (x, y) = match (self.pointer, bounds) {
            (Some(pointer), Some(bounds)) => {
                centered_overlay_origin(pointer, physical_width, physical_height, margin, bounds)
            }
            (Some(pointer), None) => (
                pointer.x - physical_width / 2.0,
                pointer.y - physical_height / 2.0,
            ),
            (None, _) => {
                let bounds = window.bounds();
                (
                    f32::from(bounds.origin.x) * scale,
                    f32::from(bounds.origin.y) * scale,
                )
            }
        };
        self.reveal_center = point(
            px((self
                .pointer
                .map(|pointer| pointer.x - x)
                .unwrap_or(physical_width / 2.0)
                .clamp(0.0, physical_width))
                / scale),
            px((self
                .pointer
                .map(|pointer| pointer.y - y)
                .unwrap_or(physical_height / 2.0)
                .clamp(0.0, physical_height))
                / scale),
        );
        if !self
            .popup
            .resize_to_physical_bounds(window, cx, x, y, physical_width, physical_height)
        {
            tracing::error!("调整便捷粘贴窗口大小失败");
        }
        self.apply_reveal_clip(window);
    }

    fn scroll(&mut self, event: &ScrollWheelEvent, window: &Window, cx: &mut Context<Self>) {
        let delta = crate::ui::smooth_scroll::vertical_wheel_delta(
            event.delta.pixel_delta(window.line_height()),
            event.modifiers.shift,
        );
        if delta.is_zero() {
            return;
        }
        let direction = f32::from(delta).signum() as isize;
        if self.list_ids.is_empty() {
            return;
        }

        let last_index = self.list_ids.len() - 1;
        let next_index = if direction > 0 {
            if self.selected_index == 0 {
                last_index
            } else {
                self.selected_index - 1
            }
        } else {
            if self.selected_index == last_index {
                0
            } else {
                self.selected_index + 1
            }
        };
        if next_index == self.selected_index {
            return;
        }
        self.selected_index = next_index;
        crate::services::sound::play(crate::services::sound::SoundEvent::QuickPasteScroll);
        self.load_visible_records(cx);
        cx.notify();
    }

    fn select_index(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.list_ids.len() || index == self.selected_index {
            return;
        }
        self.selected_index = index;
        self.load_visible_records(cx);
        cx.notify();
    }

    fn navigate(
        &mut self,
        navigation: crate::hotkey::QuickPasteNavigation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.keyboard_navigation_used = true;
        match navigation {
            crate::hotkey::QuickPasteNavigation::Up => self.move_selection(-1, cx),
            crate::hotkey::QuickPasteNavigation::Down => self.move_selection(1, cx),
            crate::hotkey::QuickPasteNavigation::Left
            | crate::hotkey::QuickPasteNavigation::Right => {
                let next_source = match self.source {
                    PersistedQuickPasteTab::Clipboard => PersistedQuickPasteTab::Favorites,
                    PersistedQuickPasteTab::Favorites => PersistedQuickPasteTab::Clipboard,
                };
                self.select_source(next_source, window, cx);
            }
        }
    }

    fn move_selection(&mut self, direction: isize, cx: &mut Context<Self>) {
        if self.list_ids.is_empty() {
            return;
        }
        let count = self.list_ids.len() as isize;
        let next_index = (self.selected_index as isize + direction).rem_euclid(count) as usize;
        if next_index == self.selected_index {
            return;
        }
        self.selected_index = next_index;
        crate::services::sound::play(crate::services::sound::SoundEvent::QuickPasteScroll);
        self.load_visible_records(cx);
        cx.notify();
    }

    fn set_mouse_inside(&mut self, mouse_inside: bool, window: &Window) {
        if self.mouse_inside == mouse_inside {
            return;
        }
        self.mouse_inside = mouse_inside;
        if let Err(error) =
            platform_window::set_window_opacity(window, if mouse_inside { 1.0 } else { 0.2 })
        {
            tracing::warn!(%error, "更新便捷粘贴窗口透明度失败");
        }
    }

    fn load_visible_records(&mut self, cx: &mut Context<Self>) {
        let start = self.visible_start();
        let end = (start + self.visible_row_count()).min(self.list_ids.len());
        let prefetch_start = start.saturating_sub(QUICK_PASTE_PREFETCH_ROWS);
        let prefetch_end = (end + QUICK_PASTE_PREFETCH_ROWS).min(self.list_ids.len());
        let preview_indices = (prefetch_start..prefetch_end)
            .filter(|index| {
                !self
                    .records
                    .get(index)
                    .is_some_and(|record| record.preview_loaded)
                    && !self.loading_preview_indices.contains(index)
            })
            .collect::<Vec<_>>();
        if !preview_indices.is_empty() {
            self.loading_preview_indices
                .extend(preview_indices.iter().copied());
            self.spawn_preview_load(preview_indices, cx);
        }

        let image_indices = (start..end)
            .filter(|index| {
                self.records
                    .get(index)
                    .is_some_and(|record| record.preview_loaded && !record.image_loaded)
                    && !self.loading_image_indices.contains(index)
            })
            .collect::<Vec<_>>();
        if !image_indices.is_empty() {
            self.loading_image_indices
                .extend(image_indices.iter().copied());
            self.spawn_image_load(image_indices, cx);
        }
    }

    fn spawn_preview_load(&self, indices: Vec<usize>, cx: &mut Context<Self>) {
        let ids = indices
            .iter()
            .map(|index| self.list_ids[*index])
            .collect::<Vec<_>>();
        let repository = self.history_repository.clone();
        let favorite_repository = self.favorite_repository.clone();
        let source = self.source;
        let source_generation = self.source_generation;
        let history_config = self.history_config.clone();
        let query = HistoryQuery {
            load_image_sources: false,
            ..self.query.clone()
        };
        let index_by_id = ids
            .iter()
            .enumerate()
            .map(|(offset, list_id)| (*list_id, indices[offset]))
            .collect::<HashMap<_, _>>();
        let task = cx.background_spawn(async move {
            query_list_records(
                source,
                &repository,
                &favorite_repository,
                &query,
                &history_config,
                &ids,
            )
            .map(|records| {
                records
                    .into_iter()
                    .filter_map(|record| {
                        let index = *index_by_id.get(&record.history_id)?;
                        Some((index, QuickPasteRecord::from_initial(record)))
                    })
                    .collect::<Vec<_>>()
            })
        });
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let result = task.await;
            cx.update(|cx| {
                if let Some(view) = view.upgrade() {
                    view.update(cx, |view, cx| {
                        if view.source_generation != source_generation {
                            return;
                        }
                        for index in &indices {
                            view.loading_preview_indices.remove(index);
                        }
                        match result {
                            Ok(records) => {
                                for (index, record) in records {
                                    view.records.insert(index, record);
                                }
                                view.trim_record_cache();
                                view.load_visible_records(cx);
                                cx.notify();
                            }
                            Err(error) => tracing::error!("加载便捷粘贴预览失败：{error}"),
                        }
                    });
                }
            });
        })
        .detach();
    }

    fn spawn_image_load(&self, indices: Vec<usize>, cx: &mut Context<Self>) {
        let ids = indices
            .iter()
            .map(|index| self.list_ids[*index])
            .collect::<Vec<_>>();
        let repository = self.history_repository.clone();
        let favorite_repository = self.favorite_repository.clone();
        let image_preview_service = self.image_preview_service.clone();
        let source = self.source;
        let source_generation = self.source_generation;
        let history_config = self.history_config.clone();
        let query = self.query.clone();
        let image_max_size = self.image_max_size;
        let index_by_id = ids
            .iter()
            .enumerate()
            .map(|(offset, list_id)| (*list_id, indices[offset]))
            .collect::<HashMap<_, _>>();
        let task = cx.background_spawn(async move {
            query_list_records(
                source,
                &repository,
                &favorite_repository,
                &query,
                &history_config,
                &ids,
            )
            .map(|records| {
                records
                    .into_iter()
                    .filter_map(|record| {
                        let index = *index_by_id.get(&record.history_id)?;
                        Some((
                            index,
                            QuickPasteRecord::from(record, &image_preview_service, image_max_size),
                        ))
                    })
                    .collect::<Vec<_>>()
            })
        });
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let result = task.await;
            cx.update(|cx| {
                if let Some(view) = view.upgrade() {
                    view.update(cx, |view, cx| {
                        if view.source_generation != source_generation {
                            return;
                        }
                        for index in &indices {
                            view.loading_image_indices.remove(index);
                        }
                        match result {
                            Ok(records) => {
                                for (index, record) in records {
                                    view.records.insert(index, record);
                                }
                                view.trim_record_cache();
                                cx.notify();
                            }
                            Err(error) => tracing::error!("加载便捷粘贴图片预览失败：{error}"),
                        }
                    });
                }
            });
        })
        .detach();
    }

    fn trim_record_cache(&mut self) {
        let start = self
            .visible_start()
            .saturating_sub(QUICK_PASTE_PREFETCH_ROWS);
        let end = (self.visible_start() + self.visible_row_count() + QUICK_PASTE_PREFETCH_ROWS)
            .min(self.list_ids.len());
        self.records
            .retain(|index, _| *index >= start && *index < end);
    }

    fn render_record(&self, index: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = crate::window_material::excluded_window_theme(cx);
        let selected = index == self.selected_index;
        let (background, foreground, border) = if selected {
            (theme.primary, theme.primary_foreground, theme.primary)
        } else {
            (theme.background, theme.foreground, theme.border)
        };
        let translator = crate::i18n::translator(cx);
        let record = self.records.get(&index);
        let content_label = record
            .map(|record| content_kind_label(record.content_kind, &translator))
            .unwrap_or_else(|| translator.translate("quick_paste.loading"));
        let preview = record
            .map(|record| {
                record
                    .preview_text
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_owned()
            })
            .filter(|preview| !preview.is_empty())
            .unwrap_or_else(|| content_label.clone());
        let history_id = record
            .map(|record| record.history_id)
            .or_else(|| self.list_ids.get(index).copied())
            .unwrap_or_default();
        let is_pinned = record.is_some_and(|record| record.is_pinned);
        let has_been_pasted = record.is_some_and(|record| record.paste_count > 0);
        let has_status = is_pinned || has_been_pasted;
        let text_color = foreground;
        let badge_color = if selected {
            theme.primary_foreground
        } else {
            theme.muted_foreground
        };
        let index_label = (index + 1).to_string();
        let status_background = theme.background;
        let mut status_indicator = div()
            .absolute()
            .top_0()
            .bottom_0()
            .left_0()
            .w(ui_rem(18.0))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_0()
            .bg(status_background)
            // 卡片边框为 1.5px、外圆角为 9px，状态栏使用对应的内圆角，避免背景露出卡片圆角。
            .rounded_tl(px(7.5))
            .rounded_bl(px(7.5))
            .overflow_hidden();
        if is_pinned {
            status_indicator = status_indicator.child(
                div()
                    .size(ui_rem(14.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        Icon::new(QuickPasteStatusIcon::Pinned)
                            .xsmall()
                            .text_color(theme.blue),
                    ),
            );
        }
        if has_been_pasted {
            status_indicator = status_indicator.child(
                div()
                    .size(ui_rem(14.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        Icon::new(QuickPasteStatusIcon::Pasted)
                            .xsmall()
                            .text_color(theme.yellow),
                    ),
            );
        }

        div()
            .id(format!("quick-paste-item-{history_id}"))
            .w(px(self.width))
            .h(self.row_height)
            .flex()
            .items_center()
            .justify_center()
            .on_click(cx.listener(move |view, _, _, cx| {
                view.select_index(index, cx);
            }))
            .child(
                div()
                    // 左右预留阴影空间，选中项的阴影不会被窗口裁剪。
                    .w(px(if selected {
                        self.width - QUICK_PASTE_SHADOW_MARGIN * 2.0
                    } else {
                        (self.width * 0.92).min(self.width - QUICK_PASTE_SHADOW_MARGIN * 2.0)
                    }))
                    .h(self.content_height)
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .px(px(14.0))
                    .overflow_hidden()
                    .relative()
                    .rounded(px(9.0))
                    .border(px(1.5))
                    .border_color(border)
                    .bg(background)
                    .text_color(text_color)
                    .shadow(quick_paste_shadow(0.32, QUICK_PASTE_SHADOW_BLUR))
                    .pl(px(if has_status { 24.0 } else { 14.0 }))
                    .pr(px(14.0))
                    .when_some(
                        record.and_then(|record| record.image.clone()),
                        |row, image| {
                            row.child(
                                img(image)
                                    .size(ui_rem(30.0))
                                    .flex_shrink_0()
                                    .object_fit(ObjectFit::Contain),
                            )
                        },
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .text_size(self.preview_font_size)
                            .line_height(self.content_height)
                            .truncate()
                            .whitespace_nowrap()
                            .child(preview),
                    )
                    .when(has_status, |row| row.child(status_indicator))
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .bottom_0()
                            .w_auto()
                            .min_w_auto()
                            .flex_none()
                            .flex()
                            .flex_col()
                            .items_center()
                            .bg(background)
                            .rounded_tr(px(9.0))
                            .rounded_br(px(9.0))
                            .overflow_hidden()
                            .child(
                                div()
                                    .w_auto()
                                    .min_w_auto()
                                    .flex_none()
                                    .h(relative(0.5))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .px(px(3.0))
                                    .text_size(self.preview_font_size * 0.82)
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .line_height(self.preview_font_size * 0.82)
                                    .text_color(foreground)
                                    .whitespace_nowrap()
                                    .child(index_label),
                            )
                            .child(
                                div()
                                    .w_auto()
                                    .min_w_auto()
                                    .flex_none()
                                    .h(relative(0.5))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .px(px(3.0))
                                    .text_size(self.preview_font_size * 0.70)
                                    .line_height(self.preview_font_size * 0.70)
                                    .text_color(badge_color)
                                    .whitespace_nowrap()
                                    .child(content_label),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_source_switcher(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = crate::window_material::excluded_window_theme(cx);
        let translator = crate::i18n::translator(cx);
        let clipboard_label = translator.translate("main.tab.clipboard");
        let favorites_label = translator.translate("main.tab.favorites");
        let clipboard_selected = self.source == PersistedQuickPasteTab::Clipboard;
        let favorites_selected = self.source == PersistedQuickPasteTab::Favorites;

        div()
            .w(px(self.width))
            .h(px(self.tab_height))
            .flex()
            .items_center()
            .px(px(12.0))
            .child(
                div()
                    .w_full()
                    .h(ui_rem(28.0))
                    .flex()
                    .rounded(px(7.0))
                    .border(px(1.0))
                    .border_color(theme.border)
                    .bg(theme.background)
                    .p(px(2.0))
                    .shadow(quick_paste_shadow(0.32, QUICK_PASTE_SHADOW_BLUR))
                    .child(
                        div()
                            .id("quick-paste-source-clipboard")
                            .h_full()
                            .flex_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(5.0))
                            .cursor_pointer()
                            .bg(if clipboard_selected {
                                theme.primary
                            } else {
                                theme.background
                            })
                            .when(clipboard_selected, |tab| {
                                tab.shadow(quick_paste_shadow(0.30, 2.0))
                            })
                            .text_color(if clipboard_selected {
                                theme.primary_foreground
                            } else {
                                theme.foreground
                            })
                            .text_size(ui_rem(12.0))
                            .child(clipboard_label)
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.select_source(PersistedQuickPasteTab::Clipboard, window, cx);
                            })),
                    )
                    .child(
                        div()
                            .id("quick-paste-source-favorites")
                            .h_full()
                            .flex_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(5.0))
                            .cursor_pointer()
                            .bg(if favorites_selected {
                                theme.primary
                            } else {
                                theme.background
                            })
                            .when(favorites_selected, |tab| {
                                tab.shadow(quick_paste_shadow(0.30, 2.0))
                            })
                            .text_color(if favorites_selected {
                                theme.primary_foreground
                            } else {
                                theme.foreground
                            })
                            .text_size(ui_rem(12.0))
                            .child(favorites_label)
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.select_source(PersistedQuickPasteTab::Favorites, window, cx);
                            })),
                    ),
            )
            .into_any_element()
    }
}

fn apply_reveal_clip(window: &Window, parameters: Option<(f32, f32, f32)>) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static FAILURE_REPORTED: AtomicBool = AtomicBool::new(false);
    let result = if let Some((center_x, center_y, diameter)) = parameters {
        platform_window::set_window_circle_clip(window, center_x, center_y, diameter)
    } else {
        platform_window::clear_window_circle_clip(window)
    };
    match result {
        Ok(()) => {
            FAILURE_REPORTED.store(false, Ordering::Relaxed);
        }
        Err(error) => {
            // 动画逐帧调用，仅在连续失败的第一帧记录。
            if !FAILURE_REPORTED.swap(true, Ordering::Relaxed) {
                tracing::warn!(%error, "更新便捷粘贴圆形窗口区域失败");
            }
        }
    }
}

#[derive(Clone, Copy)]
enum QuickPasteStatusIcon {
    Pinned,
    Pasted,
}

impl IconNamed for QuickPasteStatusIcon {
    fn path(self) -> SharedString {
        match self {
            Self::Pinned => "assets/icons/pinned-filled.svg",
            Self::Pasted => "assets/icons/check.svg",
        }
        .into()
    }
}

impl Render for QuickPasteWindowView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rem_size = gpui_component::Theme::global(cx).font_size;
        if window.rem_size() != rem_size && !self.closing {
            window.set_rem_size(rem_size);
            self.place_window(window, self.popup, cx);
        }
        let start = self.visible_start();
        let list_height = f32::from(self.row_height) * self.visible_row_count().max(1) as f32;
        // 底部预留阴影空间，最后一行卡片的阴影不会被窗口裁剪。
        let total_height = self.tab_height + list_height + QUICK_PASTE_SHADOW_MARGIN;
        let list_content = if self.list_ids.is_empty() {
            let translator = crate::i18n::translator(cx);
            let theme = crate::window_material::excluded_window_theme(cx);
            div()
                .w(px(self.width))
                .h(px(list_height))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .w_full()
                        .h(self.content_height)
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(9.0))
                        .border(px(1.0))
                        .border_color(theme.border)
                        .bg(theme.background)
                        .text_color(theme.muted_foreground)
                        .text_size(self.preview_font_size)
                        .child(translator.translate(match self.source {
                            PersistedQuickPasteTab::Clipboard => "quick_paste.empty",
                            PersistedQuickPasteTab::Favorites => "quick_paste.favorites_empty",
                        })),
                )
                .on_scroll_wheel(cx.listener(|view, event: &ScrollWheelEvent, window, cx| {
                    window.prevent_default();
                    view.scroll(event, window, cx);
                }))
        } else {
            div()
                .w(px(self.width))
                .h(px(list_height))
                .flex()
                .flex_col()
                .children(
                    (start..(start + self.visible_row_count()).min(self.list_ids.len()))
                        .map(|index| self.render_record(index, cx)),
                )
                .on_scroll_wheel(cx.listener(|view, event: &ScrollWheelEvent, window, cx| {
                    window.prevent_default();
                    view.scroll(event, window, cx);
                }))
        };
        let content = div()
            .w(px(self.width))
            .h(px(total_height))
            .flex()
            .flex_col()
            .child(self.render_source_switcher(cx))
            .child(list_content);

        if self.reveal_progress >= 1.0 {
            return content;
        }

        let center_x = f32::from(self.reveal_center.x);
        let center_y = f32::from(self.reveal_center.y);
        let radius = center_x
            .max(self.width - center_x)
            .hypot(center_y.max(total_height - center_y));
        let diameter = (radius * 2.0 * self.reveal_progress).max(1.0);
        let mask_left = center_x - diameter / 2.0;
        let mask_top = center_y - diameter / 2.0;
        div()
            .relative()
            .w(px(self.width))
            .h(px(total_height))
            .child(
                div()
                    .absolute()
                    .left(px(mask_left))
                    .top(px(mask_top))
                    .size(px(diameter))
                    .rounded_full()
                    .overflow_hidden()
                    .child(
                        div()
                            .absolute()
                            .left(px(diameter / 2.0 - center_x))
                            .top(px(diameter / 2.0 - center_y))
                            .w(px(self.width))
                            .h(px(total_height))
                            .child(content),
                    ),
            )
    }
}

fn query_list_records(
    source: PersistedQuickPasteTab,
    history_repository: &HistoryRepository,
    favorite_repository: &FavoriteRepository,
    query: &HistoryQuery,
    history_config: &HistoryConfig,
    list_ids: &[HistoryItemId],
) -> Result<
    Vec<crate::services::database::query::HistoryListRecord>,
    crate::services::database::query::HistoryQueryError,
> {
    match source {
        PersistedQuickPasteTab::Clipboard => {
            history_repository.query_by_ids_scoped(query, list_ids, history_config)
        }
        PersistedQuickPasteTab::Favorites => {
            favorite_repository.query_by_ids_scoped(query, list_ids, history_config)
        }
    }
}

impl QuickPasteRecord {
    fn from_initial(record: crate::services::database::query::HistoryListRecord) -> Self {
        Self {
            history_id: record.history_id,
            preview_text: record.preview_text,
            content_kind: record.content_kind,
            paste_count: record.paste_count,
            is_pinned: record.is_pinned,
            image: None,
            preview_loaded: true,
            image_loaded: false,
        }
    }

    fn from(
        record: crate::services::database::query::HistoryListRecord,
        image_preview_service: &crate::services::image_preview::ImagePreviewService,
        image_max_size: (u32, u32),
    ) -> Self {
        let image = record
            .preview_options
            .iter()
            .filter(|preview| preview.role() == crate::domain::capture::FormatRole::Image)
            .find_map(|preview| {
                let source = preview.image_source.as_ref()?;
                match image_preview_service.load(
                    source,
                    preview.format_kind,
                    &preview.format_key,
                    image_max_size,
                ) {
                    Ok(image) => Some(image),
                    Err(error) => {
                        tracing::warn!(item_id = record.item_id, format_key = %preview.format_key,
                            %error, "便捷粘贴图片预览加载失败");
                        None
                    }
                }
            });
        Self {
            history_id: record.history_id,
            preview_text: record.preview_text,
            content_kind: record.content_kind,
            paste_count: record.paste_count,
            is_pinned: record.is_pinned,
            image,
            preview_loaded: true,
            image_loaded: true,
        }
    }
}

fn initial_popup_bounds(
    pointer: Option<gpui::Point<f32>>,
    display: Option<DisplayPlacement>,
    configured_width: f32,
    cx: &App,
) -> (Bounds<Pixels>, Option<DisplayId>) {
    if let (Some(pointer), Some(display)) = (pointer, display) {
        let scale = display.scale_factor.max(1.0);
        return (
            Bounds::new(
                point(px(pointer.x / scale), px(pointer.y / scale)),
                size(px(1.0), px(1.0)),
            ),
            Some(display.display_id),
        );
    }

    (
        Bounds::centered(None, size(px(configured_width), px(1.0)), cx),
        None,
    )
}

fn centered_overlay_origin(
    pointer: gpui::Point<f32>,
    width: f32,
    height: f32,
    margin: f32,
    area: PhysicalDisplayBounds,
) -> (f32, f32) {
    let min_x = area.x + margin;
    let min_y = area.y + margin;
    let max_x = (area.x + area.width - margin - width).max(min_x);
    let max_y = (area.y + area.height - margin - height).max(min_y);
    (
        (pointer.x - width / 2.0).clamp(min_x, max_x),
        (pointer.y - height / 2.0).clamp(min_y, max_y),
    )
}

fn quick_paste_reveal_easing(progress: f32) -> f32 {
    let inverse = 1.0 - progress.clamp(0.0, 1.0);
    1.0 - inverse * inverse * inverse
}

fn content_kind_label(kind: ContentKind, translator: &crate::i18n::Translator) -> String {
    let key = match kind {
        ContentKind::Text => "quick_paste.kind.text",
        ContentKind::Link => "quick_paste.kind.link",
        ContentKind::Image => "quick_paste.kind.image",
        ContentKind::FileList => "quick_paste.kind.files",
        ContentKind::Mixed => "quick_paste.kind.mixed",
        ContentKind::Custom | ContentKind::Unknown => "quick_paste.kind.other",
    };
    translator.translate(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_selected_item_uses_nearest_survivor() {
        let previous = vec![1, 2, 3, 4, 5];
        let visible = HashSet::from([1, 4, 5]);
        assert_eq!(nearest_survivor(&previous, 2, &visible), Some(4));
        assert_eq!(nearest_survivor(&previous, 1, &visible), Some(1));
    }
}
