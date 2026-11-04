mod actions;
mod gallery;
mod heatmap;
mod history;
mod list;
mod selection;
mod split;
mod sync;
mod tab_bar;
mod tags_search;

pub(crate) use heatmap::{HeatBucket, HeatScrollbarState, ListHeatmap};
pub(super) use history::ClipboardListenerToggleResult;

use std::{
    cell::RefCell,
    cmp::Ordering,
    collections::{HashMap, HashSet, VecDeque},
    ops::Range,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::{Duration, Instant},
};

use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, Modifiers, Pixels, Point,
    RenderImage, ScrollWheelEvent, Subscription, Task, Window, px,
};
use gpui_component::{
    ReorderableVirtualListSizes, VirtualListScrollHandle,
    input::{self, InputEvent, InputState},
};

use super::tabs::{FilterId, FilterSelection, SymbolCategoryId, TabBarHover, TabId};
use super::{MainWindowService, components::emoji_picker::EmojiRowsCache, file_list::FileListView};
use crate::{
    config::{CardItemGap, ClipboardDisplayPriority, ListItemStyle},
    domain::capture::{FormatKind, FormatRole},
    domain::clipboard::{
        ApplicationIconSource, ClipboardItemId, ClipboardListItem, ContentKind, HistoryItemId,
        ImageDimensions, ImagePreviewSource, Tag, select_user_visible_previews_with_priority,
    },
    emoji::EmojiCategoryId,
    i18n::{self, Locale},
    platform::clipboard::{ClipboardEventReceiver, ClipboardMonitor, ClipboardWriteSuppressor},
    services::capture::CaptureService,
    services::custom_text::CustomTextService,
    services::database::{
        FavoriteRepository, HistoryRepository, TagRepository,
        query::{
            HistoryFilter, HistoryListRecord, HistoryQuery, HistoryQueryError, ListOrderSnapshot,
        },
    },
    services::image_preview::{ImagePreviewService, LoadedImagePreview},
    services::paste::PasteService,
    services::storage::RawBlobStore,
    state_runtime,
    symbols::SymbolGroupId,
    ui::{html as html_preview, smooth_scroll::SmoothScroll},
    windows::menu::MenuService,
    windows::preview::{PreviewAnchor, PreviewRect, PreviewSource, PreviewWindowService},
};

const INITIAL_PAGE_SIZE: usize = 1;
const MAX_QUERY_PAGE_SIZE: usize = 499;
const MAX_GALLERY_IMAGE_LOADS: usize = 2;
const LIST_PREVIEW_CHAR_LIMIT: usize = 300;
const MAX_HTML_IMAGE_PATHS_PER_RANGE: usize = 64;
const SEARCH_DEBOUNCE_DELAY: Duration = Duration::from_millis(200);
const TAGS_PANEL_CLOSE_DELAY: Duration = Duration::from_millis(150);
const WINDOW_SIZE_PERSIST_DELAY: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ListDropTarget {
    pub(crate) source_id: HistoryItemId,
    pub(crate) target_id: HistoryItemId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TagDropTarget {
    pub(crate) source_id: i64,
    pub(crate) target_id: i64,
}

pub(crate) fn display_priority_roles(priority: ClipboardDisplayPriority) -> [FormatRole; 3] {
    match priority {
        ClipboardDisplayPriority::TextHtmlImage => {
            [FormatRole::PlainText, FormatRole::Html, FormatRole::Image]
        }
        ClipboardDisplayPriority::TextImageHtml => {
            [FormatRole::PlainText, FormatRole::Image, FormatRole::Html]
        }
        ClipboardDisplayPriority::HtmlTextImage => {
            [FormatRole::Html, FormatRole::PlainText, FormatRole::Image]
        }
        ClipboardDisplayPriority::HtmlImageText => {
            [FormatRole::Html, FormatRole::Image, FormatRole::PlainText]
        }
        ClipboardDisplayPriority::ImageTextHtml => {
            [FormatRole::Image, FormatRole::PlainText, FormatRole::Html]
        }
        ClipboardDisplayPriority::ImageHtmlText => {
            [FormatRole::Image, FormatRole::Html, FormatRole::PlainText]
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AdaptiveHeightCacheKey {
    pub(crate) viewport_width: Pixels,
    pub(crate) rem_size: Pixels,
    pub(crate) font_metrics: crate::ui::fonts::AppFontMetrics,
    pub(crate) list_item_style: ListItemStyle,
    pub(crate) card_item_gap: CardItemGap,
    pub(crate) adaptive_max_lines: u8,
    pub(crate) only_show_notes: bool,
    pub(crate) show_info_row: bool,
    pub(crate) display_priority: ClipboardDisplayPriority,
    pub(crate) query_generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ListLayoutCacheKey {
    pub(crate) viewport_width: Pixels,
    pub(crate) viewport_height: Pixels,
    pub(crate) horizontal: bool,
    pub(crate) rem_size: Pixels,
    pub(crate) font_metrics: crate::ui::fonts::AppFontMetrics,
    pub(crate) list_item_style: ListItemStyle,
    pub(crate) card_item_gap: CardItemGap,
    pub(crate) list_item_height: crate::config::ListItemHeight,
    pub(crate) adaptive_max_lines: u8,
    pub(crate) only_show_notes: bool,
    pub(crate) show_info_row: bool,
    pub(crate) show_horizontal_header: bool,
    pub(crate) display_priority: ClipboardDisplayPriority,
    pub(crate) query_generation: u64,
    pub(crate) layout_generation: u64,
    pub(crate) total_item_count: usize,
}

pub(crate) struct ListLayoutCache {
    pub(crate) key: ListLayoutCacheKey,
    pub(crate) sizes: ReorderableVirtualListSizes,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ListStatus {
    Loading,
    Ready,
    Empty,
    Error,
}

pub(super) struct TagDragSnapshot {
    tags: Vec<Tag>,
}

#[derive(Clone)]
pub(crate) struct TagReorderAnimation {
    pub(crate) generation: u64,
    pub(crate) start_positions: HashMap<i64, Point<Pixels>>,
}

#[derive(Clone)]
pub(crate) struct ListReorderAnimation {
    pub(crate) generation: u64,
    pub(crate) start_offsets: HashMap<HistoryItemId, Pixels>,
    pub(crate) started_at: Instant,
}

#[derive(Clone, Copy)]
pub(crate) struct ListItemLoadAnimation {
    pub(crate) generation: u64,
    pub(crate) order: usize,
}

#[derive(Clone, Copy)]
pub(crate) struct FloatingToolbarDrag {
    pub(crate) start_mouse_y: Pixels,
    pub(crate) start_bottom: Pixels,
}

pub struct MainWindow {
    pub(crate) focus_handle: FocusHandle,
    pub items: Vec<ClipboardListItem>,
    pub selected_item_id: Option<HistoryItemId>,
    pub(crate) multi_selected_item_ids: Vec<HistoryItemId>,
    pub list_status: ListStatus,
    pub active_tab: TabId,
    pub(crate) split_panes: Option<[Entity<MainWindow>; 2]>,
    pub(crate) split_resize: Option<split::SplitResize>,
    pub(crate) split_content_bounds: Option<gpui::Bounds<Pixels>>,
    pub(crate) is_list_pane: bool,
    pub(crate) pane_active: bool,
    pub(super) pane_peer: Option<gpui::WeakEntity<MainWindow>>,
    pub symbol_category: SymbolCategoryId,
    pub(crate) emoji_category: EmojiCategoryId,
    pub(crate) symbol_search_query: String,
    pub(crate) symbol_search_input: Option<Entity<InputState>>,
    pub(super) symbol_search_input_placeholder: Option<String>,
    pub(crate) recent_emojis: Vec<String>,
    pub(crate) recent_symbols: Vec<String>,
    pub(crate) emoji_scroll_handle: VirtualListScrollHandle,
    pub(crate) emoji_rows_cache: Option<EmojiRowsCache>,
    pub(crate) emoji_category_row_indices: HashMap<EmojiCategoryId, usize>,
    pub(crate) emoji_scroll_target: Option<EmojiCategoryId>,
    pub(crate) symbol_group: SymbolGroupId,
    pub(crate) symbol_scroll_handle: VirtualListScrollHandle,
    pub(crate) symbol_category_row_indices: HashMap<SymbolGroupId, usize>,
    pub(crate) symbol_scroll_target: Option<SymbolGroupId>,
    pub(crate) selected_tag_ids: HashSet<i64>,
    pub(crate) tags_match_all: bool,
    pub(crate) tags_panel_open: bool,
    pub(crate) tags_panel_pinned: bool,
    pub(crate) tags_panel_width: f32,
    pub(crate) tags_panel_resize_start: Option<(Pixels, f32)>,
    pub(crate) tags_panel_bounds: Option<gpui::Bounds<Pixels>>,
    pub(crate) tags_button_bounds: Option<gpui::Bounds<Pixels>>,
    pub(crate) tags: Vec<Tag>,
    pub(crate) tags_loading: bool,
    pub(crate) tags_error: bool,
    pub(crate) tags_search_query: String,
    pub(crate) tags_search_input: Option<Entity<InputState>>,
    pub(super) tags_panel_close_task: Option<Task<()>>,
    pub(super) tags_search_input_locale: Option<Locale>,
    pub(crate) tag_drop_target: Option<TagDropTarget>,
    pub(super) tag_hovered_id: Option<i64>,
    pub(super) tag_drag_snapshot: Option<TagDragSnapshot>,
    pub(super) tag_drag_source_id: Option<i64>,
    pub(crate) tag_chip_positions: Rc<RefCell<HashMap<i64, Point<Pixels>>>>,
    pub(crate) tag_reorder_animation: Option<TagReorderAnimation>,
    pub(super) tag_reorder_animation_generation: u64,
    pub(crate) sidebar_collapsed: bool,
    pub(crate) titlebar_window_fixed: bool,
    pub(crate) dav_upload_busy: bool,
    pub(crate) dav_pull_busy: bool,
    pub(crate) window_drag_start: Option<Point<Pixels>>,
    pub(crate) search_query: String,
    pub(crate) search_expanded: bool,
    pub(crate) search_input: Option<Entity<InputState>>,
    pub(super) search_input_locale: Option<Locale>,
    pub(super) search_input_vertical: Option<bool>,
    pub filters: FilterSelection,
    pub(super) tab_bar_hover: TabBarHover,
    pub(super) tab_bar_hover_task: Option<Task<()>>,
    pub(super) tab_bar_hover_hitbox: Option<(crate::config::TabBarButton, gpui::HitboxId)>,
    pub query_generation: u64,
    pub has_more_items: bool,
    pub is_loading_more: bool,
    pub(super) pending_selection_index: Option<usize>,
    pub(super) multi_selection_anchor: Option<HistoryItemId>,
    pub(super) loading_page_range: Option<Range<usize>>,
    pub list_scroll_handle: VirtualListScrollHandle,
    pub(crate) list_heatmap: ListHeatmap,
    pub(crate) heat_scrollbar: HeatScrollbarState,
    pub(crate) adaptive_item_heights: HashMap<HistoryItemId, Pixels>,
    pub(super) adaptive_height_cache_key: Option<AdaptiveHeightCacheKey>,
    pub(crate) list_layout_generation: u64,
    pub(crate) list_layout_cache: Option<ListLayoutCache>,
    pub(crate) floating_toolbar_bottom: Pixels,
    pub(crate) floating_toolbar_drag: Option<FloatingToolbarDrag>,
    pub(crate) floating_toolbar_container_bounds: gpui::Bounds<Pixels>,
    pub(super) list_smooth_scroll: SmoothScroll,
    pub(super) emoji_smooth_scroll: SmoothScroll,
    pub(super) symbol_smooth_scroll: SmoothScroll,
    pub(crate) list_drop_target: Option<ListDropTarget>,
    pub(super) list_hovered_item_id: Option<HistoryItemId>,
    pub(super) keyboard_selection_mouse_move_sequence: Option<u64>,
    pub(super) preview_hover_generation: u64,
    pub(super) preview_hover_source: Option<PreviewSource>,
    pub(super) list_drag_source_id: Option<HistoryItemId>,
    pub(super) list_drag_origin_position: Option<usize>,
    pub(super) list_reorder_commit_pending: bool,
    pub(crate) list_reorder_animation: Option<ListReorderAnimation>,
    pub(super) list_reorder_animation_generation: u64,
    pub(crate) list_item_load_animations: HashMap<HistoryItemId, ListItemLoadAnimation>,
    pub(super) list_item_load_animation_generation: u64,
    /// 当前列表查询任务；刷新查询时丢弃旧任务，避免过期查询堆积占用后台线程。
    pub(super) list_query_task: Option<Task<()>>,
    pub(super) sync_refresh_task: Option<Task<()>>,
    pub(super) sync_changes: HashMap<i64, crate::services::sync::ReceivedChange>,
    pub(crate) list_order_ids: Vec<HistoryItemId>,
    pub(crate) list_order_positions: HashMap<HistoryItemId, usize>,
    pub total_item_count: usize,
    pub(super) history_repository: HistoryRepository,
    pub(super) favorite_repository: FavoriteRepository,
    pub(crate) tag_repository: TagRepository,
    pub(super) tags_query_generation: u64,
    pub(super) capture_service: CaptureService,
    pub(super) custom_text_service: CustomTextService,
    pub(super) paste_service: PasteService,
    pub(super) image_preview_service: ImagePreviewService,
    pub(crate) image_library: crate::services::image_library::ImageLibraryService,
    pub(crate) gallery_sort: state_runtime::PersistedGallerySort,
    pub(crate) gallery_groups: Vec<crate::services::image_library::ImageLibraryGroup>,
    pub(crate) gallery_groups_loading: bool,
    pub(super) gallery_groups_loaded: bool,
    pub(super) gallery_groups_generation: u64,
    pub(crate) selected_gallery_group_id: Option<String>,
    pub(crate) gallery_images: Vec<crate::services::image_library::ImageLibraryImage>,
    pub(crate) gallery_images_cache: super::components::gallery_picker::GalleryImagesCache,
    pub(crate) gallery_images_loading: bool,
    pub(super) gallery_images_generation: u64,
    pub(crate) gallery_scroll_handle: VirtualListScrollHandle,
    pub(crate) loaded_gallery_images: HashMap<PathBuf, Arc<RenderImage>>,
    pub(super) gallery_image_loading: HashSet<PathBuf>,
    pub(super) gallery_image_queue: VecDeque<PathBuf>,
    pub(super) gallery_visible_image_paths: HashSet<PathBuf>,
    pub(super) gallery_image_load_failures: HashSet<PathBuf>,
    pub(crate) loaded_gallery_group_icons: HashMap<PathBuf, Arc<RenderImage>>,
    pub(super) gallery_group_icon_loading: HashSet<PathBuf>,
    pub(super) gallery_group_icon_failures: HashSet<PathBuf>,
    pub(crate) loaded_gallery_animations: HashMap<PathBuf, Arc<RenderImage>>,
    pub(super) gallery_animation_loading: HashMap<PathBuf, Arc<AtomicBool>>,
    pub(super) gallery_animation_targets: Vec<PathBuf>,
    pub(super) gallery_animation_budget: usize,
    pub(super) gallery_animation_failures: HashSet<PathBuf>,
    pub(super) pending_gallery_image_drops: Vec<Arc<RenderImage>>,
    pub(crate) gallery_hovered_image: Option<PathBuf>,
    pub(crate) gallery_drop_hovering: bool,
    pub(crate) gallery_import_progress: Option<GalleryImportProgress>,
    pub(super) _clipboard_monitor: Option<ClipboardMonitor>,
    pub(super) clipboard_listener_task: Option<Task<()>>,
    pub(super) clipboard_listener_enabled: bool,
    pub(super) clipboard_write_suppressor: ClipboardWriteSuppressor,
    pub(crate) loaded_items: HashMap<HistoryItemId, ClipboardListItem>,
    pub(crate) loaded_images: HashMap<ImagePreviewKey, LoadedImagePreview>,
    pub(crate) loaded_html_images: HashMap<PathBuf, Arc<RenderImage>>,
    pub(crate) loaded_application_icons: HashMap<ApplicationIconKey, Arc<RenderImage>>,
    pub(crate) file_list_views: HashMap<FileListKey, Entity<FileListView>>,
    pub(super) _tags_search_input_subscription: Option<Subscription>,
    pub(super) _search_input_subscription: Option<Subscription>,
    pub(super) _symbol_search_input_subscription: Option<Subscription>,
    pub(super) _window_bounds_subscription: Option<Subscription>,
    pub(super) _window_size_persist_task: Option<Task<()>>,
    pub(super) _window_activation_subscription: Option<Subscription>,
    pub(super) _search_debounce_task: Option<Task<()>>,
    pub(super) image_loading: HashSet<ImagePreviewKey>,
    pub(super) image_load_failures: HashSet<ImagePreviewKey>,
    pub(super) image_cache_item_ids: HashSet<ClipboardItemId>,
    pub(super) image_cache_invalidated_item_ids: HashSet<ClipboardItemId>,
    pub(super) image_cache_needs_cleanup: bool,
    pub(super) html_image_loading: HashSet<PathBuf>,
    pub(super) html_image_load_failures: HashSet<PathBuf>,
    pub(super) html_image_cache_paths: HashSet<PathBuf>,
    pub(super) file_list_loading: HashSet<FileListKey>,
    pub(super) file_list_failures: HashSet<FileListKey>,
    pub(super) file_list_cache_keys: HashSet<FileListKey>,
    pub(super) file_list_cache_needs_cleanup: bool,
    pub(super) application_icon_loading: HashSet<ApplicationIconKey>,
    pub(super) application_icon_load_failures: HashSet<ApplicationIconKey>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ImagePreviewKey {
    item_id: ClipboardItemId,
    format_namespace: String,
    format_key: String,
}

impl ImagePreviewKey {
    fn new(item_id: ClipboardItemId, format_namespace: &str, format_key: &str) -> Self {
        Self {
            item_id,
            format_namespace: format_namespace.to_owned(),
            format_key: format_key.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct FileListKey {
    pub(crate) item_id: ClipboardItemId,
    pub(crate) format_namespace: String,
    pub(crate) format_key: String,
}

impl FileListKey {
    pub(crate) fn new(item_id: ClipboardItemId, format_namespace: &str, format_key: &str) -> Self {
        Self {
            item_id,
            format_namespace: format_namespace.to_owned(),
            format_key: format_key.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ApplicationIconKey {
    sha256: [u8; 32],
}

impl ApplicationIconKey {
    fn new(source: &ApplicationIconSource) -> Self {
        Self {
            sha256: source.sha256,
        }
    }
}

impl MainWindow {
    pub fn new(
        history_repository: HistoryRepository,
        favorite_repository: FavoriteRepository,
        tag_repository: TagRepository,
        capture_service: CaptureService,
        custom_text_service: CustomTextService,
        paste_service: PasteService,
        raw_blob_store: RawBlobStore,
        image_library: crate::services::image_library::ImageLibraryService,
        clipboard_monitor: Option<ClipboardMonitor>,
        clipboard_write_suppressor: ClipboardWriteSuppressor,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::with_preview_service(
            history_repository,
            favorite_repository,
            tag_repository,
            capture_service,
            custom_text_service,
            paste_service,
            ImagePreviewService::new(raw_blob_store),
            image_library,
            clipboard_monitor,
            clipboard_write_suppressor,
            cx,
        )
    }

    fn with_preview_service(
        history_repository: HistoryRepository,
        favorite_repository: FavoriteRepository,
        tag_repository: TagRepository,
        capture_service: CaptureService,
        custom_text_service: CustomTextService,
        paste_service: PasteService,
        image_preview_service: ImagePreviewService,
        image_library: crate::services::image_library::ImageLibraryService,
        clipboard_monitor: Option<ClipboardMonitor>,
        clipboard_write_suppressor: ClipboardWriteSuppressor,
        cx: &mut Context<Self>,
    ) -> Self {
        let persisted_state = state_runtime::state(cx);
        let tags_panel_width = persisted_state.main_window.restored_tags_panel_width();
        let clipboard_listener_enabled = clipboard_monitor.is_some();
        Self {
            focus_handle: cx.focus_handle(),
            selected_item_id: None,
            list_status: ListStatus::Loading,
            active_tab: persisted_state.main_window.active_tab,
            split_panes: None,
            split_resize: None,
            split_content_bounds: None,
            is_list_pane: false,
            pane_active: true,
            pane_peer: None,
            symbol_category: persisted_state.main_window.symbol_category,
            emoji_category: EmojiCategoryId::Recent,
            symbol_search_query: String::new(),
            symbol_search_input: None,
            symbol_search_input_placeholder: None,
            recent_emojis: persisted_state.main_window.recent_emojis,
            recent_symbols: persisted_state.main_window.recent_symbols,
            emoji_scroll_handle: VirtualListScrollHandle::new(),
            emoji_rows_cache: None,
            emoji_category_row_indices: HashMap::new(),
            emoji_scroll_target: None,
            symbol_group: SymbolGroupId::Recent,
            symbol_scroll_handle: VirtualListScrollHandle::new(),
            symbol_category_row_indices: HashMap::new(),
            symbol_scroll_target: None,
            selected_tag_ids: HashSet::new(),
            tags_match_all: false,
            tags_panel_open: false,
            tags_panel_pinned: false,
            tags_panel_width,
            tags_panel_resize_start: None,
            tags_panel_bounds: None,
            tags_button_bounds: None,
            tags: Vec::new(),
            tags_loading: false,
            tags_error: false,
            tags_search_query: String::new(),
            tags_search_input: None,
            tags_panel_close_task: None,
            tags_search_input_locale: None,
            tag_drop_target: None,
            tag_hovered_id: None,
            tag_drag_snapshot: None,
            tag_drag_source_id: None,
            tag_chip_positions: Rc::new(RefCell::new(HashMap::new())),
            tag_reorder_animation: None,
            tag_reorder_animation_generation: 0,
            sidebar_collapsed: false,
            titlebar_window_fixed: false,
            dav_upload_busy: false,
            dav_pull_busy: false,
            window_drag_start: None,
            search_query: String::new(),
            search_expanded: false,
            search_input: None,
            search_input_locale: None,
            search_input_vertical: None,
            filters: FilterSelection::default(),
            tab_bar_hover: TabBarHover::default(),
            tab_bar_hover_task: None,
            tab_bar_hover_hitbox: None,
            query_generation: 0,
            has_more_items: false,
            is_loading_more: false,
            pending_selection_index: None,
            multi_selection_anchor: None,
            loading_page_range: None,
            list_scroll_handle: VirtualListScrollHandle::new(),
            list_heatmap: ListHeatmap::default(),
            heat_scrollbar: HeatScrollbarState::default(),
            adaptive_item_heights: HashMap::new(),
            adaptive_height_cache_key: None,
            list_layout_generation: 0,
            list_layout_cache: None,
            floating_toolbar_bottom: px(16.0),
            floating_toolbar_drag: None,
            floating_toolbar_container_bounds: gpui::Bounds::default(),
            list_smooth_scroll: SmoothScroll::default(),
            emoji_smooth_scroll: SmoothScroll::default(),
            symbol_smooth_scroll: SmoothScroll::default(),
            list_drop_target: None,
            list_hovered_item_id: None,
            keyboard_selection_mouse_move_sequence: None,
            preview_hover_generation: 0,
            preview_hover_source: None,
            list_drag_source_id: None,
            list_drag_origin_position: None,
            list_reorder_commit_pending: false,
            list_reorder_animation: None,
            list_reorder_animation_generation: 0,
            list_item_load_animations: HashMap::new(),
            list_item_load_animation_generation: 0,
            list_query_task: None,
            sync_refresh_task: None,
            sync_changes: HashMap::new(),
            list_order_ids: Vec::new(),
            list_order_positions: HashMap::new(),
            total_item_count: 0,
            history_repository,
            favorite_repository,
            tag_repository,
            tags_query_generation: 0,
            capture_service,
            custom_text_service,
            paste_service,
            image_preview_service,
            image_library,
            gallery_sort: persisted_state.main_window.gallery_sort,
            gallery_groups: Vec::new(),
            gallery_groups_loading: false,
            gallery_groups_loaded: false,
            gallery_groups_generation: 0,
            selected_gallery_group_id: None,
            gallery_images: Vec::new(),
            gallery_images_cache: Default::default(),
            gallery_images_loading: false,
            gallery_images_generation: 0,
            gallery_scroll_handle: VirtualListScrollHandle::new(),
            loaded_gallery_images: HashMap::new(),
            gallery_image_loading: HashSet::new(),
            gallery_image_queue: VecDeque::new(),
            gallery_visible_image_paths: HashSet::new(),
            gallery_image_load_failures: HashSet::new(),
            loaded_gallery_group_icons: HashMap::new(),
            gallery_group_icon_loading: HashSet::new(),
            gallery_group_icon_failures: HashSet::new(),
            loaded_gallery_animations: HashMap::new(),
            gallery_animation_loading: HashMap::new(),
            gallery_animation_targets: Vec::new(),
            gallery_animation_budget: 0,
            gallery_animation_failures: HashSet::new(),
            pending_gallery_image_drops: Vec::new(),
            gallery_hovered_image: None,
            gallery_drop_hovering: false,
            gallery_import_progress: None,
            _clipboard_monitor: clipboard_monitor,
            clipboard_listener_task: None,
            clipboard_listener_enabled,
            clipboard_write_suppressor,
            items: Vec::new(),
            multi_selected_item_ids: Vec::new(),
            loaded_items: HashMap::new(),
            loaded_images: HashMap::new(),
            loaded_html_images: HashMap::new(),
            loaded_application_icons: HashMap::new(),
            image_loading: HashSet::new(),
            image_load_failures: HashSet::new(),
            image_cache_item_ids: HashSet::new(),
            image_cache_invalidated_item_ids: HashSet::new(),
            image_cache_needs_cleanup: false,
            html_image_loading: HashSet::new(),
            html_image_load_failures: HashSet::new(),
            html_image_cache_paths: HashSet::new(),
            file_list_views: HashMap::new(),
            _tags_search_input_subscription: None,
            _search_input_subscription: None,
            _symbol_search_input_subscription: None,
            _window_bounds_subscription: None,
            _window_size_persist_task: None,
            _window_activation_subscription: None,
            _search_debounce_task: None,
            file_list_loading: HashSet::new(),
            file_list_failures: HashSet::new(),
            file_list_cache_keys: HashSet::new(),
            file_list_cache_needs_cleanup: false,
            application_icon_loading: HashSet::new(),
            application_icon_load_failures: HashSet::new(),
        }
    }
}

fn hover_preview_allowed(menu_is_open: bool, window_is_visible: bool) -> bool {
    !menu_is_open && window_is_visible
}

impl Focusable for MainWindow {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

fn record_to_list_item(record: HistoryListRecord) -> ClipboardListItem {
    ClipboardListItem {
        history_id: record.history_id,
        item_id: record.item_id,
        favorite_id: record.favorite_id,
        tag_ids: record.tag_ids,
        note_text: record.note_text,
        preview_text: record.preview_text,
        preview_char_count: record.preview_char_count,
        preview_options: record.preview_options,
        all_format_options: record.all_format_options,
        content_kind: record.content_kind,
        paste_count: record.paste_count,
        is_pinned: record.is_pinned,
        sort_order: record.sort_order,
        last_seen_at: record.last_seen_at,
        shortcut: String::new(),
        source_app_name: record.source_app_name,
        source_device_name: record.source_device_name,
        source_app_icon: record.source_app_icon,
        is_selected: false,
    }
}

fn sort_tags(tags: &mut [Tag]) {
    tags.sort_by(|left, right| {
        right
            .sort_order
            .cmp(&left.sort_order)
            .then_with(|| right.id.cmp(&left.id))
    });
}

fn reorder_tags_by_ids(tags: &mut [Tag], order_ids: &[i64]) {
    let positions = order_ids
        .iter()
        .enumerate()
        .map(|(index, tag_id)| (*tag_id, index))
        .collect::<HashMap<_, _>>();
    tags.sort_by_key(|tag| positions.get(&tag.id).copied().unwrap_or(usize::MAX));
    let tag_count = tags.len();
    for (index, tag) in tags.iter_mut().enumerate() {
        tag.sort_order = i64::try_from(tag_count - index - 1).unwrap_or(i64::MAX);
    }
}

fn split_page(
    mut records: Vec<HistoryListRecord>,
    page_size: usize,
) -> (Vec<HistoryListRecord>, bool) {
    let has_more = records.len() > page_size;
    records.truncate(page_size);
    (records, has_more)
}

fn compare_list_items(left: &ClipboardListItem, right: &ClipboardListItem) -> Ordering {
    right
        .is_pinned
        .cmp(&left.is_pinned)
        .then_with(|| right.sort_order.cmp(&left.sort_order))
        .then_with(|| right.last_seen_at.cmp(&left.last_seen_at))
        .then_with(|| right.history_id.cmp(&left.history_id))
}

fn insert_sorted_item(items: &mut Vec<ClipboardListItem>, item: ClipboardListItem) {
    let index = items
        .binary_search_by(|existing| compare_list_items(existing, &item))
        .unwrap_or_else(|index| index);
    items.insert(index, item);
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct GalleryImportProgress {
    pub(crate) completed: usize,
    pub(crate) total: usize,
}

fn resolve_multi_selection_item_ids(
    active_tab: TabId,
    selected_ids: &[HistoryItemId],
    history_repository: &HistoryRepository,
    favorite_repository: &FavoriteRepository,
) -> Result<Vec<ClipboardItemId>, String> {
    let item_ids = match active_tab {
        TabId::Clipboard => history_repository
            .item_ids_for_history_ids(selected_ids)
            .map_err(|error| error.to_string())?,
        TabId::Favorites => favorite_repository
            .item_ids_for_favorite_ids(selected_ids)
            .map_err(|error| error.to_string())?,
        TabId::Emoji => return Err("当前页面不支持合并所选内容".to_owned()),
    };
    selected_ids
        .iter()
        .map(|list_id| {
            item_ids
                .get(list_id)
                .copied()
                .ok_or_else(|| format!("未找到所选内容：{list_id}"))
        })
        .collect()
}

fn show_multi_selection_feedback(
    result: Result<(), String>,
    title: String,
    success_message: String,
    failure_message: String,
    cx: &mut App,
) {
    match result {
        Ok(()) => crate::windows::notification::NotificationService::show_message(
            title,
            success_message,
            crate::windows::notification::NotificationLevel::Success,
            cx,
        ),
        Err(error) => {
            tracing::error!("多选操作失败：{error}");
            crate::windows::notification::NotificationService::show_message(
                title,
                format!("{failure_message}：{error}"),
                crate::windows::notification::NotificationLevel::Error,
                cx,
            );
        }
    }
}

fn refresh_shortcuts_for_order(items: &mut [ClipboardListItem], order_ids: &[HistoryItemId]) {
    if items.len() < 16 {
        for item in items {
            if let Some(index) = order_ids.iter().position(|id| *id == item.history_id) {
                item.shortcut = format!("Ctrl+{}", index + 1);
            }
        }
        return;
    }
    let positions = order_ids
        .iter()
        .enumerate()
        .map(|(index, history_id)| (*history_id, index))
        .collect::<HashMap<_, _>>();
    for item in items {
        if let Some(index) = positions.get(&item.history_id) {
            item.shortcut = format!("Ctrl+{}", index + 1);
        }
    }
}

fn refresh_shortcuts_after_move_to(
    items: &mut [ClipboardListItem],
    source_id: HistoryItemId,
    source_position: usize,
    target_position: usize,
) {
    for item in items {
        let Some(old_position) = item
            .shortcut
            .strip_prefix("Ctrl+")
            .and_then(|value| value.parse::<usize>().ok())
            .and_then(|value| value.checked_sub(1))
        else {
            continue;
        };
        let new_position = if item.history_id == source_id {
            target_position
        } else if source_position < target_position
            && old_position > source_position
            && old_position <= target_position
        {
            old_position - 1
        } else if source_position > target_position
            && old_position >= target_position
            && old_position < source_position
        {
            old_position + 1
        } else {
            old_position
        };
        if new_position != old_position {
            item.shortcut = format!("Ctrl+{}", new_position + 1);
        }
    }
}

fn refresh_shortcuts_after_removal(items: &mut [ClipboardListItem], removed_positions: &[usize]) {
    for item in items {
        let Some(position) = item
            .shortcut
            .strip_prefix("Ctrl+")
            .and_then(|value| value.parse::<usize>().ok())
            .and_then(|value| value.checked_sub(1))
        else {
            continue;
        };
        let removed_before = removed_positions.partition_point(|removed| *removed < position);
        if removed_before > 0 {
            item.shortcut = format!("Ctrl+{}", position - removed_before + 1);
        }
    }
}

fn refresh_shortcuts_after_insert(
    items: &mut [ClipboardListItem],
    inserted_id: HistoryItemId,
    inserted_position: usize,
) {
    for item in items {
        if item.history_id == inserted_id {
            item.shortcut = format!("Ctrl+{}", inserted_position + 1);
            continue;
        }
        let Some(position) = item
            .shortcut
            .strip_prefix("Ctrl+")
            .and_then(|value| value.parse::<usize>().ok())
            .and_then(|value| value.checked_sub(1))
        else {
            continue;
        };
        if position >= inserted_position {
            item.shortcut = format!("Ctrl+{}", position + 2);
        }
    }
}

fn assign_page_shortcuts(items: &mut [ClipboardListItem], offset: usize) {
    for (index, item) in items.iter_mut().enumerate() {
        item.shortcut = format!("Ctrl+{}", offset.saturating_add(index).saturating_add(1));
    }
}

fn keyboard_hover_is_suppressed(
    keyboard_mouse_move_sequence: Option<u64>,
    current_mouse_move_sequence: Option<u64>,
) -> bool {
    keyboard_mouse_move_sequence
        .is_some_and(|keyboard_sequence| Some(keyboard_sequence) == current_mouse_move_sequence)
}

fn shortcut_uses_favorite_removal(
    shortcut: crate::config::SessionShortcutId,
    active_tab: TabId,
) -> bool {
    shortcut == crate::config::SessionShortcutId::ToggleFavorite
        || (shortcut == crate::config::SessionShortcutId::DeleteItem
            && active_tab == TabId::Favorites)
}

pub(crate) fn move_id_to_target(
    order_ids: &mut Vec<HistoryItemId>,
    source_id: HistoryItemId,
    target_id: HistoryItemId,
) -> bool {
    let Some(source_position) = order_ids.iter().position(|id| *id == source_id) else {
        return false;
    };
    let source_id = order_ids.remove(source_position);
    let Some(target_position) = order_ids.iter().position(|id| *id == target_id) else {
        order_ids.insert(source_position, source_id);
        return false;
    };
    let insert_position = target_position + usize::from(source_position <= target_position);
    order_ids.insert(insert_position, source_id);
    true
}

#[cfg(test)]
mod tests {
    use super::{
        TabId, hover_preview_allowed, keyboard_hover_is_suppressed, move_id_to_target,
        shortcut_uses_favorite_removal,
    };
    use crate::config::SessionShortcutId;

    #[test]
    fn hover_preview_is_blocked_while_menu_is_open() {
        assert!(!hover_preview_allowed(true, true));
        assert!(hover_preview_allowed(false, true));
    }

    #[test]
    fn hover_preview_is_blocked_while_main_window_is_hidden() {
        assert!(!hover_preview_allowed(false, false));
    }

    #[test]
    fn mouse_hover_does_not_override_keyboard_selection_until_mouse_moves() {
        assert!(keyboard_hover_is_suppressed(Some(7), Some(7)));
        assert!(!keyboard_hover_is_suppressed(Some(7), Some(8)));
        assert!(!keyboard_hover_is_suppressed(None, None));
    }

    #[test]
    fn delete_shortcut_removes_favorites_only_on_favorites_tab() {
        assert!(shortcut_uses_favorite_removal(
            SessionShortcutId::DeleteItem,
            TabId::Favorites
        ));
        assert!(!shortcut_uses_favorite_removal(
            SessionShortcutId::DeleteItem,
            TabId::Clipboard
        ));
        assert!(shortcut_uses_favorite_removal(
            SessionShortcutId::ToggleFavorite,
            TabId::Clipboard
        ));
    }

    #[test]
    fn moving_id_down_inserts_it_after_the_target() {
        let mut order_ids = vec![1, 2, 3, 4];

        assert!(move_id_to_target(&mut order_ids, 1, 3));
        assert_eq!(order_ids, vec![2, 3, 1, 4]);
    }

    #[test]
    fn moving_id_up_inserts_it_before_the_target() {
        let mut order_ids = vec![1, 2, 3, 4];

        assert!(move_id_to_target(&mut order_ids, 4, 2));
        assert_eq!(order_ids, vec![1, 4, 2, 3]);
    }

    #[test]
    fn moving_id_to_missing_target_keeps_the_original_order() {
        let mut order_ids = vec![1, 2, 3];

        assert!(!move_id_to_target(&mut order_ids, 2, 9));
        assert_eq!(order_ids, vec![1, 2, 3]);
    }
}
/// URL 查询参数编码：对非字母数字及 -_.~ 之外的字符做 UTF-8 percent-encoding。
fn url_encode_query(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}
