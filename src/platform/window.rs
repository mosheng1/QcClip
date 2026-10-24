use gpui::{
    App, Bounds, DisplayId, Entity, Pixels, Point, Render, TitlebarOptions, Window,
    WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowHandle, WindowKind,
    WindowOptions, px, size,
};

#[cfg(not(target_os = "windows"))]
use gpui::PlatformDisplay;

#[cfg(target_os = "windows")]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(target_os = "windows")]
static PREVIEW_FOREGROUND_EVENT: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "windows")]
unsafe extern "system" fn preview_foreground_event(
    _hook: windows::Win32::UI::Accessibility::HWINEVENTHOOK,
    event: u32,
    _window: windows::Win32::Foundation::HWND,
    _object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    if event == windows::Win32::UI::WindowsAndMessaging::EVENT_SYSTEM_FOREGROUND {
        PREVIEW_FOREGROUND_EVENT.store(true, Ordering::Release);
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn install_preview_foreground_hook() -> Option<isize> {
    use windows::Win32::UI::{
        Accessibility::SetWinEventHook,
        WindowsAndMessaging::{
            EVENT_SYSTEM_FOREGROUND, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
        },
    };

    PREVIEW_FOREGROUND_EVENT.store(false, Ordering::Release);
    // SAFETY: 回调为静态函数，不捕获 Rust 状态；仅监听系统前台窗口事件，不注入其他进程。
    let hook = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(preview_foreground_event),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };
    (!hook.is_invalid()).then_some(hook.0 as isize)
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn install_preview_foreground_hook() -> Option<isize> {
    None
}

#[cfg(target_os = "windows")]
pub(crate) fn uninstall_preview_foreground_hook(hook: isize) {
    use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, UnhookWinEvent};

    if hook != 0 {
        // SAFETY: hook 来自本模块成功安装的 WinEvent Hook 句柄。
        unsafe {
            let _ = UnhookWinEvent(HWINEVENTHOOK(hook as *mut _));
        }
    }
    PREVIEW_FOREGROUND_EVENT.store(false, Ordering::Release);
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn uninstall_preview_foreground_hook(_hook: isize) {}

#[cfg(target_os = "windows")]
pub(crate) fn preview_foreground_event_pending() -> bool {
    PREVIEW_FOREGROUND_EVENT.swap(false, Ordering::AcqRel)
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn preview_foreground_event_pending() -> bool {
    false
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct PhysicalDisplayBounds {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) width: f32,
    pub(crate) height: f32,
}

#[cfg(target_os = "windows")]
pub(crate) fn physical_cursor_position() -> Option<Point<f32>> {
    raw_input::Display::get_cursor_position().map(|(x, y)| Point {
        x: x as f32,
        y: y as f32,
    })
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn physical_cursor_position() -> Option<Point<f32>> {
    None
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DisplayPlacement {
    pub(crate) display_id: DisplayId,
    pub(crate) display_key: Option<u64>,
    pub(crate) scale_factor: f32,
    pub(crate) bounds: PhysicalDisplayBounds,
}

pub(crate) fn display_for_physical_point(cx: &App, point: Point<f32>) -> Option<DisplayPlacement> {
    #[cfg(target_os = "windows")]
    {
        let _ = cx;
        windows_monitor_for_point(point)
    }

    #[cfg(not(target_os = "windows"))]
    {
        let display = cx.displays().into_iter().find(|display| {
            let bounds = display.bounds();
            point.x >= f32::from(bounds.origin.x)
                && point.x < f32::from(bounds.origin.x + bounds.size.width)
                && point.y >= f32::from(bounds.origin.y)
                && point.y < f32::from(bounds.origin.y + bounds.size.height)
        })?;

        Some(DisplayPlacement {
            display_id: display.id(),
            display_key: stable_display_key(display.as_ref()),
            scale_factor: 1.0,
            bounds: PhysicalDisplayBounds {
                x: f32::from(display.bounds().origin.x),
                y: f32::from(display.bounds().origin.y),
                width: f32::from(display.bounds().size.width),
                height: f32::from(display.bounds().size.height),
            },
        })
    }
}

pub(crate) fn display_for_key(cx: &App, display_key: u64) -> Option<DisplayPlacement> {
    #[cfg(target_os = "windows")]
    {
        let _ = cx;
        let monitor = raw_input::Display::get_available_monitors()
            .into_iter()
            .find(|monitor| stable_display_key_from_name(&monitor.name) == display_key)?;
        let point = Point {
            x: (monitor.offset.0 + monitor.size.0 / 2.0) as f32,
            y: (monitor.offset.1 + monitor.size.1 / 2.0) as f32,
        };
        windows_monitor_for_point(point)
    }

    #[cfg(not(target_os = "windows"))]
    {
        let display = cx
            .displays()
            .into_iter()
            .find(|display| stable_display_key(display.as_ref()) == Some(display_key))?;
        Some(DisplayPlacement {
            display_id: display.id(),
            display_key: Some(display_key),
            scale_factor: 1.0,
            bounds: PhysicalDisplayBounds {
                x: f32::from(display.bounds().origin.x),
                y: f32::from(display.bounds().origin.y),
                width: f32::from(display.bounds().size.width),
                height: f32::from(display.bounds().size.height),
            },
        })
    }
}

#[cfg(not(target_os = "windows"))]
fn stable_display_key(display: &dyn PlatformDisplay) -> Option<u64> {
    display
        .uuid()
        .ok()
        .map(|uuid| stable_display_key_from_name(&uuid.to_string()))
}

fn stable_display_key_from_name(name: &str) -> u64 {
    // FNV-1a 的固定初始值保证写入 state.json 后下次启动仍能得到同一个键。
    name.bytes()
        .fold(14_695_981_039_346_656_037u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(1_099_511_628_211)
        })
}

#[cfg(target_os = "windows")]
fn windows_monitor_for_point(point: Point<f32>) -> Option<DisplayPlacement> {
    use raw_input::Display;
    use windows::Win32::{
        Foundation::POINT,
        Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromPoint},
    };

    let monitor = Display::get_monitor_from_point(point.x as f64, point.y as f64)?;
    let scale_factor = (monitor.scale_factor as f32).max(1.0);

    // SAFETY: 这里只读取系统根据物理桌面坐标返回的显示器句柄，不修改系统状态。
    let target = unsafe {
        MonitorFromPoint(
            POINT {
                x: point.x.round() as i32,
                y: point.y.round() as i32,
            },
            MONITOR_DEFAULTTONULL,
        )
    };
    if target.is_invalid() {
        return None;
    }

    Some(DisplayPlacement {
        // GPUI 在 Windows 上使用 HMONITOR 数值作为 DisplayId，直接传递句柄可以避免
        // Win32 和 GPUI 的显示器枚举顺序不同导致屏幕映射错误。
        display_id: DisplayId::new(target.0 as u64),
        display_key: Some(stable_display_key_from_name(&monitor.name)),
        scale_factor,
        bounds: PhysicalDisplayBounds {
            x: monitor.offset.0 as f32,
            y: monitor.offset.1 as f32,
            width: monitor.size.0 as f32,
            height: monitor.size.1 as f32,
        },
    })
}

#[cfg(target_os = "windows")]
fn windows_work_area_for_point(point: Point<f32>) -> Option<DisplayPlacement> {
    use raw_input::Display;
    use windows::Win32::{
        Foundation::POINT,
        Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromPoint},
    };

    let monitor = Display::get_monitor_from_point(point.x as f64, point.y as f64)?;
    let scale_factor = (monitor.scale_factor as f32).max(1.0);
    let target = unsafe {
        MonitorFromPoint(
            POINT {
                x: point.x.round() as i32,
                y: point.y.round() as i32,
            },
            MONITOR_DEFAULTTONULL,
        )
    };
    if target.is_invalid() {
        return None;
    }
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(target, &mut info) }.as_bool() {
        return None;
    }
    Some(DisplayPlacement {
        display_id: DisplayId::new(target.0 as u64),
        display_key: Some(stable_display_key_from_name(&monitor.name)),
        scale_factor,
        bounds: PhysicalDisplayBounds {
            x: info.rcWork.left as f32,
            y: info.rcWork.top as f32,
            width: (info.rcWork.right - info.rcWork.left) as f32,
            height: (info.rcWork.bottom - info.rcWork.top) as f32,
        },
    })
}

#[derive(Clone, Copy, Debug, Default)]
struct PopupBorderOffset {
    width: i32,
    height: i32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct BorderlessPopup;

impl BorderlessPopup {
    pub(crate) fn work_area_for_physical_point(
        cx: &App,
        point: Point<f32>,
    ) -> Option<DisplayPlacement> {
        work_area_for_physical_point(cx, point)
    }

    pub(crate) fn display_for_physical_point(
        cx: &App,
        point: Point<f32>,
    ) -> Option<DisplayPlacement> {
        display_for_physical_point(cx, point)
    }
}

pub(crate) fn open_borderless_popup<V>(
    cx: &mut App,
    initial_bounds: Bounds<Pixels>,
    display_id: Option<DisplayId>,
    build_root_view: impl FnOnce(&mut Window, &mut App, BorderlessPopup) -> Entity<V> + 'static,
) -> Result<WindowHandle<V>, String>
where
    V: 'static + Render,
{
    open_borderless_popup_with_options(
        cx,
        initial_bounds,
        display_id,
        false,
        false,
        false,
        build_root_view,
    )
}

pub(crate) fn open_borderless_popup_visible<V>(
    cx: &mut App,
    initial_bounds: Bounds<Pixels>,
    display_id: Option<DisplayId>,
    build_root_view: impl FnOnce(&mut Window, &mut App, BorderlessPopup) -> Entity<V> + 'static,
) -> Result<WindowHandle<V>, String>
where
    V: 'static + Render,
{
    open_borderless_popup_with_options(
        cx,
        initial_bounds,
        display_id,
        false,
        true,
        true,
        build_root_view,
    )
}

pub(crate) fn open_borderless_mouse_passthrough_popup<V>(
    cx: &mut App,
    initial_bounds: Bounds<Pixels>,
    display_id: Option<DisplayId>,
    build_root_view: impl FnOnce(&mut Window, &mut App, BorderlessPopup) -> Entity<V> + 'static,
) -> Result<WindowHandle<V>, String>
where
    V: 'static + Render,
{
    open_borderless_popup_with_options(
        cx,
        initial_bounds,
        display_id,
        true,
        false,
        true,
        build_root_view,
    )
}

pub(crate) fn open_borderless_overlay<V>(
    cx: &mut App,
    initial_bounds: Bounds<Pixels>,
    display_id: Option<DisplayId>,
    build_root_view: impl FnOnce(&mut Window, &mut App, BorderlessPopup) -> Entity<V> + 'static,
) -> Result<WindowHandle<V>, String>
where
    V: 'static + Render,
{
    #[cfg(not(target_os = "windows"))]
    let options = {
        let mut options = borderless_popup_options(initial_bounds, display_id, false, false);
        options.show = true;
        options
    };
    #[cfg(target_os = "windows")]
    let options = borderless_popup_options(initial_bounds, display_id, false, false);

    cx.open_window(options, move |window, cx| {
        configure_borderless_popup(window, false);
        if let Err(error) = set_window_shadow_visible(window, false) {
            tracing::error!("关闭便捷粘贴窗口阴影失败：{error}");
        }
        window.set_background_appearance(WindowBackgroundAppearance::Transparent);
        let view = build_root_view(window, cx, BorderlessPopup);
        window.refresh();
        view
    })
    .map_err(|error| error.to_string())
}

fn open_borderless_popup_with_options<V>(
    cx: &mut App,
    initial_bounds: Bounds<Pixels>,
    display_id: Option<DisplayId>,
    mouse_passthrough: bool,
    initially_visible: bool,
    animate_when_inactive: bool,
    build_root_view: impl FnOnce(&mut Window, &mut App, BorderlessPopup) -> Entity<V> + 'static,
) -> Result<WindowHandle<V>, String>
where
    V: 'static + Render,
{
    cx.open_window(
        borderless_popup_options(
            initial_bounds,
            display_id,
            initially_visible,
            animate_when_inactive,
        ),
        move |window, cx| {
            let popup = BorderlessPopup::configure(window, mouse_passthrough);
            window.set_background_appearance(WindowBackgroundAppearance::Transparent);
            crate::ui::theme::attach_system_appearance_watch(window, cx);
            if !mouse_passthrough {
                crate::window_material::register_window(window, cx);
            }
            let view = build_root_view(window, cx, popup);
            window.refresh();
            view
        },
    )
    .map_err(|error| error.to_string())
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct BorderlessNormalWindow {
    border_offset: PopupBorderOffset,
    hwnd: isize,
}

pub(crate) fn open_borderless_normal_window<V>(
    cx: &mut App,
    initial_bounds: Bounds<Pixels>,
    display_id: Option<DisplayId>,
    build_root_view: impl FnOnce(&mut Window, &mut App, BorderlessNormalWindow) -> Entity<V> + 'static,
) -> Result<WindowHandle<V>, String>
where
    V: 'static + Render,
{
    let window = cx
        .open_window(
            borderless_normal_window_options(initial_bounds, display_id),
            move |window, cx| {
                configure_frameless_window(window, false);
                // 客户区在调整框架后变大，窗口创建时读到的 viewport 尺寸已经过期。
                window.bounds_changed(cx);
                let normal_window = BorderlessNormalWindow::configure(window);
                crate::ui::theme::attach_system_appearance_watch(window, cx);
                crate::window_material::register_window(window, cx);
                let view = build_root_view(window, cx, normal_window);
                window.refresh();
                view
            },
        )
        .map_err(|error| error.to_string())?;
    window
        .update(cx, |_, window, _| window.activate_window())
        .map_err(|error| error.to_string())?;
    Ok(window)
}

#[cfg(target_os = "windows")]
pub(crate) fn set_window_enabled(window: &Window, enabled: bool) -> Result<(), String> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::{
        Foundation::HWND,
        UI::{
            Input::KeyboardAndMouse::{EnableWindow, IsWindowEnabled},
            WindowsAndMessaging::IsWindow,
        },
    };

    let handle = HasWindowHandle::window_handle(window)
        .map_err(|error| format!("无法获取窗口句柄：{error}"))?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return Err("当前平台不支持设置窗口交互状态".to_owned());
    };
    let hwnd = HWND(handle.hwnd.get() as *mut _);
    // SAFETY: hwnd 来自当前 GPUI 窗口句柄，仅查询并修改该窗口的启用状态。
    if !unsafe { IsWindow(Some(hwnd)).as_bool() } {
        return Err("调用方窗口句柄无效".to_owned());
    }

    // SAFETY: hwnd 已通过 IsWindow 校验，EnableWindow 只改变该窗口的输入状态。
    unsafe {
        let _ = EnableWindow(hwnd, enabled);
        if IsWindowEnabled(hwnd).as_bool() != enabled {
            return Err("设置窗口交互状态失败".to_owned());
        }
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn set_window_enabled(_window: &Window, _enabled: bool) -> Result<(), String> {
    Ok(())
}

/// `ACCENT_ENABLE_BLURBEHIND`，Windows 10 的窗口背景模糊。
#[cfg(target_os = "windows")]
const ACCENT_ENABLE_BLURBEHIND: u32 = 3;
/// `ACCENT_ENABLE_ACRYLICBLURBEHIND`，Windows 的系统亚克力材质。
#[cfg(target_os = "windows")]
const ACCENT_ENABLE_ACRYLICBLURBEHIND: u32 = 4;

/// 标记当前窗口是否启用了系统亚克力，用于在窗口尺寸变化后重新提交材质。
#[cfg(target_os = "windows")]
const ACRYLIC_MATERIAL_PROP: windows::core::PCWSTR = windows::core::w!("QcClipAcrylicMaterial");
/// 保存原生亚克力 tint，窗口尺寸变化时重新应用材质需要读取该值。
#[cfg(target_os = "windows")]
const ACRYLIC_TINT_PROP: windows::core::PCWSTR = windows::core::w!("QcClipAcrylicTint");

/// 为系统材质下发额外的 Windows 合成属性。
#[cfg(target_os = "windows")]
pub(crate) fn apply_window_material(
    window: &Window,
    material: crate::config::WindowMaterial,
    brightness: f32,
    light_mode: bool,
) {
    use crate::config::WindowMaterial;

    let Ok(hwnd) = window_hwnd(window) else {
        return;
    };

    match material {
        WindowMaterial::Acrylic => {
            set_acrylic_material_enabled(hwnd, true);
            let tint = acrylic_gradient_color(brightness, light_mode);
            set_acrylic_gradient_color(hwnd, tint);
            set_accent_policy(hwnd, ACCENT_ENABLE_ACRYLICBLURBEHIND, 0, tint);
        }
        WindowMaterial::Blur => {
            set_acrylic_material_enabled(hwnd, false);
            set_accent_policy(hwnd, ACCENT_ENABLE_BLURBEHIND, 0, 0);
        }
        WindowMaterial::Disabled | WindowMaterial::Transparent | WindowMaterial::CustomImage => {
            set_acrylic_material_enabled(hwnd, false);
        }
    }
}

/// 给独立圆窗（收藏轮盘）应用材质：先椭圆裁剪 HWND 再设置 GPUI 背景外观。
/// 亚克力/模糊走 GPUI 的 Blurred（Windows 上为 ACCENT_ENABLE_ACRYLICBLURBEHIND），
/// DWM 材质跟随椭圆窗口区域，天然圆形；透明材质恢复普通 alpha 透明。
#[cfg(target_os = "windows")]
pub(crate) fn apply_circle_window_material(
    window: &Window,
    material: crate::config::WindowMaterial,
) -> Result<(), String> {
    use crate::config::WindowMaterial;

    let scale = window.scale_factor().max(1.0);
    let viewport = window.viewport_size();
    let width = f32::from(viewport.width);
    let height = f32::from(viewport.height);
    let diameter = width.min(height).max(1.0);
    // 圆心取客户区中心；SetWindowRgn 使用客户区坐标（逻辑像素）。
    let center_x = width / 2.0 / scale;
    let center_y = height / 2.0 / scale;
    set_window_circle_clip(window, center_x, center_y, diameter / scale)?;

    // GPUI 的 Blurred 在 Windows 上即 ACCENT_ENABLE_ACRYLICBLURBEHIND（带近零 tint），
    // 视觉主体由内容层的圆形 tint 提供；透明/关闭恢复普通透明。
    let appearance = match crate::window_material::supported_material(material) {
        WindowMaterial::Acrylic | WindowMaterial::Blur => gpui::WindowBackgroundAppearance::Blurred,
        WindowMaterial::Disabled | WindowMaterial::Transparent | WindowMaterial::CustomImage => {
            gpui::WindowBackgroundAppearance::Transparent
        }
    };
    window.set_background_appearance(appearance);
    Ok(())
}

#[cfg(target_os = "windows")]
fn set_acrylic_material_enabled(hwnd: windows::Win32::Foundation::HWND, enabled: bool) {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::UI::WindowsAndMessaging::{RemovePropW, SetPropW};

    // SAFETY: hwnd 来自当前 GPUI 窗口，属性值只用于标记当前窗口的材质状态。
    unsafe {
        if enabled {
            let _ = SetPropW(hwnd, ACRYLIC_MATERIAL_PROP, Some(HANDLE(1 as _)));
        } else {
            let _ = RemovePropW(hwnd, ACRYLIC_MATERIAL_PROP);
            let _ = RemovePropW(hwnd, ACRYLIC_TINT_PROP);
        }
    }
}

#[cfg(target_os = "windows")]
fn set_acrylic_gradient_color(hwnd: windows::Win32::Foundation::HWND, color: u32) {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::UI::WindowsAndMessaging::SetPropW;

    // 属性值仅保存 32 位 tint；加一以保留 color=0 时的有效属性标记。
    // SAFETY: hwnd 来自当前 GPUI 窗口，属性值只作为整数保存，不会被解引用。
    unsafe {
        let _ = SetPropW(
            hwnd,
            ACRYLIC_TINT_PROP,
            Some(HANDLE((color as usize + 1) as _)),
        );
    }
}

#[cfg(target_os = "windows")]
fn acrylic_gradient_color(brightness: f32, light_mode: bool) -> u32 {
    if !brightness.is_finite() {
        return 0;
    }
    if light_mode {
        // 原生 Acrylic 自带灰色材质层，浅色主题用连续的白色 tint 曲线抵消灰雾。
        // 这不能完全等同于 GPUI 的白色叠加，但不会在 100% 明度处突然跳变。
        let strength = (0.55 + (brightness - 1.0) * 0.75).clamp(0.0, 1.0);
        let alpha = (strength * 255.0).round() as u32;
        (alpha << 24) | 0x00FF_FFFF
    } else if brightness < 1.0 {
        let alpha = ((1.0 - brightness).clamp(0.0, 1.0) * 255.0).round() as u32;
        alpha << 24
    } else if brightness > 1.0 {
        let strength = (brightness - 1.0).clamp(0.0, 1.0);
        let alpha = (strength * 255.0).round() as u32;
        (alpha << 24) | 0x00FF_FFFF
    } else {
        // GPUI 为保持 Acrylic 激活，会把零 alpha tint 的 alpha 提升到 1。
        1 << 24
    }
}

#[cfg(target_os = "windows")]
fn acrylic_gradient_color_for_hwnd(hwnd: windows::Win32::Foundation::HWND) -> u32 {
    use windows::Win32::UI::WindowsAndMessaging::GetPropW;

    let stored = unsafe { GetPropW(hwnd, ACRYLIC_TINT_PROP).0 as usize };
    stored.checked_sub(1).map(|color| color as u32).unwrap_or(0)
}

#[cfg(target_os = "windows")]
fn acrylic_material_enabled(hwnd: windows::Win32::Foundation::HWND) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::GetPropW;

    // SAFETY: hwnd 是当前窗口过程收到的有效窗口句柄。
    unsafe { !GetPropW(hwnd, ACRYLIC_MATERIAL_PROP).0.is_null() }
}

#[cfg(target_os = "windows")]
fn refresh_acrylic_material(hwnd: windows::Win32::Foundation::HWND) {
    set_accent_policy(
        hwnd,
        ACCENT_ENABLE_ACRYLICBLURBEHIND,
        0,
        acrylic_gradient_color_for_hwnd(hwnd),
    );
}

/// 写入 `WCA_ACCENT_POLICY`，这是 Windows 10 起使用的窗口合成属性。
#[cfg(target_os = "windows")]
fn set_accent_policy(
    hwnd: windows::Win32::Foundation::HWND,
    state: u32,
    flags: u32,
    gradient_color: u32,
) {
    use std::ffi::c_void;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
    use windows::core::PCSTR;

    #[repr(C)]
    struct WindowCompositionAttributeData {
        attribute: u32,
        data: *mut c_void,
        size: usize,
    }

    #[repr(C)]
    struct AccentPolicy {
        state: u32,
        flags: u32,
        gradient_color: u32,
        animation_id: u32,
    }

    type SetWindowCompositionAttribute =
        unsafe extern "system" fn(HWND, *mut WindowCompositionAttributeData) -> isize;

    const WCA_ACCENT_POLICY: u32 = 19;

    let Some(function): Option<SetWindowCompositionAttribute> = (|| unsafe {
        let module = GetModuleHandleA(PCSTR(b"user32.dll\0".as_ptr())).ok()?;
        let address = GetProcAddress(module, PCSTR(b"SetWindowCompositionAttribute\0".as_ptr()))?;
        // SAFETY: user32 导出的函数签名与当前进程声明的 SetWindowCompositionAttribute ABI 一致。
        Some(std::mem::transmute(address))
    })() else {
        return;
    };

    let mut policy = AccentPolicy {
        state,
        flags,
        gradient_color,
        animation_id: 0,
    };
    let mut data = WindowCompositionAttributeData {
        attribute: WCA_ACCENT_POLICY,
        data: &mut policy as *mut _ as *mut c_void,
        size: std::mem::size_of::<AccentPolicy>(),
    };
    // SAFETY: 数据结构符合 WCA_ACCENT_POLICY ABI，窗口句柄来自当前进程的有效 GPUI 窗口。
    unsafe {
        let _ = function(hwnd, &mut data);
    }
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn apply_window_material(
    _window: &Window,
    _material: crate::config::WindowMaterial,
    _brightness: f32,
    _light_mode: bool,
) {
}

#[cfg(target_os = "windows")]
pub(super) fn window_hwnd(window: &Window) -> Result<windows::Win32::Foundation::HWND, String> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::{Foundation::HWND, UI::WindowsAndMessaging::IsWindow};

    let handle = HasWindowHandle::window_handle(window)
        .map_err(|error| format!("无法获取窗口句柄：{error}"))?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return Err("当前平台不支持设置窗口显示状态".to_owned());
    };
    let hwnd = HWND(handle.hwnd.get() as *mut _);
    // SAFETY: hwnd 来自当前 GPUI 窗口句柄，仅查询该窗口是否仍然有效。
    if !unsafe { IsWindow(Some(hwnd)).as_bool() } {
        return Err("主窗口句柄无效".to_owned());
    }
    Ok(hwnd)
}

#[cfg(target_os = "windows")]
pub(crate) fn set_window_rendering_enabled(window: &Window, enabled: bool) -> Result<(), String> {
    use windows::Win32::{
        Foundation::WPARAM,
        UI::WindowsAndMessaging::{SendMessageW, WM_SETREDRAW},
    };

    let hwnd = window_hwnd(window)?;
    // SAFETY: 同步通知当前 GPUI 窗口暂停或恢复绘制，不改变贴边窗口的系统可见性。
    unsafe {
        SendMessageW(hwnd, WM_SETREDRAW, Some(WPARAM(usize::from(enabled))), None);
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn set_window_rendering_enabled(_window: &Window, _enabled: bool) -> Result<(), String> {
    Ok(())
}

#[cfg(target_os = "windows")]
pub(crate) fn set_window_visible(window: &Window, visible: bool) -> Result<(), String> {
    use windows::Win32::UI::WindowsAndMessaging::{SW_HIDE, SW_SHOWNOACTIVATE, ShowWindowAsync};

    let hwnd = window_hwnd(window)?;
    let command = if visible { SW_SHOWNOACTIVATE } else { SW_HIDE };
    // SAFETY: hwnd 已通过 IsWindow 校验，ShowWindowAsync 只改变该窗口的显示状态。
    unsafe {
        let _ = ShowWindowAsync(hwnd, command);
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn set_window_visible(_window: &Window, _visible: bool) -> Result<(), String> {
    Err("当前平台不支持设置窗口显示状态".to_owned())
}

#[cfg(target_os = "windows")]
pub(crate) fn set_window_opacity(window: &Window, opacity: f32) -> Result<(), String> {
    let hwnd = window_hwnd(window)?;
    set_window_opacity_for_hwnd(hwnd, opacity)
}

/// 将窗口可见区域裁剪为以客户区坐标为圆心的圆形。
///
/// GPUI 当前的内容遮罩只支持矩形，因此便捷粘贴动画使用 Windows 窗口区域来保证
/// 子内容也会被真正按圆形裁剪。
#[cfg(target_os = "windows")]
pub(crate) fn set_window_circle_clip(
    window: &Window,
    center_x: f32,
    center_y: f32,
    diameter: f32,
) -> Result<(), String> {
    use windows::Win32::Graphics::Gdi::{CreateEllipticRgn, DeleteObject, SetWindowRgn};

    let hwnd = window_hwnd(window)?;
    let scale = window.scale_factor().max(1.0);
    let diameter = (diameter * scale).round().max(1.0) as i32;
    let center_x = (center_x * scale).round() as i32;
    let center_y = (center_y * scale).round() as i32;
    let left = center_x.saturating_sub(diameter / 2);
    let top = center_y.saturating_sub(diameter / 2);
    let right = left.saturating_add(diameter);
    let bottom = top.saturating_add(diameter);
    // SAFETY: hwnd 已通过 IsWindow 校验，区域参数是有限的客户区坐标。
    let region = unsafe { CreateEllipticRgn(left, top, right, bottom) };
    if region.is_invalid() {
        return Err("创建便捷粘贴圆形窗口区域失败".to_owned());
    }
    // SAFETY: region 的所有权在 SetWindowRgn 成功后交给系统窗口。
    let result = unsafe { SetWindowRgn(hwnd, Some(region), true) };
    if result == 0 {
        // SAFETY: SetWindowRgn 失败时区域所有权仍由调用方持有。
        unsafe {
            let _ = DeleteObject(region.into());
        }
        return Err("设置便捷粘贴圆形窗口区域失败".to_owned());
    }
    Ok(())
}

#[cfg(target_os = "windows")]
pub(crate) fn clear_window_circle_clip(window: &Window) -> Result<(), String> {
    use windows::Win32::Graphics::Gdi::SetWindowRgn;

    let hwnd = window_hwnd(window)?;
    // SAFETY: hwnd 已通过 IsWindow 校验，传入 None 会清除当前窗口区域。
    let result = unsafe { SetWindowRgn(hwnd, None, true) };
    if result == 0 {
        return Err("清除便捷粘贴窗口区域失败".to_owned());
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn set_window_opacity(_window: &Window, _opacity: f32) -> Result<(), String> {
    Ok(())
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn set_window_circle_clip(
    _window: &Window,
    _center_x: f32,
    _center_y: f32,
    _diameter: f32,
) -> Result<(), String> {
    Ok(())
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn clear_window_circle_clip(_window: &Window) -> Result<(), String> {
    Ok(())
}

#[cfg(target_os = "windows")]
pub(crate) fn set_window_opacity_with_target(
    target: WindowAnimationTarget,
    opacity: f32,
) -> Result<(), String> {
    let hwnd = windows::Win32::Foundation::HWND(target.hwnd as *mut _);
    set_window_opacity_for_hwnd(hwnd, opacity)
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn set_window_opacity_with_target(
    _target: WindowAnimationTarget,
    _opacity: f32,
) -> Result<(), String> {
    Ok(())
}

#[cfg(target_os = "windows")]
fn set_window_opacity_for_hwnd(
    hwnd: windows::Win32::Foundation::HWND,
    opacity: f32,
) -> Result<(), String> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongPtrW, LWA_ALPHA, SetLayeredWindowAttributes, SetWindowLongPtrW,
        WS_EX_LAYERED,
    };

    let alpha = (opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
    // SAFETY: hwnd 已通过 IsWindow 校验，透明度只作用于当前窗口。
    unsafe {
        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        if ex_style & WS_EX_LAYERED.0 as isize == 0 {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex_style | WS_EX_LAYERED.0 as isize);
        }
        SetLayeredWindowAttributes(
            hwnd,
            windows::Win32::Foundation::COLORREF(0),
            alpha,
            LWA_ALPHA,
        )
    }
    .map_err(|error| format!("设置窗口透明度失败：{error}"))
}

#[cfg(target_os = "windows")]
pub(crate) fn raise_window_to_topmost(window: &Window) -> Result<(), String> {
    use windows::Win32::UI::WindowsAndMessaging::{
        HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos,
    };

    let hwnd = window_hwnd(window)?;
    // SAFETY: hwnd 已通过 IsWindow 校验，只提升当前窗口在置顶窗口中的层级，不改变位置、尺寸和焦点。
    unsafe {
        SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
        )
    }
    .map_err(|error| format!("提升窗口置顶层级失败：{error}"))
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn raise_window_to_topmost(_window: &Window) -> Result<(), String> {
    Ok(())
}

pub(crate) fn activate_window_preserving_bounds(window: &Window) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::UI::{
            Input::KeyboardAndMouse::{SetActiveWindow, SetFocus},
            WindowsAndMessaging::{
                HWND_TOPMOST, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SetForegroundWindow,
                SetWindowPos,
            },
        };

        let hwnd = window_hwnd(window)?;
        // GPUI 首次激活会恢复创建时的尺寸，面板由原生 API 激活以保留滑动位置。
        // SAFETY: hwnd 已通过 IsWindow 校验，只激活当前窗口，不修改其尺寸和位置。
        unsafe {
            SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
            )
            .map_err(|error| format!("激活主窗口失败：{error}"))?;
            let _ = SetActiveWindow(hwnd);
            let _ = SetFocus(Some(hwnd));
            let _ = SetForegroundWindow(hwnd);
        }
    }
    #[cfg(not(target_os = "windows"))]
    window.activate_window();
    Ok(())
}

#[cfg(target_os = "windows")]
pub(crate) fn set_window_shadow_visible(window: &Window, visible: bool) -> Result<(), String> {
    use windows::Win32::Graphics::Dwm::{
        DWMNCRP_DISABLED, DWMNCRP_USEWINDOWSTYLE, DWMWA_BORDER_COLOR, DWMWA_COLOR_DEFAULT,
        DWMWA_COLOR_NONE, DWMWA_NCRENDERING_POLICY, DWMWA_WINDOW_CORNER_PREFERENCE,
        DWMWCP_DONOTROUND, DWMWCP_ROUND, DwmSetWindowAttribute,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_STYLE, GetWindowLongPtrW, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOOWNERZORDER,
        SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos, WS_THICKFRAME,
    };

    let hwnd = window_hwnd(window)?;
    let previous_metrics = client_area_metrics(hwnd)?;
    if let Err(error) = super::window_appearance::set_decorations_visible(hwnd, visible) {
        tracing::warn!("同步窗口装饰状态失败：{error}");
    }
    // 普通窗口的系统阴影由窗口框架和 DWM 非客户区共同参与绘制。贴边隐藏时需要
    // 同时关闭这两部分，恢复时再全部还原，否则会留下透明的框架区域。
    let previous_style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) };
    let thick_frame = WS_THICKFRAME.0 as isize;
    let next_style = if visible {
        previous_style | thick_frame
    } else {
        previous_style & !thick_frame
    };
    let rendering_policy = if visible {
        DWMNCRP_USEWINDOWSTYLE
    } else {
        DWMNCRP_DISABLED
    };
    let corner_preference = if visible {
        DWMWCP_ROUND
    } else {
        DWMWCP_DONOTROUND
    };
    let border_color = if visible {
        DWMWA_COLOR_DEFAULT
    } else {
        DWMWA_COLOR_NONE
    };

    let previous_window_x = previous_metrics
        .client_left
        .saturating_sub(previous_metrics.frame_offset_x);
    let previous_window_y = previous_metrics
        .client_top
        .saturating_sub(previous_metrics.frame_offset_y);
    let previous_window_width = previous_metrics
        .client_width
        .saturating_add(previous_metrics.frame_width);
    let previous_window_height = previous_metrics
        .client_height
        .saturating_add(previous_metrics.frame_height);

    // SAFETY: hwnd 已通过 IsWindow 校验，只修改当前窗口的窗口样式和 DWM 外观。
    unsafe {
        SetWindowLongPtrW(hwnd, GWL_STYLE, next_style);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_NCRENDERING_POLICY,
            &rendering_policy as *const _ as _,
            std::mem::size_of_val(&rendering_policy) as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner_preference as *const _ as _,
            std::mem::size_of_val(&corner_preference) as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &border_color as *const _ as _,
            std::mem::size_of_val(&border_color) as u32,
        );
        SetWindowPos(
            hwnd,
            None,
            previous_window_x,
            previous_window_y,
            previous_window_width,
            previous_window_height,
            SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_FRAMECHANGED,
        )
    }
    .map_err(|error| format!("刷新窗口框架样式失败：{error}"))?;

    let current_metrics = client_area_metrics(hwnd)?;
    let target_window_x = previous_metrics
        .client_left
        .saturating_sub(current_metrics.frame_offset_x);
    let target_window_y = previous_metrics
        .client_top
        .saturating_sub(current_metrics.frame_offset_y);
    let target_window_width = previous_metrics
        .client_width
        .saturating_add(current_metrics.frame_width);
    let target_window_height = previous_metrics
        .client_height
        .saturating_add(current_metrics.frame_height);
    if target_window_x == previous_window_x
        && target_window_y == previous_window_y
        && target_window_width == previous_window_width
        && target_window_height == previous_window_height
    {
        return Ok(());
    }

    // 样式切换可能改变非客户区大小，补偿外框尺寸和位置以保持客户区几何不变。
    // SAFETY: hwnd 仍是当前有效窗口，只恢复切换前的客户区位置和尺寸。
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            target_window_x,
            target_window_y,
            target_window_width,
            target_window_height,
            SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER,
        )
    }
    .map_err(|error| format!("保持窗口客户区尺寸失败：{error}"))
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn set_window_shadow_visible(_window: &Window, _visible: bool) -> Result<(), String> {
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PhysicalClientBounds {
    pub(crate) left: i32,
    pub(crate) top: i32,
    pub(crate) right: i32,
    pub(crate) bottom: i32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LogicalDisplayBounds {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) width: f32,
    pub(crate) height: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LogicalClientBounds {
    pub(crate) left: f32,
    pub(crate) top: f32,
    pub(crate) width: f32,
    pub(crate) height: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CoordinateConverter {
    scale_factor: f32,
}

impl CoordinateConverter {
    pub(crate) fn new(scale_factor: f32) -> Self {
        Self {
            scale_factor: scale_factor.max(0.01),
        }
    }

    pub(crate) fn physical_to_logical(self, value: f32) -> f32 {
        value / self.scale_factor
    }

    pub(crate) fn logical_to_physical(self, value: f32) -> f32 {
        value * self.scale_factor
    }

    pub(crate) fn logical_to_physical_i32(self, value: f32) -> i32 {
        self.logical_to_physical(value).round() as i32
    }

    pub(crate) fn logical_to_physical_ceil_i32(self, value: f32) -> i32 {
        self.logical_to_physical(value).ceil() as i32
    }

    pub(crate) fn physical_point_to_logical(self, point: Point<f32>) -> Point<f32> {
        Point {
            x: self.physical_to_logical(point.x),
            y: self.physical_to_logical(point.y),
        }
    }

    pub(crate) fn logical_display_bounds(
        self,
        bounds: PhysicalDisplayBounds,
    ) -> LogicalDisplayBounds {
        LogicalDisplayBounds {
            x: self.physical_to_logical(bounds.x),
            y: self.physical_to_logical(bounds.y),
            width: self.physical_to_logical(bounds.width),
            height: self.physical_to_logical(bounds.height),
        }
    }

    pub(crate) fn physical_client_bounds(
        self,
        bounds: LogicalClientBounds,
    ) -> PhysicalClientBounds {
        let left = self.logical_to_physical_i32(bounds.left);
        let top = self.logical_to_physical_i32(bounds.top);
        let width = self.logical_to_physical_i32(bounds.width).max(1);
        let height = self.logical_to_physical_i32(bounds.height).max(1);
        PhysicalClientBounds {
            left,
            top,
            right: left.saturating_add(width),
            bottom: top.saturating_add(height),
        }
    }

    pub(crate) fn logical_client_size(self, bounds: PhysicalClientBounds) -> (f32, f32) {
        (
            self.physical_to_logical((bounds.right - bounds.left).max(1) as f32),
            self.physical_to_logical((bounds.bottom - bounds.top).max(1) as f32),
        )
    }
}

pub(crate) fn physical_client_bounds_from_logical(
    bounds: LogicalClientBounds,
    scale_factor: f32,
) -> PhysicalClientBounds {
    CoordinateConverter::new(scale_factor).physical_client_bounds(bounds)
}

pub(crate) fn logical_client_offset_relative_to_display(
    bounds: PhysicalClientBounds,
    display: DisplayPlacement,
) -> Point<f32> {
    let converter = CoordinateConverter::new(display.scale_factor);
    converter.physical_point_to_logical(Point {
        x: bounds.left as f32 - display.bounds.x,
        y: bounds.top as f32 - display.bounds.y,
    })
}

#[cfg(test)]
mod coordinate_converter_tests {
    use super::{
        CoordinateConverter, DisplayPlacement, PhysicalClientBounds, PhysicalDisplayBounds,
        logical_client_offset_relative_to_display,
    };
    use gpui::{DisplayId, Point};

    #[test]
    fn logical_and_physical_coordinates_use_the_same_scale() {
        let converter = CoordinateConverter::new(1.5);

        assert_eq!(converter.logical_to_physical_i32(8.0), 12);
        assert_eq!(converter.physical_to_logical(12.0), 8.0);
        assert_eq!(
            converter.physical_client_bounds(super::LogicalClientBounds {
                left: 100.0,
                top: -20.0,
                width: 360.0,
                height: 520.0,
            }),
            super::PhysicalClientBounds {
                left: 150,
                top: -30,
                right: 690,
                bottom: 750,
            }
        );
    }

    #[test]
    fn window_position_is_stored_relative_to_the_display_in_logical_pixels() {
        let display = DisplayPlacement {
            display_id: DisplayId::new(1),
            display_key: Some(2),
            scale_factor: 1.5,
            bounds: PhysicalDisplayBounds {
                x: 1920.0,
                y: -150.0,
                width: 2560.0,
                height: 1440.0,
            },
        };

        let relative = logical_client_offset_relative_to_display(
            PhysicalClientBounds {
                left: 2100,
                top: 60,
                right: 2460,
                bottom: 580,
            },
            display,
        );

        assert_eq!(relative, Point { x: 120.0, y: 140.0 });
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct WindowAnimationTarget {
    hwnd: isize,
    frame_offset_x: i32,
    frame_offset_y: i32,
    frame_width: i32,
    frame_height: i32,
}

#[cfg(target_os = "windows")]
pub(crate) fn window_animation_target(window: &Window) -> Result<WindowAnimationTarget, String> {
    let hwnd = window_hwnd(window)?;
    let metrics = client_area_metrics(hwnd)?;
    Ok(WindowAnimationTarget {
        hwnd: hwnd.0 as isize,
        frame_offset_x: metrics.frame_offset_x,
        frame_offset_y: metrics.frame_offset_y,
        frame_width: metrics.frame_width,
        frame_height: metrics.frame_height,
    })
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn window_animation_target(_window: &Window) -> Result<WindowAnimationTarget, String> {
    Err("当前平台不支持窗口动画".to_owned())
}

#[cfg(target_os = "windows")]
pub(crate) fn physical_client_bounds(window: &Window) -> Result<PhysicalClientBounds, String> {
    let hwnd = window_hwnd(window)?;
    let metrics = client_area_metrics(hwnd)?;
    Ok(PhysicalClientBounds {
        left: metrics.client_left,
        top: metrics.client_top,
        right: metrics.client_left.saturating_add(metrics.client_width),
        bottom: metrics.client_top.saturating_add(metrics.client_height),
    })
}

#[cfg(target_os = "windows")]
pub(crate) fn is_window_resize_hit_test(
    window: &Window,
    position: Point<f32>,
) -> Result<bool, String> {
    use windows::Win32::{
        Foundation::LPARAM,
        UI::WindowsAndMessaging::{SendMessageW, WM_NCHITTEST},
    };

    let hwnd = window_hwnd(window)?;
    let x = position.x.round() as i32;
    let y = position.y.round() as i32;
    // WM_NCHITTEST 使用 LPARAM 的低/高 16 位传递带符号的屏幕坐标。
    let packed_position = (u32::from(y as u16) << 16) | u32::from(x as u16);
    // SAFETY: hwnd 已通过 IsWindow 校验，消息参数是当前鼠标的屏幕坐标。
    let hit_test = unsafe {
        SendMessageW(
            hwnd,
            WM_NCHITTEST,
            None,
            Some(LPARAM(packed_position as isize)),
        )
    };
    Ok(is_resize_hit_test_value(hit_test.0))
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn is_window_resize_hit_test(
    _window: &Window,
    _position: Point<f32>,
) -> Result<bool, String> {
    Ok(false)
}

#[cfg(target_os = "windows")]
fn is_resize_hit_test_value(hit_test: isize) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTLEFT, HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT,
    };

    matches!(
        hit_test as u32,
        HTLEFT | HTRIGHT | HTTOP | HTTOPLEFT | HTTOPRIGHT | HTBOTTOM | HTBOTTOMLEFT | HTBOTTOMRIGHT
    )
}

#[cfg(target_os = "windows")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ResizeEdgeRestrictions {
    left: bool,
    right: bool,
    top: bool,
    bottom: bool,
}

#[cfg(target_os = "windows")]
fn filter_resize_hit_test_value(hit_test: isize, restrictions: ResizeEdgeRestrictions) -> isize {
    use windows::Win32::UI::WindowsAndMessaging::{
        HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCLIENT, HTLEFT, HTRIGHT, HTTOP, HTTOPLEFT,
        HTTOPRIGHT,
    };

    let hit_test = hit_test as u32;
    match hit_test {
        HTLEFT if restrictions.left => HTCLIENT as isize,
        HTRIGHT if restrictions.right => HTCLIENT as isize,
        HTTOP if restrictions.top => HTCLIENT as isize,
        HTBOTTOM if restrictions.bottom => HTCLIENT as isize,
        HTTOPLEFT => filter_corner_hit_test(
            restrictions.left,
            restrictions.top,
            HTLEFT,
            HTTOP,
            HTTOPLEFT,
        ),
        HTTOPRIGHT => filter_corner_hit_test(
            restrictions.right,
            restrictions.top,
            HTRIGHT,
            HTTOP,
            HTTOPRIGHT,
        ),
        HTBOTTOMLEFT => filter_corner_hit_test(
            restrictions.left,
            restrictions.bottom,
            HTLEFT,
            HTBOTTOM,
            HTBOTTOMLEFT,
        ),
        HTBOTTOMRIGHT => filter_corner_hit_test(
            restrictions.right,
            restrictions.bottom,
            HTRIGHT,
            HTBOTTOM,
            HTBOTTOMRIGHT,
        ),
        _ => hit_test as isize,
    }
}

#[cfg(target_os = "windows")]
fn filter_corner_hit_test(
    first_restricted: bool,
    second_restricted: bool,
    first_hit_test: u32,
    second_hit_test: u32,
    corner_hit_test: u32,
) -> isize {
    use windows::Win32::UI::WindowsAndMessaging::HTCLIENT;

    match (first_restricted, second_restricted) {
        (true, true) => HTCLIENT as isize,
        (true, false) => second_hit_test as isize,
        (false, true) => first_hit_test as isize,
        (false, false) => corner_hit_test as isize,
    }
}

#[cfg(target_os = "windows")]
fn is_native_resize_mouse_down(message: u32, hit_test: isize) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{WM_NCLBUTTONDBLCLK, WM_NCLBUTTONDOWN};

    matches!(message, WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK) && is_resize_hit_test_value(hit_test)
}

#[cfg(test)]
mod window_resize_hit_test_tests {
    #[cfg(target_os = "windows")]
    #[test]
    fn native_resize_press_preserves_edges_without_taking_client_or_titlebar_clicks() {
        use windows::Win32::UI::WindowsAndMessaging::{
            HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCAPTION, HTCLIENT, HTCLOSE, HTLEFT,
            HTMAXBUTTON, HTMINBUTTON, HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT, WM_LBUTTONDOWN,
            WM_NCLBUTTONDBLCLK, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_NCMOUSEMOVE, WM_NCRBUTTONDOWN,
        };

        for edge in [
            HTLEFT,
            HTRIGHT,
            HTTOP,
            HTTOPLEFT,
            HTTOPRIGHT,
            HTBOTTOM,
            HTBOTTOMLEFT,
            HTBOTTOMRIGHT,
        ] {
            for message in [WM_NCLBUTTONDOWN, WM_NCLBUTTONDBLCLK] {
                assert!(super::is_native_resize_mouse_down(message, edge as isize));
            }
            for message in [
                WM_LBUTTONDOWN,
                WM_NCLBUTTONUP,
                WM_NCMOUSEMOVE,
                WM_NCRBUTTONDOWN,
            ] {
                assert!(!super::is_native_resize_mouse_down(message, edge as isize));
            }
        }
        for area in [HTCLIENT, HTCAPTION, HTCLOSE, HTMAXBUTTON, HTMINBUTTON] {
            assert!(!super::is_native_resize_mouse_down(
                WM_NCLBUTTONDOWN,
                area as isize
            ));
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn only_resize_edges_and_corners_are_ignored() {
        use windows::Win32::UI::WindowsAndMessaging::{
            HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCAPTION, HTCLIENT, HTLEFT, HTNOWHERE, HTRIGHT,
            HTTOP, HTTOPLEFT, HTTOPRIGHT,
        };

        let resize_hit_tests = [
            HTLEFT,
            HTRIGHT,
            HTTOP,
            HTTOPLEFT,
            HTTOPRIGHT,
            HTBOTTOM,
            HTBOTTOMLEFT,
            HTBOTTOMRIGHT,
        ];
        for hit_test in resize_hit_tests {
            assert!(super::is_resize_hit_test_value(hit_test as isize));
        }

        assert!(!super::is_resize_hit_test_value(HTNOWHERE as isize));
        assert!(!super::is_resize_hit_test_value(HTCLIENT as isize));
        assert!(!super::is_resize_hit_test_value(HTCAPTION as isize));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn screen_facing_resize_edges_are_filtered_by_window_position() {
        use windows::Win32::{
            Foundation::RECT,
            UI::WindowsAndMessaging::{
                HTBOTTOM, HTBOTTOMLEFT, HTCLIENT, HTLEFT, HTRIGHT, HTTOP, HTTOPLEFT,
            },
        };

        let monitor = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        let left_window = RECT {
            left: 0,
            top: 200,
            right: 500,
            bottom: 800,
        };
        let left_restrictions =
            super::resize_edge_restrictions_for_rect(left_window, monitor, &[monitor]);
        assert!(left_restrictions.left);
        assert!(!left_restrictions.right);
        assert_eq!(
            super::filter_resize_hit_test_value(HTLEFT as isize, left_restrictions),
            HTCLIENT as isize
        );
        assert_eq!(
            super::filter_resize_hit_test_value(HTTOPLEFT as isize, left_restrictions),
            HTTOP as isize
        );

        let bottom_window = RECT {
            left: 600,
            top: 500,
            right: 1200,
            bottom: 1080,
        };
        let bottom_restrictions =
            super::resize_edge_restrictions_for_rect(bottom_window, monitor, &[monitor]);
        assert!(bottom_restrictions.bottom);
        assert_eq!(
            super::filter_resize_hit_test_value(HTBOTTOM as isize, bottom_restrictions),
            HTCLIENT as isize
        );
        assert_eq!(
            super::filter_resize_hit_test_value(HTBOTTOMLEFT as isize, bottom_restrictions),
            HTLEFT as isize
        );
        assert_eq!(
            super::filter_resize_hit_test_value(HTRIGHT as isize, bottom_restrictions),
            HTRIGHT as isize
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn internal_monitor_edges_remain_resizable() {
        use windows::Win32::Foundation::RECT;

        let left_monitor = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        let right_monitor = RECT {
            left: 1920,
            top: 0,
            right: 3840,
            bottom: 1080,
        };
        let window = RECT {
            left: 1420,
            top: 200,
            right: 1920,
            bottom: 800,
        };
        let restrictions = super::resize_edge_restrictions_for_rect(
            window,
            left_monitor,
            &[left_monitor, right_monitor],
        );
        assert!(!restrictions.left);
        assert!(!restrictions.right);
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn set_client_position(window: &Window, x: i32, y: i32) -> Result<(), String> {
    let hwnd = window_hwnd(window)?;
    let metrics = client_area_metrics(hwnd)?;
    set_client_position_native(hwnd, x, y, metrics.frame_offset_x, metrics.frame_offset_y)?;

    // 移动到不同 DPI 的显示器后，WM_DPICHANGED 会更新非客户区框架尺寸。
    // 用新框架再校正一次，避免客户区位置偏移几个物理像素。
    let updated_metrics = client_area_metrics(hwnd)?;
    if updated_metrics.client_left != x || updated_metrics.client_top != y {
        set_client_position_native(
            hwnd,
            x,
            y,
            updated_metrics.frame_offset_x,
            updated_metrics.frame_offset_y,
        )?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn set_client_position_native(
    hwnd: windows::Win32::Foundation::HWND,
    x: i32,
    y: i32,
    frame_offset_x: i32,
    frame_offset_y: i32,
) -> Result<(), String> {
    use windows::Win32::UI::WindowsAndMessaging::{
        SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos,
    };

    let window_x = x.saturating_sub(frame_offset_x);
    let window_y = y.saturating_sub(frame_offset_y);
    // SAFETY: hwnd 已通过 IsWindow 校验，只更新当前窗口的位置，不改变尺寸、焦点或层级。
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            window_x,
            window_y,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOZORDER,
        )
    }
    .map_err(|error| format!("移动主窗口失败：{error}"))
}

#[cfg(target_os = "windows")]
pub(crate) fn set_client_position_with_target(
    target: WindowAnimationTarget,
    x: i32,
    y: i32,
) -> Result<(), String> {
    use windows::Win32::UI::WindowsAndMessaging::{
        SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos,
    };

    let hwnd = windows::Win32::Foundation::HWND(target.hwnd as *mut _);
    let window_x = x.saturating_sub(target.frame_offset_x);
    let window_y = y.saturating_sub(target.frame_offset_y);
    // SAFETY: target 来自当前 GPUI 窗口，动画期间只更新该窗口的位置。
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            window_x,
            window_y,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOZORDER,
        )
    }
    .map_err(|error| format!("移动主窗口动画帧失败：{error}"))
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn set_client_position_with_target(
    _target: WindowAnimationTarget,
    _x: i32,
    _y: i32,
) -> Result<(), String> {
    Err("当前平台不支持按屏幕坐标移动窗口".to_owned())
}

#[cfg(target_os = "windows")]
pub(crate) fn set_client_bounds(
    window: &Window,
    bounds: PhysicalClientBounds,
) -> Result<(), String> {
    set_client_bounds_from_top(
        window,
        bounds.left,
        bounds.top,
        bounds.right,
        bounds.bottom.saturating_sub(bounds.top),
    )
}

#[cfg(target_os = "windows")]
pub(crate) fn set_client_bounds_from_top(
    window: &Window,
    left: i32,
    top: i32,
    right: i32,
    height: i32,
) -> Result<(), String> {
    let target = window_animation_target(window)?;
    set_client_bounds_from_top_with_target(target, left, top, right, height)
}

#[cfg(target_os = "windows")]
pub(crate) fn set_client_bounds_from_top_with_target(
    target: WindowAnimationTarget,
    left: i32,
    top: i32,
    right: i32,
    height: i32,
) -> Result<(), String> {
    let width = (right - left).max(1);
    let height = height.max(1);
    let hwnd = windows::Win32::Foundation::HWND(target.hwnd as *mut _);
    set_client_bounds_native(
        hwnd,
        left,
        top,
        width,
        height,
        target.frame_offset_x,
        target.frame_offset_y,
        target.frame_width,
        target.frame_height,
    )?;

    // 跨显示器 DPI 时，第一次 SetWindowPos 可能触发 WM_DPICHANGED，
    // 此时窗口非客户区尺寸已经改变，需要按新框架再校正一次客户区尺寸。
    let metrics = client_area_metrics(hwnd)?;
    if metrics.client_left != left
        || metrics.client_top != top
        || metrics.client_width != width
        || metrics.client_height != height
    {
        set_client_bounds_native(
            hwnd,
            left,
            top,
            width,
            height,
            metrics.frame_offset_x,
            metrics.frame_offset_y,
            metrics.frame_width,
            metrics.frame_height,
        )?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn set_client_bounds_native(
    hwnd: windows::Win32::Foundation::HWND,
    left: i32,
    top: i32,
    width: i32,
    height: i32,
    frame_offset_x: i32,
    frame_offset_y: i32,
    frame_width: i32,
    frame_height: i32,
) -> Result<(), String> {
    use windows::Win32::UI::WindowsAndMessaging::{SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos};

    let window_x = left.saturating_sub(frame_offset_x);
    let window_y = top.saturating_sub(frame_offset_y);
    // SAFETY: hwnd 来自当前 GPUI 窗口，只调整当前窗口客户区位置和尺寸。
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            window_x,
            window_y,
            width.saturating_add(frame_width),
            height.saturating_add(frame_height),
            SWP_NOACTIVATE | SWP_NOZORDER,
        )
    }
    .map_err(|error| format!("调整窗口客户区失败：{error}"))
}

pub(crate) fn work_area_for_physical_point(
    cx: &App,
    point: Point<f32>,
) -> Option<DisplayPlacement> {
    #[cfg(target_os = "windows")]
    {
        let _ = cx;
        windows_work_area_for_point(point)
    }

    #[cfg(not(target_os = "windows"))]
    {
        display_for_physical_point(cx, point)
    }
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn set_client_bounds_from_top_with_target(
    _target: WindowAnimationTarget,
    _left: i32,
    _top: i32,
    _right: i32,
    _height: i32,
) -> Result<(), String> {
    Err("当前平台不支持按屏幕坐标调整窗口客户区".to_owned())
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn physical_client_bounds(_window: &Window) -> Result<PhysicalClientBounds, String> {
    Err("当前平台不支持读取窗口客户区屏幕边界".to_owned())
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn set_client_position(_window: &Window, _x: i32, _y: i32) -> Result<(), String> {
    Err("当前平台不支持按屏幕坐标移动窗口".to_owned())
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn set_client_bounds(
    _window: &Window,
    _bounds: PhysicalClientBounds,
) -> Result<(), String> {
    Err("当前平台不支持按屏幕坐标调整窗口客户区".to_owned())
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn set_client_bounds_from_top(
    _window: &Window,
    _left: i32,
    _top: i32,
    _right: i32,
    _height: i32,
) -> Result<(), String> {
    Err("当前平台不支持按屏幕坐标调整窗口客户区".to_owned())
}

#[cfg(target_os = "windows")]
#[derive(Clone, Copy, Debug)]
struct ClientAreaMetrics {
    client_left: i32,
    client_top: i32,
    frame_offset_x: i32,
    frame_offset_y: i32,
    frame_width: i32,
    frame_height: i32,
    client_width: i32,
    client_height: i32,
}

#[cfg(target_os = "windows")]
fn client_area_metrics(
    hwnd: windows::Win32::Foundation::HWND,
) -> Result<ClientAreaMetrics, String> {
    use windows::Win32::{
        Foundation::{POINT, RECT},
        Graphics::Gdi::ClientToScreen,
        UI::WindowsAndMessaging::{GetClientRect, GetWindowRect},
    };

    let mut window_rect = RECT::default();
    // SAFETY: hwnd 已通过 IsWindow 校验，只读取当前窗口外框的屏幕坐标。
    unsafe { GetWindowRect(hwnd, &mut window_rect) }
        .map_err(|error| format!("读取主窗口外框失败：{error}"))?;

    let mut client_rect = RECT::default();
    // SAFETY: hwnd 已通过 IsWindow 校验，只读取当前窗口客户区尺寸。
    unsafe { GetClientRect(hwnd, &mut client_rect) }
        .map_err(|error| format!("读取主窗口客户区尺寸失败：{error}"))?;

    let mut client_origin = POINT { x: 0, y: 0 };
    // SAFETY: hwnd 已通过 IsWindow 校验，client_origin 是可写的客户区坐标结构体。
    if !unsafe { ClientToScreen(hwnd, &mut client_origin).as_bool() } {
        return Err("转换主窗口客户区坐标失败".to_owned());
    }

    Ok(ClientAreaMetrics {
        client_left: client_origin.x,
        client_top: client_origin.y,
        frame_offset_x: client_origin.x.saturating_sub(window_rect.left),
        frame_offset_y: client_origin.y.saturating_sub(window_rect.top),
        frame_width: (window_rect.right - window_rect.left)
            .saturating_sub(client_rect.right - client_rect.left)
            .max(0),
        frame_height: (window_rect.bottom - window_rect.top)
            .saturating_sub(client_rect.bottom - client_rect.top)
            .max(0),
        client_width: (client_rect.right - client_rect.left).max(1),
        client_height: (client_rect.bottom - client_rect.top).max(1),
    })
}

#[cfg(target_os = "windows")]
pub(crate) fn install_window_drag_constraints_hook(
    window: &Window,
    on_move_started: fn(),
    on_move_finished: fn(),
) -> Result<(), String> {
    use std::ffi::c_void;

    use windows::Win32::{
        Foundation::HANDLE,
        UI::WindowsAndMessaging::{
            GWL_STYLE, GWLP_WNDPROC, GetPropW, GetWindowLongPtrW, RemovePropW, SetPropW,
            SetWindowLongPtrW,
        },
    };

    let hwnd = window_hwnd(window)?;
    // SAFETY: hwnd 已通过 IsWindow 校验，仅检查当前窗口是否已经安装了拖拽消息处理。
    let existing = unsafe { GetPropW(hwnd, WINDOW_CONSTRAINTS_PROC_PROP) };
    if !existing.0.is_null() {
        // 贴边隐藏重新启用时，复用原窗口过程并再次关闭系统分屏样式。
        let hook = existing.0 as *mut WindowConstraintsHook;
        // SAFETY: 属性由本模块安装，指针在窗口销毁前有效。
        unsafe {
            (*hook).on_move_started = on_move_started;
            (*hook).on_move_finished = on_move_finished;
            (*hook).screen_boundaries =
                crate::windows::main::screen_boundary::ScreenBoundaries::capture();
            disable_window_snap(hwnd, (*hook).original_style);
        }
        return Ok(());
    }

    // 系统 Aero Snap 依赖可调整大小/最大化样式。保留原样式，贴边隐藏关闭时恢复。
    // SAFETY: hwnd 已通过 IsWindow 校验，仅修改当前窗口样式。
    let original_style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) };
    // SAFETY: 只移除当前窗口的系统分屏相关样式，不修改全局系统设置。
    unsafe { disable_window_snap(hwnd, original_style) };

    // SAFETY: hwnd 是当前 GPUI 窗口，保存原窗口过程并安装本模块的消息转发过程。
    let previous = unsafe {
        SetWindowLongPtrW(
            hwnd,
            GWLP_WNDPROC,
            window_constraints_proc as *const c_void as isize,
        )
    };
    if previous == 0 {
        // SAFETY: 安装窗口过程失败时恢复原始样式。
        unsafe { restore_window_style(hwnd, original_style) };
        return Err("设置窗口拖拽消息处理失败".to_owned());
    }

    let hook = Box::new(WindowConstraintsHook {
        previous,
        original_style,
        on_move_started,
        on_move_finished,
        screen_boundaries: crate::windows::main::screen_boundary::ScreenBoundaries::capture(),
    });
    let hook_ptr = Box::into_raw(hook);
    // SAFETY: hook_ptr 在窗口销毁时由 window_constraints_proc 回收，窗口属性保存其稳定地址。
    if unsafe {
        SetPropW(
            hwnd,
            WINDOW_CONSTRAINTS_PROC_PROP,
            Some(HANDLE(hook_ptr.cast())),
        )
    }
    .is_err()
    {
        // SAFETY: SetPropW 失败时窗口仍由本模块持有，恢复原窗口过程并回收分配的状态。
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_WNDPROC, previous);
            restore_window_style(hwnd, original_style);
            drop(Box::from_raw(hook_ptr));
            let _ = RemovePropW(hwnd, WINDOW_CONSTRAINTS_PROC_PROP);
        }
        return Err("保存窗口拖拽消息处理状态失败".to_owned());
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn install_window_drag_constraints_hook(
    _window: &Window,
    _on_move_started: fn(),
    _on_move_finished: fn(),
) -> Result<(), String> {
    Ok(())
}

#[cfg(target_os = "windows")]
const WINDOW_CONSTRAINTS_PROC_PROP: windows::core::PCWSTR =
    windows::core::w!("QcClipWindowConstraintsProc");

#[cfg(target_os = "windows")]
struct WindowConstraintsHook {
    previous: isize,
    original_style: isize,
    on_move_started: fn(),
    on_move_finished: fn(),
    screen_boundaries: crate::windows::main::screen_boundary::ScreenBoundaries,
}

#[cfg(target_os = "windows")]
unsafe fn disable_window_snap(hwnd: windows::Win32::Foundation::HWND, original_style: isize) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_STYLE, GetWindowLongPtrW, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
        SWP_NOOWNERZORDER, SWP_NOSIZE, SetWindowLongPtrW, SetWindowPos, WS_MAXIMIZEBOX,
    };

    let current_style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) };
    // 只移除最大化/系统分屏样式，保留 WS_THICKFRAME 以支持窗口边缘调整大小。
    let snap_styles = WS_MAXIMIZEBOX.0 as isize;
    if current_style == (original_style & !snap_styles) {
        return;
    }
    unsafe {
        SetWindowLongPtrW(hwnd, GWL_STYLE, original_style & !snap_styles);
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_FRAMECHANGED,
        );
    }
}

#[cfg(target_os = "windows")]
unsafe fn restore_window_style(hwnd: windows::Win32::Foundation::HWND, style: isize) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_STYLE, GetWindowLongPtrW, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
        SWP_NOOWNERZORDER, SWP_NOSIZE, SetWindowLongPtrW, SetWindowPos,
    };

    if unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } == style {
        return;
    }
    unsafe {
        SetWindowLongPtrW(hwnd, GWL_STYLE, style);
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_FRAMECHANGED,
        );
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn restore_window_drag_constraints_hook(window: &Window) -> Result<(), String> {
    use std::ffi::c_void;

    use windows::Win32::{
        Foundation::HANDLE,
        UI::WindowsAndMessaging::{
            GWLP_WNDPROC, GetPropW, RemovePropW, SetPropW, SetWindowLongPtrW,
        },
    };

    let hwnd = window_hwnd(window)?;
    let property = unsafe { GetPropW(hwnd, WINDOW_CONSTRAINTS_PROC_PROP) };
    if property.0.is_null() {
        return Ok(());
    }
    let hook = property.0 as *mut WindowConstraintsHook;
    // SAFETY: 属性由本模块安装，指针在窗口销毁前有效。
    let (previous, original_style) = unsafe { ((*hook).previous, (*hook).original_style) };
    // 先移除属性，恢复窗口过程失败时仍可把属性放回，避免留下不一致状态。
    let removed = unsafe { RemovePropW(hwnd, WINDOW_CONSTRAINTS_PROC_PROP) }
        .map_err(|error| format!("移除窗口拖拽消息处理失败：{error}"))?;
    if removed.0.is_null() {
        return Err("移除窗口拖拽消息处理失败：窗口属性不存在".to_owned());
    }

    // SAFETY: previous 是安装钩子时保存的原窗口过程地址。
    let restored = unsafe { SetWindowLongPtrW(hwnd, GWLP_WNDPROC, previous) };
    if restored == 0 {
        let state_restored =
            unsafe { SetPropW(hwnd, WINDOW_CONSTRAINTS_PROC_PROP, Some(HANDLE(removed.0))) };
        if state_restored.is_err() {
            return Err("恢复窗口消息处理失败，且无法恢复钩子状态".to_owned());
        }
        return Err("恢复原窗口消息处理失败".to_owned());
    }

    unsafe {
        restore_window_style(hwnd, original_style);
        drop(Box::from_raw(
            removed.0 as *mut c_void as *mut WindowConstraintsHook,
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn restore_window_drag_constraints_hook(_window: &Window) -> Result<(), String> {
    Ok(())
}

#[cfg(target_os = "windows")]
unsafe extern "system" fn window_constraints_proc(
    hwnd: windows::Win32::Foundation::HWND,
    message: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::{
        Foundation::{POINT, RECT},
        UI::WindowsAndMessaging::{
            CallWindowProcW, DefWindowProcW, GetPropW, MINMAXINFO, RemovePropW, WM_ENTERSIZEMOVE,
            WM_EXITSIZEMOVE, WM_GETMINMAXINFO, WM_MOVING, WM_NCDESTROY, WNDPROC,
        },
    };

    let hook_value = unsafe { GetPropW(hwnd, WINDOW_CONSTRAINTS_PROC_PROP) };
    let hook = hook_value.0 as *mut WindowConstraintsHook;
    if message == WM_ENTERSIZEMOVE && !hook.is_null() {
        // WM_ENTERSIZEMOVE 同时覆盖系统移动和窗口边缘缩放。
        // SAFETY: 回调由安装窗口钩子的 Rust 代码提供，窗口销毁前保持有效。
        unsafe { ((*hook).on_move_started)() };
    }
    if message == WM_MOVING && !hook.is_null() && lparam.0 != 0 {
        // WM_MOVING 只负责阻止窗口完全离开虚拟桌面，不执行边缘吸附。
        // SAFETY: WM_MOVING 的 lParam 是 Windows 提供的可写 RECT 指针，消息处理期间有效。
        let rect = unsafe { &mut *(lparam.0 as *mut RECT) };
        if let Ok(metrics) = client_area_metrics(hwnd) {
            let width = rect.right - rect.left;
            let height = rect.bottom - rect.top;
            let client_left = rect.left.saturating_add(metrics.frame_offset_x);
            let client_top = rect.top.saturating_add(metrics.frame_offset_y);
            let (client_left, client_top) = unsafe {
                (*hook).screen_boundaries.constrain_client_position(
                    client_left,
                    client_top,
                    width.saturating_sub(metrics.frame_width).max(1),
                    height.saturating_sub(metrics.frame_height).max(1),
                )
            };
            let left = client_left.saturating_sub(metrics.frame_offset_x);
            let top = client_top.saturating_sub(metrics.frame_offset_y);
            rect.left = left;
            rect.top = top;
            rect.right = left.saturating_add(width);
            rect.bottom = top.saturating_add(height);
        }
    }

    let result = if hook.is_null() || unsafe { (*hook).previous == 0 } {
        // SAFETY: 没有可转发的旧窗口过程时，使用系统默认窗口过程。
        unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
    } else {
        // SAFETY: previous 是 SetWindowLongPtrW 返回的原窗口过程地址。
        let previous: WNDPROC = unsafe { std::mem::transmute::<isize, WNDPROC>((*hook).previous) };
        unsafe { CallWindowProcW(previous, hwnd, message, wparam, lparam) }
    };

    if message == WM_GETMINMAXINFO && !hook.is_null() && lparam.0 != 0 {
        // WM_GETMINMAXINFO 约束最大化和最大跟踪尺寸，避免窗口尺寸超过当前工作区。
        // SAFETY: WM_GETMINMAXINFO 的 lParam 是 Windows 提供的可写 MINMAXINFO 指针。
        let info = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
        if let Some(work_area) = work_area_for_window(hwnd)
            && let Ok(metrics) = client_area_metrics(hwnd)
        {
            info.ptMaxPosition = POINT {
                x: work_area.x.round() as i32 - metrics.frame_offset_x,
                y: work_area.y.round() as i32 - metrics.frame_offset_y,
            };
            info.ptMaxSize = POINT {
                x: work_area.width.round() as i32 + metrics.frame_width,
                y: work_area.height.round() as i32 + metrics.frame_height,
            };
            info.ptMaxTrackSize = info.ptMaxSize;
        }
    }

    if message == WM_EXITSIZEMOVE && !hook.is_null() {
        // WM_EXITSIZEMOVE 表示 Windows 的系统移动循环已经结束，此时再执行贴边吸附。
        // SAFETY: 回调由安装窗口钩子的 Rust 代码提供，窗口销毁前保持有效。
        unsafe { ((*hook).on_move_finished)() };
    }

    if message == WM_NCDESTROY && !hook.is_null() {
        // SAFETY: 仅在窗口销毁时移除属性并回收安装时分配的状态。
        if let Ok(removed) = unsafe { RemovePropW(hwnd, WINDOW_CONSTRAINTS_PROC_PROP) }
            && !removed.0.is_null()
        {
            unsafe { drop(Box::from_raw(removed.0 as *mut WindowConstraintsHook)) };
        }
    }
    result
}

#[cfg(target_os = "windows")]
fn work_area_for_window(hwnd: windows::Win32::Foundation::HWND) -> Option<PhysicalDisplayBounds> {
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    };

    // SAFETY: hwnd 是当前仍有效的窗口句柄，仅查询其所在显示器。
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    if monitor.is_invalid() {
        return None;
    }
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: info 是可写的有效 MONITORINFO，monitor 来自 MonitorFromWindow。
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return None;
    }
    Some(PhysicalDisplayBounds {
        x: info.rcWork.left as f32,
        y: info.rcWork.top as f32,
        width: (info.rcWork.right - info.rcWork.left) as f32,
        height: (info.rcWork.bottom - info.rcWork.top) as f32,
    })
}

fn borderless_popup_options(
    bounds: Bounds<Pixels>,
    display_id: Option<DisplayId>,
    initially_visible: bool,
    animate_when_inactive: bool,
) -> WindowOptions {
    let mut options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        kind: WindowKind::PopUp,
        // 菜单窗口由 Win32 原生框架提供圆角和边框。必须让 GPUI 使用系统的
        // WM_NCCALCSIZE 结果，否则它为自绘标题栏保留顶部客户区起点，会吞掉原生框架的顶部边距。
        titlebar: if cfg!(target_os = "windows") {
            Some(TitlebarOptions {
                appears_transparent: false,
                ..Default::default()
            })
        } else {
            None
        },
        window_background: WindowBackgroundAppearance::Transparent,
        window_decorations: Some(WindowDecorations::Client),
        display_id,
        focus: false,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        window_min_size: Some(size(px(1.0), px(1.0))),
        show: initially_visible || !cfg!(target_os = "windows"),
        ..Default::default()
    };
    if animate_when_inactive {
        options.inactive_frame_interval = None;
    }
    options
}

#[cfg(test)]
mod borderless_popup_options_tests {
    use super::borderless_popup_options;
    use gpui::{Bounds, DisplayId};

    #[test]
    fn notification_popup_can_be_shown_during_creation() {
        let options =
            borderless_popup_options(Bounds::default(), Some(DisplayId::new(1)), true, true);

        assert!(options.show);
        assert!(!options.focus);
        assert!(options.inactive_frame_interval.is_none());
    }
}

fn borderless_normal_window_options(
    bounds: Bounds<Pixels>,
    display_id: Option<DisplayId>,
) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        kind: WindowKind::Normal,
        titlebar: Some(TitlebarOptions {
            appears_transparent: true,
            ..Default::default()
        }),
        window_background: WindowBackgroundAppearance::Opaque,
        window_decorations: Some(WindowDecorations::Client),
        display_id,
        focus: true,
        is_movable: true,
        is_resizable: false,
        is_minimizable: false,
        show: true,
        ..Default::default()
    }
}

impl BorderlessPopup {
    fn configure(window: &Window, mouse_passthrough: bool) -> Self {
        configure_borderless_popup(window, mouse_passthrough);
        Self
    }

    pub(crate) fn resize_to_physical_bounds(
        self,
        window: &mut Window,
        cx: &mut App,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    ) -> bool {
        set_borderless_popup_bounds(window, cx, x, y, width, height)
    }

    pub(crate) fn prepare_to_physical_bounds(
        self,
        window: &mut Window,
        cx: &mut App,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    ) -> bool {
        #[cfg(target_os = "windows")]
        {
            set_borderless_popup_bounds_with_visibility(window, cx, x, y, width, height, false)
        }
        #[cfg(not(target_os = "windows"))]
        {
            set_borderless_popup_bounds(window, cx, x, y, width, height)
        }
    }

    pub(crate) fn set_animation_bounds(
        self,
        window: &mut Window,
        cx: &mut App,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    ) -> bool {
        #[cfg(target_os = "windows")]
        {
            let left = x.round() as i32;
            let top = y.round() as i32;
            let right = left.saturating_add(width.round().max(1.0) as i32);
            let height = height.round().max(1.0) as i32;
            if let Err(error) = set_client_bounds_from_top(window, left, top, right, height) {
                tracing::error!("调整菜单动画窗口边界失败：{error}");
                return false;
            }
            window.bounds_changed(cx);
            true
        }
        #[cfg(not(target_os = "windows"))]
        {
            set_borderless_popup_bounds(window, cx, x, y, width, height)
        }
    }

    pub(crate) fn set_visible(self, window: &Window, visible: bool) {
        set_borderless_popup_visible(window, visible);
    }

    pub(crate) fn set_opacity(self, window: &Window, opacity: f32) -> Result<(), String> {
        set_window_opacity(window, opacity)
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn native_window_handle(self, window: &Window) -> Option<isize> {
        window_hwnd(window).ok().map(|hwnd| hwnd.0 as isize)
    }
}

impl BorderlessNormalWindow {
    fn configure(window: &Window) -> Self {
        #[cfg(target_os = "windows")]
        let (hwnd, border_offset) = configure_borderless_normal_window(window);
        #[cfg(not(target_os = "windows"))]
        let (hwnd, border_offset) = (0, configure_borderless_normal_window(window));

        Self {
            border_offset,
            hwnd,
        }
    }

    pub(crate) fn resize_to_physical_bounds(
        self,
        window: &mut Window,
        cx: &App,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    ) -> bool {
        set_borderless_normal_window_bounds(
            window,
            cx,
            self.hwnd,
            x,
            y,
            width,
            height,
            self.border_offset,
        )
    }
}

/// 让窗口保持无边框，同时由 DWM 提供窗口阴影与圆角。
///
/// 去掉系统标题栏相关样式位可以避免 DWM 在非客户区绘制最小化/最大化/关闭按钮
/// ——窗口背景透明后这些按钮会从自绘 UI 下方透出来。`WS_THICKFRAME` 必须保留，
/// DWM 的窗口阴影和圆角依赖它；`keep_min_max` 用于保留窗口自身绘制的最小化、
/// 最大化能力。
#[cfg(target_os = "windows")]
pub(crate) fn configure_frameless_window(window: &Window, keep_min_max: bool) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::{
        Foundation::HWND,
        Graphics::Dwm::{
            DWMNCRP_USEWINDOWSTYLE, DWMWA_NCRENDERING_POLICY, DWMWA_WINDOW_CORNER_PREFERENCE,
            DWMWCP_ROUND, DwmSetWindowAttribute,
        },
        UI::WindowsAndMessaging::{
            GWL_STYLE, GetWindowLongPtrW, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
            SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos,
            WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_SYSMENU, WS_THICKFRAME,
        },
    };

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        tracing::error!("无法获取窗口句柄，未应用无边框窗口样式");
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let hwnd = HWND(handle.hwnd.get() as *mut _);

    let mut removed = WS_SYSMENU.0 as isize;
    if !keep_min_max {
        removed |= WS_MINIMIZEBOX.0 as isize | WS_MAXIMIZEBOX.0 as isize;
    }

    // SAFETY: hwnd 来自当前 GPUI 窗口，只调整该窗口的样式位与 DWM 外观。
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        // 创建时 GPUI 按 is_resizable 决定是否带 WS_THICKFRAME，先记录再改写样式。
        let resizable = style & WS_THICKFRAME.0 as isize != 0;
        // 先安装客户区覆盖处理，再触发框架刷新，让本次刷新就按新规则计算客户区。
        install_frameless_client_area_hook(hwnd, resizable);

        let frameless = (style & !removed) | WS_THICKFRAME.0 as isize;
        if frameless != style {
            SetWindowLongPtrW(hwnd, GWL_STYLE, frameless);
            let _ = SetWindowPos(
                hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_NOMOVE
                    | SWP_NOSIZE
                    | SWP_NOZORDER
                    | SWP_NOACTIVATE
                    | SWP_NOOWNERZORDER
                    | SWP_FRAMECHANGED,
            );
        }

        let rendering_policy = DWMNCRP_USEWINDOWSTYLE;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_NCRENDERING_POLICY,
            &rendering_policy as *const _ as _,
            std::mem::size_of_val(&rendering_policy) as u32,
        );
        let corner_preference = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner_preference as *const _ as _,
            std::mem::size_of_val(&corner_preference) as u32,
        );

        notify_client_size_changed(hwnd);
    }
}

/// 客户区调整后主动同步一次尺寸。
///
/// 窗口尚未显示时系统不会为框架变化补发 `WM_SIZE`，GPUI 会沿用创建时的旧客户区尺寸，
/// 表现为自绘内容没有铺满窗口、需要交互一次才刷新。
#[cfg(target_os = "windows")]
fn notify_client_size_changed(hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::{
        Foundation::{LPARAM, RECT, WPARAM},
        UI::WindowsAndMessaging::{GetClientRect, SIZE_RESTORED, SendMessageW, WM_SIZE},
    };

    let mut client = RECT::default();
    // SAFETY: client 是本栈帧上的有效 RECT，hwnd 来自当前 GPUI 窗口。
    if unsafe { GetClientRect(hwnd, &mut client) }.is_err() {
        return;
    }
    let width = (client.right - client.left).max(1);
    let height = (client.bottom - client.top).max(1);
    let size_lparam = ((height as isize & 0xFFFF) << 16) | (width as isize & 0xFFFF);
    // SAFETY: 只向当前窗口投递标准 WM_SIZE 消息，参数按窗口消息约定打包。
    unsafe {
        SendMessageW(
            hwnd,
            WM_SIZE,
            Some(WPARAM(SIZE_RESTORED as usize)),
            Some(LPARAM(size_lparam)),
        );
    }
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn configure_frameless_window(_window: &Window, _keep_min_max: bool) {}

/// 无边框窗口的窗口过程属性名。
#[cfg(target_os = "windows")]
const FRAMELESS_PROC_PROP: windows::core::PCWSTR = windows::core::w!("QcClipFramelessProc");

#[cfg(target_os = "windows")]
struct FramelessWindowHook {
    previous: isize,
    /// 窗口创建时是否允许缩放，用于在客户区覆盖整窗后补回边缘缩放热区。
    resizable: bool,
    resizing: bool,
    resize_finished: bool,
}

#[cfg(target_os = "windows")]
pub(crate) fn take_window_resize_finished(window: &Window) -> Result<bool, String> {
    Ok(take_window_resize_finished_for_hwnd(window_hwnd(window)?))
}

#[cfg(target_os = "windows")]
fn take_window_resize_finished_for_hwnd(hwnd: windows::Win32::Foundation::HWND) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::GetPropW;

    // SAFETY: 属性由本模块安装，查询与窗口消息处理均发生在窗口线程。
    let hook = unsafe { GetPropW(hwnd, FRAMELESS_PROC_PROP) }.0 as *mut FramelessWindowHook;
    if hook.is_null() {
        return false;
    }
    // SAFETY: 窗口线程独占访问仍有效的钩子状态。
    unsafe { std::mem::take(&mut (*hook).resize_finished) }
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn take_window_resize_finished(_window: &Window) -> Result<bool, String> {
    Ok(false)
}

/// 安装客户区覆盖整个窗口的消息处理。
///
/// GPUI 处理 `WM_NCCALCSIZE` 时只恢复顶边，左右下三边保留窗口框架的内缩；
/// 窗口材质会填满整块非客户区，于是自绘 UI 外围出现一圈纯材质区域（Win10 尤其明显）。
/// 这里直接声明客户区等于窗口矩形，同时保留 `WS_THICKFRAME` 带来的阴影与圆角，
/// 拖动与缩放仍由 GPUI 自己的命中测试负责。
#[cfg(target_os = "windows")]
fn install_frameless_client_area_hook(hwnd: windows::Win32::Foundation::HWND, resizable: bool) {
    use std::ffi::c_void;
    use windows::Win32::{
        Foundation::HANDLE,
        UI::WindowsAndMessaging::{GWLP_WNDPROC, GetPropW, SetPropW, SetWindowLongPtrW},
    };

    // SAFETY: 已安装过时直接复用，避免重复包装窗口过程。
    if !unsafe { GetPropW(hwnd, FRAMELESS_PROC_PROP) }.0.is_null() {
        return;
    }

    // SAFETY: hwnd 是当前 GPUI 窗口，保存原窗口过程并安装本模块的消息转发过程。
    let previous = unsafe {
        SetWindowLongPtrW(
            hwnd,
            GWLP_WNDPROC,
            frameless_window_proc as *const c_void as isize,
        )
    };
    if previous == 0 {
        return;
    }

    let hook_ptr = Box::into_raw(Box::new(FramelessWindowHook {
        previous,
        resizable,
        resizing: false,
        resize_finished: false,
    }));
    // SAFETY: hook_ptr 在窗口销毁时由 frameless_window_proc 回收，窗口属性保存其稳定地址。
    if unsafe { SetPropW(hwnd, FRAMELESS_PROC_PROP, Some(HANDLE(hook_ptr.cast()))) }.is_err() {
        // SAFETY: 保存失败时恢复原窗口过程并回收分配的状态。
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_WNDPROC, previous);
            drop(Box::from_raw(hook_ptr));
        }
    }
}

#[cfg(target_os = "windows")]
unsafe extern "system" fn frameless_window_proc(
    hwnd: windows::Win32::Foundation::HWND,
    message: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::LRESULT;
    use windows::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, GetPropW, HTCLIENT, IsIconic, IsZoomed, RemovePropW, SIZE_MINIMIZED,
        WM_ENTERSIZEMOVE, WM_EXITSIZEMOVE, WM_NCCALCSIZE, WM_NCDESTROY, WM_NCHITTEST, WM_SIZE,
        WM_SIZING,
    };

    // SAFETY: 属性由本模块安装，指针在窗口销毁前有效。
    let hook = unsafe { GetPropW(hwnd, FRAMELESS_PROC_PROP) }.0 as *mut FramelessWindowHook;
    if !hook.is_null() {
        // WM_SIZING 只来自手动缩放，动画的 SetWindowPos 不会触发它。
        // SAFETY: 钩子状态在窗口消息处理期间有效，只由窗口线程访问。
        unsafe {
            if message == WM_ENTERSIZEMOVE {
                (*hook).resizing = false;
            } else if message == WM_SIZING {
                (*hook).resizing = true;
            }
        }
    }
    // 最大化与最小化时交回 GPUI 处理，避免与系统的工作区计算冲突。
    let is_zoom_or_minimized = unsafe { IsZoomed(hwnd).as_bool() || IsIconic(hwnd).as_bool() };

    if !is_zoom_or_minimized
        && !hook.is_null()
        // SAFETY: hook 由本模块安装，窗口销毁前保持有效。
        && unsafe { (*hook).resizable }
        && is_native_resize_mouse_down(message, wparam.0 as isize)
    {
        // 缩放边缘优先交给系统，避免 GPUI 控件消费按下事件后阻止窗口缩放。
        // SAFETY: 消息来自当前窗口，参数保持原样传递给系统窗口过程。
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }

    // `WM_NCCALCSIZE` 的 `rgrc[0]` 已是窗口矩形，返回 0 即表示客户区覆盖整个窗口。
    if message == WM_NCCALCSIZE && wparam.0 != 0 && !is_zoom_or_minimized {
        return LRESULT(0);
    }

    if message == WM_NCHITTEST
        && !is_zoom_or_minimized
        && !hook.is_null()
        // SAFETY: hook 由本模块安装，窗口销毁前保持有效。
        && unsafe { (*hook).resizable }
    {
        // 先让 GPUI 认领自绘标题栏的拖动区域与窗口按钮，其余区域再补上缩放热区。
        let restrictions = unsafe { screen_edge_resize_restrictions(hwnd) };
        let result = filter_resize_hit_test_value(
            unsafe { call_hook_previous(hook, hwnd, message, wparam, lparam) }.0,
            restrictions,
        );
        let result = LRESULT(result);
        if result == LRESULT(HTCLIENT as isize)
            && let Some(edge) = unsafe { resize_hit_test(hwnd, lparam) }
        {
            return LRESULT(filter_resize_hit_test_value(edge.0, restrictions));
        }
        return result;
    }

    let result = unsafe { call_hook_previous(hook, hwnd, message, wparam, lparam) };

    if message == WM_EXITSIZEMOVE && !hook.is_null() {
        // SAFETY: 原窗口过程处理完缩放结束消息后，客户区尺寸已稳定。
        unsafe {
            (*hook).resize_finished |= (*hook).resizing;
            (*hook).resizing = false;
        }
    }

    if message == WM_SIZE && wparam.0 != SIZE_MINIMIZED as usize && acrylic_material_enabled(hwnd) {
        refresh_acrylic_material(hwnd);
    }

    if message == WM_NCDESTROY && !hook.is_null() {
        // SAFETY: 仅在窗口销毁时移除属性并回收安装时分配的状态。
        if let Ok(removed) = unsafe { RemovePropW(hwnd, FRAMELESS_PROC_PROP) }
            && !removed.0.is_null()
        {
            unsafe { drop(Box::from_raw(removed.0 as *mut FramelessWindowHook)) };
        }
        set_acrylic_material_enabled(hwnd, false);
    }

    result
}

#[cfg(all(test, target_os = "windows"))]
mod native_resize_tests {
    use super::*;
    use windows::Win32::{
        Foundation::{HWND, LPARAM, RECT, WPARAM},
        UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, SWP_NOACTIVATE, SWP_NOZORDER, SendMessageW,
            SetWindowPos, WM_ENTERSIZEMOVE, WM_EXITSIZEMOVE, WM_SIZING, WS_POPUP,
        },
    };

    struct TestWindow(HWND);

    impl Drop for TestWindow {
        fn drop(&mut self) {
            // SAFETY: 测试窗口由当前线程创建，并在同一线程销毁。
            let _ = unsafe { DestroyWindow(self.0) };
        }
    }

    #[test]
    fn only_manual_resize_produces_a_finished_resize() {
        // SAFETY: 使用系统窗口类创建不显示的测试窗口，所有操作均在创建线程执行。
        let hwnd = unsafe {
            CreateWindowExW(
                Default::default(),
                windows::core::w!("STATIC"),
                windows::core::w!("尺寸保存测试"),
                WS_POPUP,
                0,
                0,
                360,
                520,
                None,
                None,
                None,
                None,
            )
        }
        .expect("无法创建尺寸保存测试窗口");
        let window = TestWindow(hwnd);
        install_frameless_client_area_hook(window.0, true);

        // SAFETY: 窗口句柄有效，SetWindowPos 模拟动画引发的尺寸变化。
        unsafe {
            SetWindowPos(window.0, None, 0, 0, 360, 1, SWP_NOACTIVATE | SWP_NOZORDER)
                .expect("无法模拟窗口动画尺寸");
        }
        assert!(!take_window_resize_finished_for_hwnd(window.0));

        // SAFETY: 系统移动消息不携带指针参数。
        unsafe {
            SendMessageW(window.0, WM_ENTERSIZEMOVE, None, None);
            SendMessageW(window.0, WM_EXITSIZEMOVE, None, None);
        }
        assert!(!take_window_resize_finished_for_hwnd(window.0));

        let mut bounds = RECT {
            left: 0,
            top: 0,
            right: 480,
            bottom: 640,
        };
        // SAFETY: WM_SIZING 的矩形指针在同步消息处理期间有效。
        unsafe {
            SendMessageW(window.0, WM_ENTERSIZEMOVE, None, None);
            SendMessageW(
                window.0,
                WM_SIZING,
                Some(WPARAM(8)),
                Some(LPARAM(&mut bounds as *mut RECT as isize)),
            );
            SetWindowPos(
                window.0,
                None,
                0,
                0,
                480,
                640,
                SWP_NOACTIVATE | SWP_NOZORDER,
            )
            .expect("无法模拟手动缩放窗口");
        }
        assert!(!take_window_resize_finished_for_hwnd(window.0));
        // SAFETY: 缩放结束消息不携带指针参数。
        unsafe {
            SendMessageW(window.0, WM_EXITSIZEMOVE, None, None);
        }
        assert!(take_window_resize_finished_for_hwnd(window.0));
        assert!(!take_window_resize_finished_for_hwnd(window.0));
    }
}

#[cfg(target_os = "windows")]
unsafe fn call_hook_previous(
    hook: *mut FramelessWindowHook,
    hwnd: windows::Win32::Foundation::HWND,
    message: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::{CallWindowProcW, DefWindowProcW, WNDPROC};

    if hook.is_null() || unsafe { (*hook).previous == 0 } {
        // SAFETY: 没有可转发的旧窗口过程时，使用系统默认窗口过程。
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    // SAFETY: previous 是 SetWindowLongPtrW 返回的原窗口过程地址。
    let previous: WNDPROC = unsafe { std::mem::transmute::<isize, WNDPROC>((*hook).previous) };
    unsafe { CallWindowProcW(previous, hwnd, message, wparam, lparam) }
}

/// 计算左、右、下三边的缩放热区。
///
/// GPUI 自带的命中测试只处理顶边，左右下三边原本由系统非客户区提供；
/// 客户区覆盖整窗后需要在这里补回来。顶边仍交给 GPUI，避免覆盖标题栏的拖动区域。
#[cfg(target_os = "windows")]
unsafe fn resize_hit_test(
    hwnd: windows::Win32::Foundation::HWND,
    lparam: windows::Win32::Foundation::LPARAM,
) -> Option<windows::Win32::Foundation::LRESULT> {
    use windows::Win32::{
        Foundation::{LRESULT, RECT},
        UI::{
            HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi},
            WindowsAndMessaging::{
                GetWindowRect, HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTLEFT, HTRIGHT,
                SM_CXPADDEDBORDER, SM_CXSIZEFRAME, SM_CYSIZEFRAME,
            },
        },
    };

    let mut rect = RECT::default();
    // SAFETY: rect 是本栈帧上的有效 RECT，hwnd 来自当前 GPUI 窗口。
    unsafe { GetWindowRect(hwnd, &mut rect) }.ok()?;
    // 保持 Windows 原生的缩放热区宽度，边缘按下事件由窗口钩子优先交给系统。
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let padded = unsafe { GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi) };
    let frame_x = unsafe { GetSystemMetricsForDpi(SM_CXSIZEFRAME, dpi) } + padded;
    let frame_y = unsafe { GetSystemMetricsForDpi(SM_CYSIZEFRAME, dpi) } + padded;

    // WM_NCHITTEST 的 lParam 低 16 位为 x、高 16 位为 y，均为有符号屏幕坐标。
    let packed = lparam.0 as u32;
    let x = (packed & 0xFFFF) as u16 as i16 as i32;
    let y = ((packed >> 16) & 0xFFFF) as u16 as i16 as i32;
    let left = x - rect.left;
    let right = rect.right - 1 - x;
    let top = y - rect.top;
    let bottom = rect.bottom - 1 - y;
    if top < frame_y {
        return None;
    }

    // 只认窗口内部的边缘，避开光标在窗口外或已在客户区内部的情况。
    let on_left = (0..frame_x).contains(&left);
    let on_right = (0..frame_x).contains(&right);
    let on_bottom = (0..frame_y).contains(&bottom);
    let code = if on_bottom && on_left {
        HTBOTTOMLEFT
    } else if on_bottom && on_right {
        HTBOTTOMRIGHT
    } else if on_bottom {
        HTBOTTOM
    } else if on_left {
        HTLEFT
    } else if on_right {
        HTRIGHT
    } else {
        return None;
    };
    Some(LRESULT(code as isize))
}

#[cfg(target_os = "windows")]
unsafe fn screen_edge_resize_restrictions(
    hwnd: windows::Win32::Foundation::HWND,
) -> ResizeEdgeRestrictions {
    use windows::Win32::{
        Foundation::LPARAM,
        Graphics::Gdi::{
            EnumDisplayMonitors, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO,
            MonitorFromWindow,
        },
    };

    let Ok(client) = client_area_metrics(hwnd) else {
        return ResizeEdgeRestrictions::default();
    };
    let window_rect = windows::Win32::Foundation::RECT {
        left: client.client_left,
        top: client.client_top,
        right: client.client_left.saturating_add(client.client_width),
        bottom: client.client_top.saturating_add(client.client_height),
    };

    // SAFETY: hwnd 来自当前窗口，使用最近显示器作为窗口边界判定基准。
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    if monitor.is_invalid() {
        return ResizeEdgeRestrictions::default();
    }
    let mut monitor_info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: monitor 来自 MonitorFromWindow，monitor_info 是有效的可写结构体。
    if !unsafe { GetMonitorInfoW(monitor, &mut monitor_info) }.as_bool() {
        return ResizeEdgeRestrictions::default();
    }

    let mut monitors = Vec::new();
    // SAFETY: 回调仅向本次调用栈上的 Vec 写入显示器矩形。
    let _ = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor_rect),
            LPARAM(&mut monitors as *mut _ as isize),
        )
    };
    resize_edge_restrictions_for_rect(window_rect, monitor_info.rcMonitor, &monitors)
}

#[cfg(target_os = "windows")]
unsafe extern "system" fn collect_monitor_rect(
    _monitor: windows::Win32::Graphics::Gdi::HMONITOR,
    _dc: windows::Win32::Graphics::Gdi::HDC,
    rect: *mut windows::Win32::Foundation::RECT,
    data: windows::Win32::Foundation::LPARAM,
) -> windows::core::BOOL {
    if !rect.is_null() && data.0 != 0 {
        // SAFETY: rect 由 EnumDisplayMonitors 提供，data 指向调用方栈上的 Vec。
        let monitor_rect = unsafe { *rect };
        let monitors = unsafe { &mut *(data.0 as *mut Vec<windows::Win32::Foundation::RECT>) };
        monitors.push(monitor_rect);
    }
    windows::core::BOOL(1)
}

#[cfg(target_os = "windows")]
fn resize_edge_restrictions_for_rect(
    window: windows::Win32::Foundation::RECT,
    monitor: windows::Win32::Foundation::RECT,
    monitors: &[windows::Win32::Foundation::RECT],
) -> ResizeEdgeRestrictions {
    const TOLERANCE: i32 = 2;

    let has_adjacent = |side: fn(&windows::Win32::Foundation::RECT) -> i32,
                        other_side: fn(&windows::Win32::Foundation::RECT) -> i32,
                        start: fn(&windows::Win32::Foundation::RECT) -> i32,
                        end: fn(&windows::Win32::Foundation::RECT) -> i32| {
        monitors.iter().any(|other| {
            (other_side(other) - side(&monitor)).abs() <= TOLERANCE
                && start(other) < end(&monitor)
                && end(other) > start(&monitor)
        })
    };

    ResizeEdgeRestrictions {
        left: (window.left - monitor.left).abs() <= TOLERANCE
            && !has_adjacent(
                |rect| rect.left,
                |rect| rect.right,
                |rect| rect.top,
                |rect| rect.bottom,
            ),
        right: (window.right - monitor.right).abs() <= TOLERANCE
            && !has_adjacent(
                |rect| rect.right,
                |rect| rect.left,
                |rect| rect.top,
                |rect| rect.bottom,
            ),
        top: (window.top - monitor.top).abs() <= TOLERANCE
            && !has_adjacent(
                |rect| rect.top,
                |rect| rect.bottom,
                |rect| rect.left,
                |rect| rect.right,
            ),
        bottom: (window.bottom - monitor.bottom).abs() <= TOLERANCE
            && !has_adjacent(
                |rect| rect.bottom,
                |rect| rect.top,
                |rect| rect.left,
                |rect| rect.right,
            ),
    }
}

#[cfg(target_os = "windows")]
pub fn configure_window(window: &Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::{
        Foundation::HWND,
        Graphics::Dwm::{
            DWM_WINDOW_CORNER_PREFERENCE, DWMWA_WINDOW_CORNER_PREFERENCE, DwmSetWindowAttribute,
        },
        UI::WindowsAndMessaging::{
            GWL_EXSTYLE, GetWindowLongPtrW, HWND_TOPMOST, SWP_FRAMECHANGED, SWP_NOACTIVATE,
            SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos, WS_EX_APPWINDOW,
            WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
        },
    };

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        tracing::error!("无法获取窗口句柄，窗口无法置顶");
        return;
    };

    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        tracing::error!("当前平台不支持窗口置顶");
        return;
    };

    let hwnd = HWND(handle.hwnd.get() as *mut _);

    // 保留工具窗口行为，避免普通窗口出现在任务栏中。可调整大小样式由 GPUI 在创建时设置。
    // SAFETY: hwnd 来自当前 GPUI 窗口句柄，只修改该窗口的扩展样式并通知系统刷新窗口框架。
    let ex_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
    let tool_window_style = (ex_style
        | WS_EX_LAYERED.0 as isize
        | WS_EX_TOOLWINDOW.0 as isize
        | WS_EX_NOACTIVATE.0 as isize)
        & !(WS_EX_APPWINDOW.0 as isize);
    unsafe {
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, tool_window_style);
    }

    // Normal 窗口和工具窗口的 DWM 默认圆角策略可能不同，这里固定使用标准圆角。
    let corner_preference = DWM_WINDOW_CORNER_PREFERENCE(2);
    if super::window_appearance::supports_native_rounding()
        && let Err(error) = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &corner_preference as *const _ as _,
                std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
            )
        }
    {
        tracing::error!("设置窗口圆角失败：{error}");
    }

    if let Err(error) = unsafe {
        SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER | SWP_FRAMECHANGED,
        )
    } {
        tracing::error!("刷新窗口框架失败：{error}");
    }

    if let Err(error) = unsafe {
        SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        )
    } {
        tracing::error!("设置窗口置顶失败：{error}");
    }
    configure_frameless_window(window, false);
}

#[cfg(not(target_os = "windows"))]
pub fn configure_window(_window: &Window) {}

#[cfg(target_os = "windows")]
fn configure_borderless_popup(window: &Window, mouse_passthrough: bool) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Graphics::Dwm::{
        DWMNCRP_DISABLED, DWMNCRP_USEWINDOWSTYLE, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE,
        DWMWA_NCRENDERING_POLICY, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND, DWMWCP_ROUND,
        DwmSetWindowAttribute,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GWL_STYLE, GetWindowLongPtrW, HWND_TOPMOST, SWP_FRAMECHANGED, SWP_NOACTIVATE,
        SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SetWindowLongPtrW, SetWindowPos,
        WS_EX_APPWINDOW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP, WS_VISIBLE,
    };

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        tracing::error!("无法获取无边框弹窗句柄，将使用默认窗口样式");
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        tracing::error!("当前平台不支持无边框弹窗样式配置");
        return;
    };
    let hwnd = windows::Win32::Foundation::HWND(handle.hwnd.get() as *mut _);

    // 菜单弹窗使用 DWM 原生圆角和阴影，但不添加 WS_THICKFRAME。后者会启用窗口调整大小
    // 命中区域，并在透明菜单上暴露一圈非客户区；鼠标穿透弹窗同样保持纯客户区。
    // SAFETY: hwnd 来自当前 GPUI 无边框弹窗，只修改该窗口的样式、DWM 外观和窗口层级。
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        let visible = style & WS_VISIBLE.0 as isize;
        let native_frame = !mouse_passthrough;
        SetWindowLongPtrW(hwnd, GWL_STYLE, WS_POPUP.0 as isize | visible);

        let rendering_policy = if native_frame {
            DWMNCRP_USEWINDOWSTYLE
        } else {
            DWMNCRP_DISABLED
        };
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_NCRENDERING_POLICY,
            &rendering_policy as *const _ as _,
            std::mem::size_of_val(&rendering_policy) as u32,
        );
        let corner_preference = if native_frame {
            DWMWCP_ROUND
        } else {
            DWMWCP_DONOTROUND
        };
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner_preference as *const _ as _,
            std::mem::size_of_val(&corner_preference) as u32,
        );
        // 原生框架仍负责圆角、阴影和非客户区几何，但不绘制默认黑色边框，避免在
        // 菜单客户区之外出现一条额外的顶部黑色区域。
        let border_color = DWMWA_COLOR_NONE;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &border_color as *const _ as _,
            std::mem::size_of_val(&border_color) as u32,
        );

        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let ex_style = (ex_style
            | WS_EX_TOOLWINDOW.0 as isize
            | WS_EX_NOACTIVATE.0 as isize
            | if mouse_passthrough {
                WS_EX_LAYERED.0 as isize
            } else {
                0
            })
            & !(WS_EX_APPWINDOW.0 as isize);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex_style);

        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_FRAMECHANGED,
        );
    }

    if mouse_passthrough {
        install_mouse_passthrough(hwnd);
    }
}

#[cfg(not(target_os = "windows"))]
fn configure_borderless_popup(_window: &Window, _mouse_passthrough: bool) {}

#[cfg(target_os = "windows")]
const MOUSE_PASSTHROUGH_PROC_PROP: windows::core::PCWSTR =
    windows::core::w!("QcClipMousePassthroughProc");

#[cfg(target_os = "windows")]
unsafe extern "system" fn mouse_passthrough_window_proc(
    hwnd: windows::Win32::Foundation::HWND,
    message: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use std::ffi::c_void;
    use windows::Win32::UI::WindowsAndMessaging::{
        CallWindowProcW, DefWindowProcW, GetPropW, HTTRANSPARENT, RemovePropW, WM_NCDESTROY,
        WM_NCHITTEST,
    };

    if message == WM_NCHITTEST {
        return windows::Win32::Foundation::LRESULT(HTTRANSPARENT as isize);
    }

    let previous = unsafe { GetPropW(hwnd, MOUSE_PASSTHROUGH_PROC_PROP) };
    let result = if previous.0.is_null() {
        unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
    } else {
        let previous = unsafe {
            std::mem::transmute::<*mut c_void, windows::Win32::UI::WindowsAndMessaging::WNDPROC>(
                previous.0,
            )
        };
        unsafe { CallWindowProcW(previous, hwnd, message, wparam, lparam) }
    };

    if message == WM_NCDESTROY {
        let _ = unsafe { RemovePropW(hwnd, MOUSE_PASSTHROUGH_PROC_PROP) };
    }
    result
}

#[cfg(target_os = "windows")]
fn install_mouse_passthrough(hwnd: windows::Win32::Foundation::HWND) {
    use std::ffi::c_void;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::UI::WindowsAndMessaging::{
        GWLP_WNDPROC, RemovePropW, SetPropW, SetWindowLongPtrW,
    };

    let previous = unsafe {
        SetWindowLongPtrW(
            hwnd,
            GWLP_WNDPROC,
            mouse_passthrough_window_proc as *const c_void as isize,
        )
    };
    if previous == 0 {
        let _ = unsafe {
            windows::Win32::UI::WindowsAndMessaging::RemovePropW(hwnd, MOUSE_PASSTHROUGH_PROC_PROP)
        };
        tracing::error!("设置拖拽预览窗口过程失败");
        return;
    }

    if unsafe {
        SetPropW(
            hwnd,
            MOUSE_PASSTHROUGH_PROC_PROP,
            Some(HANDLE(previous as *mut c_void)),
        )
    }
    .is_err()
    {
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_WNDPROC, previous);
            let _ = RemovePropW(hwnd, MOUSE_PASSTHROUGH_PROC_PROP);
        }
        tracing::error!("设置拖拽预览鼠标穿透属性失败");
    }
}

#[cfg(target_os = "windows")]
fn configure_borderless_normal_window(window: &Window) -> (isize, PopupBorderOffset) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Graphics::Dwm::{
        DWM_WINDOW_CORNER_PREFERENCE, DWMWA_WINDOW_CORNER_PREFERENCE, DwmSetWindowAttribute,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongPtrW, HWND_TOPMOST, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
        SWP_NOOWNERZORDER, SWP_NOSIZE, SetWindowLongPtrW, SetWindowPos, WS_EX_APPWINDOW,
        WS_EX_TOOLWINDOW,
    };

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        tracing::error!("无法获取普通窗口句柄，将使用默认窗口样式");
        return (0, PopupBorderOffset::default());
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        tracing::error!("当前平台不支持无边框普通窗口样式配置");
        return (0, PopupBorderOffset::default());
    };
    let hwnd_value = handle.hwnd.get();
    let hwnd = windows::Win32::Foundation::HWND(hwnd_value as *mut _);

    // Normal 窗口仍保留普通窗口类型，同时作为临时工具窗口隐藏任务栏图标。
    // 不设置 WS_EX_NOACTIVATE，确保输入框可以正常获得焦点。
    // SAFETY: hwnd 来自当前 GPUI 窗口句柄，只修改该窗口的样式和 DWM 外观属性。
    unsafe {
        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let ex_style = (ex_style | WS_EX_TOOLWINDOW.0 as isize) & !(WS_EX_APPWINDOW.0 as isize);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex_style);

        let corner_preference = DWM_WINDOW_CORNER_PREFERENCE(2);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner_preference as *const _ as _,
            std::mem::size_of_val(&corner_preference) as u32,
        );
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_FRAMECHANGED,
        );
    }

    (hwnd_value, native_border_offset(hwnd))
}

#[cfg(not(target_os = "windows"))]
fn configure_borderless_normal_window(_window: &Window) -> PopupBorderOffset {
    PopupBorderOffset::default()
}

#[cfg(target_os = "windows")]
fn native_border_offset(hwnd: windows::Win32::Foundation::HWND) -> PopupBorderOffset {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, GetWindowRect};

    unsafe {
        let mut window_rect = RECT::default();
        let mut client_rect = RECT::default();
        if GetWindowRect(hwnd, &mut window_rect).is_err()
            || GetClientRect(hwnd, &mut client_rect).is_err()
        {
            return PopupBorderOffset::default();
        }
        PopupBorderOffset {
            width: (window_rect.right - window_rect.left) - (client_rect.right - client_rect.left),
            height: (window_rect.bottom - window_rect.top) - (client_rect.bottom - client_rect.top),
        }
    }
}

#[cfg(target_os = "windows")]
fn physical_client_origin_on_desktop(window: &Window) -> Option<Point<f32>> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::{Foundation::POINT, Graphics::Gdi::ClientToScreen};

    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return None;
    };
    let hwnd = windows::Win32::Foundation::HWND(handle.hwnd.get() as *mut _);
    let mut origin = POINT { x: 0, y: 0 };
    // SAFETY: hwnd 是当前窗口句柄，origin 是本地可写坐标结构。
    if !unsafe { ClientToScreen(hwnd, &mut origin).as_bool() } {
        return None;
    }
    Some(Point {
        x: origin.x as f32,
        y: origin.y as f32,
    })
}

#[cfg(target_os = "windows")]
pub(crate) fn logical_client_origin_on_desktop(window: &Window) -> Option<Point<f32>> {
    let origin = physical_client_origin_on_desktop(window)?;
    Some(CoordinateConverter::new(window.scale_factor()).physical_point_to_logical(origin))
}

#[cfg(target_os = "windows")]
pub fn logical_to_physical_desktop(window: &Window, point: Point<Pixels>) -> Option<Point<f32>> {
    let origin = physical_client_origin_on_desktop(window)?;
    let scale_factor = window.scale_factor().max(1.0);
    let converter = CoordinateConverter::new(scale_factor);
    Some(Point {
        x: origin.x + converter.logical_to_physical(f32::from(point.x)),
        y: origin.y + converter.logical_to_physical(f32::from(point.y)),
    })
}

#[cfg(not(target_os = "windows"))]
pub fn logical_to_physical_desktop(window: &Window, point: Point<Pixels>) -> Option<Point<f32>> {
    let bounds = window.bounds();
    Some(Point {
        x: f32::from(bounds.origin.x) + f32::from(point.x),
        y: f32::from(bounds.origin.y) + f32::from(point.y),
    })
}

#[cfg(target_os = "windows")]
fn set_borderless_popup_bounds(
    window: &mut Window,
    cx: &mut App,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
) -> bool {
    set_borderless_popup_bounds_with_visibility(window, cx, x, y, width, height, true)
}

#[cfg(target_os = "windows")]
fn set_borderless_popup_bounds_with_visibility(
    window: &mut Window,
    cx: &mut App,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    show_after_update: bool,
) -> bool {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::{
        Foundation::HWND,
        UI::WindowsAndMessaging::{HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOOWNERZORDER, SetWindowPos},
    };

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return false;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return false;
    };
    let hwnd = HWND(handle.hwnd.get() as *mut _);
    let window_x = x.round() as i32;
    let window_y = y.round() as i32;
    let window_width = width.round().max(1.0) as i32;
    let window_height = height.round().max(1.0) as i32;
    let updated = unsafe {
        SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            window_x,
            window_y,
            window_width,
            window_height,
            SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        )
        .is_ok()
    };
    if !updated {
        return false;
    }

    // 先定位窗口触发 DPI 更新，再按实际客户区边框校正目标客户区尺寸。
    if let Err(error) = set_client_bounds_from_top(
        window,
        window_x,
        window_y,
        window_x.saturating_add(window_width),
        window_height,
    ) {
        tracing::error!("调整菜单窗口客户区失败：{error}");
    }
    window.bounds_changed(cx);
    if show_after_update {
        set_borderless_popup_visible(window, true);
    }
    true
}

#[cfg(not(target_os = "windows"))]
fn set_borderless_popup_bounds(
    window: &mut Window,
    _cx: &mut App,
    _x: f32,
    _y: f32,
    width: f32,
    height: f32,
) -> bool {
    let scale_factor = window.scale_factor().max(1.0);
    window.resize(size(
        px(width / scale_factor.max(1.0)),
        px(height / scale_factor.max(1.0)),
    ));
    true
}

#[cfg(target_os = "windows")]
fn set_borderless_popup_visible(window: &Window, visible: bool) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::UI::WindowsAndMessaging::{SW_HIDE, SW_SHOWNOACTIVATE, ShowWindowAsync};

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let hwnd = windows::Win32::Foundation::HWND(handle.hwnd.get() as *mut _);
    let command = if visible { SW_SHOWNOACTIVATE } else { SW_HIDE };
    unsafe {
        let _ = ShowWindowAsync(hwnd, command);
    }
}

#[cfg(not(target_os = "windows"))]
fn set_borderless_popup_visible(_window: &Window, _visible: bool) {}

#[cfg(target_os = "windows")]
fn set_borderless_normal_window_bounds(
    window: &mut Window,
    cx: &App,
    hwnd: isize,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    border_offset: PopupBorderOffset,
) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_SHOWWINDOW, SetWindowPos,
    };

    if hwnd == 0 {
        return false;
    }

    // 调用方已经将 GPUI 逻辑尺寸转换为物理尺寸，这里不能再次乘以缩放因子。
    let content_width = width.ceil().max(1.0) as i32;
    let content_height = height.ceil().max(1.0) as i32;
    let window_width = (content_width + border_offset.width).max(1);
    let window_height = (content_height + border_offset.height).max(1);
    let hwnd = windows::Win32::Foundation::HWND(hwnd as *mut _);

    window
        .spawn(cx, async move |_cx| {
            // SAFETY: hwnd 来自当前 GPUI 普通窗口；尺寸和坐标来自已完成布局的内容。
            if unsafe {
                SetWindowPos(
                    hwnd,
                    None,
                    x.round() as i32,
                    y.round() as i32,
                    window_width,
                    window_height,
                    SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_SHOWWINDOW,
                )
                .is_err()
            } {
                tracing::error!("调整普通弹窗尺寸失败");
            }
        })
        .detach();

    true
}

#[cfg(not(target_os = "windows"))]
fn set_borderless_normal_window_bounds(
    window: &mut Window,
    _cx: &App,
    _hwnd: isize,
    _x: f32,
    _y: f32,
    width: f32,
    height: f32,
    _border_offset: PopupBorderOffset,
) -> bool {
    let scale_factor = window.scale_factor().max(1.0);
    window.resize(size(px(width / scale_factor), px(height / scale_factor)));
    true
}
