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
#[derive(Clone, Copy, Debug)]
struct WheelColors {
    sector: u32,
    hover: u32,
    empty: u32,
    center: u32,
    center_border: u32,
}

impl Default for WheelColors {
    fn default() -> Self {
        Self {
            sector: WHEEL_SECTOR_FILL,
            hover: WHEEL_SECTOR_HOVER_FILL,
            empty: WHEEL_SECTOR_EMPTY_FILL,
            center: WHEEL_CENTER_FILL,
            center_border: WHEEL_CENTER_BORDER,
        }
    }
}

impl WheelColors {
    /// 从外观配置推导：跟随材质时按明暗模式选默认色，独立调色时覆盖对应项。
    fn resolve(cx: &App) -> Self {
        let appearance = crate::config_runtime::appearance_config(cx);
        let wheel = &appearance.wheel_appearance;
        let mut colors = Self::default();
        if !wheel.follow_window_material {
            // 独立调色：按亮暗模式取一组可读默认，再用用户色覆盖非空项。
            let dark = gpui_component::Theme::global(cx).mode.is_dark();
            if !dark {
                colors.sector = 0xFFFFFFD9;
                colors.hover = 0x3D6BFFF2;
                colors.empty = 0xF2F2F780;
                colors.center = 0xFFFFFFE6;
                colors.center_border = 0xC9C9D480;
            }
        }
        // 用户自定义色在两种模式下都生效（跟随材质时同样允许覆盖扇区底色）。
        if let Some(color) = parse_hex_rgba(&wheel.sector_color, colors.sector) {
            colors.sector = color;
        }
        if let Some(color) = parse_hex_rgba(&wheel.hover_color, colors.hover) {
            colors.hover = color;
        }
        if let Some(color) = parse_hex_rgba(&wheel.center_color, colors.center) {
            colors.center = color;
        }
        colors
    }
}

/// 按比例压低 0xRRGGBBAA 的 Alpha 通道，用于材质模式下让背景透出来。
fn with_reduced_alpha(color: u32, factor: f32) -> u32 {
    let alpha = (color & 0xFF) as f32 * factor.clamp(0.0, 1.0);
    ((color & 0xFFFFFF00) as u32) | (alpha.round() as u32 & 0xFF)
}

/// 解析 "RRGGBB"、"RRGGBBAA"、"RGB" 十六进制颜色为 0xRRGGBBAA。
/// 非法或为空返回 fallback；"RRGGBB" 补 FF 不透明度。
fn parse_hex_rgba(value: &str, fallback: u32) -> Option<u32> {
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

    cx.set_global(WheelSession {
        origin: Some(pointer),
        scale,
        entries: page.slots,
        total: page.total,
        page: 0,
        _page_listener: None,
        _click_guard: None,
        colors: WheelColors::resolve(cx),
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

    let diameter = WHEEL_DIAMETER * scale;
    let (x, y) = match display.map(|display| display.bounds) {
        Some(bounds) => {
            let margin = 8.0 * scale;
            centered_overlay_origin(pointer, diameter, diameter, margin, bounds)
        }
        None => (pointer.x - diameter / 2.0, pointer.y - diameter / 2.0),
    };

    let result = platform_window::open_borderless_overlay(
        cx,
        Bounds {
            origin: point(px(x / scale), px(y / scale)),
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
            // 初始定位到光标处。
            let _ = handle.update(cx, |view, window, cx| {
                view.place_window(window, cx);
            });
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

/// 轮盘打开期间的翻页：`forward` 为 true 表示向后翻一页（右键），false 向前翻（左键）。
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
        Some((next, next != page))
    });
    let Some((next, changed)) = next.filter(|(_, changed)| *changed) else {
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
}

impl WheelWindowView {
    fn new(popup: platform_window::BorderlessPopup) -> Self {
        Self {
            popup,
            placed: false,
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
    }
}

impl Render for WheelWindowView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (origin, cursor, entries, page, total_pages, colors) = cx
            .update_global::<WheelSession, _>(|session, _| {
                (
                    session.origin,
                    platform_window::physical_cursor_position(),
                    session.entries.clone(),
                    session.page,
                    session.total_pages(),
                    session.colors,
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
        let follow_material = crate::config_runtime::appearance_config(cx)
            .wheel_appearance
            .follow_window_material;
        // 跟随材质时降低扇区不透明度，让圆形裁剪内的材质背景透出来。
        let mut canvas_colors = colors;
        if follow_material {
            canvas_colors.sector = with_reduced_alpha(colors.sector, 0.45);
            canvas_colors.empty = with_reduced_alpha(colors.empty, 0.45);
            canvas_colors.hover = with_reduced_alpha(colors.hover, 0.85);
            canvas_colors.center = with_reduced_alpha(colors.center, 0.55);
            canvas_colors.center_border = with_reduced_alpha(colors.center_border, 0.60);
        }
        let center = point(px(WHEEL_DIAMETER / 2.0), px(WHEEL_DIAMETER / 2.0));
        // 跟随窗口材质时叠加主程序同款材质背景层（亚克力色调/模糊/背景图 + 不透明度遮罩），
        // 并整体裁剪成圆形，避免材质溢出轮盘外沿形成方形区域。
        let material_background = follow_material
            .then(|| crate::window_material::background_element(_window, cx))
            .map(|background| {
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .size(px(WHEEL_DIAMETER))
                    .rounded_full()
                    .overflow_hidden()
                    .child(background)
            });

        let mut root = div().relative().size_full();
        if let Some(background) = material_background {
            root = root.child(background);
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
                let title_color = if is_hovered {
                    gpui::rgb(0xFFFFFFu32)
                } else {
                    gpui::rgb(0xEDEDF2u32)
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
                    .child(
                        div()
                            .max_w_full()
                            .text_size(px(12.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(title_color)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(entry.title.clone()),
                    )
                    .child(
                        div()
                            .max_w_full()
                            .text_size(px(10.0))
                            .text_color(if is_hovered {
                                gpui::rgb(0xE2E8FFu32)
                            } else {
                                gpui::rgb(0x9A9AA5u32)
                            })
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(entry.preview.clone()),
                    )
            },
        ));
        root = root.child(
            div()
                .absolute()
                .left(px(WHEEL_DIAMETER / 2.0 - WHEEL_CENTER_RADIUS))
                .top(px(WHEEL_DIAMETER / 2.0 - WHEEL_CENTER_RADIUS))
                .w(px(WHEEL_CENTER_RADIUS * 2.0))
                .h(px(WHEEL_CENTER_RADIUS * 2.0))
                .rounded_full()
                .bg(rgba(colors.center))
                .border_1()
                .border_color(rgba(colors.center_border))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(gpui::rgb(0x8A8A95u32))
                        .when(total_pages > 1, |this| {
                            this.child(format!("{} / {}", page + 1, total_pages))
                        }),
                ),
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
