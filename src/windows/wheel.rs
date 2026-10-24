use std::time::Duration;

use crate::platform::window as platform_window;
use crate::services::database::{FavoriteRepository, query::HistoryQuery};
use crate::services::paste::PasteService;
use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, Global, Path, PathBuilder, Point, Render, Window, WindowHandle, canvas,
    div, point, px, rgba, size,
};

const WHEEL_DIAMETER: f32 = 340.0;
/// 选定判定中心死区半径（与中键阈值类似的防误触）。
const WHEEL_CENTER_RADIUS: f32 = 52.0;
const WHEEL_SECTOR_COUNT: usize = 6;
/// 扇区环形内/外半径（逻辑像素）。
const WHEEL_SECTOR_INNER_RADIUS: f32 = 58.0;
const WHEEL_SECTOR_OUTER_RADIUS: f32 = 164.0;
/// 相邻扇区之间的视觉缝隙（度）。
const WHEEL_SECTOR_GAP_DEG: f32 = 1.5;
const WHEEL_SECTOR_ANGLE_DEG: f32 = 360.0 / WHEEL_SECTOR_COUNT as f32;
/// 扇区中心放置标签的半径与标签盒尺寸。
const WHEEL_LABEL_RADIUS: f32 = (WHEEL_SECTOR_INNER_RADIUS + WHEEL_SECTOR_OUTER_RADIUS) / 2.0;
const WHEEL_LABEL_WIDTH: f32 = 104.0;
const WHEEL_LABEL_HEIGHT: f32 = 40.0;
const WHEEL_TITLE_MAX_CHARS: usize = 10;
const WHEEL_PREVIEW_MAX_CHARS: usize = 20;

const WHEEL_SECTOR_FILL: u32 = 0x23232BE6;
const WHEEL_SECTOR_HOVER_FILL: u32 = 0x3D6BFFF2;
const WHEEL_SECTOR_EMPTY_FILL: u32 = 0x1A1A20A0;
const WHEEL_CENTER_FILL: u32 = 0x16161CCC;
const WHEEL_CENTER_BORDER: u32 = 0x3A3A4699;

/// 轮盘打开期间生效的外观快照。
#[derive(Clone, Debug)]
struct WheelColors {
    sector: u32,
    hover: u32,
    empty: u32,
    center: u32,
    center_border: u32,
    /// 标题/预览/页码文字颜色与字体设置。
    title: u32,
    preview: u32,
    page: u32,
    font_family: Option<gpui::SharedString>,
    title_font_size: f32,
    preview_font_size: f32,
}

impl Default for WheelColors {
    fn default() -> Self {
        Self {
            sector: WHEEL_SECTOR_FILL,
            hover: WHEEL_SECTOR_HOVER_FILL,
            empty: WHEEL_SECTOR_EMPTY_FILL,
            center: WHEEL_CENTER_FILL,
            center_border: WHEEL_CENTER_BORDER,
            title: 0xEDEDF2,
            preview: 0x9A9AA5,
            page: 0xE8E8ED,
            font_family: None,
            title_font_size: crate::config::WHEEL_TITLE_FONT_SIZE_DEFAULT,
            preview_font_size: crate::config::WHEEL_PREVIEW_FONT_SIZE_DEFAULT,
        }
    }
}

impl WheelColors {
    /// 从外观配置推导：默认色按亮暗模式选择，用户配置覆盖非空项；文字/字体同样可覆盖。
    fn resolve(cx: &App) -> Self {
        let appearance = crate::config_runtime::appearance_config(cx);
        let wheel = &appearance.wheel_appearance;
        let mut colors = Self::default();
        let dark = gpui_component::Theme::global(cx).mode.is_dark();
        if !dark {
            colors.sector = 0xFFFFFFD9;
            colors.hover = 0x3D6BFFF2;
            colors.empty = 0xF2F2F780;
            colors.center = 0xFFFFFFE6;
            colors.center_border = 0xC9C9D480;
            // 亮色模式下文字换深色系。
            colors.title = 0x1F1F24;
            colors.preview = 0x5A5A66;
            colors.page = 0x4A4A55;
        }
        if !dark {
            // 亮色模式下预览色略深保证对比。
            colors.preview = 0x5A5A66;
        }
        if let Some(color) = parse_hex_rgba(&wheel.sector_color) {
            colors.sector = color;
        }
        if let Some(color) = parse_hex_rgba(&wheel.hover_color) {
            colors.hover = color;
        }
        if let Some(color) = parse_hex_rgba(&wheel.center_color) {
            colors.center = color;
        }
        if let Some(color) = parse_hex_rgba(&wheel.title_color) {
            colors.title = color;
        }
        if let Some(color) = parse_hex_rgba(&wheel.preview_color) {
            colors.preview = color;
        }
        if let Some(color) = parse_hex_rgba(&wheel.page_color) {
            colors.page = color;
        }
        colors.font_family = wheel
            .font_family
            .as_ref()
            .map(|family| gpui::SharedString::from(family.clone()));
        colors.title_font_size = wheel.title_font_size;
        colors.preview_font_size = wheel.preview_font_size;
        colors
    }
}

/// 按比例压低 0xRRGGBBAA 的 Alpha 通道，用于材质模式下让背景透出来。
fn with_reduced_alpha(color: u32, factor: f32) -> u32 {
    let alpha = (color & 0xFF) as f32 * factor.clamp(0.0, 1.0);
    ((color & 0xFFFFFF00) as u32) | (alpha.round() as u32 & 0xFF)
}

/// 解析 "RRGGBB"、"RRGGBBAA"、"RGB" 十六进制颜色为 0xRRGGBBAA。
/// 非法或为空返回 None（调用方保持原色）；"RRGGBB" 补 FF 不透明度。
fn parse_hex_rgba(value: &str) -> Option<u32> {
    let value = value.trim().trim_start_matches('#');
    if value.is_empty() {
        return None;
    }
    let expand = |c: char| -> Option<u32> { c.to_digit(16).map(|d| d * 16 + d) };
    let channels: Vec<u32> = match value.len() {
        3 => value
            .chars()
            .map(|c| expand(c).ok_or(()))
            .collect::<Result<_, _>>()
            .ok()?,
        6 | 8 => (0..value.len())
            .step_by(2)
            .map(|i| u32::from_str_radix(&value[i..i + 2], 16).map_err(|_| ()))
            .collect::<Result<_, _>>()
            .ok()?,
        _ => return None,
    };
    let mut out = 0u32;
    for channel in &channels {
        out = (out << 8) | channel;
    }
    if channels.len() == 3 {
        out = (out << 8) | 0xFF;
    }
    Some(out)
}

/// 轮盘扇区数据。
#[derive(Debug, Clone)]
struct WheelEntry {
    item_id: i64,
    title: String,
    preview: String,
}

#[derive(Default)]
pub(crate) struct WheelWindowService {
    favorite_repository: Option<FavoriteRepository>,
    paste_service: Option<PasteService>,
    window: Option<WindowHandle<WheelWindowView>>,
}

impl Global for WheelWindowService {}

impl WheelWindowService {
    pub(crate) fn install(
        favorite_repository: FavoriteRepository,
        paste_service: PasteService,
        cx: &mut App,
    ) {
        cx.set_global(Self {
            favorite_repository: Some(favorite_repository),
            paste_service: Some(paste_service),
            window: None,
        });
    }

    pub(crate) fn replace_data_sources(
        database: crate::services::database::Database,
        blob_store: crate::services::storage::RawBlobStore,
        clipboard_write_suppressor: crate::platform::clipboard::ClipboardWriteSuppressor,
        cx: &mut App,
    ) {
        let favorite_repository =
            FavoriteRepository::with_blob_store(database.clone(), blob_store.clone());
        let paste_service = PasteService::with_clipboard_write_suppressor(
            database,
            blob_store,
            clipboard_write_suppressor,
        );
        Self::close(cx);
        cx.update_global::<Self, _>(|service, _| {
            service.favorite_repository = Some(favorite_repository);
            service.paste_service = Some(paste_service);
        });
    }

    pub(crate) fn close(cx: &mut App) {
        let window = cx.update_global::<Self, _>(|service, _| service.window.take());
        if let Some(window) = window {
            let _ = window.update(cx, |_, window, _| window.remove_window());
        }
    }
}

/// 供输入层兜底：轮盘尚未弹出时关闭（幂等，无窗口则什么都不做）。
pub(crate) fn close_wheel_window(cx: &mut App) {
    WheelWindowService::close(cx);
}

/// 轮盘打开期间的指针起点、分页状态与已加载条目。
struct WheelSession {
    origin: Option<Point<f32>>,
    scale: f32,
    /// 当前页的 6 个槽位。
    entries: Vec<WheelEntry>,
    /// 总收藏条数（含各页）。
    total: usize,
    /// 当前页码，从 0 开始。
    page: usize,
    /// 触发侧键仍按住时左/右键翻页的原始输入监听；松开后移除。
    _page_listener: Option<crate::platform::input::WheelPageListener>,
    /// 左/右键点击拦截（防点击穿透到底下窗口）；轮盘关闭即解除。
    _click_guard: Option<crate::platform::input::WheelClickSwallowGuard>,
    /// 打开时解析的外观配色快照。
    colors: WheelColors,
    /// 打开瞬间截取的背景截图（已做 GPU 模糊前的原始像素），用于亚克力效果。
    backdrop: Option<std::sync::Arc<gpui::RenderImage>>,
}

impl Default for WheelSession {
    fn default() -> Self {
        Self {
            origin: None,
            scale: 1.0,
            entries: Vec::new(),
            total: 0,
            page: 0,
            _page_listener: None,
            _click_guard: None,
            colors: WheelColors::default(),
            backdrop: None,
        }
    }
}

impl WheelSession {
    fn total_pages(&self) -> usize {
        self.total.div_ceil(WHEEL_SECTOR_COUNT).max(1)
    }
}

impl Global for WheelSession {}

/// 侧键按住达到阈值：打开轮盘。返回是否成功进入“等待松开”状态。
pub(crate) fn open_wheel_window(cx: &mut App) -> bool {
    let should_open =
        cx.update_global::<WheelWindowService, _>(|service, _| service.window.is_none());
    if !should_open {
        return false;
    }
    let page = load_wheel_page(cx, 0);
    if page.slots.iter().filter(|entry| entry.item_id != 0).count() < 2 {
        return false;
    }
    let Some(pointer) = platform_window::physical_cursor_position() else {
        return false;
    };
    let display = platform_window::work_area_for_physical_point(cx, pointer);
    let scale = display
        .map(|display| display.scale_factor)
        .unwrap_or(1.0)
        .max(1.0);
    // 先算出窗口最终物理位置（可能被工作区夹取，不等同于光标位置），
    // 截图必须按这个位置取，否则模糊底图会与轮盘错位、取到旁边的内容。
    let diameter = WHEEL_DIAMETER * scale;
    let (window_x, window_y) = match display.map(|display| display.bounds) {
        Some(bounds) => {
            let margin = 8.0 * scale;
            centered_overlay_origin(pointer, diameter, diameter, margin, bounds)
        }
        None => (pointer.x - diameter / 2.0, pointer.y - diameter / 2.0),
    };

    cx.set_global(WheelSession {
        // 判定圆心取窗口真实中心，而不是鼠标位置：窗口贴边时会被夹回工作区，
        // 用鼠标位置做圆心会导致扇区与视觉错位、贴边侧选不中。
        origin: Some(point(window_x + diameter / 2.0, window_y + diameter / 2.0)),
        scale,
        entries: page.slots,
        total: page.total,
        page: 0,
        _page_listener: None,
        _click_guard: None,
        colors: WheelColors::resolve(cx),
        backdrop: capture_wheel_backdrop(window_x, window_y, diameter),
    });
    // 轮盘存续期间监听左/右键翻页 + 拦截点击穿透；窗口关闭（Session 清理）后任务自动退出。
    let (page_listener, page_receiver) = crate::platform::input::WheelPageListener::install();
    let click_guard = crate::platform::input::WheelClickSwallowGuard::install();
    cx.update_global::<WheelSession, _>(|session, _| {
        session._page_listener = Some(page_listener);
        session._click_guard = click_guard;
    });
    cx.spawn(async move |cx| {
        while let Ok(forward) = page_receiver.recv().await {
            let alive = cx.update(|cx| {
                cx.update_global::<WheelSession, _>(|session, _| session._page_listener.is_some())
            });
            if !alive {
                break;
            }
            cx.update(|cx| page_wheel(forward, cx));
        }
    })
    .detach();

    let result = platform_window::open_borderless_overlay(
        cx,
        Bounds {
            origin: point(px(window_x / scale), px(window_y / scale)),
            size: size(px(WHEEL_DIAMETER), px(WHEEL_DIAMETER)),
        },
        display.map(|display| display.display_id),
        |_window, cx, popup| cx.new(|_| WheelWindowView::new(popup)),
    );
    match result {
        Ok(window) => {
            let handle = window.clone();
            cx.update_global::<WheelWindowService, _>(|service, _| {
                service.window = Some(window);
            });
            // 初始定位到光标处；resize 完成后应用「椭圆裁剪 + 主窗口同款亚克力」。
            // SetWindowRgn 会被 resize/framechanged 重置，所以在这里以及随后
            // 的延迟重放里各执行一次，确保最终窗口区域是圆形。
            let wheel_appearance = crate::config_runtime::appearance_config(cx).wheel_appearance;
            let wheel_material = wheel_appearance.material;
            let wheel_brightness = wheel_appearance.material_brightness;
            let light_mode = !gpui_component::Theme::global(cx).mode.is_dark();
            let _ = handle.update(cx, |view, window, cx| {
                view.place_window(window, cx);
                #[cfg(target_os = "windows")]
                if let Err(error) = platform_window::apply_circle_window_material(
                    window,
                    wheel_material,
                    wheel_brightness,
                    light_mode,
                ) {
                    tracing::error!("应用轮盘材质失败：{error}");
                }
            });
            // 二次重放：resize 的 WM_SIZE/FRAMECHANGED 会清掉窗口区域。
            #[cfg(target_os = "windows")]
            {
                let replay_handle = handle.clone();
                cx.spawn(async move |cx| {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(24))
                        .await;
                    let _ = replay_handle.update(cx, |_, window, _| {
                        if let Err(error) = platform_window::apply_circle_window_material(
                            window,
                            wheel_material,
                            wheel_brightness,
                            light_mode,
                        ) {
                            tracing::debug!("轮盘圆形区域二次应用失败：{error}");
                        }
                    });
                })
                .detach();
            }
            start_wheel_pointer_tracking(cx);
            true
        }
        Err(error) => {
            tracing::error!("打开收藏轮盘失败：{error}");
            false
        }
    }
}

/// 轮盘打开期间以 ~60fps 重绘，让指向的扇区实时高亮。
fn start_wheel_pointer_tracking(cx: &mut App) {
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(16))
                .await;
            let window = cx.update(|cx| {
                cx.update_global::<WheelWindowService, _>(|service, _| service.window.clone())
            });
            let Some(window) = window else { break };
            let _ = window.update(cx, |_, _, cx| cx.notify());
        }
    })
    .detach();
}

/// 轮盘打开期间的翻页：`forward` 为 true 表示下一页（右键），false 上一页（左键）。
/// 无轮盘、单页或越界时为无操作。
pub(crate) fn page_wheel(forward: bool, cx: &mut App) {
    let next = cx.update_global::<WheelSession, _>(|session, _| {
        let pages = session.total_pages();
        if pages <= 1 {
            return None;
        }
        let page = session.page;
        let next = if forward {
            (page + 1) % pages
        } else {
            page.checked_sub(1).unwrap_or(pages - 1)
        };
        (next != page).then_some(next)
    });
    let Some(next) = next else {
        return;
    };
    let page = load_wheel_page(cx, next);
    cx.update_global::<WheelSession, _>(|session, _| {
        session.page = next;
        session.total = page.total;
        session.entries = page.slots;
    });
}

/// 关闭会话状态：清掉条目并移除翻页监听与点击拦截（窗口由 WheelWindowService 负责关）。
fn reset_wheel_session(cx: &mut App) {
    cx.update_global::<WheelSession, _>(|session, _| {
        session._page_listener = None;
        session._click_guard = None;
        session.entries.clear();
        session.total = 0;
        session.page = 0;
        session.colors = WheelColors::default();
        session.backdrop = None;
    });
}

/// 侧键松开：按当前指向选定并关闭。
pub(crate) fn commit_wheel_selection(cx: &mut App) {
    let (delta, entries) = cx.update_global::<WheelSession, _>(|session, _| {
        (
            session
                .origin
                .zip(platform_window::physical_cursor_position())
                .map(|(origin, cursor)| {
                    (
                        f64::from(cursor.x - origin.x),
                        f64::from(cursor.y - origin.y),
                    )
                }),
            session.entries.clone(),
        )
    });
    let index = delta.and_then(|(dx, dy)| sector_index_for_delta(dx, dy));
    WheelWindowService::close(cx);
    reset_wheel_session(cx);
    let Some(index) = index.filter(|index| *index < entries.len()) else {
        return;
    };
    let entry = &entries[index];
    // 空槽（item_id == 0）或标题为空都视为无内容，不粘贴。
    if entry.item_id == 0 || entry.title.is_empty() {
        return;
    }
    let item_id = entry.item_id;
    let paste_service =
        cx.update_global::<WheelWindowService, _>(|service, _| service.paste_service.clone());
    let Some(paste_service) = paste_service else {
        return;
    };
    cx.spawn(async move |cx| {
        // 等轮盘窗口彻底消失，Ctrl+V 才能落到用户原本的前台窗口。
        cx.background_executor()
            .timer(WHEEL_CLOSE_SETTLE_DURATION)
            .await;
        let paste_task = cx.background_spawn(async move {
            paste_service
                .paste_item(item_id, None)
                .map_err(|error| error.to_string())
        });
        match paste_task.await {
            Ok(()) => cx.update(|cx| crate::windows::main::MainWindowService::refresh_history(cx)),
            Err(error) => tracing::error!("轮盘粘贴失败：{error}"),
        }
    })
    .detach();
}

/// 一页轮盘数据：6 个槽位 + 总条数。
struct WheelPage {
    slots: Vec<WheelEntry>,
    total: usize,
}

/// 在轮盘窗口出现前，按窗口最终物理位置截取方形屏幕区域，作为模糊底图。
/// 必须用窗口位置而不是光标位置：窗口会被工作区夹取，两者可能不一致。
/// 失败时返回 None（轮盘退化为半透明圆盘，不影响功能）。
fn capture_wheel_backdrop(
    window_x: f32,
    window_y: f32,
    diameter: f32,
) -> Option<std::sync::Arc<gpui::RenderImage>> {
    // 截取尺寸按物理像素，最多 1024，避免超大屏时内存爆炸。
    let side = diameter.round().clamp(16.0, 1024.0) as u32;
    let left = window_x.round().max(0.0);
    let top = window_y.round().max(0.0);
    let mut rgba = crate::platform::window::capture_screen_region(left, top, side, side).ok()?;
    // GDI 截屏的 alpha 通道不可靠，统一补成不透明。
    for pixel in rgba.pixels_mut() {
        pixel.0[3] = 255;
    }
    Some(std::sync::Arc::new(gpui::RenderImage::new(vec![
        image::Frame::new(rgba),
    ])))
}

fn load_wheel_page(cx: &mut App, page: usize) -> WheelPage {
    let repository =
        cx.update_global::<WheelWindowService, _>(|service, _| service.favorite_repository.clone());
    let Some(repository) = repository else {
        return WheelPage {
            slots: Vec::new(),
            total: 0,
        };
    };
    let query = HistoryQuery {
        offset: (page * WHEEL_SECTOR_COUNT) as u32,
        limit: WHEEL_SECTOR_COUNT as u32,
        preview_char_limit: Some(160),
        load_image_sources: false,
        ..HistoryQuery::default()
    };
    let total = repository
        .count(&HistoryQuery {
            preview_char_limit: Some(1),
            load_image_sources: false,
            ..HistoryQuery::default()
        })
        .unwrap_or(0);
    match repository.query(&query) {
        Ok(records) => {
            let mut slots = vec![
                WheelEntry {
                    item_id: 0,
                    title: String::new(),
                    preview: String::new(),
                };
                WHEEL_SECTOR_COUNT
            ];
            for (index, record) in records.into_iter().take(WHEEL_SECTOR_COUNT).enumerate() {
                slots[index] = WheelEntry {
                    item_id: record.item_id,
                    title: truncate_chars(&record.preview_text, WHEEL_TITLE_MAX_CHARS),
                    preview: truncate_chars(&record.preview_text, WHEEL_PREVIEW_MAX_CHARS),
                };
            }
            WheelPage { slots, total }
        }
        Err(error) => {
            tracing::error!("读取收藏轮盘数据失败：{error}");
            WheelPage {
                slots: Vec::new(),
                total,
            }
        }
    }
}

/// 与 quick_paste 相同的物理坐标夹取逻辑。
fn centered_overlay_origin(
    pointer: Point<f32>,
    width: f32,
    height: f32,
    margin: f32,
    area: platform_window::PhysicalDisplayBounds,
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

/// 位移 → 扇区序号。扇区从正上方开始顺时针均分；中心死区返回 None。
fn sector_index_for_delta(dx: f64, dy: f64) -> Option<usize> {
    let distance = (dx * dx + dy * dy).sqrt();
    if distance < f64::from(WHEEL_CENTER_RADIUS) {
        return None;
    }
    // 屏幕坐标 y 向下；以正上方为 0，顺时针为正。
    let angle = (dx as f32).atan2(-(dy as f32));
    let angle = if angle < 0.0 {
        angle + std::f32::consts::TAU
    } else {
        angle
    };
    let sector_angle = std::f32::consts::TAU / WHEEL_SECTOR_COUNT as f32;
    Some(((angle + sector_angle / 2.0) / sector_angle).floor() as usize % WHEEL_SECTOR_COUNT)
}

/// 粘贴前等待窗口完全消失，避免 Ctrl+V 落进轮盘自身。
const WHEEL_CLOSE_SETTLE_DURATION: Duration = Duration::from_millis(100);

fn truncate_chars(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max {
        trimmed.to_owned()
    } else {
        let truncated: String = trimmed.chars().take(max).collect();
        format!("{truncated}…")
    }
}

/// 轮盘视图：透明圆窗，扇区卡片环绕布局，窗口自身接收鼠标移动更新高亮。
struct WheelWindowView {
    popup: platform_window::BorderlessPopup,
    placed: bool,
    /// place_window 记录的最终物理直径，供材质椭圆裁剪使用。
    window_physical_diameter: f32,
}

impl WheelWindowView {
    fn new(popup: platform_window::BorderlessPopup) -> Self {
        Self {
            popup,
            placed: false,
            window_physical_diameter: 0.0,
        }
    }

    fn place_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.placed {
            return;
        }
        self.placed = true;
        window.set_rem_size(gpui_component::Theme::global(cx).font_size);
        let (origin, scale, diameter) = cx.update_global::<WheelSession, _>(|session, _| {
            (
                session.origin,
                session.scale,
                WHEEL_DIAMETER * session.scale,
            )
        });
        let Some(origin) = origin else { return };
        let (x, y) = match platform_window::work_area_for_physical_point(cx, origin) {
            Some(display) => {
                let margin = 8.0 * scale;
                centered_overlay_origin(origin, diameter, diameter, margin, display.bounds)
            }
            None => (origin.x - diameter / 2.0, origin.y - diameter / 2.0),
        };
        if !self
            .popup
            .resize_to_physical_bounds(window, cx, x, y, diameter, diameter)
        {
            tracing::error!("调整收藏轮盘窗口大小失败");
        }
        self.window_physical_diameter = diameter;
    }
}

impl Render for WheelWindowView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (origin, cursor, entries, page, total_pages, colors, backdrop) = cx
            .update_global::<WheelSession, _>(|session, _| {
                (
                    session.origin,
                    platform_window::physical_cursor_position(),
                    session.entries.clone(),
                    session.page,
                    session.total_pages(),
                    session.colors.clone(),
                    session.backdrop.clone(),
                )
            });
        let hovered = origin
            .zip(cursor)
            .map(|(origin, cursor)| {
                sector_index_for_delta(
                    f64::from(cursor.x - origin.x),
                    f64::from(cursor.y - origin.y),
                )
            })
            .flatten();
        // 画布回调里要用的悬停/条目/配色快照。
        let canvas_hovered = hovered;
        let canvas_entries = entries.clone();
        // 材质由内容层实现：截屏 + GPU 高斯模糊，裁成圆形铺底。
        // 这样模糊是真的、形状是圆的，且不存在 DWM accent 那种比圆大的矩形灰块。
        let uses_system_material = matches!(
            crate::window_material::supported_material(
                crate::config_runtime::appearance_config(cx)
                    .wheel_appearance
                    .material
            ),
            crate::config::WindowMaterial::Acrylic | crate::config::WindowMaterial::Blur
        );
        let mut canvas_colors = colors;
        if uses_system_material {
            canvas_colors.sector = with_reduced_alpha(canvas_colors.sector, 0.55);
            canvas_colors.empty = with_reduced_alpha(canvas_colors.empty, 0.45);
            canvas_colors.hover = with_reduced_alpha(canvas_colors.hover, 0.80);
            canvas_colors.center = with_reduced_alpha(canvas_colors.center, 0.55);
            canvas_colors.center_border = with_reduced_alpha(canvas_colors.center_border, 0.60);
        }
        let center = point(px(WHEEL_DIAMETER / 2.0), px(WHEEL_DIAMETER / 2.0));
        // 模糊底图层：截图 + GPU blur + 大圆裁剪 + 明暗 tint。
        let dark_mode = gpui_component::Theme::global(cx).mode.is_dark();
        let brightness = crate::config_runtime::appearance_config(cx)
            .wheel_appearance
            .material_brightness;
        let blur_radius = if matches!(
            crate::window_material::supported_material(
                crate::config_runtime::appearance_config(cx)
                    .wheel_appearance
                    .material
            ),
            crate::config::WindowMaterial::Blur
        ) {
            28.0
        } else {
            18.0
        };
        let backdrop_layer = backdrop.filter(|_| uses_system_material).map(|image| {
            // 明暗：<1 压暗，>1 提亮；1.0 时完全不叠色。
            let tint = if brightness < 1.0 {
                let alpha = ((1.0 - brightness).clamp(0.0, 1.0) * 255.0).round() as u32;
                Some(gpui::rgba((alpha << 24) | 0x000000u32))
            } else if brightness > 1.0 {
                let alpha = ((brightness - 1.0).clamp(0.0, 1.0) * 200.0).round() as u32;
                Some(gpui::rgba((alpha << 24) | 0xFFFFFFu32))
            } else {
                None
            };
            let mut container = div()
                .absolute()
                .left_0()
                .top_0()
                .size(px(WHEEL_DIAMETER))
                .rounded_full()
                .overflow_hidden()
                .child(
                    gpui::img(image)
                        .size_full()
                        .object_fit(gpui::ObjectFit::Cover)
                        .blur(px(blur_radius)),
                );
            if let Some(color) = tint {
                container = container.child(div().absolute().inset_0().bg(color));
            }
            let _ = dark_mode;
            container
        });

        let mut root = div().relative().size_full();
        if let Some(layer) = backdrop_layer {
            root = root.child(layer);
        }
        root = root.child(
            canvas(
                move |_, _, _| {},
                move |bounds, _, window, _| {
                    // 画布原点在窗口左上角，扇区路径围绕画布中心绘制。
                    let offset_x = bounds.origin.x;
                    let offset_y = bounds.origin.y;
                    for index in 0..WHEEL_SECTOR_COUNT {
                        let filled = canvas_entries
                            .get(index)
                            .map(|entry| !entry.title.is_empty())
                            .unwrap_or(false);
                        let is_hovered = canvas_hovered == Some(index) && filled;
                        let fill = if is_hovered {
                            canvas_colors.hover
                        } else if filled {
                            canvas_colors.sector
                        } else {
                            canvas_colors.empty
                        };
                        if let Some(mut sector) = ring_sector_path(
                            center,
                            WHEEL_SECTOR_INNER_RADIUS,
                            WHEEL_SECTOR_OUTER_RADIUS,
                            index,
                        ) {
                            // Path 坐标以路径自身 bounds 为基准，平移到画布位置。
                            sector.bounds.origin = point(
                                sector.bounds.origin.x + offset_x,
                                sector.bounds.origin.y + offset_y,
                            );
                            window.paint_path(sector, rgba(fill));
                        }
                    }
                },
            )
            .absolute()
            .size_full(),
        );
        root = root.children(entries.iter().take(WHEEL_SECTOR_COUNT).enumerate().map(
            |(index, entry)| {
                let angle = sector_center_angle_deg(index);
                let rad = deg_to_rad(angle);
                // 与命中判定同约定：0°=正上方，顺时针。
                let label_x =
                    center.x + px(WHEEL_LABEL_RADIUS * rad.sin()) - px(WHEEL_LABEL_WIDTH / 2.0);
                let label_y =
                    center.y - px(WHEEL_LABEL_RADIUS * rad.cos()) - px(WHEEL_LABEL_HEIGHT / 2.0);
                let is_hovered = hovered == Some(index);
                // 文字颜色/字号/字体全部读配置快照；扇区底色已足够，不再自动提亮。
                let title_color = canvas_colors.title;
                let preview_color = canvas_colors.preview;
                let _ = is_hovered;
                let title = div()
                    .max_w_full()
                    .text_size(px(canvas_colors.title_font_size))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(gpui::rgb(title_color))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(entry.title.clone());
                let title = if let Some(family) = &canvas_colors.font_family {
                    title.font_family(family.clone())
                } else {
                    title
                };
                let preview = div()
                    .max_w_full()
                    .text_size(px(canvas_colors.preview_font_size))
                    .text_color(gpui::rgb(preview_color))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(entry.preview.clone());
                let preview = if let Some(family) = &canvas_colors.font_family {
                    preview.font_family(family.clone())
                } else {
                    preview
                };
                div()
                    .absolute()
                    .left(label_x)
                    .top(label_y)
                    .w(px(WHEEL_LABEL_WIDTH))
                    .h(px(WHEEL_LABEL_HEIGHT))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_0p5()
                    .child(title)
                    .child(preview)
            },
        ));
        let page_label = div()
            .text_size(px(11.0))
            .text_color(gpui::rgb(canvas_colors.page))
            .when(total_pages > 1, |this| {
                this.child(format!("{} / {}", page + 1, total_pages))
            });
        let page_label = if let Some(family) = &canvas_colors.font_family {
            page_label.font_family(family.clone())
        } else {
            page_label
        };
        root = root.child(
            div()
                .absolute()
                .left(px(WHEEL_DIAMETER / 2.0 - WHEEL_CENTER_RADIUS))
                .top(px(WHEEL_DIAMETER / 2.0 - WHEEL_CENTER_RADIUS))
                .w(px(WHEEL_CENTER_RADIUS * 2.0))
                .h(px(WHEEL_CENTER_RADIUS * 2.0))
                .rounded_full()
                .bg(rgba(canvas_colors.center))
                .border_1()
                .border_color(rgba(canvas_colors.center_border))
                .flex()
                .items_center()
                .justify_center()
                .child(page_label),
        );
        root
    }
}

fn deg_to_rad(deg: f32) -> f32 {
    deg * std::f32::consts::PI / 180.0
}

/// 扇区 i 的中心方向角（度）：0°=正上方，顺时针。
fn sector_center_angle_deg(index: usize) -> f32 {
    WHEEL_SECTOR_ANGLE_DEG * index as f32
}

/// 环形扇区路径：内弧 + 两条径向边 + 外弧（含视觉缝隙）。
fn ring_sector_path(
    center: Point<gpui::Pixels>,
    inner: f32,
    outer: f32,
    index: usize,
) -> Option<Path<gpui::Pixels>> {
    let half_span = (WHEEL_SECTOR_ANGLE_DEG - WHEEL_SECTOR_GAP_DEG) / 2.0;
    let center_angle = sector_center_angle_deg(index);
    let start_angle = center_angle - half_span;
    let end_angle = center_angle + half_span;
    let start_rad = deg_to_rad(start_angle);
    let end_rad = deg_to_rad(end_angle);

    let point_at = |radius: f32, angle: f32| {
        // 角度约定与命中判定一致：0°=正上方，顺时针为正；屏幕 y 向下。
        point(
            center.x + px(radius * angle.sin()),
            center.y - px(radius * angle.cos()),
        )
    };

    let inner_start = point_at(inner, start_rad);
    let outer_start = point_at(outer, start_rad);
    let outer_end = point_at(outer, end_rad);
    let inner_end = point_at(inner, end_rad);

    let radius_px = px(outer);
    let mut builder = PathBuilder::fill();
    builder.move_to(inner_start);
    builder.line_to(outer_start);
    // 外弧：从 start 沿顺时针到 end；跨度 < 180° 时不需要 large_arc。
    builder.arc_to(point(radius_px, radius_px), px(0.0), false, true, outer_end);
    builder.line_to(inner_end);
    // 内弧：从 end 逆时针回到 start。
    let inner_radius_px = px(inner);
    builder.arc_to(
        point(inner_radius_px, inner_radius_px),
        px(0.0),
        false,
        false,
        inner_start,
    );
    builder.close();
    builder.build().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_sector(dx: f64, dy: f64, expected: Option<usize>) {
        assert_eq!(sector_index_for_delta(dx, dy), expected, "dx={dx} dy={dy}");
    }

    #[test]
    fn center_dead_zone_returns_none() {
        assert_sector(0.0, 0.0, None);
        assert_sector(10.0, -10.0, None);
    }

    #[test]
    fn six_sectors_clockwise_from_top() {
        let radius: f64 = 120.0;
        // 扇区中心分布（渲染与判定共用同一公式）：0°=上、60°=右上、120°=右下、180°=下、240°=左下、300°=左上
        // 正上方 → 扇区 0
        assert_sector(0.0, -radius, Some(0));
        // 右上（扇区 1 中心 60° 方向，避开分界线）
        let inside: f64 = 60.0f64.to_radians();
        assert_sector(radius * inside.sin(), -radius * inside.cos(), Some(1));
        // 正右（90°，落在扇区 1[30°~90°) 与扇区 2[90°~150°) 的分界线上，
        // 偏移 1° 取扇区 2 内部）
        let right_down: f64 = 91.0f64.to_radians();
        assert_sector(
            radius * right_down.sin(),
            -radius * right_down.cos(),
            Some(2),
        );
        // 正下 → 扇区 3（y 向下，180° 恰为扇区 3 中心）
        assert_sector(0.0, radius, Some(3));
        // 正左（270° 为扇区 4[210°~270°) 与扇区 5 的分界线，取扇区 4 内部 269°）
        let left_up: f64 = 269.0f64.to_radians();
        assert_sector(radius * left_up.sin(), -radius * left_up.cos(), Some(4));
    }
}
