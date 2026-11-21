use std::{
    path::Path,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use gpui::{App, AppContext as _, BorrowAppContext as _, Global, RenderImage};

use crate::{config::ApplicationFilterApplication, domain::capture::SourceApplication};

#[derive(Clone)]
pub(crate) struct ApplicationInfo {
    pub(crate) application: ApplicationFilterApplication,
    pub(crate) icon: Option<Arc<RenderImage>>,
}

impl ApplicationInfo {
    pub(crate) fn from_application(application: ApplicationFilterApplication) -> Self {
        Self {
            application,
            icon: None,
        }
    }
}

static FOREGROUND_SENDER: OnceLock<async_channel::Sender<isize>> = OnceLock::new();
/// 当前前台应用是否命中“指定应用”过滤。
///
/// CapsLock 双剪贴板工作线程不是 GPUI 线程，只能读取这个原子标记；
/// 命中期间忽略所有组合键命令，与“禁用内容监听/全局快捷键”的设置语义一致。
static FOREGROUND_FILTER_MATCHED: AtomicBool = AtomicBool::new(false);

pub(crate) fn foreground_filter_matched() -> bool {
    FOREGROUND_FILTER_MATCHED.load(Ordering::Relaxed)
}

pub(crate) struct ApplicationFilterService {
    #[cfg(target_os = "windows")]
    _hook: Option<isize>,
    clipboard_blocked: bool,
    clipboard_was_active: bool,
    hotkeys_blocked: bool,
    edge_hide_blocked: bool,
}

impl Global for ApplicationFilterService {}

#[cfg(target_os = "windows")]
impl Drop for ApplicationFilterService {
    fn drop(&mut self) {
        let Some(hook) = self._hook.take() else {
            return;
        };
        if hook == 0 {
            return;
        }
        use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, UnhookWinEvent};
        // SAFETY: hook 来自本服务成功安装的 WinEvent Hook 句柄，销毁时只解除该句柄。
        unsafe {
            let _ = UnhookWinEvent(HWINEVENTHOOK(hook as *mut _));
        }
    }
}

impl ApplicationFilterService {
    pub(crate) fn install(cx: &mut App) {
        let (sender, receiver) = async_channel::unbounded();
        let _ = FOREGROUND_SENDER.set(sender);
        #[cfg(target_os = "windows")]
        let hook = install_foreground_hook();
        #[cfg(target_os = "windows")]
        if hook.is_none() {
            tracing::error!("安装前台窗口变化监听失败，应用过滤无法实时更新");
        }

        cx.set_global(Self {
            #[cfg(target_os = "windows")]
            _hook: hook,
            clipboard_blocked: false,
            clipboard_was_active: false,
            hotkeys_blocked: false,
            edge_hide_blocked: false,
        });

        cx.spawn(async move |cx| {
            while let Ok(window) = receiver.recv().await {
                let _ = cx.update(|cx| Self::handle_foreground(window, cx));
            }
        })
        .detach();

        Self::refresh(cx);
    }

    pub(crate) fn refresh(cx: &mut App) {
        #[cfg(target_os = "windows")]
        let window = current_foreground_window();
        #[cfg(not(target_os = "windows"))]
        let window = None;
        let Some(window) = window else {
            Self::apply_foreground(None, cx);
            return;
        };
        Self::handle_foreground(window, cx);
    }

    fn handle_foreground(window: isize, cx: &mut App) {
        crate::services::focus::record_foreground_window(window);
        #[cfg(target_os = "windows")]
        let application = application_for_window(window);
        #[cfg(not(target_os = "windows"))]
        let application = None;
        Self::apply_foreground(application, cx);
    }

    fn apply_foreground(application: Option<ApplicationFilterApplication>, cx: &mut App) {
        let config = crate::config_runtime::application_filter_config(cx);
        let matched = config.enabled
            && application.as_ref().is_some_and(|application| {
                config
                    .applications
                    .iter()
                    .any(|filter| same_application(application, filter))
            });
        FOREGROUND_FILTER_MATCHED.store(matched, Ordering::Relaxed);
        let Some(_) = cx.try_global::<Self>() else {
            return;
        };
        let (clipboard_blocked, clipboard_was_active, hotkeys_blocked, edge_hide_blocked) = cx
            .read_global::<Self, _>(|service, _| {
                (
                    service.clipboard_blocked,
                    service.clipboard_was_active,
                    service.hotkeys_blocked,
                    service.edge_hide_blocked,
                )
            });
        let disable_clipboard = matched && config.disable_clipboard_monitor;
        if disable_clipboard && !clipboard_blocked {
            let was_active = crate::windows::main::MainWindowService::clipboard_listener_active(cx);
            if was_active {
                crate::windows::main::MainWindowService::stop_clipboard_listener(cx);
            }
            cx.update_global::<Self, _>(|service, _| {
                service.clipboard_blocked = true;
                service.clipboard_was_active = was_active;
            });
        } else if !disable_clipboard && clipboard_blocked {
            if clipboard_was_active {
                crate::windows::main::MainWindowService::restart_clipboard_listener(cx);
            }
            cx.update_global::<Self, _>(|service, _| {
                service.clipboard_blocked = false;
                service.clipboard_was_active = false;
            });
        }

        let disable_hotkeys = matched && config.disable_global_hotkeys;
        if disable_hotkeys && !hotkeys_blocked {
            if crate::hotkey::global_hotkeys_enabled(cx) {
                if let Err(error) = crate::hotkey::toggle_global_hotkeys(cx) {
                    tracing::error!("按应用过滤注销全局快捷键失败：{error}");
                } else {
                    cx.update_global::<Self, _>(|service, _| service.hotkeys_blocked = true);
                }
            }
        } else if !disable_hotkeys && hotkeys_blocked {
            if !crate::hotkey::global_hotkeys_enabled(cx) {
                if let Err(error) = crate::hotkey::toggle_global_hotkeys(cx) {
                    tracing::error!("按应用过滤恢复全局快捷键失败：{error}");
                }
            }
            cx.update_global::<Self, _>(|service, _| service.hotkeys_blocked = false);
        }

        let disable_edge_hide = matched && config.disable_edge_hide;
        if disable_edge_hide != edge_hide_blocked {
            cx.update_global::<Self, _>(|service, _| service.edge_hide_blocked = disable_edge_hide);
            crate::windows::main::MainWindowService::set_edge_hide_temporarily_blocked(
                disable_edge_hide,
                cx,
            );
        }
        crate::windows::main::MainWindowService::notify_main_view(cx);
    }
}

pub(crate) fn enumerate_running_applications() -> Vec<ApplicationInfo> {
    #[cfg(target_os = "windows")]
    {
        enumerate_windows_applications()
    }
    #[cfg(not(target_os = "windows"))]
    {
        Vec::new()
    }
}

pub(crate) fn resolve_manual_application(
    input: &str,
) -> Result<ApplicationFilterApplication, String> {
    let query = input.trim();
    if query.is_empty() {
        return Err("请输入应用进程名称或应用名称".to_owned());
    }
    #[cfg(target_os = "windows")]
    {
        // 显式进程名和绝对路径不依赖窗口或进程存活，名称规则也不绑定版本目录。
        if let Some(application) = manual_executable_application(query) {
            return Ok(application);
        }
        if let Some(application) = enumerate_windows_applications()
            .into_iter()
            .map(|application| application.application)
            .find(|application| {
                application.process_name.eq_ignore_ascii_case(query)
                    || application.display_name.eq_ignore_ascii_case(query)
                    || application.process_path.eq_ignore_ascii_case(query)
            })
        {
            return Ok(application);
        }
        if let Some(current) = current_foreground_window().and_then(application_for_window)
            && (current.process_name.eq_ignore_ascii_case(query)
                || current.display_name.eq_ignore_ascii_case(query)
                || current.process_path.eq_ignore_ascii_case(query))
        {
            return Ok(current);
        }
        Err("未找到匹配的运行中应用".to_owned())
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = query;
        Err("当前平台不支持读取前台应用".to_owned())
    }
}

#[cfg(target_os = "windows")]
fn manual_executable_application(input: &str) -> Option<ApplicationFilterApplication> {
    let input = input
        .strip_prefix('"')
        .and_then(|input| input.strip_suffix('"'))
        .unwrap_or(input);
    let path = Path::new(input);
    let process_name = path.file_name()?.to_str()?;
    if !path.extension()?.eq_ignore_ascii_case("exe")
        || process_name.len() <= 4
        || input.chars().any(|character| {
            character.is_control() || matches!(character, '<' | '>' | '"' | '|' | '?' | '*')
        })
        || (!path.is_absolute() && input != process_name)
        || process_name.contains(':')
    {
        return None;
    }
    Some(ApplicationFilterApplication {
        process_name: process_name.to_owned(),
        process_path: if path.is_absolute() {
            input.to_owned()
        } else {
            String::new()
        },
        display_name: path.file_stem()?.to_str()?.to_owned(),
    })
}

pub(crate) fn same_application(
    application: &ApplicationFilterApplication,
    filter: &ApplicationFilterApplication,
) -> bool {
    if !application.process_path.is_empty() && !filter.process_path.is_empty() {
        return application
            .process_path
            .eq_ignore_ascii_case(&filter.process_path);
    }
    application
        .process_name
        .eq_ignore_ascii_case(&filter.process_name)
        || application
            .display_name
            .eq_ignore_ascii_case(&filter.display_name)
}

/// 判断剪贴板来源是否需要按“禁用内容监听”规则跳过入库。
///
/// 剪贴板来源优先使用剪贴板所有者进程，因此即使来源程序在后台写入剪贴板，
/// 这里仍然可以根据来源进程路径或名称进行判断。
pub(crate) fn should_skip_clipboard_source(source: &SourceApplication, cx: &App) -> bool {
    let config = crate::config_runtime::application_filter_config(cx);
    config.enabled
        && config.disable_clipboard_monitor
        && config
            .applications
            .iter()
            .any(|filter| source_matches_application(source, filter))
}

fn source_matches_application(
    source: &SourceApplication,
    filter: &ApplicationFilterApplication,
) -> bool {
    if let Some(source_path) = source.path.as_deref().filter(|path| !path.is_empty()) {
        if !filter.process_path.is_empty() {
            return source_path.eq_ignore_ascii_case(&filter.process_path);
        }
        if Path::new(source_path)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case(&filter.process_name))
        {
            return true;
        }
    }

    let Some(source_name) = source.name.as_deref().filter(|name| !name.is_empty()) else {
        return false;
    };
    source_name.eq_ignore_ascii_case(&filter.display_name)
        || source_name.eq_ignore_ascii_case(&filter.process_name)
        || Path::new(&filter.process_name)
            .file_stem()
            .and_then(|name| name.to_str())
            .is_some_and(|process_name| source_name.eq_ignore_ascii_case(process_name))
}

#[cfg(target_os = "windows")]
fn install_foreground_hook() -> Option<isize> {
    use windows::Win32::UI::{
        Accessibility::SetWinEventHook,
        WindowsAndMessaging::{EVENT_SYSTEM_FOREGROUND, WINEVENT_OUTOFCONTEXT},
    };
    // SAFETY: 回调为静态函数，只把窗口句柄发送到应用线程，不访问 Rust UI 状态。
    let hook = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(foreground_event),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        )
    };
    (!hook.is_invalid()).then_some(hook.0 as isize)
}

#[cfg(target_os = "windows")]
unsafe extern "system" fn foreground_event(
    _hook: windows::Win32::UI::Accessibility::HWINEVENTHOOK,
    event: u32,
    window: windows::Win32::Foundation::HWND,
    _object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    use windows::Win32::UI::WindowsAndMessaging::EVENT_SYSTEM_FOREGROUND;
    if event == EVENT_SYSTEM_FOREGROUND
        && let Some(sender) = FOREGROUND_SENDER.get()
    {
        let _ = sender.try_send(window.0 as isize);
    }
}

#[cfg(target_os = "windows")]
fn current_foreground_window() -> Option<isize> {
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    // SAFETY: 只读取系统当前前台窗口句柄。
    let window = unsafe { GetForegroundWindow() };
    (!window.0.is_null()).then_some(window.0 as isize)
}

#[cfg(target_os = "windows")]
fn application_for_window(window: isize) -> Option<ApplicationFilterApplication> {
    use windows::Win32::Foundation::HWND;
    application_for_hwnd(HWND(window as *mut _))
}

#[cfg(target_os = "windows")]
fn enumerate_windows_applications() -> Vec<ApplicationInfo> {
    use windows::Win32::Foundation::LPARAM;
    use windows::Win32::UI::WindowsAndMessaging::EnumWindows;
    use windows::core::BOOL;

    let mut applications: Vec<ApplicationInfo> = Vec::new();
    let pointer = &mut applications as *mut Vec<ApplicationInfo>;
    unsafe extern "system" fn callback(
        window: windows::Win32::Foundation::HWND,
        parameter: windows::Win32::Foundation::LPARAM,
    ) -> BOOL {
        use windows::Win32::UI::WindowsAndMessaging::{GW_OWNER, GetWindow, IsWindowVisible};
        if !unsafe { IsWindowVisible(window).as_bool() }
            || unsafe { GetWindow(window, GW_OWNER) }.is_ok_and(|owner| !owner.is_invalid())
        {
            return BOOL(1);
        }
        let applications = unsafe { &mut *(parameter.0 as *mut Vec<ApplicationInfo>) };
        let Some(application) = application_for_hwnd(window) else {
            return BOOL(1);
        };
        if applications
            .iter()
            .any(|item| same_application(&item.application, &application))
        {
            return BOOL(1);
        }
        let icon = if application.process_path.is_empty() {
            None
        } else {
            load_application_icon(&application.process_path)
        };
        applications.push(ApplicationInfo { application, icon });
        BOOL(1)
    }
    // SAFETY: EnumWindows 仅在调用期间使用 applications 指针，回调不会逃逸。
    let result = unsafe { EnumWindows(Some(callback), LPARAM(pointer as isize)) };
    if let Err(error) = result {
        tracing::error!("枚举运行中应用失败：{error}");
    }
    applications
        .sort_by_key(|application| application.application.display_name.to_ascii_lowercase());
    applications
}

#[cfg(target_os = "windows")]
fn application_for_hwnd(
    window: windows::Win32::Foundation::HWND,
) -> Option<ApplicationFilterApplication> {
    let process_path = process_path_from_window(window).unwrap_or_default();
    let process_name = Path::new(&process_path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_owned();
    let display_name = Path::new(&process_path)
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or(&process_name)
        .to_owned();
    (!process_name.is_empty()).then_some(ApplicationFilterApplication {
        process_name,
        process_path,
        display_name,
    })
}

#[cfg(target_os = "windows")]
fn process_path_from_window(window: windows::Win32::Foundation::HWND) -> Option<String> {
    use windows::{
        Win32::{
            Foundation::{CloseHandle, HANDLE},
            System::Threading::{
                OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
                QueryFullProcessImageNameW,
            },
            UI::WindowsAndMessaging::GetWindowThreadProcessId,
        },
        core::PWSTR,
    };
    let mut process_id = 0_u32;
    // SAFETY: 只读取窗口所属进程 ID。
    if unsafe { GetWindowThreadProcessId(window, Some(&mut process_id)) } == 0 {
        return None;
    }
    let process =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id) }.ok()?;
    let mut buffer = vec![0_u16; 32_768];
    let mut length = buffer.len() as u32;
    // SAFETY: buffer 为可写 UTF-16 缓冲区，长度由 API 限制。
    let result = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
    };
    // SAFETY: process 句柄来自 OpenProcess，关闭后不再使用。
    let _ = unsafe { CloseHandle(HANDLE(process.0)) };
    result.ok()?;
    Some(String::from_utf16_lossy(&buffer[..length as usize]))
}

#[cfg(target_os = "windows")]
fn load_application_icon(path: &str) -> Option<Arc<RenderImage>> {
    let icon = file_icon_provider::get_file_icon(path, 32).ok()?;
    let pixels = usize::try_from(icon.width)
        .ok()?
        .checked_mul(usize::try_from(icon.height).ok()?)?
        .checked_mul(4)?;
    if icon.pixels.len() != pixels {
        return None;
    }
    let mut pixels = icon.pixels;
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let image = image::RgbaImage::from_raw(icon.width, icon.height, pixels)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(image)])))
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;

    #[test]
    fn manual_process_name_does_not_require_a_running_application() {
        let application = resolve_manual_application("  qcclip-non-running-filter-test.exe  ")
            .expect("未运行的进程名应可添加");
        assert_eq!(
            application.process_name,
            "qcclip-non-running-filter-test.exe"
        );
        assert_eq!(application.display_name, "qcclip-non-running-filter-test");
        assert!(application.process_path.is_empty());
    }

    #[test]
    fn manual_webview_rule_matches_different_runtime_versions() {
        let filter = resolve_manual_application("msedgewebview2.exe").unwrap();
        for version in ["155.0.4283.60", "155.0.4283.62"] {
            let path = format!(
                r"C:\Program Files (x86)\Microsoft\EdgeWebView\Application\{version}\msedgewebview2.exe"
            );
            let application = ApplicationFilterApplication {
                process_name: "msedgewebview2.exe".to_owned(),
                process_path: path.clone(),
                display_name: "msedgewebview2".to_owned(),
            };
            assert!(same_application(&application, &filter));
            assert!(source_matches_application(
                &SourceApplication {
                    name: Some("msedgewebview2".to_owned()),
                    path: Some(path.clone()),
                },
                &filter,
            ));
            assert!(source_matches_application(
                &SourceApplication {
                    name: None,
                    path: Some(path),
                },
                &filter,
            ));
        }
    }

    #[test]
    fn manual_process_name_matches_without_source_path_case_insensitively() {
        let filter = resolve_manual_application("MSEDGEWEBVIEW2.EXE").unwrap();
        for name in ["msedgewebview2", "msedgewebview2.exe"] {
            assert!(source_matches_application(
                &SourceApplication {
                    name: Some(name.to_owned()),
                    path: None,
                },
                &filter,
            ));
        }
    }

    #[test]
    fn manual_absolute_path_keeps_path_specific_matching() {
        let path = r"C:\QcClip Test\msedgewebview2.exe";
        let filter = resolve_manual_application(&format!("\"{path}\"")).unwrap();
        assert_eq!(filter.process_path, path);
        assert_eq!(filter.process_name, "msedgewebview2.exe");
        for (source_path, expected) in [
            (r"c:\qcclip test\MSEDGEWEBVIEW2.EXE", true),
            (r"C:\Other\msedgewebview2.exe", false),
        ] {
            let application = ApplicationFilterApplication {
                process_path: source_path.to_owned(),
                ..filter.clone()
            };
            assert_eq!(same_application(&application, &filter), expected);
            assert_eq!(
                source_matches_application(
                    &SourceApplication {
                        name: Some("msedgewebview2".to_owned()),
                        path: Some(source_path.to_owned()),
                    },
                    &filter,
                ),
                expected,
            );
        }
    }

    #[test]
    fn manual_executable_rejects_invalid_or_ambiguous_inputs() {
        for input in [
            "",
            ".exe",
            "msedgewebview2",
            "*.exe",
            "bad?.exe",
            "bad\nname.exe",
            "name.exe:other.exe",
            r"relative\msedgewebview2.exe",
            r"C:msedgewebview2.exe",
            r"\msedgewebview2.exe",
            "\"msedgewebview2.exe",
        ] {
            assert!(
                manual_executable_application(input).is_none(),
                "不应接受无效或不明确的输入：{input:?}",
            );
        }
    }

    #[test]
    fn manual_process_rule_does_not_match_other_or_missing_sources() {
        let filter = resolve_manual_application("msedgewebview2.exe").unwrap();
        for source in [
            SourceApplication::default(),
            SourceApplication {
                name: Some("msedge".to_owned()),
                path: Some(r"C:\Browser\msedge.exe".to_owned()),
            },
        ] {
            assert!(!source_matches_application(&source, &filter));
        }
        assert!(resolve_manual_application("   ").is_err());
    }
}
