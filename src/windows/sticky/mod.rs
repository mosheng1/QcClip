//! 贴在屏幕上的便签窗口：每个剪贴板条目一个独立的置顶小窗，
//! 支持文本查看与就地编辑、拆词选择、图片、文件和网页内容。

mod image_view;
mod palette;
mod selection;
mod view;

use gpui::{
    App, AppContext as _, BorrowAppContext as _, Bounds, Entity, Global, Pixels, TitlebarOptions,
    WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowHandle, WindowKind,
    WindowOptions, point, px, size,
};
use gpui_component::Root;

use crate::{
    config::ClipboardDisplayPriority,
    domain::{
        capture::FormatRole,
        clipboard::{
            ClipboardItemId, ContentKind, FormatPreview, select_user_visible_previews_with_priority,
        },
    },
    services::{
        custom_text::CustomTextService,
        database::{HistoryRepository, query::HistoryQuery},
        image_preview::ImagePreviewService,
        paste::PasteService,
    },
    state_runtime::{self, PersistedStickyNote},
};

use view::StickyNote;

const HEADER_HEIGHT: f32 = 34.0;
const MIN_WIDTH: f32 = 180.0;
/// 折叠后的胶囊尺寸；窗口最小尺寸按胶囊设置，否则无法缩到胶囊大小。
const CAPSULE_WIDTH: f32 = 168.0;
const CAPSULE_HEIGHT: f32 = 30.0;
const DEFAULT_TEXT_WIDTH: f32 = 340.0;
const MAX_IMAGE_EDGE: f32 = 440.0;
const CASCADE_OFFSET: f32 = 28.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StickyMode {
    Text,
    Html,
    Image,
    File,
}

impl StickyMode {
    fn label_key(self) -> &'static str {
        match self {
            Self::Text => "sticky.mode.text",
            Self::Html => "sticky.mode.html",
            Self::Image => "sticky.mode.image",
            Self::File => "sticky.mode.file",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Text => "file-text",
            Self::Html => "typography",
            Self::Image => "photo",
            Self::File => "folder",
        }
    }
}

/// 打开便签前在后台读取的内容快照。
#[derive(Clone)]
pub(super) struct StickyData {
    item_id: ClipboardItemId,
    modes: Vec<(StickyMode, FormatPreview)>,
    /// 完整纯文本，只在存在文本格式时读取。
    text: Option<String>,
}

struct StickyEntry {
    item_id: ClipboardItemId,
    window: WindowHandle<Root>,
    view: Entity<StickyNote>,
}

#[derive(Clone)]
pub(super) struct StickyServices {
    pub(super) history_repository: HistoryRepository,
    pub(super) paste_service: PasteService,
    pub(super) custom_text_service: CustomTextService,
    pub(super) image_preview_service: ImagePreviewService,
}

pub(crate) struct StickyService {
    services: StickyServices,
    notes: Vec<StickyEntry>,
    /// 正在后台读取、尚未打开窗口的条目，避免连续点击重复创建。
    pending: Vec<ClipboardItemId>,
    quitting: bool,
}

impl Global for StickyService {}

impl StickyService {
    pub(crate) fn install(
        history_repository: HistoryRepository,
        paste_service: PasteService,
        custom_text_service: CustomTextService,
        image_preview_service: ImagePreviewService,
        cx: &mut App,
    ) {
        cx.set_global(Self {
            services: StickyServices {
                history_repository,
                paste_service,
                custom_text_service,
                image_preview_service,
            },
            notes: Vec::new(),
            pending: Vec::new(),
            quitting: false,
        });
        cx.on_window_closed(|cx, window_id| {
            let removed = cx.update_global::<Self, _>(|service, _| {
                if service.quitting {
                    return false;
                }
                let before = service.notes.len();
                service
                    .notes
                    .retain(|entry| entry.window.window_id() != window_id);
                service.notes.len() != before
            });
            if removed {
                Self::persist(cx);
            }
        })
        .detach();
        cx.on_app_quit(|cx| {
            Self::persist(cx);
            cx.update_global::<Self, _>(|service, _| service.quitting = true);
            async {}
        })
        .detach();
    }

    /// 数据导入或切换数据目录后，旧条目 ID 已失效：关闭现有便签并按新状态重新恢复。
    pub(crate) fn replace_data_sources(
        database: crate::services::database::Database,
        blob_store: crate::services::storage::RawBlobStore,
        clipboard_write_suppressor: crate::platform::clipboard::ClipboardWriteSuppressor,
        cx: &mut App,
    ) {
        if cx.try_global::<Self>().is_none() {
            return;
        }
        let windows = cx.update_global::<Self, _>(|service, _| {
            service.services = StickyServices {
                history_repository: HistoryRepository::new(database.clone(), blob_store.clone()),
                paste_service: PasteService::with_clipboard_write_suppressor(
                    database.clone(),
                    blob_store.clone(),
                    clipboard_write_suppressor,
                ),
                custom_text_service: CustomTextService::new(database, blob_store.clone()),
                image_preview_service: ImagePreviewService::new(blob_store),
            };
            service.pending.clear();
            service
                .notes
                .drain(..)
                .map(|entry| entry.window)
                .collect::<Vec<_>>()
        });
        for window in windows {
            let _ = window.update(cx, |_, window, _| window.remove_window());
        }
        Self::restore(cx);
    }

    /// 把条目贴到屏幕上；已经贴出的条目只会被提到最前并闪烁提示。
    pub(crate) fn open(item_id: ClipboardItemId, cx: &mut App) {
        if Self::focus_existing(item_id, false, cx) {
            return;
        }
        Self::load_and_open(item_id, None, cx);
    }

    /// 贴到屏幕并直接进入拆词；已经贴出的便签会切换到拆词界面。
    pub(crate) fn open_split(item_id: ClipboardItemId, cx: &mut App) {
        if Self::focus_existing(item_id, true, cx) {
            return;
        }
        let mut note =
            PersistedStickyNote::new(item_id, state_runtime::state(cx).sticky_preferences);
        note.split = true;
        Self::load_and_open(item_id, Some(note), cx);
    }

    /// 启动时恢复上次退出前贴在屏幕上的便签。
    pub(crate) fn restore(cx: &mut App) {
        for note in state_runtime::state(cx).sticky_notes {
            if note.item_id > 0 {
                Self::load_and_open(note.item_id, Some(note), cx);
            }
        }
    }

    pub(crate) fn close_all(cx: &mut App) {
        let windows = cx.update_global::<Self, _>(|service, _| {
            service
                .notes
                .drain(..)
                .map(|entry| entry.window)
                .collect::<Vec<_>>()
        });
        for window in windows {
            let _ = window.update(cx, |_, window, _| window.remove_window());
        }
        Self::persist(cx);
    }

    pub(super) fn services(cx: &App) -> StickyServices {
        cx.global::<Self>().services.clone()
    }

    pub(super) fn persist(cx: &mut App) {
        let Some(service) = cx.try_global::<Self>() else {
            return;
        };
        if service.quitting {
            return;
        }
        let notes = service
            .notes
            .iter()
            .map(|entry| entry.view.read(cx).persisted())
            .collect::<Vec<_>>();
        state_runtime::set_sticky_notes(notes, cx);
    }

    fn focus_existing(item_id: ClipboardItemId, split: bool, cx: &mut App) -> bool {
        let Some((window, view)) = cx.read_global::<Self, _>(|service, _| {
            service
                .notes
                .iter()
                .find(|entry| entry.item_id == item_id)
                .map(|entry| (entry.window, entry.view.clone()))
        }) else {
            return cx.global::<Self>().pending.contains(&item_id);
        };
        window
            .update(cx, |_, window, cx| {
                if let Err(error) = crate::platform::window::raise_window_to_topmost(window) {
                    tracing::debug!(%error, "提升便签窗口层级失败");
                }
                view.update(cx, |note, cx| {
                    if split {
                        note.show_split(cx);
                    }
                    note.flash(window, cx);
                });
            })
            .is_ok()
    }

    fn load_and_open(
        item_id: ClipboardItemId,
        restored: Option<PersistedStickyNote>,
        cx: &mut App,
    ) {
        cx.update_global::<Self, _>(|service, _| service.pending.push(item_id));
        let services = Self::services(cx);
        let roles =
            display_priority_roles(crate::config_runtime::clipboard_config(cx).display_priority);
        let task = cx.background_spawn(async move { load_data(&services, item_id, &roles) });
        cx.spawn(async move |cx| {
            let result = task.await;
            cx.update(|cx| {
                cx.update_global::<Self, _>(|service, _| {
                    service.pending.retain(|pending| *pending != item_id);
                });
                match result {
                    Ok(Some(data)) => {
                        if let Err(error) = Self::open_window(data, restored, cx) {
                            tracing::error!("打开便签窗口失败：{error}");
                        }
                    }
                    Ok(None) => {
                        tracing::info!(item_id, "便签对应的内容已不存在，已移除");
                        Self::persist(cx);
                    }
                    Err(error) => tracing::error!(item_id, "读取便签内容失败：{error}"),
                }
            });
        })
        .detach();
    }

    fn open_window(
        data: StickyData,
        restored: Option<PersistedStickyNote>,
        cx: &mut App,
    ) -> Result<(), String> {
        let item_id = data.item_id;
        let mut note = restored.unwrap_or_else(|| {
            PersistedStickyNote::new(item_id, state_runtime::state(cx).sticky_preferences)
        });
        note.preferences = note.preferences.normalized();
        let bounds = restored_bounds(&note, cx).unwrap_or_else(|| {
            let cascade = cx.global::<Self>().notes.len() % 8;
            initial_bounds(&data, cascade, cx)
        });
        let collapsed = note.collapsed;
        let window_bounds = if collapsed {
            Bounds::new(bounds.origin, size(px(CAPSULE_WIDTH), px(CAPSULE_HEIGHT)))
        } else {
            bounds
        };
        let topmost = note.preferences.topmost;
        let opacity = note.preferences.opacity;
        let view_slot = std::rc::Rc::new(std::cell::RefCell::new(None));
        let view_slot_for_build = view_slot.clone();
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(window_bounds)),
                    titlebar: Some(TitlebarOptions {
                        appears_transparent: true,
                        ..Default::default()
                    }),
                    kind: WindowKind::Normal,
                    focus: false,
                    show: true,
                    is_movable: true,
                    is_resizable: true,
                    is_minimizable: false,
                    window_background: WindowBackgroundAppearance::Transparent,
                    window_decorations: Some(WindowDecorations::Client),
                    window_min_size: Some(size(px(CAPSULE_WIDTH), px(CAPSULE_HEIGHT))),
                    ..Default::default()
                },
                move |window, cx| {
                    crate::platform::window::configure_frameless_window(window, false);
                    if let Err(error) =
                        crate::platform::window::configure_sticky_window(window, topmost)
                    {
                        tracing::error!("设置便签窗口样式失败：{error}");
                    }
                    if opacity < 0.999
                        && let Err(error) =
                            crate::platform::window::set_window_opacity(window, opacity)
                    {
                        tracing::error!("恢复便签透明度失败：{error}");
                    }
                    window.bounds_changed(cx);
                    // 便签与主题窗口共用窗口材质，背景遮罩由便签按自身颜色绘制。
                    crate::window_material::register_window(window, cx);
                    crate::ui::theme::attach_system_appearance_watch(window, cx);
                    let view = cx.new(|cx| {
                        StickyNote::new(
                            data,
                            note,
                            window_bounds,
                            size(f32::from(bounds.size.width), f32::from(bounds.size.height)),
                            window,
                            cx,
                        )
                    });
                    *view_slot_for_build.borrow_mut() = Some(view.clone());
                    cx.new(|cx| {
                        gpui::Styled::bg(Root::new(view, window, cx), gpui::transparent_black())
                    })
                },
            )
            .map_err(|error| error.to_string())?;
        let Some(view) = view_slot.borrow_mut().take() else {
            return Err("便签视图未创建".to_owned());
        };
        cx.update_global::<Self, _>(|service, _| {
            service.notes.push(StickyEntry {
                item_id,
                window,
                view,
            });
        });
        Self::persist(cx);
        Ok(())
    }
}

fn load_data(
    services: &StickyServices,
    item_id: ClipboardItemId,
    roles: &[FormatRole; 3],
) -> Result<Option<StickyData>, String> {
    let query = HistoryQuery {
        load_image_sources: true,
        load_html_content: true,
        ..Default::default()
    };
    let Some((_, previews)) = services
        .history_repository
        .find_item_previews(item_id, &query)
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let mut modes: Vec<(StickyMode, FormatPreview)> = Vec::new();
    for preview in select_user_visible_previews_with_priority(&previews, roles) {
        if let Some(mode) = mode_for_preview(&preview)
            && !modes.iter().any(|(existing, _)| *existing == mode)
        {
            modes.push((mode, preview));
        }
    }
    let text = if modes.iter().any(|(mode, _)| *mode == StickyMode::Text) {
        let full_text = services
            .paste_service
            .item_plain_text(item_id)
            .map_err(|error| error.to_string())?;
        full_text.or_else(|| {
            modes
                .iter()
                .find(|(mode, _)| *mode == StickyMode::Text)
                .map(|(_, preview)| preview.text.clone())
        })
    } else {
        None
    };
    Ok(Some(StickyData {
        item_id,
        modes,
        text,
    }))
}

fn mode_for_preview(preview: &FormatPreview) -> Option<StickyMode> {
    match preview.role() {
        FormatRole::PlainText | FormatRole::Rtf => Some(StickyMode::Text),
        FormatRole::Html if preview.html_content.is_some() => Some(StickyMode::Html),
        FormatRole::Image if preview.image_source.is_some() => Some(StickyMode::Image),
        FormatRole::FileList | FormatRole::VirtualFile if preview.image_source.is_some() => {
            if preview.content_kind == ContentKind::Image {
                Some(StickyMode::Image)
            } else {
                Some(StickyMode::File)
            }
        }
        _ => None,
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

/// 恢复保存的位置；窗口已完全离开所有显示器时返回 None，改用默认位置。
fn restored_bounds(note: &PersistedStickyNote, cx: &App) -> Option<Bounds<Pixels>> {
    if !(note.width.is_finite()
        && note.height.is_finite()
        && note.x.is_finite()
        && note.y.is_finite())
        || note.width < MIN_WIDTH
        || note.height < HEADER_HEIGHT
    {
        return None;
    }
    let bounds = Bounds::new(
        point(px(note.x), px(note.y)),
        size(px(note.width), px(note.height)),
    );
    // 标题栏至少有一部分落在某个显示器内，才能保证用户还能拖动它。
    let header = Bounds::new(bounds.origin, size(bounds.size.width, px(HEADER_HEIGHT)));
    cx.displays()
        .iter()
        .any(|display| display.bounds().intersects(&header))
        .then_some(bounds)
}

fn initial_bounds(data: &StickyData, cascade: usize, cx: &App) -> Bounds<Pixels> {
    let (width, height) = match data.modes.first().map(|(mode, preview)| (*mode, preview)) {
        Some((StickyMode::Image, preview)) => {
            let (image_width, image_height) = preview
                .image_dimensions
                .map(|dimensions| (dimensions.width as f32, dimensions.height as f32))
                .filter(|(width, height)| *width > 0.0 && *height > 0.0)
                .unwrap_or((MAX_IMAGE_EDGE, MAX_IMAGE_EDGE * 0.75));
            let scale = (MAX_IMAGE_EDGE / image_width.max(image_height)).min(1.0);
            (
                (image_width * scale).max(MIN_WIDTH),
                (image_height * scale).max(80.0) + HEADER_HEIGHT,
            )
        }
        Some((StickyMode::File, _)) => (DEFAULT_TEXT_WIDTH, 260.0),
        Some((StickyMode::Html, _)) => (380.0, 320.0),
        _ => {
            // 短文本使用紧凑高度，长文本最多占用约 14 行。
            let text = data.text.as_deref().unwrap_or_default();
            let approximate_lines = text
                .lines()
                .map(|line| (line.chars().count() as f32 / 22.0).ceil().max(1.0))
                .sum::<f32>()
                .clamp(2.0, 14.0);
            (
                DEFAULT_TEXT_WIDTH,
                HEADER_HEIGHT + 28.0 + approximate_lines * 22.0,
            )
        }
    };
    let mut bounds = Bounds::centered(None, size(px(width), px(height)), cx);
    let offset = px(CASCADE_OFFSET * cascade as f32);
    bounds.origin.x += offset;
    bounds.origin.y += offset;
    bounds
}
