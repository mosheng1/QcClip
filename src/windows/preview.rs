use crate::ui::scale::ui_rem;

use std::{collections::HashMap, fs, ops::Range, path::PathBuf, sync::Arc, time::Duration};

use file_icon_provider::Icon;
use gpui::prelude::*;
use gpui::{
    Anchor, Animation, AnimationExt, AnyElement, App, AppContext, Bounds, Context, Global,
    IntoElement, LineFragment, ListAlignment, ListSizingBehavior, ListState, ObjectFit, Pixels,
    Point, Render, RenderImage, ScrollHandle, ScrollWheelEvent, Size, StatefulInteractiveElement,
    Task, Window, WindowHandle, anchored, canvas, deferred, div, img, list, point, px, size,
};
use gpui_component::ActiveTheme as _;

use crate::{
    config::{
        ClipboardDisplayPriority, ClipboardPreviewType, ClipboardWindowPosition,
        PreviewInformationField,
    },
    domain::{
        capture::FormatRole,
        clipboard::{
            ClipboardListItem, ContentKind, FormatPreview, HistoryItemId, ImagePreviewSource,
            select_user_visible_previews_with_priority_and_filter,
        },
    },
    i18n,
    platform::window::{
        BorderlessPopup, logical_to_physical_desktop, open_borderless_mouse_passthrough_popup,
        physical_client_bounds, work_area_for_physical_point,
    },
    services::image_preview::ImagePreviewService,
    state_runtime::{self, PersistedPreviewWindowState},
    ui::{fonts::AppFontMetrics, html as html_preview, icon},
};

const PREVIEW_IDLE_TIMEOUT: Duration = Duration::from_secs(10);
const PREVIEW_MAIN_GAP: f32 = 18.0;
const PREVIEW_OFFSET: f32 = 14.0;
const HORIZONTAL_PREVIEW_MIN_WIDTH: f32 = 400.0;
const TEXT_SCROLL_STEP: f32 = 120.0;
const TEXT_LINE_HEIGHT: f32 = 22.4;
const TEXT_HORIZONTAL_PADDING: f32 = 24.0;
const TEXT_VERTICAL_PADDING: f32 = 16.0;
const IMAGE_PREVIEW_SIZE_STEP: f32 = 48.0;
const TEXT_MIN_WIDTH: f32 = 48.0;
const TEXT_MIN_HEIGHT: f32 = 46.0;
const PREVIEW_INFO_MAX_WIDTH: f32 = 960.0;
const PREVIEW_INFO_MIN_WIDTH: f32 = 160.0;
const PREVIEW_INFO_GAP: f32 = 8.0;
const PREVIEW_MAX_HEIGHT_RATIO: f32 = 0.8;
const PREVIEW_CORNER_RADIUS: f32 = 8.0;
const FILE_ICON_SIZE: u16 = 32;
const IMAGE_PREVIEW_MAX_SIZE: (u32, u32) = (4096, 4096);

fn preview_container_shadow() -> Vec<gpui::BoxShadow> {
    vec![
        gpui::BoxShadow::new(px(0.0), px(0.0), gpui::hsla(0.0, 0.0, 0.0, 0.28))
            .blur_radius(px(12.0))
            .spread_radius(px(0.0)),
    ]
}

fn preview_information_shadow() -> Vec<gpui::BoxShadow> {
    vec![
        gpui::BoxShadow::new(px(0.0), px(0.0), gpui::hsla(0.0, 0.0, 0.0, 0.20))
            .blur_radius(px(3.0))
            .spread_radius(px(0.0)),
    ]
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PreviewRect {
    pub(crate) left: f32,
    pub(crate) top: f32,
    pub(crate) width: f32,
    pub(crate) height: f32,
}

impl PreviewRect {
    pub(crate) fn from_bounds(window: &Window, bounds: Bounds<Pixels>) -> Option<Self> {
        let origin = logical_to_physical_desktop(window, bounds.origin)?;
        let scale = window.scale_factor().max(1.0);
        Some(Self {
            left: origin.x,
            top: origin.y,
            width: f32::from(bounds.size.width) * scale,
            height: f32::from(bounds.size.height) * scale,
        })
    }

    pub(crate) fn from_window(window: &Window) -> Option<Self> {
        if let Ok(bounds) = physical_client_bounds(window) {
            return Some(Self {
                left: bounds.left as f32,
                top: bounds.top as f32,
                width: (bounds.right - bounds.left).max(1) as f32,
                height: (bounds.bottom - bounds.top).max(1) as f32,
            });
        }

        Self::from_bounds(window, window.bounds())
    }

    fn center(self) -> Point<f32> {
        point(self.left + self.width / 2.0, self.top + self.height / 2.0)
    }

    fn contains(self, point: Point<f32>) -> bool {
        point.x >= self.left
            && point.x < self.left + self.width
            && point.y >= self.top
            && point.y < self.top + self.height
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PreviewAnchor {
    pub(crate) rect: PreviewRect,
    pointer: Point<f32>,
}

impl PreviewAnchor {
    pub(crate) fn from_bounds(window: &Window, bounds: Bounds<Pixels>) -> Option<Self> {
        let rect = PreviewRect::from_bounds(window, bounds)?;
        Some(Self {
            rect,
            pointer: logical_to_physical_desktop(window, window.mouse_position())
                .unwrap_or_else(|| rect.center()),
        })
    }

    fn center(self) -> Point<f32> {
        self.rect.center()
    }

    fn pointer(self) -> Point<f32> {
        self.pointer
    }

    pub(crate) fn with_pointer(mut self, pointer: Point<f32>) -> Self {
        self.pointer = pointer;
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PreviewSource {
    History(HistoryItemId),
    Favorite(i64),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreviewMode {
    Text,
    Html,
    Image,
    File,
}

#[derive(Clone, Debug)]
struct PreviewFileEntry {
    path: PathBuf,
    name: String,
    size: Option<u64>,
    is_directory: bool,
    exists: bool,
    icon: Option<Arc<RenderImage>>,
}

impl PreviewFileEntry {
    fn from_path(path: PathBuf) -> Self {
        let metadata = fs::metadata(&path).ok();
        let exists = metadata.is_some();
        let is_directory = metadata.as_ref().is_some_and(|metadata| metadata.is_dir());
        let size = metadata
            .as_ref()
            .filter(|metadata| !metadata.is_dir())
            .map(|metadata| metadata.len());
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        Self {
            path,
            name,
            size,
            is_directory,
            exists,
            icon: None,
        }
    }

    fn type_label(&self) -> String {
        if self.is_directory {
            return "目录".to_owned();
        }
        self.path
            .extension()
            .and_then(|extension| extension.to_str())
            .filter(|extension| !extension.is_empty())
            .map(|extension| extension.to_ascii_uppercase())
            .unwrap_or_else(|| "文件".to_owned())
    }
}

pub(crate) fn render_file_icon(icon: Icon) -> Option<Arc<RenderImage>> {
    let pixels = usize::try_from(icon.width)
        .ok()?
        .checked_mul(usize::try_from(icon.height).ok()?)?
        .checked_mul(4)?;
    if icon.pixels.len() != pixels {
        return None;
    }
    let mut bgra = icon.pixels;
    for pixel in bgra.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let image = image::RgbaImage::from_raw(icon.width, icon.height, bgra)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(image)])))
}

impl PreviewMode {
    fn label(self, translator: &i18n::Translator) -> String {
        translator.translate(match self {
            Self::Text => "main.format.plain_text",
            Self::Html => "main.preview.format_html",
            Self::Image => "main.format.image",
            Self::File => "main.format.file",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct LogicalRect {
    left: f32,
    top: f32,
    width: f32,
    height: f32,
}

impl LogicalRect {
    fn right(self) -> f32 {
        self.left + self.width
    }

    fn bottom(self) -> f32 {
        self.top + self.height
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreviewPlacementSide {
    Right,
    Left,
    Bottom,
    Top,
    Overlay,
    PointerRight,
    PointerLeft,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PreviewPlacement {
    position: Point<f32>,
    anchor: Anchor,
    side: PreviewPlacementSide,
}

#[derive(Default)]
pub(crate) struct PreviewWindowService {
    window: Option<WindowHandle<PreviewWindow>>,
    image_preview_service: Option<ImagePreviewService>,
    generation: u64,
    pending_show: Option<Task<()>>,
    idle_destroy_task: Option<Task<()>>,
    active_source: Option<PreviewSource>,
}

impl Global for PreviewWindowService {}

impl PreviewWindowService {
    pub(crate) fn install(image_preview_service: ImagePreviewService, cx: &mut App) {
        cx.set_global(Self {
            image_preview_service: Some(image_preview_service),
            ..Default::default()
        });
        cx.on_window_closed(|cx, window_id| {
            cx.defer(move |cx| {
                cx.update_global::<Self, _>(|service, _| {
                    if service
                        .window
                        .is_some_and(|window| window.window_id() == window_id)
                    {
                        service.window = None;
                        service.idle_destroy_task = None;
                        service.active_source = None;
                    }
                });
            });
        })
        .detach();
    }

    pub(crate) fn show(
        item: ClipboardListItem,
        source: PreviewSource,
        mut anchor: PreviewAnchor,
        main_window: Option<PreviewRect>,
        display_priority: ClipboardDisplayPriority,
        cx: &mut App,
    ) {
        if let Some(pointer) =
            crate::platform::input::pointer_snapshot().map(|snapshot| snapshot.position)
        {
            anchor = anchor.with_pointer(pointer);
        }
        let hover_delay =
            Duration::from_millis(u64::from(crate::config_runtime::hover_preview_delay_ms(cx)));
        cx.update_global::<Self, _>(|service, cx| {
            service.idle_destroy_task = None;
            service.generation = service.generation.wrapping_add(1);
            let generation = service.generation;
            service.pending_show = Some(cx.spawn(async move |cx| {
                cx.background_executor().timer(hover_delay).await;
                cx.update(|cx| {
                    cx.update_global::<Self, _>(|service, cx| {
                        if service.generation == generation {
                            service.pending_show = None;
                            service.show_now(
                                item,
                                source,
                                anchor,
                                main_window,
                                display_priority,
                                cx,
                            );
                        }
                    });
                });
            }));
        });
    }

    pub(crate) fn hide(cx: &mut App) {
        cx.update_global::<Self, _>(|service, cx| {
            service.pending_show = None;
            service.generation = service.generation.wrapping_add(1);
            service.active_source = None;
            if let Some(window) = service.window {
                let _ = window.update(cx, |preview, window, cx| preview.hide(window, cx));
                service.schedule_idle_destroy(window, cx);
            }
        });
    }

    fn defer_hide(cx: &mut App) {
        let generation = cx.global::<Self>().generation;
        cx.defer(move |cx| {
            // 旧预览的关闭回调不能取消重新进入后发起的新预览。
            if cx.global::<Self>().generation == generation {
                crate::windows::main::MainWindowService::cancel_hover_preview(cx);
            }
        });
    }

    pub(crate) fn destroy(cx: &mut App) {
        cx.update_global::<Self, _>(|service, cx| {
            service.pending_show = None;
            service.idle_destroy_task = None;
            service.generation = service.generation.wrapping_add(1);
            service.active_source = None;
            if let Some(window) = service.window.take() {
                let _ = window.update(cx, |preview, window, cx| preview.hide(window, cx));
                let _ = window.update(cx, |_, window, _| window.remove_window());
            }
        });
    }

    pub(crate) fn replace_image_preview_service(
        image_preview_service: ImagePreviewService,
        cx: &mut App,
    ) {
        cx.update_global::<Self, _>(|service, _| {
            service.image_preview_service = Some(image_preview_service);
        });
    }

    fn schedule_idle_destroy(&mut self, window: WindowHandle<PreviewWindow>, cx: &mut App) {
        let generation = self.generation;
        self.idle_destroy_task = Some(cx.spawn(async move |cx| {
            cx.background_executor().timer(PREVIEW_IDLE_TIMEOUT).await;
            cx.update(|cx| {
                cx.update_global::<Self, _>(|service, cx| {
                    if service.generation != generation
                        || service.active_source.is_some()
                        || service.window != Some(window)
                    {
                        return;
                    }
                    let _ = window.update(cx, |_, window, _| window.remove_window());
                    service.window = None;
                    service.idle_destroy_task = None;
                });
            });
        }));
    }

    pub(crate) fn update_anchor(source: PreviewSource, anchor: PreviewAnchor, cx: &mut App) {
        cx.update_global::<Self, _>(|service, cx| {
            if service.active_source != Some(source) {
                return;
            }
            let Some(window) = service.window else {
                return;
            };
            let _ = window.update(cx, |preview, _window, cx| {
                if !preview.visible || preview.source != Some(source) {
                    return;
                }
                let Some(current) = preview.anchor else {
                    return;
                };
                if current.rect != anchor.rect {
                    preview.anchor = Some(PreviewAnchor {
                        rect: anchor.rect,
                        ..current
                    });
                    cx.notify();
                }
            });
        });
    }

    pub(crate) fn handle_wheel(
        source: PreviewSource,
        event: &ScrollWheelEvent,
        window: &Window,
        cx: &mut App,
    ) {
        if !event.modifiers.control {
            return;
        }
        cx.update_global::<Self, _>(|service, cx| {
            let Some(window_handle) = service.window else {
                return;
            };
            let _ = window_handle.update(cx, |preview, _preview_window, cx| {
                if preview.source != Some(source) || !preview.visible {
                    return;
                }
                cx.stop_propagation();
                let delta_y = event.delta_y(window.line_height());
                if event.modifiers.alt {
                    preview.cycle_mode(delta_y, cx);
                } else {
                    preview.scroll(delta_y, cx);
                }
            });
        });
    }

    fn show_now(
        &mut self,
        item: ClipboardListItem,
        source: PreviewSource,
        anchor: PreviewAnchor,
        main_window: Option<PreviewRect>,
        display_priority: ClipboardDisplayPriority,
        cx: &mut App,
    ) {
        let Some(image_preview_service) = self.image_preview_service.clone() else {
            return;
        };
        let clipboard_config = crate::config_runtime::clipboard_config(cx);
        if item.note_text.is_empty()
            && select_preview_options(
                &item.preview_options,
                &display_priority_roles(display_priority),
                &clipboard_config.preview_types,
            )
            .iter()
            .all(|preview| mode_for_preview(preview).is_none())
        {
            return;
        }
        let Some(display) = work_area_for_physical_point(cx, anchor.center()) else {
            return;
        };
        let window_position = crate::config_runtime::clipboard_config(cx).window_position;
        self.active_source = Some(source);
        let scale = display.scale_factor.max(1.0);
        let bounds = Bounds::new(
            point(px(display.bounds.x / scale), px(display.bounds.y / scale)),
            size(
                px(display.bounds.width / scale),
                px(display.bounds.height / scale),
            ),
        );
        let window = match self.window {
            Some(window) => window,
            None => {
                let result = open_borderless_mouse_passthrough_popup(
                    cx,
                    bounds,
                    Some(display.display_id),
                    move |_window, cx, popup| {
                        cx.new(|_| PreviewWindow::new(popup, image_preview_service))
                    },
                );
                let window = match result {
                    Ok(window) => window,
                    Err(error) => {
                        tracing::error!("打开全屏预览窗口失败：{error}");
                        return;
                    }
                };
                self.window = Some(window);
                window
            }
        };
        if let Err(error) = window.update(cx, |preview, window, cx| {
            preview.show(
                item,
                source,
                anchor,
                main_window,
                display_priority,
                window_position,
                window,
                cx,
            );
        }) {
            tracing::error!("更新全屏预览窗口失败：{error}");
            self.window = None;
            self.active_source = None;
        }
    }
}

struct PreviewWindow {
    popup: BorderlessPopup,
    image_preview_service: ImagePreviewService,
    item: Option<ClipboardListItem>,
    previews: Vec<FormatPreview>,
    mode: Option<PreviewMode>,
    source: Option<PreviewSource>,
    anchor: Option<PreviewAnchor>,
    main_window: Option<PreviewRect>,
    display_priority: ClipboardDisplayPriority,
    window_position: ClipboardWindowPosition,
    generation: u64,
    reveal_generation: u64,
    content_size: Option<(f32, f32)>,
    surface_bounds: Option<LogicalRect>,
    image: Option<Arc<RenderImage>>,
    image_dimensions: Option<(u32, u32)>,
    image_byte_length: Option<u64>,
    image_source_path: Option<PathBuf>,
    image_base_size: f32,
    image_base_size_limit: Option<f32>,
    html_images: HashMap<PathBuf, Arc<RenderImage>>,
    file_entries: Vec<PreviewFileEntry>,
    content_scrollable: bool,
    loading: bool,
    error: Option<String>,
    visible: bool,
    foreground_hook: Option<isize>,
    pointer_frame_scheduled: bool,
    text_content: Arc<str>,
    text_line_ranges: Arc<Vec<Range<usize>>>,
    text_wrap_width: Option<f32>,
    font_metrics: Option<AppFontMetrics>,
    text_line_height: Pixels,
    text_list_state: ListState,
    html_scroll: ScrollHandle,
    file_scroll: ScrollHandle,
    note_scroll: ScrollHandle,
}

impl PreviewWindow {
    fn new(popup: BorderlessPopup, image_preview_service: ImagePreviewService) -> Self {
        Self {
            popup,
            image_preview_service,
            item: None,
            previews: Vec::new(),
            mode: None,
            source: None,
            anchor: None,
            main_window: None,
            display_priority: ClipboardDisplayPriority::default(),
            window_position: ClipboardWindowPosition::default(),
            generation: 0,
            reveal_generation: 0,
            content_size: None,
            surface_bounds: None,
            image: None,
            image_dimensions: None,
            image_byte_length: None,
            image_source_path: None,
            image_base_size: PersistedPreviewWindowState::DEFAULT_IMAGE_BASE_SIZE,
            image_base_size_limit: None,
            html_images: HashMap::new(),
            file_entries: Vec::new(),
            content_scrollable: false,
            loading: false,
            error: None,
            visible: false,
            foreground_hook: None,
            pointer_frame_scheduled: false,
            text_content: Arc::from(""),
            text_line_ranges: Arc::new(Vec::new()),
            text_wrap_width: None,
            font_metrics: None,
            text_line_height: px(TEXT_LINE_HEIGHT),
            text_list_state: ListState::new(0, ListAlignment::Top, px(2048.0)),
            html_scroll: ScrollHandle::new(),
            file_scroll: ScrollHandle::new(),
            note_scroll: ScrollHandle::new(),
        }
    }

    fn show(
        &mut self,
        item: ClipboardListItem,
        source: PreviewSource,
        anchor: PreviewAnchor,
        main_window: Option<PreviewRect>,
        display_priority: ClipboardDisplayPriority,
        window_position: ClipboardWindowPosition,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.generation = self.generation.wrapping_add(1);
        self.reveal_generation = self.reveal_generation.wrapping_add(1);
        self.anchor = Some(anchor);
        self.main_window = main_window;
        self.source = Some(source);
        self.display_priority = display_priority;
        self.window_position = window_position;
        self.visible = true;
        if self.foreground_hook.is_none() {
            self.foreground_hook = crate::platform::window::install_preview_foreground_hook();
        }
        self.schedule_pointer_follow(window, cx);
        self.set_item(item, cx);
        self.popup.set_visible(window, true);
    }

    fn schedule_pointer_follow(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.pointer_frame_scheduled {
            return;
        }
        self.pointer_frame_scheduled = true;

        let preview = cx.entity();
        window.on_next_frame(move |window, cx| {
            let pointer =
                crate::platform::input::pointer_snapshot().map(|snapshot| snapshot.position);
            let _ = preview.update(cx, |preview, cx| {
                preview.pointer_frame_scheduled = false;
                if !preview.visible {
                    return;
                }

                if let (Some(pointer), Some(main_window)) = (pointer, preview.main_window)
                    && !main_window.contains(pointer)
                {
                    preview.hide(window, cx);
                    PreviewWindowService::defer_hide(cx);
                    return;
                }

                if crate::platform::window::preview_foreground_event_pending() {
                    preview.hide(window, cx);
                    PreviewWindowService::defer_hide(cx);
                    return;
                }

                if let (Some(pointer), Some(anchor)) = (pointer, preview.anchor)
                    && anchor.pointer() != pointer
                {
                    preview.anchor = Some(anchor.with_pointer(pointer));
                    cx.notify();
                }

                preview.schedule_pointer_follow(window, cx);
            });
        });
    }

    fn set_item(&mut self, item: ClipboardListItem, cx: &mut Context<Self>) {
        self.item = Some(item);
        self.previews = self
            .item
            .as_ref()
            .map(|item| {
                select_preview_options(
                    &item.preview_options,
                    &display_priority_roles(self.display_priority),
                    &crate::config_runtime::clipboard_config(cx).preview_types,
                )
            })
            .unwrap_or_default();
        self.mode = self
            .previews
            .first()
            .and_then(|preview| mode_for_preview(preview));
        self.reset_text_list();
        self.content_size = None;
        self.surface_bounds = None;
        self.image = None;
        self.image_dimensions = self
            .current_preview()
            .and_then(|preview| preview.image_dimensions)
            .map(|dimensions| (dimensions.width, dimensions.height));
        self.image_byte_length = None;
        self.image_source_path = None;
        self.image_base_size = state_runtime::state(cx)
            .preview_window
            .restored_image_base_size();
        self.image_base_size_limit = None;
        self.html_images.clear();
        self.file_entries.clear();
        self.content_scrollable = false;
        self.loading = false;
        self.error = None;
        self.html_scroll.set_offset(Point::default());
        self.file_scroll.set_offset(Point::default());
        self.note_scroll.set_offset(Point::default());
        if self.image.is_none() {
            self.load_resource(cx);
        }
        cx.notify();
    }

    fn current_preview(&self) -> Option<&FormatPreview> {
        let mode = self.mode?;
        self.previews
            .iter()
            .find(|preview| mode_for_preview(preview) == Some(mode))
    }

    fn reset_text_list(&mut self) {
        let text = self
            .current_preview()
            .filter(|_| self.mode == Some(PreviewMode::Text))
            .map(|preview| preview.text.clone())
            .unwrap_or_default();
        let text_content: Arc<str> = Arc::from(text);
        let text_line_ranges = Arc::new(text_line_ranges(&text_content));
        self.text_content = text_content;
        self.text_line_ranges = text_line_ranges.clone();
        self.text_wrap_width = None;
        self.text_list_state
            .reset_with_uniform_height(text_line_ranges.len(), px(TEXT_LINE_HEIGHT));
    }

    fn ensure_text_layout(&mut self, wrap_width: f32, window: &Window, cx: &App) {
        if self.mode != Some(PreviewMode::Text) {
            return;
        }
        let wrap_width = wrap_width.max(1.0).round();
        if self
            .text_wrap_width
            .is_some_and(|current| (current - wrap_width).abs() < 0.5)
        {
            return;
        }

        let ranges = wrapped_text_line_ranges(
            &self.text_content,
            px(wrap_width),
            gpui::font(cx.theme().font_family.clone()),
            window,
        );
        self.text_line_ranges = Arc::new(ranges);
        self.text_list_state
            .reset_with_uniform_height(self.text_line_ranges.len(), self.text_line_height);
        self.text_wrap_width = Some(wrap_width);
        self.content_size = None;
        self.surface_bounds = None;
        self.reveal_generation = self.reveal_generation.wrapping_add(1);
    }

    fn load_resource(&mut self, cx: &mut Context<Self>) {
        let Some(preview) = self.current_preview().cloned() else {
            return;
        };
        let Some(mode) = self.mode else { return };
        let generation = self.generation;
        if mode == PreviewMode::Image
            && let Some(source) = preview.image_source.clone()
        {
            self.loading = true;
            let service = self.image_preview_service.clone();
            let format_kind = preview.format_kind;
            let format_key = preview.format_key.clone();
            let dimensions_hint = preview.image_dimensions;
            let task = cx.background_spawn(async move {
                service.load_with_dimensions_hint(
                    &source,
                    format_kind,
                    &format_key,
                    IMAGE_PREVIEW_MAX_SIZE,
                    dimensions_hint,
                )
            });
            cx.spawn(async move |this, cx| {
                let result = task.await;
                if let Some(this) = this.upgrade() {
                    let _ = this.update(cx, |this, cx| {
                        if this.generation != generation {
                            return;
                        }
                        this.loading = false;
                        match result {
                            Ok(loaded) => {
                                let dimensions = loaded
                                    .dimensions
                                    .map(|dimensions| (dimensions.width, dimensions.height));
                                if this.image_dimensions != dimensions {
                                    this.reveal_generation = this.reveal_generation.wrapping_add(1);
                                }
                                this.image_dimensions = dimensions;
                                this.image_byte_length = loaded.byte_length;
                                this.image_source_path = loaded.source_path;
                                this.image = Some(loaded.image);
                            }
                            Err(error) => {
                                tracing::warn!(format_key = %preview.format_key, %error, "加载图片预览失败");
                                this.error = Some(error.to_string());
                            }
                        }
                        cx.notify();
                    });
                }
            })
            .detach();
            return;
        }

        if mode == PreviewMode::Html
            && let Some(html) = preview.html_content.clone()
        {
            let paths = html_preview::local_image_paths(&html);
            if paths.is_empty() {
                return;
            }
            self.loading = true;
            let service = self.image_preview_service.clone();
            let task = cx.background_spawn(async move {
                let mut images = HashMap::new();
                let mut failed_count = 0;
                for path in paths {
                    match service.load_local_path(&path, IMAGE_PREVIEW_MAX_SIZE) {
                        Ok(image) => {
                            images.insert(path, image);
                        }
                        Err(error) => {
                            failed_count += 1;
                            tracing::warn!(%error, "加载 HTML 本地图片预览失败");
                        }
                    }
                }
                (images, failed_count)
            });
            cx.spawn(async move |this, cx| {
                let (images, failed_count) = task.await;
                if let Some(this) = this.upgrade() {
                    let _ = this.update(cx, |this, cx| {
                        if this.generation != generation {
                            return;
                        }
                        this.loading = false;
                        this.html_images = images;
                        if failed_count > 0 {
                            tracing::warn!(failed_count, "部分 HTML 本地图片预览加载失败");
                        }
                        this.content_size = None;
                        this.surface_bounds = None;
                        this.reveal_generation = this.reveal_generation.wrapping_add(1);
                        cx.notify();
                    });
                }
            })
            .detach();
            return;
        }

        if mode == PreviewMode::File
            && let Some(source) = preview.image_source.clone()
        {
            self.loading = true;
            let service = self.image_preview_service.clone();
            let task = cx.background_spawn(async move { service.load_file_list_paths(&source) });
            cx.spawn(async move |this, cx| {
                let result = task.await;
                if let Some(this) = this.upgrade() {
                    let _ = this.update(cx, |this, cx| {
                        if this.generation != generation {
                            return;
                        }
                        this.loading = false;
                        match result {
                            Ok(paths) => {
                                let icon_paths = paths.clone();
                                let icon_task = cx.background_spawn(async move {
                                    icon_paths
                                        .iter()
                                        .enumerate()
                                        .map(|(index, path)| {
                                            let icon = path
                                                .exists()
                                                .then(|| {
                                                    file_icon_provider::get_file_icon(
                                                        path,
                                                        FILE_ICON_SIZE,
                                                    )
                                                    .ok()
                                                })
                                                .flatten()
                                                .and_then(render_file_icon);
                                            (index, icon)
                                        })
                                        .collect::<Vec<_>>()
                                });
                                let icon_generation = generation;
                                cx.spawn(async move |this, cx| {
                                    let icons = icon_task.await;
                                    if let Some(this) = this.upgrade() {
                                        let _ = this.update(cx, |this, cx| {
                                            if this.generation != icon_generation {
                                                return;
                                            }
                                            for (index, icon) in icons {
                                                if let Some(entry) =
                                                    this.file_entries.get_mut(index)
                                                {
                                                    entry.icon = icon;
                                                }
                                            }
                                            cx.notify();
                                        });
                                    }
                                })
                                .detach();
                                this.content_size = None;
                                this.surface_bounds = None;
                                this.reveal_generation = this.reveal_generation.wrapping_add(1);
                                this.file_entries =
                                    paths.into_iter().map(PreviewFileEntry::from_path).collect();
                            }
                            Err(error) => {
                                tracing::warn!(%error, "加载文件列表预览失败");
                                this.error = Some(error.to_string());
                            }
                        }
                        cx.notify();
                    });
                }
            })
            .detach();
        }
    }

    fn cycle_mode(&mut self, delta_y: f32, cx: &mut Context<Self>) {
        let modes = self.previews.iter().filter_map(mode_for_preview).fold(
            Vec::new(),
            |mut modes, mode| {
                if !modes.contains(&mode) {
                    modes.push(mode);
                }
                modes
            },
        );
        if modes.len() <= 1 {
            return;
        }
        let current = self.mode.unwrap_or(modes[0]);
        let index = modes.iter().position(|mode| *mode == current).unwrap_or(0);
        let next = if delta_y > 0.0 {
            modes[(index + modes.len() - 1) % modes.len()]
        } else {
            modes[(index + 1) % modes.len()]
        };
        self.generation = self.generation.wrapping_add(1);
        self.mode = Some(next);
        self.reset_text_list();
        self.content_size = None;
        self.reveal_generation = self.reveal_generation.wrapping_add(1);
        self.surface_bounds = None;
        self.image = None;
        self.image_dimensions = None;
        self.image_byte_length = None;
        self.image_source_path = None;
        self.image_base_size = state_runtime::state(cx)
            .preview_window
            .restored_image_base_size();
        self.image_base_size_limit = None;
        self.loading = false;
        self.error = None;
        self.content_scrollable = false;
        if self.image.is_none() {
            self.load_resource(cx);
        }
        cx.notify();
    }

    fn scroll(&mut self, delta_y: f32, cx: &mut Context<Self>) {
        let max_note_offset = self.note_scroll.max_offset().y;
        if max_note_offset > px(0.0) {
            let offset = self.note_scroll.offset();
            let next = (offset.y + px(delta_y.signum() * TEXT_SCROLL_STEP))
                .clamp(-max_note_offset, px(0.0));
            if next != offset.y {
                self.note_scroll.set_offset(point(offset.x, next));
                cx.notify();
                return;
            }
        }
        let Some(mode) = self.mode else { return };
        if mode == PreviewMode::Image {
            let Some(limit) = self.image_base_size_limit else {
                return;
            };
            let next = Self::next_image_base_size(self.image_base_size, limit, delta_y);
            if next != self.image_base_size {
                self.image_base_size = next;
                state_runtime::set_preview_image_base_size(next, cx);
                cx.notify();
            }
            return;
        }
        if mode == PreviewMode::Text {
            self.text_list_state
                .scroll_by(px(-delta_y.signum() * TEXT_SCROLL_STEP));
        } else {
            let handle = match mode {
                PreviewMode::Html => &self.html_scroll,
                PreviewMode::File => &self.file_scroll,
                PreviewMode::Text | PreviewMode::Image => return,
            };
            let mut offset = handle.offset();
            let max_offset = handle.max_offset();
            offset.y =
                (offset.y + px(delta_y.signum() * TEXT_SCROLL_STEP)).clamp(-max_offset.y, px(0.0));
            handle.set_offset(offset);
        }
        cx.notify();
    }

    fn hide(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.visible = false;
        if let Some(hook) = self.foreground_hook.take() {
            crate::platform::window::uninstall_preview_foreground_hook(hook);
        }
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let popup = self.popup;
        let preview = cx.entity();
        self.item = None;
        self.previews.clear();
        self.mode = None;
        self.source = None;
        self.anchor = None;
        self.main_window = None;
        self.content_size = None;
        self.surface_bounds = None;
        self.image = None;
        self.image_byte_length = None;
        self.image_source_path = None;
        self.image_base_size_limit = None;
        self.html_images.clear();
        self.file_entries.clear();
        self.text_content = Arc::from("");
        self.text_line_ranges = Arc::new(Vec::new());
        self.text_wrap_width = None;
        self.text_list_state.reset(0);
        self.content_scrollable = false;
        cx.notify();
        window.on_next_frame(move |window, cx| {
            let _ = preview.update(cx, |preview, _| {
                if !preview.visible && preview.generation == generation {
                    popup.set_visible(window, false);
                }
            });
        });
    }

    fn render_content(
        &self,
        max_width: f32,
        max_height: f32,
        image_size: Option<Size<f32>>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = crate::window_material::excluded_window_theme(cx);
        let translator = i18n::translator(cx);
        let Some(preview) = self.current_preview() else {
            return div().into_any_element();
        };
        match self.mode {
            Some(PreviewMode::Image) => {
                let image_content = if let Some(image) = self.image.clone() {
                    let image = img(image).object_fit(ObjectFit::Contain);
                    if let Some(size) = image_size {
                        image
                            .w(px(size.width))
                            .h(px(size.height))
                            .into_any_element()
                    } else {
                        image
                            .max_w(px(max_width))
                            .max_h(px(max_height))
                            .into_any_element()
                    }
                } else if let Some(error) = &self.error {
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(theme.muted_foreground)
                        .child(format!("图片不可用：{error}"))
                        .into_any_element()
                } else {
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(theme.muted_foreground)
                        .child("正在加载图片…")
                        .into_any_element()
                };
                div().flex_none().child(image_content).into_any_element()
            }
            Some(PreviewMode::File) => {
                let rows = self
                    .file_entries
                    .iter()
                    .enumerate()
                    .map(|(index, entry)| {
                        let missing = !entry.exists;
                        let icon_name = if entry.is_directory {
                            "folder"
                        } else if entry
                            .path
                            .extension()
                            .and_then(|extension| extension.to_str())
                            .is_some_and(|extension| {
                                matches!(
                                    extension.to_ascii_lowercase().as_str(),
                                    "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "svg"
                                )
                            })
                        {
                            "photo"
                        } else {
                            "file-description"
                        };
                        let icon_color = if missing {
                            theme.danger
                        } else {
                            theme.foreground
                        };
                        let icon_element = if let Some(image) = entry.icon.clone() {
                            img(image)
                                .size(ui_rem(32.0))
                                .object_fit(ObjectFit::Contain)
                                .into_any_element()
                        } else {
                            icon(icon_name, 24.0, icon_color).into_any_element()
                        };
                        let row_background = if missing {
                            theme.danger
                        } else {
                            theme.background
                        };
                        let row_background_opacity = if missing { 0.12 } else { 0.42 };
                        let row_border = if missing {
                            gpui::ColorExt::opacity(&theme.danger, 0.55)
                        } else {
                            gpui::ColorExt::opacity(&theme.border, 0.55)
                        };
                        let file_name_color = if missing {
                            theme.danger
                        } else {
                            theme.foreground
                        };
                        let status_background = if missing {
                            theme.danger
                        } else {
                            gpui::transparent_black()
                        };
                        let status_border = if missing {
                            gpui::ColorExt::opacity(&theme.danger, 0.65)
                        } else {
                            gpui::ColorExt::opacity(&theme.border, 0.6)
                        };
                        let status_color = if missing {
                            theme.danger_foreground
                        } else {
                            theme.muted_foreground
                        };
                        let file_name = div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(ui_rem(13.0))
                            .text_color(file_name_color)
                            .child(entry.name.clone());
                        let size_label =
                            i18n::file_size(&translator, entry.size, entry.is_directory);
                        let row = div()
                            .id(("preview-file-row", index as u64))
                            .flex_none()
                            .w_full()
                            .min_h(ui_rem(54.0))
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(row_border)
                            .bg(gpui::ColorExt::opacity(
                                &row_background,
                                row_background_opacity,
                            ))
                            .child(
                                div()
                                    .size(ui_rem(32.0))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(icon_element),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .justify_center()
                                    .child(file_name)
                                    .child(
                                        div()
                                            .w_full()
                                            .truncate()
                                            .text_size(ui_rem(11.0))
                                            .text_color(theme.muted_foreground)
                                            .child(entry.path.to_string_lossy().into_owned()),
                                    ),
                            )
                            .child(
                                div()
                                    .w(ui_rem(82.0))
                                    .flex_none()
                                    .flex()
                                    .flex_col()
                                    .items_end()
                                    .gap_1()
                                    .text_size(ui_rem(11.0))
                                    .text_color(theme.muted_foreground)
                                    .child(size_label)
                                    .child(
                                        div()
                                            .max_w(ui_rem(82.0))
                                            .truncate()
                                            .rounded_full()
                                            .border_1()
                                            .border_color(status_border)
                                            .bg(status_background)
                                            .px_1p5()
                                            .text_size(ui_rem(10.0))
                                            .text_color(status_color)
                                            .child(if !missing {
                                                entry.type_label()
                                            } else {
                                                "文件不存在".to_owned()
                                            }),
                                    ),
                            );
                        row.into_any_element()
                    })
                    .collect::<Vec<_>>();
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .max_h(px(max_height))
                    .p_2()
                    .id("preview-file-content")
                    .overflow_y_scroll()
                    .track_scroll(&self.file_scroll)
                    .children(rows)
                    .into_any_element()
            }
            Some(PreviewMode::Html) => {
                let html = preview.html_content.as_deref().unwrap_or_default();
                div()
                    .flex_none()
                    .max_h(px(max_height))
                    .px_2()
                    .py_2()
                    .id("preview-html-content")
                    .overflow_hidden()
                    .child(
                        div()
                            .flex_none()
                            .max_h(px((max_height
                                - TEXT_VERTICAL_PADDING
                                    * crate::ui::scale::window_scale(window))
                            .max(1.0)))
                            .id("preview-html-scroll-content")
                            .overflow_y_scroll()
                            .track_scroll(&self.html_scroll)
                            .child(
                                div().child(html_preview::render_with_images_and_max_lines(
                                    html,
                                    ui_rem(14.0).to_pixels(window.rem_size()),
                                    self.text_line_height,
                                    preview.text.clone(),
                                    &self.html_images,
                                    Some(px((max_width
                                        - 16.0 * crate::ui::scale::window_scale(window))
                                    .max(1.0))),
                                    (max_height / f32::from(self.text_line_height))
                                        .ceil()
                                        .max(1.0) as usize,
                                )),
                            ),
                    )
                    .into_any_element()
            }
            Some(PreviewMode::Text) | None => div()
                .flex_none()
                .max_w(px(max_width))
                .max_h(px(max_height))
                .id("preview-text-content")
                .overflow_hidden()
                .px_2()
                .py_2()
                .child(self.render_text_list(theme.foreground, max_width, max_height, window))
                .into_any_element(),
        }
    }

    fn render_text_list(
        &self,
        text_color: gpui::Hsla,
        max_width: f32,
        max_height: f32,
        window: &Window,
    ) -> impl IntoElement {
        let ui_scale = crate::ui::scale::window_scale(window);
        let text_content = self.text_content.clone();
        let text_line_ranges = self.text_line_ranges.clone();
        let line_height = self.text_line_height;
        list(self.text_list_state.clone(), move |index, _, _| {
            let line = text_line_ranges
                .get(index)
                .and_then(|range| text_content.get(range.clone()))
                .unwrap_or_default()
                .to_owned();
            div()
                .w_full()
                .text_size(ui_rem(14.0))
                .line_height(line_height)
                .text_color(text_color)
                .child(line)
                .into_any_element()
        })
        .with_sizing_behavior(ListSizingBehavior::Infer)
        .max_w(px((max_width - TEXT_HORIZONTAL_PADDING * ui_scale).max(1.0)))
        .max_h(px((max_height
            - TEXT_VERTICAL_PADDING * crate::ui::scale::window_scale(window))
        .max(1.0)))
    }

    fn next_image_base_size(current: f32, limit: f32, delta_y: f32) -> f32 {
        let limit = limit.clamp(
            PersistedPreviewWindowState::IMAGE_BASE_SIZE_MIN,
            PersistedPreviewWindowState::IMAGE_BASE_SIZE_MAX,
        );
        // 从实际可显示的基准开始调整，避免已保存的过大尺寸造成反向缩小无反馈。
        (current.min(limit) + delta_y.signum() * IMAGE_PREVIEW_SIZE_STEP)
            .clamp(PersistedPreviewWindowState::IMAGE_BASE_SIZE_MIN, limit)
    }

    fn image_display_size(
        &self,
        preview: &FormatPreview,
        max_width: f32,
        max_height: f32,
        scale_factor: f32,
        image_base_size: f32,
    ) -> Option<Size<f32>> {
        let (width, height) = self.image_dimensions.or_else(|| {
            preview
                .image_dimensions
                .map(|size| (size.width, size.height))
        })?;
        Self::resolve_image_display_size(
            width,
            height,
            max_width,
            max_height,
            scale_factor,
            image_base_size,
        )
    }

    fn resolve_image_display_size(
        width: u32,
        height: u32,
        max_width: f32,
        max_height: f32,
        scale_factor: f32,
        image_base_size: f32,
    ) -> Option<Size<f32>> {
        let display_scale = Self::resolve_image_display_scale(
            width,
            height,
            max_width,
            max_height,
            scale_factor,
            image_base_size,
        )?;
        let scale_factor = scale_factor.max(1.0);
        let width = width as f32 / scale_factor;
        let height = height as f32 / scale_factor;
        Some(size(width * display_scale, height * display_scale))
    }

    fn resolve_image_display_scale(
        width: u32,
        height: u32,
        max_width: f32,
        max_height: f32,
        scale_factor: f32,
        image_base_size: f32,
    ) -> Option<f32> {
        let scale_factor = scale_factor.max(1.0);
        let width = width as f32 / scale_factor;
        let height = height as f32 / scale_factor;
        if width <= 0.0 || height <= 0.0 || max_width <= 0.0 || max_height <= 0.0 {
            return None;
        }
        let base_width = max_width.min(image_base_size);
        let base_height = max_height.min(image_base_size);
        Some((base_width / width).min(base_height / height))
    }

    fn render_information(
        &self,
        max_width: f32,
        align_end: bool,
        center_align: bool,
        image_display_percentage: Option<f32>,
        window: &Window,
        cx: &App,
    ) -> Option<(AnyElement, Size<f32>)> {
        let theme = crate::window_material::excluded_window_theme(cx);
        let translator = i18n::translator(cx);
        let ui_scale = crate::ui::scale::window_scale(window);
        let max_width = max_width.min(PREVIEW_INFO_MAX_WIDTH * ui_scale).max(1.0);
        let font_size = ui_rem(11.0).to_pixels(window.rem_size());
        let row_height = self
            .font_metrics
            .map(|metrics| metrics.line_height(font_size, px(22.0 * ui_scale)))
            .map(f32::from)
            .unwrap_or(22.0 * ui_scale);
        let gap = 8.0 * ui_scale;
        let chip_padding = 6.0 * ui_scale;
        let information = crate::config_runtime::preview_information_config(cx);
        let mut rows = Vec::new();
        if let Some(preview) = self.current_preview() {
            if self.mode == Some(PreviewMode::Image) {
                let file_image = preview.role() == FormatRole::FileList;
                let byte_length = self.image_byte_length.or_else(|| {
                    if file_image {
                        None
                    } else {
                        image_source_byte_length(preview.image_source.as_ref())
                    }
                });
                let size_label = if file_image {
                    PreviewInformationField::FileSize
                } else {
                    PreviewInformationField::RawSize
                };
                let size = byte_length
                    .map(|bytes| {
                        let formatted = i18n::file_size(&translator, Some(bytes), false);
                        if bytes < 1024 {
                            formatted
                        } else {
                            format!("{formatted} · {bytes} B")
                        }
                    })
                    .unwrap_or_else(|| {
                        if self.loading {
                            translator.translate("main.preview.loading")
                        } else {
                            i18n::file_size(&translator, None, false)
                        }
                    });
                rows.push((size_label, size));
                if let Some((width, height)) = self.image_dimensions {
                    let zoom = image_display_percentage
                        .map(|value| format!(" · {value:.0}%"))
                        .unwrap_or_default();
                    rows.push((
                        PreviewInformationField::Dimensions,
                        format!("{width} × {height} px{zoom}"),
                    ));
                }
                if let Some(path) = &self.image_source_path {
                    rows.push((
                        PreviewInformationField::Name,
                        path.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                    ));
                    rows.push((
                        PreviewInformationField::Path,
                        path.to_string_lossy().into_owned(),
                    ));
                }
            } else if self.mode == Some(PreviewMode::File) {
                rows.push((
                    PreviewInformationField::Files,
                    self.file_entries.len().to_string(),
                ));
                let total_size = self
                    .file_entries
                    .iter()
                    .filter_map(|entry| entry.size)
                    .sum::<u64>();
                rows.push((
                    PreviewInformationField::KnownSize,
                    i18n::file_size(&translator, Some(total_size), false),
                ));
            } else {
                rows.push((
                    PreviewInformationField::Characters,
                    preview.char_count.to_string(),
                ));
                rows.push((
                    PreviewInformationField::Lines,
                    preview.text.split('\n').count().to_string(),
                ));
            }
            let format = self
                .image_source_path
                .as_ref()
                .and_then(|path| path.extension())
                .and_then(|extension| extension.to_str())
                .map(|extension| {
                    format!(
                        "{} · {}",
                        extension.to_ascii_uppercase(),
                        preview.format_name
                    )
                })
                .unwrap_or_else(|| preview.format_name.clone());
            rows.push((PreviewInformationField::Format, format));
        }
        if let Some(item) = &self.item {
            rows.push((
                PreviewInformationField::Source,
                i18n::clipboard_source(
                    &translator,
                    item.source_app_name.as_deref(),
                    item.source_device_name.as_deref(),
                ),
            ));
            if let Some(time) = chrono::DateTime::from_timestamp_millis(item.last_seen_at) {
                rows.push((
                    PreviewInformationField::CapturedAt,
                    time.with_timezone(&chrono::Local)
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string(),
                ));
            }
            rows.push((
                PreviewInformationField::PasteCount,
                item.paste_count.to_string(),
            ));
            let mut states = Vec::new();
            if item.is_pinned {
                states.push(translator.translate("main.preview.info.pinned"));
            }
            if item.favorite_id.is_some() {
                states.push(translator.translate("main.preview.info.favorite"));
            }
            if !item.tag_ids.is_empty() {
                states.push(translator.translate_with_args(
                    "main.preview.info.tags",
                    &[("count", item.tag_ids.len().to_string())],
                ));
            }
            if !states.is_empty() {
                rows.push((PreviewInformationField::Status, states.join(" · ")));
            }
        }
        let modes = self.previews.iter().filter_map(mode_for_preview).fold(
            Vec::new(),
            |mut modes, mode| {
                if !modes.contains(&mode) {
                    modes.push(mode);
                }
                modes
            },
        );
        if modes.len() > 1 {
            rows.push((
                PreviewInformationField::AvailableFormats,
                modes
                    .iter()
                    .map(|mode| mode.label(&translator))
                    .collect::<Vec<_>>()
                    .join(" / "),
            ));
        }
        let mut hints = Vec::new();
        if self.mode == Some(PreviewMode::Image)
            && information.enabled(PreviewInformationField::ZoomHint)
        {
            hints.push(translator.translate("main.preview.zoom_hint"));
        }
        if self.content_scrollable && information.enabled(PreviewInformationField::ScrollHint) {
            hints.push(translator.translate("main.preview.scroll_hint"));
        }
        if modes.len() > 1 && information.enabled(PreviewInformationField::SwitchFormatHint) {
            hints.push(translator.translate("main.preview.switch_format_hint"));
        }
        let mut fields = Vec::new();
        if let Some(mode) = self
            .mode
            .filter(|_| information.enabled(PreviewInformationField::ContentType))
        {
            fields.push((mode.label(&translator), false));
        }
        fields.extend(
            rows.into_iter()
                .filter(|(field, _)| information.enabled(*field))
                .map(|(field, value)| {
                    (
                        format!("{}：{value}", translator.translate(field.label_key())),
                        false,
                    )
                }),
        );
        fields.extend(hints.into_iter().map(|hint| (hint, true)));
        if fields.is_empty() {
            return None;
        }
        let mut style = window.text_style();
        style.font_family = cx.theme().font_family.clone();
        let field_widths = fields
            .iter()
            .map(|(text, _)| {
                let line = window.text_system().shape_line(
                    text.clone().into(),
                    font_size,
                    &[style.to_run(text.len())],
                    None,
                );
                (f32::from(line.width).ceil() + chip_padding * 2.0 + 2.0)
                    .min(420.0 * ui_scale)
                    .min(max_width)
            })
            .collect::<Vec<_>>();
        let natural_width =
            field_widths.iter().sum::<f32>() + gap * field_widths.len().saturating_sub(1) as f32;
        let width = natural_width.min(max_width);
        let field_rows = information_row_ranges(&field_widths, width, gap);
        let height =
            row_height * field_rows.len() as f32 + gap * field_rows.len().saturating_sub(1) as f32;
        let content =
            div()
                .flex()
                .flex_col()
                .w_full()
                .gap(px(gap))
                .children(field_rows.into_iter().map(|range| {
                    div()
                        .h(px(row_height))
                        .flex_none()
                        .flex()
                        .gap(px(gap))
                        .when(center_align, |this| this.justify_center())
                        .when(!center_align && align_end, |this| this.justify_end())
                        .children(range.map(|index| {
                            let (text, hint) = &fields[index];
                            div()
                                .w(px(field_widths[index]))
                                .h(px(row_height))
                                .flex_none()
                                .px(px(chip_padding))
                                .rounded(px(4.0 * ui_scale))
                                .border_1()
                                .border_color(gpui::ColorExt::opacity(&theme.border, 0.75))
                                .bg(theme.background)
                                .shadow(preview_information_shadow())
                                .text_color(if *hint {
                                    theme.muted_foreground
                                } else {
                                    theme.foreground
                                })
                                .truncate()
                                .child(text.clone())
                        }))
                }));
        let strip = div()
            .id("preview-information")
            .w(px(width))
            .h(px(height))
            .flex_none()
            .text_color(theme.foreground)
            .text_size(ui_rem(11.0))
            .line_height(px(row_height))
            .child(content)
            .into_any_element();
        Some((strip, size(width, height)))
    }
}

impl Drop for PreviewWindow {
    fn drop(&mut self) {
        if let Some(hook) = self.foreground_hook.take() {
            crate::platform::window::uninstall_preview_foreground_hook(hook);
        }
    }
}

impl Render for PreviewWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rem_size = crate::ui::scale::preview_rem_size(16.0, cx);
        if window.rem_size() != rem_size {
            self.text_wrap_width = None;
            self.content_size = None;
            self.surface_bounds = None;
        }
        window.set_rem_size(rem_size);
        let font_metrics = AppFontMetrics::from_window(window, cx);
        if self.font_metrics != Some(font_metrics) {
            // 字体变化会影响换行与窗口尺寸，不能沿用旧字体的测量结果。
            self.text_wrap_width = None;
            self.content_size = None;
            self.surface_bounds = None;
            self.font_metrics = Some(font_metrics);
            self.reveal_generation = self.reveal_generation.wrapping_add(1);
        }
        let font_size = ui_rem(14.0).to_pixels(rem_size);
        self.text_line_height =
            font_metrics.line_height(font_size, ui_rem(TEXT_LINE_HEIGHT).to_pixels(rem_size));
        let note_line_height =
            font_metrics.line_height(font_size, ui_rem(20.0).to_pixels(rem_size));
        let Some(anchor) = self.anchor else {
            return div().size_full().into_any_element();
        };
        let Some(display) = work_area_for_physical_point(cx, anchor.center()) else {
            return div().size_full().into_any_element();
        };
        let scale = display.scale_factor.max(1.0);
        let popup = self.popup;
        window.defer(cx, move |window, cx| {
            popup.resize_to_physical_bounds(
                window,
                cx,
                display.bounds.x,
                display.bounds.y,
                display.bounds.width,
                display.bounds.height,
            );
        });
        let work_area = LogicalRect {
            left: 0.0,
            top: 0.0,
            width: display.bounds.width / scale,
            height: display.bounds.height / scale,
        };
        let anchor_rect = LogicalRect {
            left: (anchor.rect.left - display.bounds.x) / scale,
            top: (anchor.rect.top - display.bounds.y) / scale,
            width: anchor.rect.width / scale,
            height: anchor.rect.height / scale,
        };
        let main_rect = self.main_window.map(|rect| LogicalRect {
            left: (rect.left - display.bounds.x) / scale,
            top: (rect.top - display.bounds.y) / scale,
            width: rect.width / scale,
            height: rect.height / scale,
        });
        let connector_source_rect = main_rect
            .map(|main| intersect_logical_rect(anchor_rect, main))
            .unwrap_or_else(|| intersect_logical_rect(anchor_rect, work_area));
        let pointer = point(
            (anchor.pointer().x - display.bounds.x) / scale,
            (anchor.pointer().y - display.bounds.y) / scale,
        );
        let placement_size = (self.mode != Some(PreviewMode::Image))
            .then_some(self.content_size)
            .flatten();
        let placement = choose_preview_placement_for_window(
            pointer,
            work_area,
            main_rect,
            placement_size,
            self.window_position,
        );
        let max_width = resolve_preview_max_width_for_window(
            work_area,
            main_rect,
            pointer,
            placement.side,
            self.window_position,
        );
        let max_height =
            resolve_preview_max_height_for_window(work_area, main_rect, self.window_position);
        let surface_border_width = if self.mode == Some(PreviewMode::Image) {
            0.0
        } else {
            1.0
        };
        let note_text = self
            .item
            .as_ref()
            .map(|item| item.note_text.clone())
            .unwrap_or_default();
        let (note_width, note_height) = if note_text.is_empty() {
            (0.0, 0.0)
        } else {
            let mut text_style = window.text_style();
            text_style.font_family = cx.theme().font_family.clone();
            let runs = [text_style.to_run(note_text.len())];
            let layout = window.text_system().shape_text(
                note_text.clone(),
                ui_rem(14.0).to_pixels(window.rem_size()),
                &runs,
                Some(px((max_width
                    - TEXT_HORIZONTAL_PADDING
                        * crate::ui::scale::window_scale(window)
                    - surface_border_width * 2.0)
                    .max(1.0))),
                None,
            );
            let (width, lines) = layout
                .map(|lines| {
                    (
                        lines
                            .iter()
                            .map(|line| f32::from(line.width()))
                            .fold(0.0, f32::max),
                        lines
                            .iter()
                            .map(|line| line.wrap_boundaries().len() + 1)
                            .sum::<usize>(),
                    )
                })
                .unwrap_or((0.0, 1));
            (
                (width.ceil()
                    + TEXT_HORIZONTAL_PADDING * crate::ui::scale::window_scale(window)
                    + surface_border_width * 2.0)
                    .max(TEXT_MIN_WIDTH)
                    .min(max_width),
                (lines.clamp(1, 6) as f32 * f32::from(note_line_height)
                    + TEXT_VERTICAL_PADDING * crate::ui::scale::window_scale(window))
                .min(max_height * 0.3),
            )
        };
        let content_max_height = (max_height - note_height).max(1.0);
        self.ensure_text_layout(
            max_width - TEXT_HORIZONTAL_PADDING * crate::ui::scale::window_scale(window),
            window,
            cx,
        );
        let image_size = (self.mode == Some(PreviewMode::Image))
            .then(|| self.current_preview())
            .flatten()
            .and_then(|preview| {
                self.image_display_size(
                    preview,
                    max_width,
                    content_max_height,
                    scale,
                    self.image_base_size,
                )
            });
        self.image_base_size_limit = (self.mode == Some(PreviewMode::Image))
            .then(|| self.current_preview())
            .flatten()
            .and_then(|preview| {
                self.image_display_size(
                    preview,
                    max_width,
                    content_max_height,
                    scale,
                    PersistedPreviewWindowState::IMAGE_BASE_SIZE_MAX,
                )
            })
            .map(|size| size.width.max(size.height));
        let image_display_percentage = (self.mode == Some(PreviewMode::Image))
            .then(|| self.current_preview())
            .flatten()
            .and_then(|preview| {
                let (width, height) = self.image_dimensions.or_else(|| {
                    preview
                        .image_dimensions
                        .map(|size| (size.width, size.height))
                })?;
                Self::resolve_image_display_scale(
                    width,
                    height,
                    max_width,
                    content_max_height,
                    scale,
                    self.image_base_size,
                )
                .map(|display_scale| display_scale * 100.0)
            });
        let preview_size = if self.mode == Some(PreviewMode::Image) {
            image_size.map(|size| (size.width.max(note_width), size.height + note_height))
        } else {
            self.content_size
        };
        let visible_preview_size = preview_size.map(|(width, height)| {
            let height = if self.mode == Some(PreviewMode::Image) {
                height
            } else {
                height.min(max_height)
            };
            (width.min(max_width), height)
        });
        let placement = choose_preview_placement_for_window(
            pointer,
            work_area,
            main_rect,
            visible_preview_size,
            self.window_position,
        );
        let connector_target_rect = self.surface_bounds.or_else(|| {
            visible_preview_size.map(|(width, height)| {
                preview_rect_for_placement(placement, width, height, work_area)
            })
        });
        let theme = crate::window_material::excluded_window_theme(cx);
        let measured_mode = self.mode;
        let measured_reveal_generation = self.reveal_generation;
        let preview_entity = cx.entity();
        let preview_entity_for_content = preview_entity.clone();
        let text_list_state = self.text_list_state.clone();
        let html_scroll = self.html_scroll.clone();
        let file_scroll = self.file_scroll.clone();
        let note_scroll = self.note_scroll.clone();
        let content_measure = canvas(
            move |bounds, window, cx| {
                let measured_size = (f32::from(bounds.size.width), f32::from(bounds.size.height));
                let content_scrollable = f32::from(note_scroll.max_offset().y) > 0.5
                    || match measured_mode {
                        Some(PreviewMode::Text) => {
                            f32::from(text_list_state.max_offset_for_scrollbar().y) > 0.5
                        }
                        Some(PreviewMode::Html) => f32::from(html_scroll.max_offset().y) > 0.5,
                        Some(PreviewMode::File) => f32::from(file_scroll.max_offset().y) > 0.5,
                        Some(PreviewMode::Image) | None => false,
                    };
                let preview = preview_entity_for_content.read(cx);
                if preview.mode != measured_mode
                    || preview.reveal_generation != measured_reveal_generation
                    || (preview.content_size == Some(measured_size)
                        && preview.content_scrollable == content_scrollable)
                {
                    return;
                }
                let preview_entity = preview_entity_for_content.clone();
                window.on_next_frame(move |_, cx| {
                    preview_entity.update(cx, |preview, cx| {
                        if preview.mode != measured_mode
                            || preview.reveal_generation != measured_reveal_generation
                        {
                            return;
                        }
                        if preview.content_size != Some(measured_size)
                            || preview.content_scrollable != content_scrollable
                        {
                            if measured_mode != Some(PreviewMode::Image)
                                && preview.content_size.is_none()
                            {
                                preview.reveal_generation =
                                    preview.reveal_generation.wrapping_add(1);
                            }
                            preview.content_size = Some(measured_size);
                            preview.content_scrollable = content_scrollable;
                            cx.notify();
                        }
                    });
                });
            },
            |_bounds, _state, _window, _cx| {},
        )
        .absolute()
        .inset_0()
        .size_full();
        let reveal_size = preview_size;
        let animations_enabled = crate::config_runtime::animation_enabled(
            cx,
            crate::config::AnimationKind::PreviewWindow,
        );
        let reveal_start_size = reveal_size.map(|(width, height)| {
            preview_reveal_start_size(anchor_rect, placement.side, width, height)
        });
        let surface_content = div()
            .flex_none()
            .flex()
            .flex_col()
            .when(note_height > 0.0, |this| {
                this.min_w(px(note_width)).max_w(px(max_width))
            })
            .when_some(reveal_size, |this, (width, height)| {
                this.w(px(width)).h(px(height))
            })
            .when(note_height > 0.0, |this| {
                this.child(
                    div()
                        .id("preview-note")
                        .w_full()
                        .min_w_0()
                        .h(px(note_height))
                        .flex_none()
                        .max_w(px(max_width))
                        .overflow_hidden()
                        .border_b_1()
                        .when_some(reveal_size, |this, (width, _)| {
                            this.max_w(px((width - surface_border_width * 2.0).max(1.0)))
                        })
                        .rounded_t(px(PREVIEW_CORNER_RADIUS - surface_border_width))
                        .border_color(theme.border)
                        .bg(theme.secondary)
                        .child(
                            div()
                                .id("preview-note-scroll")
                                .size_full()
                                .overflow_y_scroll()
                                .track_scroll(&self.note_scroll)
                                .child(
                                    div()
                                        .px_3()
                                        .py_2()
                                        .text_size(ui_rem(14.0))
                                        .line_height(note_line_height)
                                        .text_color(theme.foreground)
                                        .child(note_text),
                                ),
                        ),
                )
            })
            .child(self.render_content(max_width, content_max_height, image_size, window, cx))
            .child(content_measure);
        let surface_bounds_measure = canvas(
            move |bounds, window, cx| {
                let measured_bounds = LogicalRect {
                    left: f32::from(bounds.origin.x),
                    top: f32::from(bounds.origin.y),
                    width: f32::from(bounds.size.width),
                    height: f32::from(bounds.size.height),
                };
                let preview = preview_entity.read(cx);
                if preview.reveal_generation != measured_reveal_generation
                    || preview.surface_bounds == Some(measured_bounds)
                {
                    return;
                }
                let preview_entity = preview_entity.clone();
                window.on_next_frame(move |_, cx| {
                    preview_entity.update(cx, |preview, cx| {
                        if preview.reveal_generation == measured_reveal_generation
                            && preview.surface_bounds != Some(measured_bounds)
                        {
                            preview.surface_bounds = Some(measured_bounds);
                            cx.notify();
                        }
                    });
                });
            },
            |_bounds, _state, _window, _cx| {},
        )
        .absolute()
        .inset_0()
        .size_full();
        let surface = div()
            .flex_none()
            .max_w(px(max_width))
            .when(self.mode != Some(PreviewMode::Image), |this| {
                this.max_h(px(max_height))
            })
            .overflow_hidden()
            .rounded(px(PREVIEW_CORNER_RADIUS))
            .when(self.mode != Some(PreviewMode::Image), |this| {
                this.border_1().border_color(theme.border)
            })
            .bg(theme.background)
            .shadow(preview_container_shadow())
            .child(surface_content);
        let surface = if animations_enabled {
            surface
                .with_animation(
                    ("preview-surface-size", self.reveal_generation),
                    Animation::new(Duration::from_millis(190)).with_easing(gpui::ease_out_quint()),
                    move |this, progress| {
                        let Some((target_width, target_height)) = reveal_size else {
                            return this;
                        };
                        let (start_width, start_height) =
                            reveal_start_size.expect("预览尺寸存在时必须有展开动画起始尺寸");
                        this.w(px(interpolate_reveal_size(
                            start_width,
                            target_width,
                            progress,
                        )))
                        .h(px(interpolate_reveal_size(
                            start_height,
                            target_height,
                            progress,
                        )))
                    },
                )
                .into_any_element()
        } else {
            surface.into_any_element()
        };
        // 在无边框的外层测量，避免把内容边缘当成预览外框边缘。
        let surface = div()
            .relative()
            .flex_none()
            .child(surface)
            .child(surface_bounds_measure)
            .when(self.content_size.is_none(), |this| this.invisible());
        let surface = if animations_enabled {
            surface
                .with_animation(
                    ("preview-surface-opacity", self.generation),
                    Animation::new(Duration::from_millis(190)).with_easing(gpui::ease_out_quint()),
                    |this, progress| this.opacity(progress),
                )
                .into_any_element()
        } else {
            surface.into_any_element()
        };
        let preview = deferred(
            anchored()
                .position(point(px(placement.position.x), px(placement.position.y)))
                .anchor(placement.anchor)
                .snap_to_window()
                .child(surface),
        )
        .priority(1);
        let connector = connector_source_rect
            .zip(connector_target_rect)
            .map(|(source_rect, target_rect)| {
                connector_element(
                    source_rect,
                    target_rect,
                    theme.primary,
                    theme.primary_active,
                    theme.primary_active,
                )
            })
            .unwrap_or_else(|| div().into_any_element());
        let connector = deferred(connector).priority(2);
        let information = self.surface_bounds.and_then(|preview_rect| {
            let ui_scale = crate::ui::scale::window_scale(window);
            let gap = PREVIEW_INFO_GAP * ui_scale;
            // 使用实际视口和预览边界，信息条不再经过 anchored 的自动翻转。
            let viewport = window.viewport_size();
            let viewport = LogicalRect {
                left: 0.0,
                top: 0.0,
                width: f32::from(viewport.width),
                height: f32::from(viewport.height),
            };
            let (align_end, areas) = information_areas(preview_rect, main_rect, viewport, gap);
            for area in areas {
                let width = information_max_width_for_alignment(
                    preview_rect,
                    area,
                    align_end,
                    self.window_position.is_screen_edge(),
                )
                .floor();
                if width < (PREVIEW_INFO_MIN_WIDTH * ui_scale).min(viewport.width) {
                    continue;
                }
                let (strip, strip_size) = self.render_information(
                    width,
                    align_end,
                    self.window_position.is_screen_edge(),
                    image_display_percentage,
                    window,
                    cx,
                )?;
                let Some(rect) = information_rect_with_alignment(
                    preview_rect,
                    area,
                    strip_size,
                    align_end,
                    self.window_position.is_screen_edge(),
                ) else {
                    continue;
                };
                return Some(
                    deferred(
                        div()
                            .absolute()
                            .left(px(rect.left))
                            .top(px(rect.top))
                            .w(px(rect.width))
                            .h(px(rect.height))
                            .child(strip),
                    )
                    .priority(3),
                );
            }
            None
        });
        div()
            .font_family(cx.theme().font_family.clone())
            .line_height(gpui::relative(
                f32::from(self.text_line_height) / f32::from(font_size),
            ))
            .relative()
            .size_full()
            .when(self.visible, |this| {
                this.child(preview)
                    .child(connector)
                    .when_some(information, |this, information| this.child(information))
            })
            .into_any_element()
    }
}

#[derive(Clone, Copy, Debug)]
struct ConnectorAnchor {
    point: Point<f32>,
    normal: Point<f32>,
}

#[derive(Clone, Copy, Debug)]
struct ConnectorPath {
    source: Point<f32>,
    target: Point<f32>,
    control_1: Point<f32>,
    control_2: Point<f32>,
}

fn connector_element(
    source_rect: LogicalRect,
    target_rect: LogicalRect,
    line_color: gpui::Hsla,
    source_color: gpui::Hsla,
    target_color: gpui::Hsla,
) -> AnyElement {
    let Some(path) = resolve_best_connector(source_rect, target_rect) else {
        return div().into_any_element();
    };
    let line_color = line_color;
    let source_glow = gpui::ColorExt::opacity(&source_color, 0.3);
    let target_glow = gpui::ColorExt::opacity(&target_color, 0.3);
    let source = path.source;
    let target = path.target;
    let source_dot = div()
        .absolute()
        .left(px(source.x - 6.0))
        .top(px(source.y - 6.0))
        .size(px(12.0))
        .rounded_full()
        .bg(source_glow)
        .child(
            div()
                .absolute()
                .inset(px(2.0))
                .rounded_full()
                .bg(source_color),
        );
    let target_dot = div()
        .absolute()
        .left(px(target.x - 6.0))
        .top(px(target.y - 6.0))
        .size(px(12.0))
        .rounded_full()
        .bg(target_glow)
        .child(
            div()
                .absolute()
                .inset(px(2.0))
                .rounded_full()
                .bg(target_color),
        );

    div()
        .absolute()
        .inset_0()
        .child(
            canvas(
                |_bounds, _window, _cx| (),
                move |bounds, (), window, _cx| {
                    let origin = bounds.origin;
                    let mut builder = gpui::PathBuilder::stroke(px(2.0));
                    builder.move_to(point(
                        origin.x + px(path.source.x),
                        origin.y + px(path.source.y),
                    ));
                    builder.cubic_bezier_to(
                        point(origin.x + px(path.target.x), origin.y + px(path.target.y)),
                        point(
                            origin.x + px(path.control_1.x),
                            origin.y + px(path.control_1.y),
                        ),
                        point(
                            origin.x + px(path.control_2.x),
                            origin.y + px(path.control_2.y),
                        ),
                    );
                    if let Ok(path) = builder.build() {
                        window.paint_path(path, line_color);
                    }
                },
            )
            .absolute()
            .inset_0(),
        )
        .child(source_dot)
        .child(target_dot)
        .into_any_element()
}

fn rect_connection_anchors(rect: LogicalRect) -> [ConnectorAnchor; 4] {
    [
        ConnectorAnchor {
            point: point(rect.left, rect.top + rect.height / 2.0),
            normal: point(-1.0, 0.0),
        },
        ConnectorAnchor {
            point: point(rect.right(), rect.top + rect.height / 2.0),
            normal: point(1.0, 0.0),
        },
        ConnectorAnchor {
            point: point(rect.left + rect.width / 2.0, rect.top),
            normal: point(0.0, -1.0),
        },
        ConnectorAnchor {
            point: point(rect.left + rect.width / 2.0, rect.bottom()),
            normal: point(0.0, 1.0),
        },
    ]
}

fn segment_overlaps_rect_interior(start: Point<f32>, end: Point<f32>, rect: LogicalRect) -> bool {
    let margin = 6.0;
    let inner_left = rect.left + margin;
    let inner_top = rect.top + margin;
    let inner_right = rect.right() - margin;
    let inner_bottom = rect.bottom() - margin;
    if inner_left >= inner_right || inner_top >= inner_bottom {
        return false;
    }

    for step in 1..24 {
        let progress = step as f32 / 24.0;
        let point = point(
            start.x + (end.x - start.x) * progress,
            start.y + (end.y - start.y) * progress,
        );
        if point.x > inner_left
            && point.x < inner_right
            && point.y > inner_top
            && point.y < inner_bottom
        {
            return true;
        }
    }
    false
}

fn resolve_best_connector(
    source_rect: LogicalRect,
    target_rect: LogicalRect,
) -> Option<ConnectorPath> {
    let mut best: Option<(f32, ConnectorPath)> = None;
    for source in rect_connection_anchors(source_rect) {
        for target in rect_connection_anchors(target_rect) {
            let delta_x = target.point.x - source.point.x;
            let delta_y = target.point.y - source.point.y;
            let distance = delta_x.hypot(delta_y);
            let crosses_target =
                segment_overlaps_rect_interior(source.point, target.point, target_rect);
            let source_outward = delta_x * source.normal.x + delta_y * source.normal.y >= 0.0;
            let target_outward = -delta_x * target.normal.x - delta_y * target.normal.y >= 0.0;
            let control = (distance * 0.22).clamp(20.0, 64.0);
            let path = ConnectorPath {
                source: source.point,
                target: target.point,
                control_1: point(
                    source.point.x + source.normal.x * control,
                    source.point.y + source.normal.y * control,
                ),
                control_2: point(
                    target.point.x + target.normal.x * control,
                    target.point.y + target.normal.y * control,
                ),
            };
            let score = (crosses_target as u32 * 100_000) as f32
                + if source_outward { 0.0 } else { 40.0 }
                + if target_outward { 0.0 } else { 40.0 }
                + distance;
            if best.is_none_or(|(best_score, _)| score < best_score) {
                best = Some((score, path));
            }
        }
    }
    best.map(|(_, path)| path)
}

fn intersect_logical_rect(first: LogicalRect, second: LogicalRect) -> Option<LogicalRect> {
    let left = first.left.max(second.left);
    let top = first.top.max(second.top);
    let right = first.right().min(second.right());
    let bottom = first.bottom().min(second.bottom());
    (right > left && bottom > top).then_some(LogicalRect {
        left,
        top,
        width: right - left,
        height: bottom - top,
    })
}

fn preview_rect_for_placement(
    placement: PreviewPlacement,
    width: f32,
    height: f32,
    work_area: LogicalRect,
) -> LogicalRect {
    let width = width.max(1.0);
    let height = height.max(1.0);
    let mut rect = preview_rect_from_anchor(placement, width, height);
    let max_left = (work_area.right() - width).max(work_area.left);
    let max_top = (work_area.bottom() - height).max(work_area.top);
    rect.left = rect.left.clamp(work_area.left, max_left);
    rect.top = rect.top.clamp(work_area.top, max_top);
    rect
}

fn preview_rect_from_anchor(placement: PreviewPlacement, width: f32, height: f32) -> LogicalRect {
    let (left, top) = match placement.anchor {
        Anchor::TopLeft => (placement.position.x, placement.position.y),
        Anchor::TopRight => (placement.position.x - width, placement.position.y),
        Anchor::BottomLeft => (placement.position.x, placement.position.y - height),
        Anchor::BottomRight => (placement.position.x - width, placement.position.y - height),
        Anchor::LeftCenter => (placement.position.x, placement.position.y - height / 2.0),
        Anchor::RightCenter => (
            placement.position.x - width,
            placement.position.y - height / 2.0,
        ),
        Anchor::TopCenter => (placement.position.x - width / 2.0, placement.position.y),
        Anchor::BottomCenter => (
            placement.position.x - width / 2.0,
            placement.position.y - height,
        ),
    };
    LogicalRect {
        left,
        top,
        width,
        height,
    }
}

fn preview_reveal_start_size(
    anchor: LogicalRect,
    side: PreviewPlacementSide,
    target_width: f32,
    target_height: f32,
) -> (f32, f32) {
    match side {
        PreviewPlacementSide::Left | PreviewPlacementSide::Right => {
            (0.0, anchor.height.max(1.0).min(target_height.max(1.0)))
        }
        PreviewPlacementSide::Top | PreviewPlacementSide::Bottom => {
            (anchor.width.max(1.0).min(target_width.max(1.0)), 0.0)
        }
        PreviewPlacementSide::Overlay
        | PreviewPlacementSide::PointerLeft
        | PreviewPlacementSide::PointerRight => (0.0, 0.0),
    }
}

fn interpolate_reveal_size(start: f32, target: f32, progress: f32) -> f32 {
    start + (target - start) * progress
}

fn image_source_byte_length(source: Option<&ImagePreviewSource>) -> Option<u64> {
    match source? {
        ImagePreviewSource::Inline { bytes } => Some(bytes.len() as u64),
        ImagePreviewSource::External { byte_length, .. } => Some(*byte_length),
    }
}

fn information_row_ranges(widths: &[f32], available: f32, gap: f32) -> Vec<Range<usize>> {
    let available = available.max(1.0);
    let mut rows = Vec::new();
    let mut start = 0;
    let mut used = 0.0;
    for (index, width) in widths.iter().enumerate() {
        let width = width.min(available);
        let next = if index == start {
            width
        } else {
            used + gap + width
        };
        if index > start && next > available + 0.01 {
            rows.push(start..index);
            start = index;
            used = width;
        } else {
            used = next;
        }
    }
    if start < widths.len() {
        rows.push(start..widths.len());
    }
    rows
}

fn information_areas(
    preview: LogicalRect,
    main: Option<LogicalRect>,
    viewport: LogicalRect,
    gap: f32,
) -> (bool, Vec<LogicalRect>) {
    let align_end = main.is_some_and(|main| preview.right() <= main.left);
    let outside = |rect: LogicalRect| {
        [
            LogicalRect {
                left: rect.right() + gap,
                width: viewport.right() - rect.right() - gap,
                ..viewport
            },
            LogicalRect {
                width: rect.left - gap - viewport.left,
                ..viewport
            },
            LogicalRect {
                top: rect.bottom() + gap,
                height: viewport.bottom() - rect.bottom() - gap,
                ..viewport
            },
            LogicalRect {
                height: rect.top - gap - viewport.top,
                ..viewport
            },
        ]
    };
    let safe_areas = if let Some(main) = main {
        let mut areas = outside(main);
        let preferred = if preview.left >= main.right() {
            0
        } else if preview.right() <= main.left {
            1
        } else if preview.top >= main.bottom() {
            2
        } else {
            3
        };
        areas.swap(0, preferred);
        areas
            .into_iter()
            .filter_map(|area| intersect_logical_rect(area, viewport))
            .collect::<Vec<_>>()
    } else {
        vec![viewport]
    };
    let [right, left, bottom, top] = outside(preview);
    let sides = if align_end {
        [left, right]
    } else {
        [right, left]
    };
    let mut areas = Vec::new();
    // 先尝试上下长条，再向侧面翻转；所有主窗口外的空位都放不下时才叠加预览。
    for safe in &safe_areas {
        for free in [top, bottom, sides[0], sides[1]] {
            if let Some(area) = intersect_logical_rect(*safe, free) {
                areas.push(area);
            }
        }
    }
    areas.extend(safe_areas);
    (align_end, areas)
}

fn information_max_width(preview: LogicalRect, area: LogicalRect, align_end: bool) -> f32 {
    let edge = if align_end {
        preview.right()
    } else {
        preview.left
    };
    if edge >= area.left && edge <= area.right() {
        if align_end {
            edge - area.left
        } else {
            area.right() - edge
        }
    } else {
        area.width
    }
}

fn information_max_width_for_alignment(
    preview: LogicalRect,
    area: LogicalRect,
    align_end: bool,
    center_align: bool,
) -> f32 {
    if center_align {
        area.width
    } else {
        information_max_width(preview, area, align_end)
    }
}

fn information_rect(
    preview: LogicalRect,
    area: LogicalRect,
    strip_size: Size<f32>,
    align_end: bool,
) -> Option<LogicalRect> {
    if strip_size.width > information_max_width(preview, area, align_end)
        || strip_size.height > area.height
    {
        return None;
    }
    let left = if align_end {
        preview.right() - strip_size.width
    } else {
        preview.left
    };
    Some(LogicalRect {
        left: left.clamp(area.left, area.right() - strip_size.width),
        top: preview
            .top
            .clamp(area.top, area.bottom() - strip_size.height),
        width: strip_size.width,
        height: strip_size.height,
    })
}

fn information_rect_with_alignment(
    preview: LogicalRect,
    area: LogicalRect,
    strip_size: Size<f32>,
    align_end: bool,
    center_align: bool,
) -> Option<LogicalRect> {
    if !center_align {
        return information_rect(preview, area, strip_size, align_end);
    }
    if strip_size.width > area.width || strip_size.height > area.height {
        return None;
    }
    Some(LogicalRect {
        left: (preview.left + (preview.width - strip_size.width) / 2.0)
            .clamp(area.left, area.right() - strip_size.width),
        top: preview
            .top
            .clamp(area.top, area.bottom() - strip_size.height),
        width: strip_size.width,
        height: strip_size.height,
    })
}

fn choose_preview_placement(
    pointer: Point<f32>,
    work_area: LogicalRect,
    main_window: Option<LogicalRect>,
    preview_size: Option<(f32, f32)>,
) -> PreviewPlacement {
    let (preview_width, preview_height) = preview_size
        .map(|(width, height)| (width.max(TEXT_MIN_WIDTH), height.max(TEXT_MIN_HEIGHT)))
        .unwrap_or((TEXT_MIN_WIDTH, TEXT_MIN_HEIGHT));

    if let Some(main) = main_window {
        let right_space = work_area.right() - main.right() - PREVIEW_MAIN_GAP;
        let left_space = main.left - work_area.left - PREVIEW_MAIN_GAP;
        let horizontal_width = preview_width.max(HORIZONTAL_PREVIEW_MIN_WIDTH);
        let can_place_right = right_space >= horizontal_width;
        let can_place_left = left_space >= horizontal_width;
        if can_place_right || can_place_left {
            let place_right = can_place_right && (!can_place_left || right_space >= left_space);
            let position_y =
                clamp_preview_center(pointer.y, work_area.top, work_area.bottom(), preview_height);
            if place_right {
                return PreviewPlacement {
                    position: point(main.right() + PREVIEW_MAIN_GAP, position_y),
                    anchor: Anchor::LeftCenter,
                    side: PreviewPlacementSide::Right,
                };
            }
            return PreviewPlacement {
                position: point(main.left - PREVIEW_MAIN_GAP, position_y),
                anchor: Anchor::RightCenter,
                side: PreviewPlacementSide::Left,
            };
        }

        let bottom_space = work_area.bottom() - main.bottom() - PREVIEW_MAIN_GAP;
        let top_space = main.top - work_area.top - PREVIEW_MAIN_GAP;
        let can_place_bottom = bottom_space >= preview_height;
        let can_place_top = top_space >= preview_height;
        if can_place_bottom || can_place_top {
            let place_bottom = can_place_bottom && (!can_place_top || bottom_space >= top_space);
            let position_x =
                clamp_preview_center(pointer.x, work_area.left, work_area.right(), preview_width);
            if place_bottom {
                return PreviewPlacement {
                    position: point(position_x, main.bottom() + PREVIEW_MAIN_GAP),
                    anchor: Anchor::TopCenter,
                    side: PreviewPlacementSide::Bottom,
                };
            }
            return PreviewPlacement {
                position: point(position_x, main.top - PREVIEW_MAIN_GAP),
                anchor: Anchor::BottomCenter,
                side: PreviewPlacementSide::Top,
            };
        }

        return PreviewPlacement {
            position: point(
                main.left + main.width / 2.0,
                main.top + main.height / 2.0 - preview_height / 2.0,
            ),
            anchor: Anchor::TopCenter,
            side: PreviewPlacementSide::Overlay,
        };
    }

    choose_pointer_placement(pointer, work_area, preview_width, preview_height)
}

fn choose_preview_placement_for_window(
    pointer: Point<f32>,
    work_area: LogicalRect,
    main_window: Option<LogicalRect>,
    preview_size: Option<(f32, f32)>,
    window_position: ClipboardWindowPosition,
) -> PreviewPlacement {
    if let Some((free_area, side)) = screen_edge_free_area(work_area, main_window, window_position)
    {
        let (width, height) = preview_size
            .map(|(width, height)| (width.max(TEXT_MIN_WIDTH), height.max(TEXT_MIN_HEIGHT)))
            .unwrap_or((TEXT_MIN_WIDTH, TEXT_MIN_HEIGHT));
        let width = width.min((free_area.width - 24.0).max(1.0));
        let height = height.min((free_area.height - 24.0).max(1.0));
        return PreviewPlacement {
            position: point(
                free_area.left + (free_area.width - width) / 2.0,
                free_area.top + (free_area.height - height) / 2.0,
            ),
            anchor: Anchor::TopLeft,
            side,
        };
    }

    choose_preview_placement(pointer, work_area, main_window, preview_size)
}

fn screen_edge_free_area(
    work_area: LogicalRect,
    main_window: Option<LogicalRect>,
    window_position: ClipboardWindowPosition,
) -> Option<(LogicalRect, PreviewPlacementSide)> {
    let main = main_window?;
    match window_position {
        ClipboardWindowPosition::ScreenLeftCenter => {
            let left = main.right() + PREVIEW_MAIN_GAP;
            (left < work_area.right()).then_some((
                LogicalRect {
                    left,
                    width: work_area.right() - left,
                    ..work_area
                },
                PreviewPlacementSide::Right,
            ))
        }
        ClipboardWindowPosition::ScreenRightCenter => {
            let right = main.left - PREVIEW_MAIN_GAP;
            (right > work_area.left).then_some((
                LogicalRect {
                    width: right - work_area.left,
                    ..work_area
                },
                PreviewPlacementSide::Left,
            ))
        }
        ClipboardWindowPosition::ScreenTopCenter => {
            let top = main.bottom() + PREVIEW_MAIN_GAP;
            (top < work_area.bottom()).then_some((
                LogicalRect {
                    top,
                    height: work_area.bottom() - top,
                    ..work_area
                },
                PreviewPlacementSide::Bottom,
            ))
        }
        ClipboardWindowPosition::ScreenBottomCenter => {
            let bottom = main.top - PREVIEW_MAIN_GAP;
            (bottom > work_area.top).then_some((
                LogicalRect {
                    height: bottom - work_area.top,
                    ..work_area
                },
                PreviewPlacementSide::Top,
            ))
        }
        _ => None,
    }
}

fn choose_pointer_placement(
    pointer: Point<f32>,
    work_area: LogicalRect,
    preview_width: f32,
    preview_height: f32,
) -> PreviewPlacement {
    let candidates = [
        (
            point(pointer.x + PREVIEW_OFFSET, pointer.y + PREVIEW_OFFSET),
            Anchor::TopLeft,
            PreviewPlacementSide::PointerRight,
        ),
        (
            point(pointer.x - PREVIEW_OFFSET, pointer.y + PREVIEW_OFFSET),
            Anchor::TopRight,
            PreviewPlacementSide::PointerLeft,
        ),
        (
            point(pointer.x - PREVIEW_OFFSET, pointer.y - PREVIEW_OFFSET),
            Anchor::BottomRight,
            PreviewPlacementSide::PointerLeft,
        ),
        (
            point(pointer.x + PREVIEW_OFFSET, pointer.y - PREVIEW_OFFSET),
            Anchor::BottomLeft,
            PreviewPlacementSide::PointerRight,
        ),
    ];

    let first = candidates[0];
    candidates
        .into_iter()
        .map(|(position, anchor, side)| PreviewPlacement {
            position,
            anchor,
            side,
        })
        .find(|placement| {
            let rect = preview_rect_from_anchor(*placement, preview_width, preview_height);
            rect.left >= work_area.left
                && rect.top >= work_area.top
                && rect.right() <= work_area.right()
                && rect.bottom() <= work_area.bottom()
        })
        .unwrap_or(PreviewPlacement {
            position: first.0,
            anchor: first.1,
            side: first.2,
        })
}

fn clamp_preview_center(value: f32, start: f32, end: f32, size: f32) -> f32 {
    let half_size = size / 2.0;
    let min_value = start + half_size;
    let max_value = end - half_size;
    if min_value <= max_value {
        value.clamp(min_value, max_value)
    } else {
        (start + end) / 2.0
    }
}

fn resolve_preview_max_width(
    work_area: LogicalRect,
    main_window: Option<LogicalRect>,
    pointer: Point<f32>,
    side: PreviewPlacementSide,
) -> f32 {
    let work_area_max = (work_area.width - 24.0).max(1.0);
    let available = match side {
        PreviewPlacementSide::Right => main_window
            .map(|main| work_area.right() - main.right() - PREVIEW_MAIN_GAP)
            .unwrap_or(work_area_max),
        PreviewPlacementSide::Left => main_window
            .map(|main| main.left - work_area.left - PREVIEW_MAIN_GAP)
            .unwrap_or(work_area_max),
        PreviewPlacementSide::Bottom
        | PreviewPlacementSide::Top
        | PreviewPlacementSide::Overlay => work_area_max,
        PreviewPlacementSide::PointerRight => work_area.right() - pointer.x - PREVIEW_OFFSET,
        PreviewPlacementSide::PointerLeft => pointer.x - work_area.left - PREVIEW_OFFSET,
    };
    if available > 0.0 {
        available.min(work_area_max)
    } else {
        work_area_max
    }
}

fn resolve_preview_max_width_for_window(
    work_area: LogicalRect,
    main_window: Option<LogicalRect>,
    pointer: Point<f32>,
    side: PreviewPlacementSide,
    window_position: ClipboardWindowPosition,
) -> f32 {
    if let Some((free_area, _)) = screen_edge_free_area(work_area, main_window, window_position) {
        return (free_area.width - 24.0).max(1.0);
    }
    resolve_preview_max_width(work_area, main_window, pointer, side)
}

fn resolve_preview_max_height(work_area_height: f32) -> f32 {
    (work_area_height * PREVIEW_MAX_HEIGHT_RATIO).max(1.0)
}

fn resolve_preview_max_height_for_window(
    work_area: LogicalRect,
    main_window: Option<LogicalRect>,
    window_position: ClipboardWindowPosition,
) -> f32 {
    let work_area_max = resolve_preview_max_height(work_area.height);
    if let Some((free_area, _)) = screen_edge_free_area(work_area, main_window, window_position) {
        return work_area_max.min((free_area.height - 24.0).max(1.0));
    }
    work_area_max
}

fn text_line_ranges(text: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut line_start = 0;

    for (index, byte) in text.bytes().enumerate() {
        if byte != b'\n' {
            continue;
        }
        let line_end = if index > line_start && text.as_bytes()[index - 1] == b'\r' {
            index - 1
        } else {
            index
        };
        ranges.push(line_start..line_end);
        line_start = index + 1;
    }

    if line_start <= text.len() {
        ranges.push(line_start..text.len());
    }
    ranges
}

fn wrapped_text_line_ranges(
    text: &str,
    wrap_width: Pixels,
    font: gpui::Font,
    window: &Window,
) -> Vec<Range<usize>> {
    let mut line_wrapper = window
        .text_system()
        .line_wrapper(font, ui_rem(14.0).to_pixels(window.rem_size()));
    let mut ranges = Vec::new();

    for line_range in text_line_ranges(text) {
        let line = &text[line_range.clone()];
        let mut start = 0;
        for boundary in line_wrapper.wrap_line(&[LineFragment::text(line)], wrap_width) {
            if boundary.ix > start {
                ranges.push((line_range.start + start)..(line_range.start + boundary.ix));
                start = boundary.ix;
            }
        }
        ranges.push((line_range.start + start)..line_range.end);
    }

    ranges
}

fn mode_for_preview(preview: &FormatPreview) -> Option<PreviewMode> {
    match preview.role() {
        FormatRole::PlainText | FormatRole::Rtf => Some(PreviewMode::Text),
        FormatRole::Html => Some(PreviewMode::Html),
        FormatRole::Image if preview.image_source.is_some() => Some(PreviewMode::Image),
        FormatRole::FileList | FormatRole::VirtualFile if preview.image_source.is_some() => {
            if preview.content_kind == ContentKind::Image {
                Some(PreviewMode::Image)
            } else {
                Some(PreviewMode::File)
            }
        }
        _ => None,
    }
}

fn preview_type_for_preview(preview: &FormatPreview) -> Option<ClipboardPreviewType> {
    match mode_for_preview(preview) {
        Some(PreviewMode::Text) => Some(ClipboardPreviewType::Text),
        Some(PreviewMode::Html) => Some(ClipboardPreviewType::Html),
        Some(PreviewMode::Image) => Some(ClipboardPreviewType::Image),
        Some(PreviewMode::File) => Some(ClipboardPreviewType::File),
        None => None,
    }
}

fn display_priority_roles(priority: ClipboardDisplayPriority) -> [FormatRole; 3] {
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

fn select_preview_options(
    previews: &[FormatPreview],
    preferred_roles: &[FormatRole],
    preview_types: &[ClipboardPreviewType],
) -> Vec<FormatPreview> {
    select_user_visible_previews_with_priority_and_filter(previews, preferred_roles, |preview| {
        preview_type_for_preview(preview).is_some_and(|kind| preview_types.contains(&kind))
    })
}

trait ScrollWheelDelta {
    fn delta_y(&self, line_height: Pixels) -> f32;
}

impl ScrollWheelDelta for ScrollWheelEvent {
    fn delta_y(&self, line_height: Pixels) -> f32 {
        f32::from(crate::ui::smooth_scroll::vertical_wheel_delta(
            self.delta.pixel_delta(line_height),
            self.modifiers.shift,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ClipboardWindowPosition, LogicalRect, PreviewPlacementSide, PreviewWindow,
        choose_preview_placement, choose_preview_placement_for_window, image_source_byte_length,
        information_areas, information_max_width, information_max_width_for_alignment,
        information_rect, information_rect_with_alignment, information_row_ranges,
        interpolate_reveal_size, intersect_logical_rect, preview_reveal_start_size,
        preview_type_for_preview, resolve_best_connector, resolve_preview_max_height,
        resolve_preview_max_height_for_window, resolve_preview_max_width,
        resolve_preview_max_width_for_window, select_preview_options, text_line_ranges,
    };
    use crate::config::ClipboardPreviewType;
    use crate::domain::capture::{FormatKind, FormatRole};
    use crate::domain::clipboard::{ContentKind, FormatPreview, ImagePreviewSource};
    use gpui::{Anchor, point};

    fn file_preview(content_kind: ContentKind) -> FormatPreview {
        FormatPreview {
            format_namespace: "windows".to_owned(),
            format_key: "CF_HDROP".to_owned(),
            format_name: "CF_HDROP".to_owned(),
            format_order: 0,
            format_kind: FormatKind::FileList,
            text: String::new(),
            char_count: 0,
            content_kind,
            html_content: None,
            image_source: Some(ImagePreviewSource::External {
                sha256: [0; 32],
                byte_length: 1,
                relative_path: "preview.bin".to_owned(),
            }),
            image_dimensions: None,
            is_restorable: true,
        }
    }

    #[test]
    fn image_file_uses_image_preview_type() {
        assert_eq!(
            preview_type_for_preview(&file_preview(ContentKind::Image)),
            Some(ClipboardPreviewType::Image)
        );
        assert_eq!(
            preview_type_for_preview(&file_preview(ContentKind::FileList)),
            Some(ClipboardPreviewType::File)
        );
        assert_eq!(
            FormatRole::FileList,
            file_preview(ContentKind::Image).role()
        );
        let image_file = file_preview(ContentKind::Image);
        assert_eq!(
            select_preview_options(
                &[image_file.clone()],
                &[FormatRole::Image],
                &[ClipboardPreviewType::Image],
            )
            .len(),
            1
        );
        assert!(
            select_preview_options(
                &[image_file],
                &[FormatRole::Image],
                &[ClipboardPreviewType::File],
            )
            .is_empty()
        );
    }

    #[test]
    fn preview_prefers_the_larger_side_of_the_main_window() {
        let work = LogicalRect {
            left: 0.0,
            top: 0.0,
            width: 1920.0,
            height: 1080.0,
        };
        let main = LogicalRect {
            left: 700.0,
            top: 200.0,
            width: 400.0,
            height: 600.0,
        };
        let placement = choose_preview_placement(point(900.0, 400.0), work, Some(main), None);
        assert_eq!(placement.side, PreviewPlacementSide::Right);
        assert_eq!(
            resolve_preview_max_width(work, Some(main), point(900.0, 400.0), placement.side),
            802.0
        );
    }

    #[test]
    fn edge_docked_preview_is_centered_in_the_free_area() {
        let work = LogicalRect {
            left: 0.0,
            top: 0.0,
            width: 1920.0,
            height: 1080.0,
        };
        let cases = [
            (
                ClipboardWindowPosition::ScreenLeftCenter,
                LogicalRect {
                    left: 0.0,
                    top: 0.0,
                    width: 320.0,
                    height: 1080.0,
                },
                PreviewPlacementSide::Right,
                point(979.0, 440.0),
            ),
            (
                ClipboardWindowPosition::ScreenRightCenter,
                LogicalRect {
                    left: 1600.0,
                    top: 0.0,
                    width: 320.0,
                    height: 1080.0,
                },
                PreviewPlacementSide::Left,
                point(641.0, 440.0),
            ),
            (
                ClipboardWindowPosition::ScreenTopCenter,
                LogicalRect {
                    left: 0.0,
                    top: 0.0,
                    width: 1920.0,
                    height: 220.0,
                },
                PreviewPlacementSide::Bottom,
                point(810.0, 559.0),
            ),
            (
                ClipboardWindowPosition::ScreenBottomCenter,
                LogicalRect {
                    left: 0.0,
                    top: 800.0,
                    width: 1920.0,
                    height: 280.0,
                },
                PreviewPlacementSide::Top,
                point(810.0, 291.0),
            ),
        ];

        for (window_position, main, expected_side, expected_position) in cases {
            let first = choose_preview_placement_for_window(
                point(80.0, 60.0),
                work,
                Some(main),
                Some((300.0, 200.0)),
                window_position,
            );
            let second = choose_preview_placement_for_window(
                point(1800.0, 1000.0),
                work,
                Some(main),
                Some((300.0, 200.0)),
                window_position,
            );

            assert_eq!(first.side, expected_side);
            assert_eq!(first.position, expected_position);
            assert_eq!(second.position, expected_position);
        }
    }

    #[test]
    fn edge_docked_preview_height_is_limited_to_the_free_area() {
        let work = LogicalRect {
            left: 0.0,
            top: 0.0,
            width: 1920.0,
            height: 1080.0,
        };
        let main = LogicalRect {
            left: 0.0,
            top: 0.0,
            width: 1920.0,
            height: 400.0,
        };

        assert_eq!(
            resolve_preview_max_height_for_window(
                work,
                Some(main),
                ClipboardWindowPosition::ScreenTopCenter,
            ),
            638.0
        );
    }

    #[test]
    fn edge_docked_preview_width_is_limited_to_the_free_area() {
        let work = LogicalRect {
            left: 0.0,
            top: 0.0,
            width: 1920.0,
            height: 1080.0,
        };
        let main = LogicalRect {
            left: 0.0,
            top: 0.0,
            width: 1700.0,
            height: 1080.0,
        };

        assert_eq!(
            resolve_preview_max_width_for_window(
                work,
                Some(main),
                point(500.0, 500.0),
                PreviewPlacementSide::Right,
                ClipboardWindowPosition::ScreenLeftCenter,
            ),
            178.0
        );
    }

    #[test]
    fn edge_docked_information_is_centered_with_the_preview() {
        let preview = LogicalRect {
            left: 500.0,
            top: 300.0,
            width: 400.0,
            height: 240.0,
        };
        let area = LogicalRect {
            left: 320.0,
            top: 0.0,
            width: 1280.0,
            height: 292.0,
        };
        let strip_size = gpui::size(320.0, 80.0);
        let width = information_max_width_for_alignment(preview, area, false, true);
        let strip = information_rect_with_alignment(preview, area, strip_size, false, true)
            .expect("贴边预览的提示信息应放在预览外侧并居中");

        assert_eq!(width, area.width);
        assert_eq!(
            strip.left + strip.width / 2.0,
            preview.left + preview.width / 2.0
        );
        assert_eq!(strip.bottom(), preview.top - 8.0);
    }

    #[test]
    fn preview_max_height_is_eighty_percent_of_work_area() {
        assert_eq!(resolve_preview_max_height(1080.0), 864.0);
    }

    #[test]
    fn preview_reveal_grows_outward_from_each_main_window_side() {
        let anchor = LogicalRect {
            left: 100.0,
            top: 200.0,
            width: 320.0,
            height: 48.0,
        };
        let target = (480.0, 240.0);

        assert_eq!(
            preview_reveal_start_size(anchor, PreviewPlacementSide::Left, target.0, target.1),
            (0.0, 48.0)
        );
        assert_eq!(
            preview_reveal_start_size(anchor, PreviewPlacementSide::Right, target.0, target.1),
            (0.0, 48.0)
        );
        assert_eq!(
            preview_reveal_start_size(anchor, PreviewPlacementSide::Top, target.0, target.1),
            (320.0, 0.0)
        );
        assert_eq!(
            preview_reveal_start_size(anchor, PreviewPlacementSide::Bottom, target.0, target.1),
            (320.0, 0.0)
        );
    }

    #[test]
    fn preview_reveal_interpolates_to_the_target_size() {
        assert_eq!(interpolate_reveal_size(0.0, 480.0, 0.5), 240.0);
        assert_eq!(interpolate_reveal_size(48.0, 240.0, 0.5), 144.0);
    }

    #[test]
    fn text_line_ranges_preserve_empty_lines_and_normalize_crlf() {
        let text = "第一行\r\n\r\n第三行\n";
        let lines = text_line_ranges(text)
            .into_iter()
            .map(|range| &text[range])
            .collect::<Vec<_>>();

        assert_eq!(lines, vec!["第一行", "", "第三行", ""]);
    }

    #[test]
    fn text_line_ranges_return_one_empty_line_for_empty_text() {
        assert_eq!(text_line_ranges(""), vec![0..0]);
    }

    #[test]
    fn preview_position_flips_left_when_right_side_is_too_small() {
        let work = LogicalRect {
            left: 0.0,
            top: 0.0,
            width: 1920.0,
            height: 1080.0,
        };
        let main = Some(LogicalRect {
            left: 1500.0,
            top: 200.0,
            width: 300.0,
            height: 600.0,
        });
        let placement = choose_preview_placement(point(1600.0, 400.0), work, main, None);
        assert_eq!(placement.side, PreviewPlacementSide::Left);
        assert!(placement.position.x < 1500.0);
    }

    #[test]
    fn preview_uses_vertical_space_when_horizontal_space_is_below_400_logical_pixels() {
        let work = LogicalRect {
            left: 0.0,
            top: 0.0,
            width: 1200.0,
            height: 900.0,
        };
        let main = LogicalRect {
            left: 408.0,
            top: 200.0,
            width: 400.0,
            height: 500.0,
        };
        let placement =
            choose_preview_placement(point(600.0, 450.0), work, Some(main), Some((300.0, 100.0)));
        assert_eq!(placement.side, PreviewPlacementSide::Bottom);
    }

    #[test]
    fn preview_uses_vertical_space_when_both_horizontal_sides_are_too_small() {
        let work = LogicalRect {
            left: 0.0,
            top: 0.0,
            width: 1920.0,
            height: 1080.0,
        };
        let main = LogicalRect {
            left: 700.0,
            top: 300.0,
            width: 520.0,
            height: 400.0,
        };
        let placement =
            choose_preview_placement(point(900.0, 500.0), work, Some(main), Some((1000.0, 300.0)));
        assert_eq!(placement.side, PreviewPlacementSide::Bottom);
        assert_eq!(placement.anchor, Anchor::TopCenter);
        assert_eq!(placement.position.y, 718.0);
    }

    #[test]
    fn preview_falls_back_to_the_pointer_lower_right_corner() {
        let work = LogicalRect {
            left: 0.0,
            top: 0.0,
            width: 1000.0,
            height: 800.0,
        };
        let placement =
            choose_preview_placement(point(200.0, 200.0), work, None, Some((300.0, 200.0)));
        assert_eq!(placement.side, PreviewPlacementSide::PointerRight);
        assert_eq!(placement.anchor, Anchor::TopLeft);
        assert_eq!(placement.position, point(214.0, 214.0));
    }

    #[test]
    fn preview_falls_back_to_the_pointer_upper_left_corner_when_needed() {
        let work = LogicalRect {
            left: 0.0,
            top: 0.0,
            width: 1000.0,
            height: 800.0,
        };
        let placement =
            choose_preview_placement(point(900.0, 700.0), work, None, Some((300.0, 200.0)));
        assert_eq!(placement.side, PreviewPlacementSide::PointerLeft);
        assert_eq!(placement.anchor, Anchor::BottomRight);
        assert_eq!(placement.position, point(886.0, 686.0));
    }

    #[test]
    fn preview_overlays_main_window_when_surrounding_space_is_insufficient() {
        let work = LogicalRect {
            left: 0.0,
            top: 0.0,
            width: 1000.0,
            height: 800.0,
        };
        let main = LogicalRect {
            left: 100.0,
            top: 100.0,
            width: 800.0,
            height: 600.0,
        };
        let placement =
            choose_preview_placement(point(500.0, 400.0), work, Some(main), Some((300.0, 200.0)));
        assert_eq!(placement.side, PreviewPlacementSide::Overlay);
        assert_eq!(placement.anchor, Anchor::TopCenter);
        assert_eq!(placement.position, point(500.0, 300.0));
    }

    #[test]
    fn pointer_fallback_width_uses_the_selected_horizontal_space() {
        let work = LogicalRect {
            left: 0.0,
            top: 0.0,
            width: 1000.0,
            height: 800.0,
        };
        assert_eq!(
            resolve_preview_max_width(
                work,
                None,
                point(200.0, 200.0),
                PreviewPlacementSide::PointerRight,
            ),
            786.0
        );
        assert_eq!(
            resolve_preview_max_width(
                work,
                None,
                point(900.0, 200.0),
                PreviewPlacementSide::PointerLeft,
            ),
            886.0
        );
    }

    #[test]
    fn image_preview_uses_a_shared_logical_base_size() {
        assert_eq!(
            PreviewWindow::resolve_image_display_size(1920, 1080, 1000.0, 1000.0, 1.0, 480.0),
            Some(gpui::size(480.0, 270.0))
        );
        assert_eq!(
            PreviewWindow::resolve_image_display_size(160, 90, 1000.0, 1000.0, 1.0, 480.0),
            Some(gpui::size(480.0, 270.0))
        );
    }

    #[test]
    fn image_preview_percentage_reports_actual_logical_scale() {
        assert_eq!(
            PreviewWindow::resolve_image_display_scale(1920, 1080, 1000.0, 1000.0, 1.0, 480.0)
                .map(|scale| scale * 100.0),
            Some(25.0)
        );
        assert_eq!(
            PreviewWindow::resolve_image_display_scale(1920, 1080, 1000.0, 1000.0, 2.0, 480.0)
                .map(|scale| scale * 100.0),
            Some(50.0)
        );
        assert_eq!(
            PreviewWindow::resolve_image_display_scale(160, 90, 1000.0, 1000.0, 1.0, 480.0)
                .map(|scale| scale * 100.0),
            Some(300.0)
        );
    }

    #[test]
    fn image_preview_percentage_uses_the_adjusted_base_size() {
        assert_eq!(
            PreviewWindow::resolve_image_display_scale(1920, 1080, 1000.0, 1000.0, 1.0, 960.0)
                .map(|scale| scale * 100.0),
            Some(50.0)
        );
    }

    #[test]
    fn image_preview_base_size_is_limited_by_available_logical_space() {
        assert_eq!(
            PreviewWindow::resolve_image_display_size(1000, 1000, 260.0, 1000.0, 1.0, 480.0),
            Some(gpui::size(260.0, 260.0))
        );
        assert_eq!(
            PreviewWindow::resolve_image_display_size(1000, 1000, 260.0, 1000.0, 1.0, 960.0),
            Some(gpui::size(260.0, 260.0))
        );
        assert_eq!(
            PreviewWindow::resolve_image_display_size(1000, 2000, 1000.0, 300.0, 1.0, 960.0),
            Some(gpui::size(150.0, 300.0))
        );
    }

    #[test]
    fn image_preview_uses_the_adjusted_logical_base_size_on_high_dpi_displays() {
        assert_eq!(
            PreviewWindow::resolve_image_display_size(1920, 1080, 1000.0, 1000.0, 2.0, 960.0),
            Some(gpui::size(960.0, 540.0))
        );
    }

    #[test]
    fn image_preview_can_use_a_base_size_below_the_default() {
        assert_eq!(
            PreviewWindow::resolve_image_display_size(1920, 1080, 1000.0, 1000.0, 1.0, 48.0),
            Some(gpui::size(48.0, 27.0))
        );
    }

    #[test]
    fn image_preview_zoom_stops_at_visible_limit_and_reverses_immediately() {
        let mut base_size = 480.0;
        for _ in 0..20 {
            base_size = PreviewWindow::next_image_base_size(base_size, 700.0, 1.0);
        }
        assert_eq!(base_size, 700.0);
        assert_eq!(
            PreviewWindow::next_image_base_size(base_size, 700.0, 1.0),
            700.0
        );
        assert_eq!(
            PreviewWindow::next_image_base_size(base_size, 700.0, -1.0),
            652.0
        );
        assert_eq!(
            PreviewWindow::next_image_base_size(2400.0, 700.0, -1.0),
            652.0
        );
    }

    fn place_information(
        preview: LogicalRect,
        main: Option<LogicalRect>,
        viewport: LogicalRect,
        strip_size: gpui::Size<f32>,
    ) -> Option<LogicalRect> {
        let (align_end, areas) = information_areas(preview, main, viewport, 8.0);
        areas
            .into_iter()
            .find_map(|area| information_rect(preview, area, strip_size, align_end))
    }

    fn assert_information_is_clear(
        rect: LogicalRect,
        preview: LogicalRect,
        main: Option<LogicalRect>,
        work: LogicalRect,
    ) {
        assert!(rect.left >= work.left && rect.top >= work.top);
        assert!(rect.right() <= work.right() && rect.bottom() <= work.bottom());
        assert!(intersect_logical_rect(rect, preview).is_none());
        assert!(main.is_none_or(|main| intersect_logical_rect(rect, main).is_none()));
    }

    #[test]
    fn information_flips_below_preview_at_top_screen_edge() {
        let work = LogicalRect {
            left: 0.0,
            top: 0.0,
            width: 1000.0,
            height: 800.0,
        };
        let preview = LogicalRect {
            left: 450.0,
            top: 0.0,
            width: 480.0,
            height: 300.0,
        };
        let main = Some(LogicalRect {
            left: 20.0,
            top: 0.0,
            width: 400.0,
            height: 700.0,
        });
        let strip = place_information(preview, main, work, gpui::size(320.0, 220.0))
            .expect("应找到信息位置");
        assert_eq!(strip.left, preview.left);
        assert_eq!(strip.top, preview.bottom() + 8.0);
        assert_information_is_clear(strip, preview, main, work);
    }

    #[test]
    fn information_flips_outwards_when_vertical_space_is_insufficient() {
        let work = LogicalRect {
            left: 0.0,
            top: 0.0,
            width: 1400.0,
            height: 800.0,
        };
        let preview = LogicalRect {
            left: 430.0,
            top: 50.0,
            width: 480.0,
            height: 700.0,
        };
        let main = Some(LogicalRect {
            left: 0.0,
            top: 0.0,
            width: 400.0,
            height: 800.0,
        });
        let strip = place_information(preview, main, work, gpui::size(320.0, 220.0))
            .expect("右侧应有信息位置");
        assert_eq!(strip.left, preview.right() + 8.0);
        assert_information_is_clear(strip, preview, main, work);
    }

    #[test]
    fn left_preview_information_uses_actual_side_and_aligns_towards_main() {
        let work = LogicalRect {
            left: 0.0,
            top: 0.0,
            width: 1920.0,
            height: 1080.0,
        };
        let preview = LogicalRect {
            left: 120.0,
            top: 300.0,
            width: 480.0,
            height: 400.0,
        };
        let main = Some(LogicalRect {
            left: 618.0,
            top: 200.0,
            width: 600.0,
            height: 600.0,
        });
        let (align_end, _) = information_areas(preview, main, work, 8.0);
        assert!(align_end);
        let strip = place_information(preview, main, work, gpui::size(580.0, 80.0))
            .expect("左侧预览应有信息位置");
        assert_eq!(strip.right(), preview.right());
        assert_eq!(strip.bottom(), preview.top - 8.0);
        assert_information_is_clear(strip, preview, main, work);
    }

    #[test]
    fn right_preview_information_stays_on_the_right_in_the_reported_layout() {
        let work = LogicalRect {
            left: 0.0,
            top: 0.0,
            width: 1920.0,
            height: 1080.0,
        };
        let preview = LogicalRect {
            left: 887.0,
            top: 300.0,
            width: 460.0,
            height: 400.0,
        };
        let main = Some(LogicalRect {
            left: 537.0,
            top: 100.0,
            width: 333.0,
            height: 800.0,
        });
        let (align_end, _) = information_areas(preview, main, work, 8.0);
        assert!(!align_end);
        let strip = place_information(preview, main, work, gpui::size(742.0, 120.0))
            .expect("主窗口右侧有足够空间");
        assert_eq!(strip.left, preview.left);
        assert_eq!(strip.bottom(), preview.top - 8.0);
        assert_information_is_clear(strip, preview, main, work);
    }

    #[test]
    fn information_at_fractional_viewport_edge_keeps_its_computed_position() {
        let work = LogicalRect {
            left: 0.0,
            top: 0.0,
            width: 1535.25,
            height: 864.0,
        };
        let preview = LogicalRect {
            left: 793.375,
            top: 300.0,
            width: 460.0,
            height: 400.0,
        };
        let main = Some(LogicalRect {
            left: 443.375,
            top: 0.0,
            width: 332.0,
            height: 800.0,
        });
        let (_, areas) = information_areas(preview, main, work, 8.0);
        let strip = information_rect(
            preview,
            areas[0],
            gpui::size(
                information_max_width(preview, areas[0], false).floor(),
                120.0,
            ),
            false,
        )
        .expect("小数视口边缘应能放置信息");
        assert_eq!(strip.left, preview.left);
        assert!(strip.right() <= work.right());
        assert_information_is_clear(strip, preview, main, work);
    }

    #[test]
    fn information_reflows_instead_of_shifting_a_left_preview_alignment() {
        let preview = LogicalRect {
            left: 100.25,
            top: 300.0,
            width: 480.0,
            height: 300.0,
        };
        let area = LogicalRect {
            left: 0.0,
            top: 0.0,
            width: 590.25,
            height: 292.0,
        };
        let width = information_max_width(preview, area, true);
        assert_eq!(width, preview.right());
        assert!(information_rect(preview, area, gpui::size(area.width, 80.0), true).is_none());
        let strip = information_rect(preview, area, gpui::size(width.floor(), 80.0), true)
            .expect("左侧应先收窄换行再保持右边缘对齐");
        assert_eq!(strip.right(), preview.right());
    }

    #[test]
    fn information_searches_other_free_areas_before_overlaying_preview() {
        let work = LogicalRect {
            left: 0.0,
            top: 0.0,
            width: 1200.0,
            height: 800.0,
        };
        let preview = LogicalRect {
            left: 300.0,
            top: 350.0,
            width: 700.0,
            height: 300.0,
        };
        let main = Some(LogicalRect {
            left: 0.0,
            top: 0.0,
            width: 700.0,
            height: 330.0,
        });
        let strip = place_information(preview, main, work, gpui::size(320.0, 220.0))
            .expect("主窗口右上方应有空位");
        assert!(strip.bottom() <= preview.top - 8.0);
        assert_information_is_clear(strip, preview, main, work);
    }

    #[test]
    fn information_fallback_only_overlays_preview_and_never_main() {
        let work = LogicalRect {
            left: 0.0,
            top: 0.0,
            width: 1000.0,
            height: 600.0,
        };
        let preview = LogicalRect {
            left: 418.0,
            top: 0.0,
            width: 582.0,
            height: 600.0,
        };
        let main = LogicalRect {
            left: 0.0,
            top: 0.0,
            width: 400.0,
            height: 600.0,
        };
        let strip = place_information(preview, Some(main), work, gpui::size(560.0, 100.0))
            .expect("主窗口右侧应有回退位置");
        assert!(intersect_logical_rect(strip, preview).is_some());
        assert!(intersect_logical_rect(strip, main).is_none());
        let strip = place_information(work, None, work, gpui::size(320.0, 220.0))
            .expect("没有主窗口时可叠加预览");
        assert!(intersect_logical_rect(strip, work).is_some());
        assert!(place_information(work, Some(work), work, gpui::size(400.0, 80.0)).is_none());
    }

    #[test]
    fn information_width_is_limited_to_main_window_free_area_for_reflow() {
        let work = LogicalRect {
            left: 0.0,
            top: 0.0,
            width: 1000.0,
            height: 600.0,
        };
        let preview = LogicalRect {
            left: 718.0,
            top: 0.0,
            width: 282.0,
            height: 600.0,
        };
        let main = Some(LogicalRect {
            left: 0.0,
            top: 0.0,
            width: 700.0,
            height: 600.0,
        });
        let (align_end, areas) = information_areas(preview, main, work, 8.0);
        assert!(place_information(preview, main, work, gpui::size(960.0, 80.0)).is_none());
        assert!(areas.iter().all(|area| area.width <= 292.0));
        let strip = areas
            .into_iter()
            .find_map(|area| information_rect(preview, area, gpui::size(282.0, 200.0), align_end))
            .expect("换行后的信息应在主窗口外显示");
        assert_eq!(strip.left, preview.left);
        assert!(intersect_logical_rect(strip, main.unwrap()).is_none());
    }

    #[test]
    fn information_areas_handle_negative_desktop_coordinates() {
        let work = LogicalRect {
            left: -1000.0,
            top: -100.0,
            width: 1400.0,
            height: 800.0,
        };
        let preview = LogicalRect {
            left: -500.0,
            top: -50.0,
            width: 480.0,
            height: 700.0,
        };
        let main = Some(LogicalRect {
            left: 0.0,
            top: -100.0,
            width: 400.0,
            height: 800.0,
        });
        let strip = place_information(preview, main, work, gpui::size(320.0, 220.0))
            .expect("左侧应有信息位置");
        assert_eq!(strip.right(), preview.left - 8.0);
        assert_information_is_clear(strip, preview, main, work);
    }

    #[test]
    fn information_wraps_only_when_the_horizontal_row_is_full() {
        let fields = [100.0, 180.0, 80.0];
        assert_eq!(information_row_ranges(&fields, 500.0, 6.0), vec![0..3]);
        assert_eq!(
            information_row_ranges(&fields, 300.0, 6.0),
            vec![0..2, 2..3]
        );
        assert_eq!(
            information_row_ranges(&fields, 180.0, 6.0),
            vec![0..1, 1..2, 2..3]
        );
        assert_eq!(
            information_row_ranges(&[500.0, 50.0], 180.0, 6.0),
            vec![0..1, 1..2]
        );
        assert!(information_row_ranges(&[], 180.0, 6.0).is_empty());
    }

    #[test]
    fn original_image_size_is_available_before_decoding() {
        use crate::domain::clipboard::ImagePreviewSource;
        let inline = ImagePreviewSource::Inline {
            bytes: vec![0; 1234].into(),
        };
        let external = ImagePreviewSource::External {
            sha256: [0; 32],
            byte_length: 5678,
            relative_path: "image.bin.zst".into(),
        };
        assert_eq!(image_source_byte_length(Some(&inline)), Some(1234));
        assert_eq!(image_source_byte_length(Some(&external)), Some(5678));
        assert_eq!(image_source_byte_length(None), None);
    }
    #[test]
    fn connector_source_is_clipped_to_the_visible_main_window_area() {
        let item = LogicalRect {
            left: 40.0,
            top: 20.0,
            width: 260.0,
            height: 180.0,
        };
        let main = LogicalRect {
            left: 100.0,
            top: 50.0,
            width: 160.0,
            height: 100.0,
        };
        let visible = intersect_logical_rect(item, main).expect("矩形应有可见交集");
        assert_eq!(visible.left, 100.0);
        assert_eq!(visible.top, 50.0);
        assert_eq!(visible.width, 160.0);
        assert_eq!(visible.height, 100.0);
    }

    #[test]
    fn connector_prefers_opposite_horizontal_edges() {
        let source = LogicalRect {
            left: 0.0,
            top: 100.0,
            width: 100.0,
            height: 100.0,
        };
        let target = LogicalRect {
            left: 200.0,
            top: 100.0,
            width: 240.0,
            height: 100.0,
        };
        let path = resolve_best_connector(source, target).expect("应生成连接线");
        assert_eq!(path.source, point(100.0, 150.0));
        assert_eq!(path.target, point(200.0, 150.0));
    }
}
