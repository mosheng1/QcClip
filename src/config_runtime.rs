use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use atomic_write_file::AtomicWriteFile;
use gpui::{App, AppContext as _, BorrowAppContext as _, Global};
use thiserror::Error;
use tracing::warn;

use crate::{
    config::{
        AnimationKind, AppConfig, AppearanceConfig, ApplicationFilterConfig, ClipboardConfig,
        HistoryConfig, InterfaceLanguage, NotificationConfig, NotificationKind,
        NotificationPosition, QuickPasteConfig, ShortcutBinding, ShortcutsConfig, SoundConfig,
        WindowMaterial, default_window_material_opacity,
    },
    i18n::{self, Locale},
};

#[derive(Debug, Error)]
pub(crate) enum ConfigError {
    #[error("无法创建配置目录：{source}")]
    CreateDirectory { source: std::io::Error },
    #[error("无法打开配置文件：{source}")]
    Open { source: std::io::Error },
    #[error("无法写入配置文件：{source}")]
    Write { source: std::io::Error },
    #[error("无法提交配置文件：{source}")]
    Commit { source: std::io::Error },
    #[error("无法更新系统启动设置：{source}")]
    System { source: std::io::Error },
    #[error("保存配置失败：{config}；恢复系统启动设置失败：{restore}")]
    AutoStartRollback { config: String, restore: String },
    #[error("无法序列化配置：{source}")]
    Serialize { source: serde_json::Error },
    #[error("现有配置无法解析，为避免丢失数据已停止保存：{source}")]
    InvalidExistingConfig {
        source: crate::config::load::ConfigLoadError,
    },
    #[error("数据正在导入，请稍后重试")]
    DataSwitching,
    #[error("代理地址必须是有效的 HTTP 代理地址，如 http://127.0.0.1:7890")]
    InvalidUpdateProxy,
}

pub(crate) struct ConfigRuntime {
    config: AppConfig,
    path: PathBuf,
}

pub(crate) struct ConfigReload {
    previous: AppConfig,
    current: AppConfig,
}

impl Global for ConfigRuntime {}

impl ConfigRuntime {
    pub(crate) fn install(path: impl Into<PathBuf>, cx: &mut App) {
        let path = path.into();
        let config = load_config(&path);
        // 存储层在应用启动前就已实例化，这里把配置里的图片保存方式同步给存储层
        crate::services::storage::set_save_images_as_files(config.clipboard.save_images_as_files);
        cx.set_global(Self { config, path });
    }

    pub(crate) fn reload(path: impl Into<PathBuf>, cx: &mut App) -> ConfigReload {
        let path = path.into();
        let config = load_config(&path);
        crate::services::storage::set_save_images_as_files(config.clipboard.save_images_as_files);
        let previous = cx.update_global::<Self, _>(|runtime, _| {
            let previous = runtime.config.clone();
            runtime.config = config.clone();
            runtime.path = path.clone();
            previous
        });

        ConfigReload {
            previous,
            current: config,
        }
    }

    pub(crate) fn apply_imported_config(reload: ConfigReload, cx: &mut App) {
        let ConfigReload {
            previous,
            current: config,
        } = reload;
        if previous.general.language != config.general.language {
            i18n::set_locale(Locale::new(config.general.language.code()), cx);
        }
        if previous.sound != config.sound {
            crate::services::sound::set_config(config.sound.clone());
        }
        if previous.appearance != config.appearance {
            apply_appearance_change(&previous.appearance, &config.appearance, cx);
        }
        if previous.clipboard.edge_hide != config.clipboard.edge_hide {
            crate::windows::main::MainWindowService::set_edge_hide_enabled(
                config.clipboard.edge_hide,
                cx,
            );
        }
        if previous.clipboard.edge_hide_show_edge != config.clipboard.edge_hide_show_edge {
            crate::windows::main::MainWindowService::refresh_edge_hide_position(cx);
        }
        if previous.clipboard.hover_preview_enabled && !config.clipboard.hover_preview_enabled {
            crate::windows::main::MainWindowService::cancel_hover_preview(cx);
        }
        if previous.application_filter != config.application_filter {
            crate::services::application_filter::ApplicationFilterService::refresh(cx);
        }
        if previous.general.global_notification_enabled
            && !config.general.global_notification_enabled
        {
            crate::windows::notification::NotificationService::dismiss_all(cx);
        } else {
            crate::windows::notification::NotificationService::dismiss_disabled(cx);
        }
        if previous.general.launch_at_login != config.general.launch_at_login {
            if let Err(error) =
                crate::platform::auto_start::set_enabled(config.general.launch_at_login)
            {
                tracing::error!("同步开机自启设置失败：{error}");
            }
        }
        if previous.shortcuts != config.shortcuts {
            crate::windows::quick_paste::QuickPasteWindowService::cancel(cx);
            if let Err(error) = crate::hotkey::cancel_toggle_main_window_recording(cx) {
                tracing::error!("重新加载快捷键配置失败：{error}");
            }
        }
        cx.refresh_windows();
    }
}

pub(crate) fn interface_language(cx: &App) -> InterfaceLanguage {
    cx.read_global::<ConfigRuntime, _>(|runtime, _| runtime.config.general.language)
}

pub(crate) fn update_config(cx: &App) -> crate::config::UpdateConfig {
    cx.read_global::<ConfigRuntime, _>(|runtime, _| runtime.config.updates.clone())
}

pub(crate) fn set_update_config(
    mut updates: crate::config::UpdateConfig,
    cx: &mut App,
) -> Result<bool, ConfigError> {
    updates.proxy_url = updates.proxy_url.trim().to_owned();
    if updates.proxy_mode == crate::config::UpdateProxyMode::Custom
        && !crate::services::updates::valid_proxy_url(&updates.proxy_url)
    {
        return Err(ConfigError::InvalidUpdateProxy);
    }
    let (current, path, mut next) = cx.read_global::<ConfigRuntime, _>(|runtime, _| {
        (
            runtime.config.updates.clone(),
            runtime.path.clone(),
            runtime.config.clone(),
        )
    });
    if current == updates {
        return Ok(false);
    }
    next.updates = updates;
    persist_config(&path, &next)?;
    cx.update_global::<ConfigRuntime, _>(|runtime, _| runtime.config = next);
    cx.refresh_windows();
    Ok(true)
}

pub(crate) fn global_notification_enabled(cx: &App) -> bool {
    cx.read_global::<ConfigRuntime, _>(|runtime, _| {
        runtime.config.general.global_notification_enabled
    })
}

pub(crate) fn notification_enabled(kind: NotificationKind, cx: &App) -> bool {
    cx.read_global::<ConfigRuntime, _>(|runtime, _| runtime.config.allows_notification(kind))
}

pub(crate) fn notification_preferences(cx: &App) -> [(NotificationKind, bool); 6] {
    cx.read_global::<ConfigRuntime, _>(|runtime, _| {
        NotificationKind::ALL.map(|kind| (kind, runtime.config.notification_preference(kind)))
    })
}

pub(crate) fn notification_config(cx: &App) -> NotificationConfig {
    cx.read_global::<ConfigRuntime, _>(|runtime, _| runtime.config.general.notifications.clone())
}

pub(crate) fn set_notification_presentation(
    position: NotificationPosition,
    duration_ms: u32,
    cx: &mut App,
) -> Result<bool, ConfigError> {
    let (path, mut next) = cx.read_global::<ConfigRuntime, _>(|runtime, _| {
        (runtime.path.clone(), runtime.config.clone())
    });
    let previous = next.general.notifications.clone();
    let notifications = &mut next.general.notifications;
    notifications.position = position;
    notifications.duration_ms = duration_ms;
    *notifications = notifications.clone().normalized();
    if *notifications == previous {
        return Ok(false);
    }
    persist_config(&path, &next)?;
    cx.update_global::<ConfigRuntime, _>(|runtime, _| runtime.config = next);
    cx.refresh_windows();
    Ok(true)
}

pub(crate) fn set_notification_preference(
    kind: NotificationKind,
    enabled: bool,
    cx: &mut App,
) -> Result<bool, ConfigError> {
    let (path, mut next) = cx.read_global::<ConfigRuntime, _>(|runtime, _| {
        (runtime.path.clone(), runtime.config.clone())
    });
    if next.notification_preference(kind) == enabled {
        return Ok(false);
    }
    next.set_notification_preference(kind, enabled);
    persist_config(&path, &next)?;
    cx.update_global::<ConfigRuntime, _>(|runtime, _| runtime.config = next);
    if !enabled {
        crate::windows::notification::NotificationService::dismiss_disabled(cx);
    }
    cx.refresh_windows();
    Ok(true)
}

pub(crate) fn appearance_config(cx: &App) -> AppearanceConfig {
    cx.read_global::<ConfigRuntime, _>(|runtime, _| runtime.config.appearance.clone())
}

pub(crate) fn gallery_gif_autoplay(cx: &App) -> bool {
    cx.read_global::<ConfigRuntime, _>(|runtime, _| runtime.config.appearance.gallery_gif_autoplay)
}

pub(crate) fn launch_at_login(cx: &App) -> bool {
    cx.read_global::<ConfigRuntime, _>(|runtime, _| runtime.config.general.launch_at_login)
}

pub(crate) fn run_as_administrator(cx: &App) -> bool {
    cx.read_global::<ConfigRuntime, _>(|runtime, _| runtime.config.general.run_as_administrator)
}

pub(crate) fn application_filter_config(cx: &App) -> ApplicationFilterConfig {
    cx.read_global::<ConfigRuntime, _>(|runtime, _| runtime.config.application_filter.clone())
}

pub(crate) fn set_application_filter_config(
    application_filter: ApplicationFilterConfig,
    cx: &mut App,
) -> Result<bool, ConfigError> {
    let (current, path, mut next) = cx.read_global::<ConfigRuntime, _>(|runtime, _| {
        (
            runtime.config.application_filter.clone(),
            runtime.path.clone(),
            runtime.config.clone(),
        )
    });
    if current == application_filter {
        return Ok(false);
    }

    next.application_filter = application_filter;
    persist_config(&path, &next)?;
    cx.update_global::<ConfigRuntime, _>(|runtime, _| runtime.config = next);
    crate::services::application_filter::ApplicationFilterService::refresh(cx);
    cx.refresh_windows();
    Ok(true)
}

/// 返回指定动效是否启用。所有界面动画都通过此入口读取统一配置。
pub(crate) fn animation_enabled(cx: &App, kind: AnimationKind) -> bool {
    cx.read_global::<ConfigRuntime, _>(|runtime, _| {
        runtime.config.appearance.animations.enabled(kind)
    })
}

pub(crate) fn clipboard_config(cx: &App) -> ClipboardConfig {
    cx.read_global::<ConfigRuntime, _>(|runtime, _| runtime.config.clipboard.clone())
}

pub(crate) fn hover_preview_enabled(cx: &App) -> bool {
    cx.read_global::<ConfigRuntime, _>(|runtime, _| runtime.config.clipboard.hover_preview_enabled)
}

pub(crate) fn sound_config(cx: &App) -> SoundConfig {
    cx.read_global::<ConfigRuntime, _>(|runtime, _| runtime.config.sound.clone())
}

pub(crate) fn toggle_main_window_shortcut(cx: &App) -> Option<ShortcutBinding> {
    shortcuts_config(cx).toggle_main_window
}

pub(crate) fn history_config(cx: &App) -> HistoryConfig {
    cx.read_global::<ConfigRuntime, _>(|runtime, _| runtime.config.history.clone())
}

pub(crate) fn quick_paste_config(cx: &App) -> QuickPasteConfig {
    cx.read_global::<ConfigRuntime, _>(|runtime, _| runtime.config.quick_paste)
}

pub(crate) fn set_quick_paste_config(
    quick_paste: QuickPasteConfig,
    cx: &mut App,
) -> Result<bool, ConfigError> {
    let quick_paste = quick_paste.normalized();
    let (current, path, mut next) = cx.read_global::<ConfigRuntime, _>(|runtime, _| {
        (
            runtime.config.quick_paste,
            runtime.path.clone(),
            runtime.config.clone(),
        )
    });
    if current == quick_paste {
        return Ok(false);
    }

    next.quick_paste = quick_paste;
    persist_config(&path, &next)?;
    cx.update_global::<ConfigRuntime, _>(|runtime, _| runtime.config = next);
    cx.refresh_windows();
    Ok(true)
}

pub(crate) fn reset_quick_paste_config(cx: &mut App) -> Result<bool, ConfigError> {
    set_quick_paste_config(QuickPasteConfig::default(), cx)
}

pub(crate) fn set_history_config(
    history: HistoryConfig,
    cx: &mut App,
) -> Result<bool, ConfigError> {
    let (current, path, mut next) = cx.read_global::<ConfigRuntime, _>(|runtime, _| {
        (
            runtime.config.history.clone(),
            runtime.path.clone(),
            runtime.config.clone(),
        )
    });
    if current == history {
        return Ok(false);
    }
    next.history = history;
    persist_config(&path, &next)?;
    cx.update_global::<ConfigRuntime, _>(|runtime, _| runtime.config = next);
    crate::windows::main::MainWindowService::refresh_history(cx);
    Ok(true)
}

pub(crate) fn reset_history_config(cx: &mut App) -> Result<bool, ConfigError> {
    set_history_config(HistoryConfig::default(), cx)
}

pub(crate) fn shortcuts_config(cx: &App) -> ShortcutsConfig {
    cx.read_global::<ConfigRuntime, _>(|runtime, _| runtime.config.shortcuts.clone())
}

pub(crate) fn set_toggle_main_window_shortcut(
    binding: Option<ShortcutBinding>,
    cx: &mut App,
) -> Result<bool, ConfigError> {
    let mut shortcuts = shortcuts_config(cx);
    shortcuts.toggle_main_window = binding;
    set_shortcuts_config(shortcuts, cx)
}

pub(crate) fn set_shortcuts_config(
    shortcuts: ShortcutsConfig,
    cx: &mut App,
) -> Result<bool, ConfigError> {
    let (current, path, mut next) = cx.read_global::<ConfigRuntime, _>(|runtime, _| {
        (
            runtime.config.shortcuts.clone(),
            runtime.path.clone(),
            runtime.config.clone(),
        )
    });
    if current == shortcuts {
        return Ok(false);
    }

    next.shortcuts = shortcuts;
    persist_config(&path, &next)?;
    cx.update_global::<ConfigRuntime, _>(|runtime, _| runtime.config = next);
    Ok(true)
}

pub(crate) fn set_interface_language(
    language: InterfaceLanguage,
    cx: &mut App,
) -> Result<bool, ConfigError> {
    let (current, path, mut next) = cx.read_global::<ConfigRuntime, _>(|runtime, _| {
        (
            runtime.config.general.language,
            runtime.path.clone(),
            runtime.config.clone(),
        )
    });
    if current == language {
        return Ok(false);
    }

    next.general.language = language;
    persist_config(&path, &next)?;

    cx.update_global::<ConfigRuntime, _>(|runtime, _| {
        runtime.config = next;
    });
    i18n::set_locale(Locale::new(language.code()), cx);
    Ok(true)
}

pub(crate) fn set_global_notification_enabled(
    enabled: bool,
    cx: &mut App,
) -> Result<bool, ConfigError> {
    let (current, path, mut next) = cx.read_global::<ConfigRuntime, _>(|runtime, _| {
        (
            runtime.config.general.global_notification_enabled,
            runtime.path.clone(),
            runtime.config.clone(),
        )
    });
    if current == enabled {
        return Ok(false);
    }

    next.general.global_notification_enabled = enabled;
    persist_config(&path, &next)?;
    cx.update_global::<ConfigRuntime, _>(|runtime, _| runtime.config = next);
    if !enabled {
        crate::windows::notification::NotificationService::dismiss_all(cx);
    }
    cx.refresh_windows();
    Ok(true)
}

pub(crate) fn set_launch_at_login(enabled: bool, cx: &mut App) -> Result<bool, ConfigError> {
    update_with_auto_start(cx, enabled, |config| {
        config.general.launch_at_login = enabled;
    })
}

pub(crate) fn set_run_as_administrator(enabled: bool, cx: &mut App) -> Result<bool, ConfigError> {
    let (current, path, mut next) = cx.read_global::<ConfigRuntime, _>(|runtime, _| {
        (
            runtime.config.general.run_as_administrator,
            runtime.path.clone(),
            runtime.config.clone(),
        )
    });
    if current == enabled {
        return Ok(false);
    }

    next.general.run_as_administrator = enabled;
    persist_config(&path, &next)?;
    cx.update_global::<ConfigRuntime, _>(|runtime, _| runtime.config = next);
    Ok(true)
}

pub(crate) fn reset_general_config(cx: &mut App) -> Result<bool, ConfigError> {
    let (path, current, mut next) = cx.read_global::<ConfigRuntime, _>(|runtime, _| {
        (
            runtime.path.clone(),
            runtime.config.general.clone(),
            runtime.config.clone(),
        )
    });
    let default = AppConfig::default().general;
    let default_update_notification = AppConfig::default().updates.notification_enabled;
    if current == default && next.updates.notification_enabled == default_update_notification {
        return Ok(false);
    }

    if current.launch_at_login != default.launch_at_login {
        crate::platform::auto_start::set_enabled(default.launch_at_login)
            .map_err(|source| ConfigError::System { source })?;
    }
    let language = default.language;
    next.general = default;
    next.updates.notification_enabled = default_update_notification;
    if let Err(error) = persist_config(&path, &next) {
        if current.launch_at_login != next.general.launch_at_login {
            if let Err(restore_error) =
                crate::platform::auto_start::set_enabled(current.launch_at_login)
            {
                return Err(ConfigError::AutoStartRollback {
                    config: error.to_string(),
                    restore: restore_error.to_string(),
                });
            }
        }
        return Err(error);
    }

    cx.update_global::<ConfigRuntime, _>(|runtime, _| {
        runtime.config = next;
    });
    i18n::set_locale(Locale::new(language.code()), cx);
    crate::windows::notification::NotificationService::dismiss_disabled(cx);
    cx.refresh_windows();
    Ok(true)
}

fn update_with_auto_start(
    cx: &mut App,
    enabled: bool,
    update: impl FnOnce(&mut AppConfig),
) -> Result<bool, ConfigError> {
    let previous_enabled = launch_at_login(cx);
    if previous_enabled != enabled {
        crate::platform::auto_start::set_enabled(enabled)
            .map_err(|source| ConfigError::System { source })?;
    }

    let (current, path, mut next) = cx.read_global::<ConfigRuntime, _>(|runtime, _| {
        (
            runtime.config.general.launch_at_login,
            runtime.path.clone(),
            runtime.config.clone(),
        )
    });
    update(&mut next);
    if current == next.general.launch_at_login {
        return Ok(false);
    }
    if let Err(error) = persist_config(&path, &next) {
        if previous_enabled != enabled {
            if let Err(restore_error) = crate::platform::auto_start::set_enabled(previous_enabled) {
                return Err(ConfigError::AutoStartRollback {
                    config: error.to_string(),
                    restore: restore_error.to_string(),
                });
            }
        }
        return Err(error);
    }
    cx.update_global::<ConfigRuntime, _>(|runtime, _| runtime.config = next);
    Ok(true)
}

pub(crate) fn set_appearance_config(
    appearance: AppearanceConfig,
    cx: &mut App,
) -> Result<bool, ConfigError> {
    let (current, path, mut next) = cx.read_global::<ConfigRuntime, _>(|runtime, _| {
        (
            runtime.config.appearance.clone(),
            runtime.path.clone(),
            runtime.config.clone(),
        )
    });
    let material_changed = current.window_material != appearance.window_material;
    let mut appearance = appearance.normalized();
    if material_changed
        && current.window_material_opacity
            == default_window_material_opacity(current.window_material)
    {
        appearance.window_material_opacity =
            default_window_material_opacity(appearance.window_material);
    }
    if current == appearance {
        return Ok(false);
    }

    next.appearance = appearance.clone();
    persist_config(&path, &next)?;

    cx.update_global::<ConfigRuntime, _>(|runtime, _| {
        runtime.config = next;
    });
    apply_appearance_change(&current, &appearance, cx);
    Ok(true)
}

fn apply_appearance_change(previous: &AppearanceConfig, current: &AppearanceConfig, cx: &mut App) {
    if appearance_requires_global_refresh(previous, current) {
        crate::ui::theme::apply_appearance_from_config(cx);
        if background_image_needs_refresh(previous, current) {
            crate::window_material::refresh_background_image(cx);
        }
    } else {
        // 调用方会更新设置窗口，这里只额外让主窗口视图失效。
        crate::windows::main::MainWindowService::notify_main_view(cx);
    }
}

fn appearance_requires_global_refresh(
    previous: &AppearanceConfig,
    current: &AppearanceConfig,
) -> bool {
    previous.ui_scale_percent != current.ui_scale_percent
        || previous.app_font_family != current.app_font_family
        || previous.mode != current.mode
        || previous.light_theme != current.light_theme
        || previous.dark_theme != current.dark_theme
        || previous.window_material != current.window_material
        || previous.window_material_opacity != current.window_material_opacity
        || previous.background_image_path != current.background_image_path
        || previous.background_image_blur != current.background_image_blur
        || previous.window_material_brightness != current.window_material_brightness
}

fn background_image_needs_refresh(current: &AppearanceConfig, next: &AppearanceConfig) -> bool {
    current.background_image_path != next.background_image_path
        || (current.window_material != WindowMaterial::CustomImage
            && next.window_material == WindowMaterial::CustomImage)
}

pub(crate) fn reset_appearance_config(cx: &mut App) -> Result<bool, ConfigError> {
    let (current, path, mut next) = cx.read_global::<ConfigRuntime, _>(|runtime, _| {
        (
            runtime.config.appearance.clone(),
            runtime.path.clone(),
            runtime.config.clone(),
        )
    });
    let default = AppearanceConfig::default();
    if current == default {
        return Ok(false);
    }

    next.appearance = default.clone();
    persist_config(&path, &next)?;

    cx.update_global::<ConfigRuntime, _>(|runtime, _| {
        runtime.config = next;
    });
    apply_appearance_change(&current, &default, cx);
    Ok(true)
}

pub(crate) fn set_clipboard_config(
    clipboard: ClipboardConfig,
    cx: &mut App,
) -> Result<bool, ConfigError> {
    let clipboard = clipboard.normalized();
    let (current, path, mut next) = cx.read_global::<ConfigRuntime, _>(|runtime, _| {
        (
            runtime.config.clipboard.clone(),
            runtime.path.clone(),
            runtime.config.clone(),
        )
    });
    if current == clipboard {
        return Ok(false);
    }

    let edge_hide = clipboard.edge_hide;
    let edge_hide_show_edge = clipboard.edge_hide_show_edge;
    let hover_preview_enabled = clipboard.hover_preview_enabled;
    let save_images_as_files = clipboard.save_images_as_files;
    next.clipboard = clipboard;
    persist_config(&path, &next)?;

    cx.update_global::<ConfigRuntime, _>(|runtime, _| {
        runtime.config = next;
    });
    // 只影响后续捕获，已有文件保持原存储方式，不做批量重写
    crate::services::storage::set_save_images_as_files(save_images_as_files);
    if current.edge_hide != edge_hide {
        crate::windows::main::MainWindowService::set_edge_hide_enabled(edge_hide, cx);
    }
    if current.edge_hide_show_edge != edge_hide_show_edge {
        crate::windows::main::MainWindowService::refresh_edge_hide_position(cx);
    }
    if current.hover_preview_enabled && !hover_preview_enabled {
        crate::windows::main::MainWindowService::cancel_hover_preview(cx);
    }
    cx.refresh_windows();
    Ok(true)
}

pub(crate) fn reset_clipboard_config(cx: &mut App) -> Result<bool, ConfigError> {
    let (current, path, mut next) = cx.read_global::<ConfigRuntime, _>(|runtime, _| {
        (
            runtime.config.clipboard.clone(),
            runtime.path.clone(),
            runtime.config.clone(),
        )
    });
    let default = AppConfig::default().clipboard;
    if current == default {
        return Ok(false);
    }

    let save_images_as_files = default.save_images_as_files;
    next.clipboard = default;
    persist_config(&path, &next)?;

    cx.update_global::<ConfigRuntime, _>(|runtime, _| {
        runtime.config = next;
    });
    crate::services::storage::set_save_images_as_files(save_images_as_files);
    if current.edge_hide {
        crate::windows::main::MainWindowService::set_edge_hide_enabled(false, cx);
    }
    cx.refresh_windows();
    Ok(true)
}

pub(crate) fn set_sound_config(sound: SoundConfig, cx: &mut App) -> Result<bool, ConfigError> {
    let sound = sound.normalized();
    let (current, path, mut next) = cx.read_global::<ConfigRuntime, _>(|runtime, _| {
        (
            runtime.config.sound.clone(),
            runtime.path.clone(),
            runtime.config.clone(),
        )
    });
    if current == sound {
        return Ok(false);
    }

    next.sound = sound.clone();
    persist_config(&path, &next)?;

    cx.update_global::<ConfigRuntime, _>(|runtime, _| {
        runtime.config = next;
    });
    crate::services::sound::set_config(sound);
    cx.refresh_windows();
    Ok(true)
}

pub(crate) fn reset_sound_config(cx: &mut App) -> Result<bool, ConfigError> {
    let (current, path, mut next) = cx.read_global::<ConfigRuntime, _>(|runtime, _| {
        (
            runtime.config.sound.clone(),
            runtime.path.clone(),
            runtime.config.clone(),
        )
    });
    let default = SoundConfig::default();
    if current == default {
        return Ok(false);
    }

    next.sound = default.clone();
    persist_config(&path, &next)?;

    cx.update_global::<ConfigRuntime, _>(|runtime, _| {
        runtime.config = next;
    });
    crate::services::sound::set_config(default);
    cx.refresh_windows();
    Ok(true)
}

pub(crate) fn reset_shortcuts_config(cx: &mut App) -> Result<bool, ConfigError> {
    let (current, path, mut next) = cx.read_global::<ConfigRuntime, _>(|runtime, _| {
        (
            runtime.config.shortcuts.clone(),
            runtime.path.clone(),
            runtime.config.clone(),
        )
    });
    let default = ShortcutsConfig::default();
    if current == default {
        return Ok(false);
    }

    next.shortcuts = default;
    persist_config(&path, &next)?;
    cx.update_global::<ConfigRuntime, _>(|runtime, _| runtime.config = next);
    Ok(true)
}

fn load_config(path: &Path) -> AppConfig {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return AppConfig::default();
        }
        Err(error) => {
            warn!("读取配置失败，已使用默认配置：{error}");
            return AppConfig::default();
        }
    };

    match crate::config::load::from_str(&content) {
        Ok(loaded) => {
            for warning in loaded.warnings {
                warn!("配置加载警告：{warning}");
            }
            loaded.config
        }
        Err(error) => {
            warn!("配置格式无效，已使用默认配置：{error}");
            AppConfig::default()
        }
    }
}

fn persist_config(path: &Path, config: &AppConfig) -> Result<(), ConfigError> {
    let _operation =
        crate::services::data_runtime::begin_operation().map_err(|_| ConfigError::DataSwitching)?;
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|source| ConfigError::CreateDirectory { source })?;
    }

    let next = serde_json::to_value(config).map_err(|source| ConfigError::Serialize { source })?;
    let mut persisted = match fs::read_to_string(path) {
        Ok(content) => {
            let loaded = crate::config::load::from_str(&content)
                .map_err(|source| ConfigError::InvalidExistingConfig { source })?;
            let previous = serde_json::to_value(loaded.config)
                .map_err(|source| ConfigError::Serialize { source })?;
            let mut original: serde_json::Value = serde_json::from_str(&content)
                .map_err(|source| ConfigError::Serialize { source })?;
            merge_config_fields(&mut original, &previous, &next);
            original
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => next,
        Err(source) => return Err(ConfigError::Open { source }),
    };
    let version = persisted
        .get("schemaVersion")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
        .max(u64::from(crate::config::CURRENT_SCHEMA_VERSION));
    persisted["schemaVersion"] = version.into();
    let mut content = serde_json::to_string_pretty(&persisted)
        .map_err(|source| ConfigError::Serialize { source })?;
    content.push('\n');

    let mut file = AtomicWriteFile::open(path).map_err(|source| ConfigError::Open { source })?;
    file.write_all(content.as_bytes())
        .map_err(|source| ConfigError::Write { source })?;
    file.commit()
        .map_err(|source| ConfigError::Commit { source })?;
    Ok(())
}

// 只覆盖实际改动的已知字段，保留未知字段和未改动字段的原始值。
fn merge_config_fields(
    persisted: &mut serde_json::Value,
    previous: &serde_json::Value,
    next: &serde_json::Value,
) {
    if previous == next {
        return;
    }
    match (persisted, previous, next) {
        (
            serde_json::Value::Object(original),
            serde_json::Value::Object(previous),
            serde_json::Value::Object(next),
        ) => {
            for (key, value) in next {
                match (original.get_mut(key), previous.get(key)) {
                    (Some(original), Some(previous)) => {
                        merge_config_fields(original, previous, value)
                    }
                    _ => {
                        original.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (original, _, next) => *original = next.clone(),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{
        appearance_requires_global_refresh, background_image_needs_refresh, load_config,
        persist_config,
    };
    use crate::config::{AppConfig, InterfaceLanguage, WindowMaterial};

    #[test]
    fn persisted_config_can_be_loaded_again() {
        let directory = tempdir().expect("无法创建临时目录");
        let path = directory.path().join("config.json");
        let mut config = AppConfig::default();
        config.general.language = InterfaceLanguage::EnUs;
        config.clipboard.clear_tags_on_tab_switch = true;

        persist_config(&path, &config).expect("配置保存失败");

        assert_eq!(load_config(&path), config);
        assert!(
            std::fs::read_to_string(path)
                .expect("无法读取配置文件")
                .ends_with('\n')
        );
    }

    #[test]
    fn saving_preserves_unknown_fields_values_and_future_version() {
        let directory = tempdir().expect("无法创建临时目录");
        let path = directory.path().join("config.json");
        std::fs::write(
            &path,
            r#"{
            "schemaVersion": 9,
            "futureGroup": {"enabled": true},
            "appearance": {"windowMaterial": "future-material", "futureOpacity": 0.7},
            "general": {"language": "zh-CN", "futureOption": [1, 2]}
        }"#,
        )
        .expect("无法写入新版配置");
        let mut config = load_config(&path);
        config.general.language = InterfaceLanguage::EnUs;
        persist_config(&path, &config).expect("无法保存兼容配置");
        let saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("无法读取兼容配置"))
                .expect("无法解析兼容配置");
        assert_eq!(saved["schemaVersion"], 9);
        assert_eq!(saved["futureGroup"]["enabled"], true);
        assert_eq!(saved["appearance"]["windowMaterial"], "future-material");
        assert_eq!(saved["appearance"]["futureOpacity"], 0.7);
        assert_eq!(saved["general"]["futureOption"], serde_json::json!([1, 2]));
        assert_eq!(load_config(&path).general.language, InterfaceLanguage::EnUs);

        config.appearance.window_material = WindowMaterial::CustomImage;
        persist_config(&path, &config).expect("无法修改已知字段");
        let saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("无法读取修改后的配置"))
                .expect("无法解析修改后的配置");
        assert_eq!(saved["appearance"]["windowMaterial"], "customimage");
        assert_eq!(saved["appearance"]["futureOpacity"], 0.7);
    }

    #[test]
    fn saving_invalid_existing_config_does_not_overwrite_it() {
        let directory = tempdir().expect("无法创建临时目录");
        let path = directory.path().join("config.json");
        std::fs::write(&path, "{invalid").expect("无法写入无效配置");
        assert!(persist_config(&path, &AppConfig::default()).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).expect("无法核验配置"),
            "{invalid"
        );
    }

    #[test]
    fn ui_scale_changes_refresh_all_windows_without_reloading_background_images() {
        let current = AppConfig::default().appearance;
        let mut next = current.clone();
        next.ui_scale_percent = 150;
        assert!(appearance_requires_global_refresh(&current, &next));
        assert!(!background_image_needs_refresh(&current, &next));
        assert!(!appearance_requires_global_refresh(&next, &next));
    }

    #[test]
    fn app_font_changes_and_reset_refresh_all_windows() {
        let current = AppConfig::default().appearance;
        let mut next = current.clone();
        next.app_font_family = Some("Microsoft YaHei".to_owned());
        assert!(appearance_requires_global_refresh(&current, &next));
        assert!(appearance_requires_global_refresh(&next, &current));
        assert!(!background_image_needs_refresh(&current, &next));
    }

    #[test]
    fn entering_custom_image_material_refreshes_an_existing_background_path() {
        let mut current = AppConfig::default().appearance;
        current.background_image_path = Some("background.jpg".to_owned());
        let mut next = current.clone();
        next.window_material = WindowMaterial::CustomImage;

        assert!(background_image_needs_refresh(&current, &next));
    }
}
