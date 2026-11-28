use crate::ui::scale::ui_rem;

use std::{
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use gpui::prelude::*;
use gpui::{
    App, AppContext, AvailableSpace, Bounds, Context, Global, Pixels, Render, RenderImage, Window,
    WindowHandle, div, img, point, px, relative, size,
};

use crate::{
    config::{
        ClipboardDisplayPriority, NotificationConfig, NotificationKind, NotificationPosition,
    },
    config_runtime,
    domain::{
        capture::FormatRole,
        clipboard::{
            ClipboardItemId, ContentKind, FormatPreview, HistoryItemId,
            select_user_visible_previews_with_priority,
        },
    },
    platform::{input, window as platform_window},
    services::{
        database::{HistoryRepository, query::HistoryQuery},
        image_preview::ImagePreviewService,
    },
};
use gpui_component::ActiveTheme as _;
use gpui_component::{Sizable as _, h_flex};

const DEFAULT_DURATION: Duration =
    Duration::from_millis(NotificationConfig::DEFAULT_DURATION_MS as u64);
const ANIMATION_DURATION: Duration = Duration::from_millis(220);
const NOTIFICATION_MARGIN: f32 = 16.0;
const NOTIFICATION_WIDTH: f32 = 380.0;
const ANIMATION_OFFSET: f32 = 36.0;
const NOTIFICATION_CONTENT_PADDING: f32 = 24.0;
const NOTIFICATION_HEADER_MIN_WIDTH: f32 = 68.0;
const NOTIFICATION_IMAGE_HEIGHT: f32 = 96.0;
const NOTIFICATION_IMAGE_ROW_CHROME_WIDTH: f32 = 60.0;

fn render_image_from_rgba(mut image: image::RgbaImage) -> Arc<RenderImage> {
    for pixel in image.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Arc::new(RenderImage::new(vec![image::Frame::new(image)]))
}

fn flatten_line_breaks(text: &str) -> String {
    text.replace("\r\n", " ").replace(['\r', '\n'], " ")
}

fn image_notification_layout(
    image_width: f32,
    image_height: f32,
    max_width: f32,
    has_title: bool,
) -> (f32, f32, f32) {
    let horizontal_insets = NOTIFICATION_CONTENT_PADDING
        + if has_title {
            0.0
        } else {
            NOTIFICATION_IMAGE_ROW_CHROME_WIDTH
        };
    let available_image_width = (max_width - horizontal_insets).max(1.0);
    let (image_display_width, image_display_height) = if image_width > 0.0 && image_height > 0.0 {
        let scale = (NOTIFICATION_IMAGE_HEIGHT / image_height)
            .min(available_image_width / image_width)
            .min(1.0);
        (image_width * scale, image_height * scale)
    } else {
        (0.0, 0.0)
    };
    let min_width = if has_title {
        NOTIFICATION_HEADER_MIN_WIDTH + NOTIFICATION_CONTENT_PADDING
    } else {
        8.0 + horizontal_insets
    };
    (
        (image_display_width + horizontal_insets)
            .max(min_width)
            .min(max_width),
        image_display_width,
        image_display_height,
    )
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum NotificationLevel {
    #[default]
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone)]
pub(crate) enum NotificationContent {
    Text(String),
    Image(Arc<RenderImage>),
    FileCount(usize),
}

#[derive(Clone)]
pub(crate) enum NotificationAction {
    OpenUpdates,
    InstallUpdate(String),
}

#[derive(Clone)]
pub(crate) struct NotificationRequest {
    pub(crate) kind: NotificationKind,
    pub(crate) title: Option<String>,
    pub(crate) icon: Option<Arc<RenderImage>>,
    pub(crate) content: NotificationContent,
    pub(crate) level: NotificationLevel,
    pub(crate) position: NotificationPosition,
    pub(crate) max_lines: Option<usize>,
    pub(crate) duration: Duration,
    pub(crate) action_url: Option<String>,
    pub(crate) action: Option<NotificationAction>,
    pub(crate) action_label: Option<String>,
}

impl NotificationRequest {
    pub(crate) fn text(title: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            kind: NotificationKind::OperationResult,
            title: Some(title.into()),
            icon: application_icon(),
            content: NotificationContent::Text(text.into()),
            level: NotificationLevel::Success,
            position: NotificationPosition::default(),
            max_lines: Some(2),
            duration: DEFAULT_DURATION,
            action_url: None,
            action: None,
            action_label: None,
        }
    }

    fn preference_kind(&self) -> NotificationKind {
        match self.level {
            NotificationLevel::Warning | NotificationLevel::Error => NotificationKind::Attention,
            _ => self.kind,
        }
    }
}

fn clipboard_notification_content(
    preview: Option<FormatPreview>,
    content_kind: ContentKind,
    image_service: &ImagePreviewService,
) -> NotificationContent {
    let Some(preview) = preview else {
        return NotificationContent::Text(String::new());
    };
    if (content_kind == ContentKind::FileList || preview.role() == FormatRole::FileList)
        && preview.content_kind == ContentKind::FileList
        && let Some(source) = preview.image_source.as_ref()
        && let Ok(paths) = image_service.load_file_list_paths(source)
    {
        return NotificationContent::FileCount(paths.len());
    }
    if preview.content_kind == ContentKind::Image
        && let Some(source) = preview.image_source.as_ref()
        && let Ok(image) =
            image_service.load(source, preview.format_kind, &preview.format_key, (480, 240))
    {
        return NotificationContent::Image(image);
    }
    NotificationContent::Text(preview.text)
}

fn clipboard_notification_request(
    previews: &[FormatPreview],
    content_kind: ContentKind,
    image_service: &ImagePreviewService,
    priority: ClipboardDisplayPriority,
) -> NotificationRequest {
    let roles = display_priority_roles(priority);
    let preview = select_user_visible_previews_with_priority(previews, &roles)
        .into_iter()
        .next();
    let content = clipboard_notification_content(preview, content_kind, image_service);
    NotificationRequest {
        kind: NotificationKind::ClipboardPreview,
        title: None,
        icon: application_icon(),
        content,
        level: NotificationLevel::Success,
        position: NotificationPosition::default(),
        max_lines: Some(2),
        duration: DEFAULT_DURATION,
        action_url: None,
        action: None,
        action_label: None,
    }
}

fn application_icon() -> Option<Arc<RenderImage>> {
    static ICON: OnceLock<Option<Arc<RenderImage>>> = OnceLock::new();
    ICON.get_or_init(|| {
        let image = match image::load_from_memory(include_bytes!("../../assets/icon64.png")) {
            Ok(image) => image.into_rgba8(),
            Err(error) => {
                tracing::error!(%error, "读取程序通知图标失败");
                return None;
            }
        };
        Some(render_image_from_rgba(image))
    })
    .clone()
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

#[derive(Clone, Copy, Debug, PartialEq)]
struct PhysicalBounds {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

fn notification_bounds(
    position: NotificationPosition,
    area: platform_window::PhysicalDisplayBounds,
    width: f32,
    height: f32,
) -> PhysicalBounds {
    let width = width.min((area.width - NOTIFICATION_MARGIN * 2.0).max(1.0));
    let height = height.min((area.height - NOTIFICATION_MARGIN * 2.0).max(1.0));
    let left = area.x + NOTIFICATION_MARGIN;
    let right = area.x + area.width - width - NOTIFICATION_MARGIN;
    let top = area.y + NOTIFICATION_MARGIN;
    let bottom = area.y + area.height - height - NOTIFICATION_MARGIN;
    let center_x = area.x + (area.width - width) / 2.0;
    let center_y = area.y + (area.height - height) / 2.0;
    let (x, y) = match position {
        NotificationPosition::TopLeft => (left, top),
        NotificationPosition::Top => (center_x, top),
        NotificationPosition::TopRight => (right, top),
        NotificationPosition::Left => (left, center_y),
        NotificationPosition::Right => (right, center_y),
        NotificationPosition::BottomLeft => (left, bottom),
        NotificationPosition::Bottom => (center_x, bottom),
        NotificationPosition::BottomRight => (right, bottom),
    };
    PhysicalBounds {
        x,
        y,
        width,
        height,
    }
}

fn animation_offset(position: NotificationPosition) -> (f32, f32) {
    match position {
        NotificationPosition::TopLeft => (-ANIMATION_OFFSET, -ANIMATION_OFFSET),
        NotificationPosition::Top => (0.0, -ANIMATION_OFFSET),
        NotificationPosition::TopRight => (ANIMATION_OFFSET, -ANIMATION_OFFSET),
        NotificationPosition::Left => (-ANIMATION_OFFSET, 0.0),
        NotificationPosition::Right => (ANIMATION_OFFSET, 0.0),
        NotificationPosition::BottomLeft => (-ANIMATION_OFFSET, ANIMATION_OFFSET),
        NotificationPosition::Bottom => (0.0, ANIMATION_OFFSET),
        NotificationPosition::BottomRight => (ANIMATION_OFFSET, ANIMATION_OFFSET),
    }
}

fn progress_from_right(position: NotificationPosition) -> bool {
    matches!(
        position,
        NotificationPosition::TopRight
            | NotificationPosition::Right
            | NotificationPosition::BottomRight
    )
}

struct ActiveNotification {
    handle: WindowHandle<NotificationWindow>,
    kind: NotificationKind,
}

#[derive(Default)]
pub(crate) struct NotificationService {
    generation: u64,
    active: Option<ActiveNotification>,
    pending: Option<(u64, NotificationRequest)>,
    history_repository: Option<HistoryRepository>,
    image_preview_service: Option<ImagePreviewService>,
}

impl Global for NotificationService {}

#[derive(Clone, Copy, Eq, PartialEq)]
enum WindowPhase {
    Entering,
    Visible,
    Exiting,
}

struct NotificationWindow {
    request: NotificationRequest,
    popup: platform_window::BorderlessPopup,
    area: platform_window::PhysicalDisplayBounds,
    scale_factor: f32,
    phase: WindowPhase,
    phase_started: Instant,
    expires_at: Instant,
    target_bounds: Option<PhysicalBounds>,
}

impl NotificationWindow {
    fn new(
        request: NotificationRequest,
        popup: platform_window::BorderlessPopup,
        area: platform_window::PhysicalDisplayBounds,
        scale_factor: f32,
    ) -> Self {
        let now = Instant::now();
        Self {
            expires_at: now + request.duration,
            request,
            popup,
            area,
            scale_factor,
            phase: WindowPhase::Entering,
            phase_started: now,
            target_bounds: None,
        }
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.phase == WindowPhase::Exiting {
            return;
        }
        self.phase = WindowPhase::Exiting;
        self.phase_started = Instant::now();
        window.refresh();
        cx.notify();
    }

    fn measure_content(&self, window: &mut Window, cx: &mut Context<Self>) -> gpui::Size<Pixels> {
        let max_width = px(
            (self.area.width / self.scale_factor - NOTIFICATION_MARGIN * 2.0)
                .min(NOTIFICATION_WIDTH)
                .max(1.0),
        );
        let width = match &self.request.content {
            NotificationContent::Image(image) => {
                let image_size = image.size(0).to_pixels(self.scale_factor);
                let (width, _, _) = image_notification_layout(
                    f32::from(image_size.width),
                    f32::from(image_size.height),
                    f32::from(max_width),
                    self.request.title.is_some(),
                );
                px(width)
            }
            _ => self
                .render_card(cx)
                .into_any_element()
                .layout_as_root(
                    size(AvailableSpace::MaxContent, AvailableSpace::MinContent),
                    window,
                    cx,
                )
                .width
                .min(max_width),
        };
        self.render_card(cx).into_any_element().layout_as_root(
            size(AvailableSpace::Definite(width), AvailableSpace::MinContent),
            window,
            cx,
        )
    }

    fn render_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let custom_image_background = crate::window_material::uses_custom_image_background(cx);
        let translator = crate::i18n::translator(cx);
        let max_width = (self.area.width / self.scale_factor - NOTIFICATION_MARGIN * 2.0)
            .min(NOTIFICATION_WIDTH);
        let max_height =
            (self.area.height / self.scale_factor - NOTIFICATION_MARGIN * 2.0).max(72.0);
        let icon = if let Some(image) = self.request.icon.clone() {
            img(image)
                .w(px(18.0))
                .h(px(18.0))
                .object_fit(gpui::ObjectFit::Contain)
                .into_any_element()
        } else {
            crate::ui::icon::icon("clipboard-text", 20.0, theme.primary).into_any_element()
        };
        let body = match &self.request.content {
            NotificationContent::Text(text) => {
                let body = div()
                    .min_w_0()
                    .w_full()
                    .text_size(ui_rem(14.0))
                    .line_height(ui_rem(19.0))
                    .text_color(theme.muted_foreground);
                match self.request.max_lines.filter(|lines| *lines > 0) {
                    Some(lines) => body
                        .line_clamp(lines)
                        .text_ellipsis()
                        .child(flatten_line_breaks(text))
                        .into_any_element(),
                    None => body.child(text.clone()).into_any_element(),
                }
            }
            NotificationContent::Image(image) => {
                let image_size = image.size(0).to_pixels(self.scale_factor);
                let max_width = (self.area.width / self.scale_factor - NOTIFICATION_MARGIN * 2.0)
                    .min(NOTIFICATION_WIDTH);
                let (_, image_width, image_height) = image_notification_layout(
                    f32::from(image_size.width),
                    f32::from(image_size.height),
                    max_width,
                    self.request.title.is_some(),
                );
                div()
                    .w(px(image_width))
                    .h(px(image_height))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        img(image.clone())
                            .w(px(image_width))
                            .h(px(image_height))
                            .object_fit(gpui::ObjectFit::Contain),
                    )
                    .into_any_element()
            }
            NotificationContent::FileCount(count) => {
                div()
                    .w_full()
                    .text_size(ui_rem(14.0))
                    .line_height(ui_rem(19.0))
                    .text_color(theme.muted_foreground)
                    .child(translator.translate_with_args(
                        "main.preview.file_list",
                        &[("count", count.to_string())],
                    ))
                    .into_any_element()
            }
        };
        let progress = self.progress_fraction();
        let progress_color = match self.request.level {
            NotificationLevel::Info => theme.info,
            NotificationLevel::Success => theme.success,
            NotificationLevel::Warning => theme.warning,
            NotificationLevel::Error => theme.danger,
        };
        let mut progress_bar = div().w_full().h(px(3.0)).bg(theme.border);
        if progress_from_right(self.request.position) {
            progress_bar = progress_bar.flex().justify_end();
        }
        let progress_bar =
            progress_bar.child(div().h_full().w(relative(progress)).bg(progress_color));
        let content = if let Some(title) = self.request.title.as_ref() {
            let header = h_flex()
                .w_full()
                .min_w_0()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .size(ui_rem(20.0))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .truncate()
                        .child(title.clone()),
                )
                .child(
                    div()
                        .id("global-notification-close")
                        .group("global-notification-close")
                        .size(ui_rem(24.0))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.0))
                        .cursor_pointer()
                        .hover(|style| style.bg(theme.secondary))
                        .child(crate::ui::hover_icon(
                            "global-notification-close",
                            "x",
                            16.0,
                            theme.muted_foreground,
                        ))
                        .on_click(cx.listener(|view, _, window, cx| view.dismiss(window, cx))),
                );
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .px_3()
                .py_2()
                .child(header)
                .child(body)
                .into_any_element()
        } else {
            h_flex()
                .w_full()
                .min_w_0()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .child(
                    div()
                        .size(ui_rem(20.0))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon),
                )
                .child(div().flex_1().min_w_0().child(body))
                .child(
                    div()
                        .id("global-notification-close")
                        .group("global-notification-close")
                        .size(ui_rem(24.0))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.0))
                        .cursor_pointer()
                        .hover(|style| style.bg(theme.secondary))
                        .child(crate::ui::hover_icon(
                            "global-notification-close",
                            "x",
                            16.0,
                            theme.muted_foreground,
                        ))
                        .on_click(cx.listener(|view, _, window, cx| view.dismiss(window, cx))),
                )
                .into_any_element()
        };
        div()
            .w_full()
            .max_w(px(max_width))
            .max_h(px(max_height))
            .flex()
            .flex_col()
            .overflow_hidden()
            .when(!custom_image_background, |card| card.bg(theme.background))
            .text_color(theme.foreground)
            .child(progress_bar)
            .child(content)
            .when(self.request.action_url.is_some() || self.request.action.is_some(), |view| {
                let label = self.request.action_label.clone().unwrap_or_default();
                let url = self.request.action_url.clone();
                let action = self.request.action.clone();
                view.child(
                    div().px_3().pb_2().child(
                        gpui_component::button::Button::new("notification-action")
                            .small()
                            .label(label)
                            .on_click(cx.listener(move |view, _, window, cx| {
                                view.dismiss(window, cx);
                                match &action {
                                    Some(NotificationAction::OpenUpdates) => crate::windows::settings::open_about_window(cx),
                                    Some(NotificationAction::InstallUpdate(tag)) => {
                                        crate::windows::settings::open_about_window(cx);
                                        crate::services::updates::UpdateService::install_downloaded(Some(tag), cx);
                                    },
                                    None => { if let Some(url) = &url { cx.open_url(url); } }
                                }
                            })),
                    ),
                )
            })
    }

    fn progress_fraction(&self) -> f32 {
        if self.phase == WindowPhase::Exiting {
            return 0.0;
        }
        let duration = self.request.duration.as_secs_f32().max(f32::EPSILON);
        (self
            .expires_at
            .saturating_duration_since(Instant::now())
            .as_secs_f32()
            / duration)
            .clamp(0.0, 1.0)
    }

    fn schedule_next_frame(&self, window: &Window, cx: &mut Context<Self>) {
        let view = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            if let Some(view) = view.upgrade() {
                view.update(cx, |view, cx| view.tick(window, cx));
            }
        });
    }

    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.target_bounds else {
            window.refresh();
            return;
        };
        let animations_enabled = crate::config_runtime::animation_enabled(
            cx,
            crate::config::AnimationKind::Notification,
        );
        let progress = if animations_enabled {
            (self.phase_started.elapsed().as_secs_f32() / ANIMATION_DURATION.as_secs_f32())
                .clamp(0.0, 1.0)
        } else {
            1.0
        };
        if self.phase != WindowPhase::Visible {
            let eased = 1.0 - (1.0 - progress).powi(3);
            let (offset_x, offset_y) = animation_offset(self.request.position);
            let (x, y, opacity) = match self.phase {
                WindowPhase::Entering => (
                    target.x + offset_x * (1.0 - eased),
                    target.y + offset_y * (1.0 - eased),
                    eased,
                ),
                WindowPhase::Exiting => (
                    target.x + offset_x * eased,
                    target.y + offset_y * eased,
                    1.0 - eased,
                ),
                WindowPhase::Visible => unreachable!(),
            };
            if !self
                .popup
                .resize_to_physical_bounds(window, cx, x, y, target.width, target.height)
            {
                tracing::warn!("调整全局通知窗口位置失败");
            }
            if let Err(error) = self.popup.set_opacity(window, opacity) {
                tracing::debug!(%error, "设置全局通知窗口透明度失败");
            }
        }
        if self.phase == WindowPhase::Entering && progress >= 1.0 {
            self.phase = WindowPhase::Visible;
            self.phase_started = Instant::now();
        } else if self.phase == WindowPhase::Visible && Instant::now() >= self.expires_at {
            self.phase = WindowPhase::Exiting;
            self.phase_started = Instant::now();
        } else if self.phase == WindowPhase::Exiting && progress >= 1.0 {
            window.remove_window();
            return;
        }
        cx.notify();
        window.refresh();
    }
}

impl Render for NotificationWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rem_size = crate::ui::scale::standalone_rem_size(16.0, cx);
        if window.rem_size() != rem_size {
            self.target_bounds = None;
        }
        window.set_rem_size(rem_size);
        // 尺寸测量必须在布局阶段执行，下一帧回调只负责动画。
        if self.target_bounds.is_none() {
            let measured = self.measure_content(window, cx);
            self.target_bounds = Some(notification_bounds(
                self.request.position,
                self.area,
                f32::from(measured.width) * self.scale_factor,
                f32::from(measured.height) * self.scale_factor,
            ));
        }
        self.schedule_next_frame(window, cx);
        let background = crate::window_material::background_element(window, cx);
        let card = self.render_card(cx);
        div()
            .relative()
            .size_full()
            .text_size(ui_rem(13.0))
            .child(background)
            .child(card)
            .child(crate::window_material::window_frame())
    }
}

impl NotificationService {
    pub(crate) fn show_auto_start_error(error: impl std::fmt::Display, cx: &mut App) {
        let translator = crate::i18n::translator(cx);
        Self::show_message(
            translator.translate("notification.auto_start.failed.title"),
            translator.translate_with_args(
                "notification.auto_start.failed.detail",
                &[("error", error.to_string())],
            ),
            NotificationLevel::Error,
            cx,
        );
    }

    pub(crate) fn show_message(
        title: impl Into<String>,
        message: impl Into<String>,
        level: NotificationLevel,
        cx: &mut App,
    ) {
        Self::show_message_for(NotificationKind::OperationResult, title, message, level, cx);
    }

    pub(crate) fn show_message_for(
        kind: NotificationKind,
        title: impl Into<String>,
        message: impl Into<String>,
        level: NotificationLevel,
        cx: &mut App,
    ) {
        Self::show(
            NotificationRequest {
                kind,
                title: Some(title.into()),
                icon: application_icon(),
                content: NotificationContent::Text(message.into()),
                level,
                position: NotificationPosition::default(),
                max_lines: Some(3),
                duration: DEFAULT_DURATION,
                action_url: None,
                action: None,
                action_label: None,
            },
            cx,
        );
    }

    pub(crate) fn install(
        history_repository: HistoryRepository,
        image_preview_service: ImagePreviewService,
        cx: &mut App,
    ) {
        cx.set_global(Self {
            history_repository: Some(history_repository),
            image_preview_service: Some(image_preview_service),
            ..Default::default()
        });
        cx.on_window_closed(|cx, window_id| {
            cx.defer(move |cx| {
                let pending = cx.update_global::<Self, _>(|service, _| {
                    if service
                        .active
                        .as_ref()
                        .is_none_or(|active| active.handle.window_id() != window_id)
                    {
                        return None;
                    }
                    service.active = None;
                    service.pending.take().and_then(|(generation, request)| {
                        (generation == service.generation).then_some(request)
                    })
                });
                if let Some(request) = pending {
                    cx.update_global::<Self, _>(|service, cx| service.open(request, cx));
                }
            });
        })
        .detach();
    }

    pub(crate) fn show(request: NotificationRequest, cx: &mut App) {
        if !config_runtime::notification_enabled(request.preference_kind(), cx) {
            return;
        }
        cx.update_global::<Self, _>(|service, cx| service.enqueue(request, cx));
    }

    pub(crate) fn replace_data_sources(
        history_repository: HistoryRepository,
        image_preview_service: ImagePreviewService,
        cx: &mut App,
    ) {
        cx.update_global::<Self, _>(|service, _| {
            service.history_repository = Some(history_repository);
            service.image_preview_service = Some(image_preview_service);
        });
    }

    pub(crate) fn show_startup(cx: &mut App) {
        Self::show(
            NotificationRequest {
                kind: NotificationKind::Startup,
                title: None,
                icon: application_icon(),
                content: NotificationContent::Text(
                    crate::i18n::translator(cx).translate("notification.startup.success"),
                ),
                level: NotificationLevel::Success,
                position: NotificationPosition::default(),
                max_lines: Some(1),
                duration: DEFAULT_DURATION,
                action_url: None,
                action: None,
                action_label: None,
            },
            cx,
        );
    }

    pub(crate) fn show_clipboard(history_id: HistoryItemId, cx: &mut App) {
        if !config_runtime::notification_enabled(NotificationKind::ClipboardPreview, cx) {
            return;
        }
        let (repository, image_service) = cx.read_global::<Self, _>(|service, _| {
            (
                service.history_repository.clone(),
                service.image_preview_service.clone(),
            )
        });
        let (Some(repository), Some(image_service)) = (repository, image_service) else {
            return;
        };
        let priority = config_runtime::clipboard_config(cx).display_priority;
        let task = cx.background_executor().spawn(async move {
            let mut query = HistoryQuery::default();
            query.limit = 1;
            query.load_image_sources = true;
            let record = repository.find_by_id(history_id, &query).ok().flatten()?;
            Some(clipboard_notification_request(
                &record.preview_options,
                record.content_kind,
                &image_service,
                priority,
            ))
        });
        cx.spawn(async move |cx| {
            if let Some(request) = task.await {
                cx.update(|cx| Self::show(request, cx));
            }
        })
        .detach();
    }

    pub(crate) fn show_clipboard_item(item_id: ClipboardItemId, cx: &mut App) {
        if !config_runtime::notification_enabled(NotificationKind::ClipboardPreview, cx) {
            return;
        }
        let (repository, image_service) = cx.read_global::<Self, _>(|service, _| {
            (
                service.history_repository.clone(),
                service.image_preview_service.clone(),
            )
        });
        let (Some(repository), Some(image_service)) = (repository, image_service) else {
            return;
        };
        let priority = config_runtime::clipboard_config(cx).display_priority;
        let task = cx.background_executor().spawn(async move {
            let mut query = HistoryQuery::default();
            query.load_image_sources = true;
            let (content_kind, previews) = repository
                .find_item_previews(item_id, &query)
                .ok()
                .flatten()?;
            Some(clipboard_notification_request(
                &previews,
                content_kind,
                &image_service,
                priority,
            ))
        });
        cx.spawn(async move |cx| {
            if let Some(request) = task.await {
                cx.update(|cx| Self::show(request, cx));
            }
        })
        .detach();
    }

    pub(crate) fn dismiss_disabled(cx: &mut App) {
        cx.update_global::<Self, _>(|service, cx| {
            if service.pending.as_ref().is_some_and(|(_, request)| {
                !config_runtime::notification_enabled(request.preference_kind(), cx)
            }) {
                service.pending = None;
            }
            if let Some(active) = service.active.as_ref()
                && !config_runtime::notification_enabled(active.kind, cx)
                && let Err(error) = active
                    .handle
                    .update(cx, |view, window, cx| view.dismiss(window, cx))
            {
                tracing::debug!(%error, "关闭已禁用的通知失败");
            }
        });
    }

    pub(crate) fn dismiss_all(cx: &mut App) {
        cx.update_global::<Self, _>(|service, cx| {
            service.generation = service.generation.wrapping_add(1);
            service.pending = None;
            if let Some(active) = service.active.as_ref()
                && let Err(error) = active
                    .handle
                    .update(cx, |view, window, cx| view.dismiss(window, cx))
            {
                tracing::debug!(%error, "关闭全局通知窗口失败");
            }
        });
    }

    fn enqueue(&mut self, request: NotificationRequest, cx: &mut App) {
        self.generation = self.generation.wrapping_add(1);
        if let Some(active) = self.active.as_ref() {
            self.pending = Some((self.generation, request));
            if let Err(error) = active
                .handle
                .update(cx, |view, window, cx| view.dismiss(window, cx))
            {
                tracing::debug!(%error, "关闭上一条全局通知失败");
            }
        } else {
            self.open(request, cx);
        }
    }

    fn open(&mut self, mut request: NotificationRequest, cx: &mut App) {
        let kind = request.preference_kind();
        if !config_runtime::notification_enabled(kind, cx) {
            return;
        }
        let notifications = config_runtime::notification_config(cx);
        request.position = notifications.position;
        request.duration = Duration::from_millis(u64::from(notifications.duration_ms));
        let cursor = input::pointer_snapshot()
            .map(|snapshot| snapshot.position)
            .or_else(platform_window::physical_cursor_position);
        let target = cursor
            .and_then(|position| {
                platform_window::BorderlessPopup::work_area_for_physical_point(cx, position)
            })
            .or_else(|| {
                let display = cx.primary_display()?;
                let bounds = display.bounds();
                let center = point(
                    f32::from(bounds.origin.x + bounds.size.width / 2.0),
                    f32::from(bounds.origin.y + bounds.size.height / 2.0),
                );
                platform_window::BorderlessPopup::work_area_for_physical_point(cx, center).or_else(
                    || platform_window::BorderlessPopup::display_for_physical_point(cx, center),
                )
            });
        let Some(target) = target else {
            tracing::warn!("无法确定通知窗口的目标屏幕");
            return;
        };
        let scale_factor = target.scale_factor.max(1.0);
        let initial = Bounds::new(
            point(
                px(target.bounds.x / scale_factor),
                px(target.bounds.y / scale_factor),
            ),
            size(px(1.0), px(1.0)),
        );
        let area = target.bounds;
        let result = platform_window::open_borderless_popup_visible(
            cx,
            initial,
            Some(target.display_id),
            move |window, cx, popup| {
                let view = cx.new(|_| NotificationWindow::new(request, popup, area, scale_factor));
                window.refresh();
                view
            },
        );
        match result {
            Ok(handle) => self.active = Some(ActiveNotification { handle, kind }),
            Err(error) => tracing::error!(%error, "打开全局通知窗口失败"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        NotificationPosition, PhysicalBounds, animation_offset, flatten_line_breaks,
        image_notification_layout, notification_bounds, progress_from_right,
        render_image_from_rgba,
    };
    use crate::platform::window::PhysicalDisplayBounds;

    #[test]
    fn warning_and_error_notifications_use_the_attention_preference() {
        use super::{NotificationKind, NotificationLevel, NotificationRequest};
        let mut config = crate::config::AppConfig::default();
        config.set_notification_preference(NotificationKind::OperationResult, false);
        config.set_notification_preference(NotificationKind::FeatureStatus, false);
        let mut request = NotificationRequest::text("测试", "测试");
        for kind in [
            NotificationKind::OperationResult,
            NotificationKind::FeatureStatus,
        ] {
            request.kind = kind;
            for level in [NotificationLevel::Info, NotificationLevel::Success] {
                request.level = level;
                assert!(!config.allows_notification(request.preference_kind()));
            }
            for level in [NotificationLevel::Warning, NotificationLevel::Error] {
                request.level = level;
                assert_eq!(request.preference_kind(), NotificationKind::Attention);
                assert!(config.allows_notification(request.preference_kind()));
                config.set_notification_preference(NotificationKind::Attention, false);
                assert!(!config.allows_notification(request.preference_kind()));
                config.set_notification_preference(NotificationKind::Attention, true);
            }
        }
    }

    #[test]
    fn flatten_line_breaks_prevents_hard_wrapped_notification_text() {
        assert_eq!(
            flatten_line_breaks("第一行\r\n第二行\n第三行\r第四行"),
            "第一行 第二行 第三行 第四行"
        );
    }

    #[test]
    fn image_notification_layout_matches_measured_and_rendered_size() {
        assert_eq!(
            image_notification_layout(240.0, 160.0, 380.0, true),
            (168.0, 144.0, 96.0)
        );
        assert_eq!(
            image_notification_layout(960.0, 240.0, 380.0, false),
            (380.0, 296.0, 74.0)
        );
    }

    #[test]
    fn image_notification_layout_does_not_upscale_small_previews() {
        assert_eq!(
            image_notification_layout(32.0, 24.0, 380.0, false),
            (116.0, 32.0, 24.0)
        );
    }

    #[test]
    fn progress_depletes_outward_on_the_right_edge() {
        assert!(progress_from_right(NotificationPosition::TopRight));
        assert!(progress_from_right(NotificationPosition::Right));
        assert!(progress_from_right(NotificationPosition::BottomRight));
        assert!(!progress_from_right(NotificationPosition::BottomLeft));
    }

    #[test]
    fn notification_icon_converts_rgba_to_gpui_bgra() {
        let rgba = image::RgbaImage::from_pixel(1, 1, image::Rgba([10, 20, 30, 40]));
        let render_image = render_image_from_rgba(rgba);

        assert_eq!(render_image.as_bytes(0), Some(&[30, 20, 10, 40][..]));
    }

    #[test]
    fn default_position_uses_the_bottom_right_work_area_corner() {
        let area = PhysicalDisplayBounds {
            x: 100.0,
            y: 50.0,
            width: 1200.0,
            height: 800.0,
        };
        let bounds = notification_bounds(NotificationPosition::default(), area, 380.0, 120.0);
        assert_eq!(bounds.x, 904.0);
        assert_eq!(bounds.y, 714.0);
    }

    #[test]
    fn oversized_notification_is_clamped_to_the_work_area() {
        let bounds = notification_bounds(
            NotificationPosition::TopLeft,
            PhysicalDisplayBounds {
                x: 20.0,
                y: 40.0,
                width: 180.0,
                height: 120.0,
            },
            380.0,
            240.0,
        );
        assert_eq!(bounds.x, 36.0);
        assert_eq!(bounds.y, 56.0);
        assert_eq!(bounds.width, 148.0);
        assert_eq!(bounds.height, 88.0);
    }

    #[test]
    fn animation_direction_follows_the_selected_edge() {
        let area = PhysicalDisplayBounds {
            x: 100.0,
            y: 50.0,
            width: 1200.0,
            height: 800.0,
        };
        let expected = [
            ((116.0, 66.0), (-36.0, -36.0)),
            ((510.0, 66.0), (0.0, -36.0)),
            ((904.0, 66.0), (36.0, -36.0)),
            ((116.0, 390.0), (-36.0, 0.0)),
            ((904.0, 390.0), (36.0, 0.0)),
            ((116.0, 714.0), (-36.0, 36.0)),
            ((510.0, 714.0), (0.0, 36.0)),
            ((904.0, 714.0), (36.0, 36.0)),
        ];
        for (position, (origin, offset)) in NotificationPosition::ALL.into_iter().zip(expected) {
            let bounds = notification_bounds(position, area, 380.0, 120.0);
            assert_eq!((bounds.x, bounds.y), origin, "{position:?}");
            assert_eq!(animation_offset(position), offset, "{position:?}");
        }
    }

    #[test]
    fn notification_bounds_keep_negative_monitor_origins() {
        let bounds = notification_bounds(
            NotificationPosition::TopLeft,
            PhysicalDisplayBounds {
                x: -1600.0,
                y: -120.0,
                width: 1600.0,
                height: 900.0,
            },
            380.0,
            120.0,
        );
        assert_eq!(
            bounds,
            PhysicalBounds {
                x: -1584.0,
                y: -104.0,
                width: 380.0,
                height: 120.0,
            }
        );
    }
}
