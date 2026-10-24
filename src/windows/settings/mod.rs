mod components;
mod layout;
mod pages;

use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, Global, Render, ScrollAnchor,
    ScrollHandle, Subscription, TitlebarOptions, Window, WindowBackgroundAppearance, WindowBounds,
    WindowDecorations, WindowHandle, WindowKind, WindowOptions, div, px, size,
};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::slider::{SliderEvent, SliderState, SliderValue};
use gpui_component::{ActiveTheme as _, Root, v_flex};

use crate::windows::notification::{NotificationLevel, NotificationService};

use crate::{
    config::{
        ADAPTIVE_MAX_LINES_MAX, ADAPTIVE_MAX_LINES_MIN, BACKGROUND_IMAGE_BLUR_MAX,
        BACKGROUND_IMAGE_BLUR_MIN, ClipboardConfig, HistoryConfig, SOUND_VOLUME_MAX,
        WINDOW_MATERIAL_BRIGHTNESS_MAX, WINDOW_MATERIAL_BRIGHTNESS_MIN,
        WINDOW_MATERIAL_OPACITY_MAX, WINDOW_MATERIAL_OPACITY_MIN,
    },
    config_runtime, hotkey, i18n,
    services::data_management::DataManagementStats,
};

const SETTINGS_WINDOW_WIDTH: f32 = 840.0;
const SETTINGS_WINDOW_HEIGHT: f32 = 600.0;
const SETTINGS_WINDOW_MIN_WIDTH: f32 = 720.0;
const SETTINGS_WINDOW_MIN_HEIGHT: f32 = 500.0;
const SETTINGS_TITLE_BAR_HEIGHT: f32 = 36.0;
const SETTINGS_DROPDOWN_WIDTH: f32 = 156.0;
const SETTINGS_DROPDOWN_HEIGHT: f32 = 32.0;
const SETTINGS_LINE_HEIGHT_SELECTOR_WIDTH: f32 = 360.0;
const SETTINGS_SOUND_SLIDER_WIDTH: f32 = 220.0;
const SETTINGS_SEARCH_HIGHLIGHT_DURATION_MS: u64 = 2600;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum DataStatsScope {
    #[default]
    History,
    Favorites,
}

#[derive(Default)]
pub(crate) struct SettingsWindowService {
    window: Option<WindowHandle<Root>>,
    view: Option<Entity<SettingsWindowView>>,
}

impl Global for SettingsWindowService {}

impl SettingsWindowService {
    pub(crate) fn install(cx: &mut App) {
        cx.set_global(Self::default());
        cx.on_window_closed(|cx, _| {
            cx.defer(|cx| {
                let open_windows = cx.windows();
                cx.update_global::<Self, _>(|service, _| {
                    let Some(window) = service.window else {
                        return;
                    };
                    if !open_windows
                        .iter()
                        .any(|handle| handle.window_id() == window.window_id())
                    {
                        service.window = None;
                        service.view = None;
                    }
                });
            });
        })
        .detach();
    }
}

pub(crate) fn open_about_window(cx: &mut App) {
    open_settings_window(cx);
    let view = cx.global::<SettingsWindowService>().view.clone();
    if let Some(view) = view {
        view.update(cx, |view, cx| {
            view.active_page = SettingsPage::About;
            view.clear_search_highlight();
            cx.notify();
        });
    }
}

pub(crate) fn open_settings_window(cx: &mut App) {
    let existing = cx.read_global::<SettingsWindowService, _>(|service, _| service.window);
    if let Some(window) = existing
        && window
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        cx.activate(true);
        return;
    }

    let title = i18n::translator(cx).translate("settings.window.title");
    let titlebar = TitlebarOptions {
        title: Some(title.into()),
        appears_transparent: true,
        ..Default::default()
    };
    let result = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::centered(
                size(px(SETTINGS_WINDOW_WIDTH), px(SETTINGS_WINDOW_HEIGHT)),
                cx,
            )),
            titlebar: Some(titlebar),
            kind: WindowKind::Normal,
            focus: true,
            is_movable: true,
            is_resizable: true,
            is_minimizable: true,
            window_background: WindowBackgroundAppearance::Transparent,
            window_min_size: Some(size(
                px(SETTINGS_WINDOW_MIN_WIDTH),
                px(SETTINGS_WINDOW_MIN_HEIGHT),
            )),
            window_decorations: Some(WindowDecorations::Client),
            ..Default::default()
        },
        move |window, cx| {
            // 设置窗口由自身绘制最小化/最大化/关闭按钮，去掉系统标题栏样式位，
            // 只保留 DWM 提供的阴影与圆角。
            crate::platform::window::configure_frameless_window(window, true);
            // 客户区在调整框架后变大，窗口创建时读到的 viewport 尺寸已经过期。
            window.bounds_changed(cx);
            crate::window_material::register_window(window, cx);
            let view = cx.new(|cx| SettingsWindowView::new(window, cx));
            cx.update_global::<SettingsWindowService, _>(|service, _| {
                service.view = Some(view.clone())
            });
            cx.new(|cx| crate::window_material::new_root(view, window, cx))
        },
    );

    match result {
        Ok(window) => {
            cx.update_global::<SettingsWindowService, _>(|service, _| {
                service.window = Some(window);
            });
            if let Err(error) = window.update(cx, |_, window, _| window.activate_window()) {
                tracing::error!("激活设置窗口失败：{error}");
            }
        }
        Err(error) => {
            tracing::error!("打开设置窗口失败：{error}");
            let _ = notify_settings_error(SettingsPage::General, Some(error.to_string()), cx);
        }
    }
}

pub(crate) fn offer_legacy_migration(cx: &mut App) {
    if crate::state_runtime::state(cx).legacy_migration_prompt_dismissed {
        return;
    }
    let translator = i18n::translator(cx);
    cx.spawn(async move |cx| {
        let Some(source) = cx
            .background_executor()
            .spawn(async { crate::services::legacy_migration::detect_source() })
            .await
        else {
            return;
        };
        let answer = cx.update(|cx| {
            let window = crate::windows::main::MainWindowService::window_handle(cx)?;
            window
                .update(cx, |_, window, cx| {
                    window.prompt(
                        gpui::PromptLevel::Info,
                        &translator.translate("migration.title"),
                        Some(&translator.translate("migration.found")),
                        &[
                            gpui::PromptButton::ok(translator.translate("migration.review")),
                            gpui::PromptButton::cancel(translator.translate("migration.later")),
                            gpui::PromptButton::new(translator.translate("migration.never")),
                        ],
                        cx,
                    )
                })
                .ok()
        });
        let Some(answer) = answer else {
            return;
        };
        let choice = answer.await.unwrap_or(1);
        let accepted = choice == 0;
        let dismissed = choice == 2;
        cx.update(|cx| {
            if accepted || dismissed {
                let mut state = crate::state_runtime::state(cx);
                state.legacy_migration_prompt_dismissed = true;
                if let Err(error) = crate::state_runtime::set_state(state, cx) {
                    tracing::warn!("保存旧版迁移提示状态失败：{error}");
                }
            }
            if accepted {
                open_settings_window(cx);
                let view =
                    cx.read_global::<SettingsWindowService, _>(|service, _| service.view.clone());
                if let Some(view) = view {
                    view.update(cx, |view, cx| {
                        view.active_page = SettingsPage::DataManagement;
                        cx.notify();
                    });
                    pages::start_source_analysis(view, source, cx);
                }
            }
        });
    })
    .detach();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsPage {
    General,
    Clipboard,
    ApplicationFilter,
    QuickPaste,
    Sound,
    Shortcuts,
    Personalization,
    DataManagement,
    About,
}

impl SettingsPage {
    const fn index(self) -> usize {
        match self {
            Self::General => 0,
            Self::Clipboard => 1,
            Self::Shortcuts => 2,
            Self::QuickPaste => 3,
            Self::Personalization => 4,
            Self::Sound => 5,
            Self::ApplicationFilter => 6,
            Self::DataManagement => 7,
            Self::About => 8,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShortcutCategory {
    Global,
    Temporary,
}

impl SettingsPage {
    const ALL: [Self; 9] = [
        Self::General,
        Self::Clipboard,
        Self::Shortcuts,
        Self::QuickPaste,
        Self::Personalization,
        Self::Sound,
        Self::ApplicationFilter,
        Self::DataManagement,
        Self::About,
    ];

    const fn id(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Clipboard => "clipboard",
            Self::ApplicationFilter => "application-filter",
            Self::QuickPaste => "quick-paste",
            Self::Sound => "sound",
            Self::Shortcuts => "shortcuts",
            Self::Personalization => "personalization",
            Self::DataManagement => "data-management",
            Self::About => "about",
        }
    }

    const fn icon_name(self) -> &'static str {
        match self {
            Self::General => "settings",
            Self::Clipboard => "clipboard",
            Self::ApplicationFilter => "filter",
            Self::QuickPaste => "clipboard-check",
            Self::Sound => "music-cog",
            Self::Shortcuts => "keyboard",
            Self::Personalization => "palette",
            Self::DataManagement => "folder-open",
            Self::About => "info-circle",
        }
    }

    const fn title_key(self) -> &'static str {
        match self {
            Self::General => "settings.page.general",
            Self::Clipboard => "settings.page.clipboard",
            Self::ApplicationFilter => "settings.page.application_filter",
            Self::QuickPaste => "settings.page.quick_paste",
            Self::Sound => "settings.page.sound",
            Self::Shortcuts => "settings.page.shortcuts",
            Self::Personalization => "settings.page.personalization",
            Self::DataManagement => "settings.page.data_management",
            Self::About => "settings.page.about",
        }
    }
}

#[derive(Clone)]
struct SearchTarget {
    page: SettingsPage,
    title: String,
    description: String,
    ratio: f32,
}

struct SearchTargetSpec {
    page: SettingsPage,
    title_key: &'static str,
    description_key: Option<&'static str>,
    ratio: f32,
}

fn notify_settings_status(
    page: SettingsPage,
    message: Option<String>,
    level: NotificationLevel,
    cx: &mut App,
) -> Option<String> {
    if let Some(message) = message.as_ref() {
        NotificationService::show_message(
            i18n::translator(cx).translate(page.title_key()),
            message.clone(),
            level,
            cx,
        );
    }
    message
}

fn notify_settings_error(
    page: SettingsPage,
    error: Option<String>,
    cx: &mut App,
) -> Option<String> {
    notify_settings_status(page, error, NotificationLevel::Error, cx)
}

fn clipboard_hover_delay_subscription(
    slider: &Entity<SliderState>,
    window: &mut Window,
    cx: &mut Context<SettingsWindowView>,
    apply: fn(&mut ClipboardConfig, u16),
) -> Subscription {
    cx.subscribe_in(
        slider,
        window,
        move |view, _, event: &SliderEvent, _, cx| {
            let SliderEvent::Change(value) = event else {
                return;
            };
            let delay = value.end().round().clamp(
                f32::from(ClipboardConfig::TAB_BAR_HOVER_DELAY_MIN_MS),
                f32::from(ClipboardConfig::TAB_BAR_HOVER_DELAY_MAX_MS),
            ) as u16;
            let current = config_runtime::clipboard_config(cx);
            let mut clipboard = current.clone();
            apply(&mut clipboard, delay);
            if clipboard == current {
                return;
            }
            let error = config_runtime::set_clipboard_config(clipboard, cx)
                .err()
                .map(|error| {
                    tracing::error!("保存悬停停留时间失败：{error}");
                    i18n::translator(cx).translate("settings.clipboard.save_error")
                });
            view.clipboard_error = notify_settings_error(SettingsPage::Clipboard, error, cx);
            cx.notify();
        },
    )
}

fn open_settings_directory(path: std::path::PathBuf, title_key: &'static str, cx: &mut App) {
    let task = cx.background_spawn(async move {
        crate::platform::shell_open::open_with_default_program(&path)
    });
    cx.spawn(async move |cx| {
        if let Err(error) = task.await {
            tracing::error!("打开设置相关目录失败：{error}");
            cx.update(|cx| {
                NotificationService::show_message(
                    i18n::translator(cx).translate(title_key),
                    error,
                    NotificationLevel::Error,
                    cx,
                );
            });
        }
    })
    .detach();
}

fn search_target_specs() -> &'static [SearchTargetSpec] {
    use SettingsPage::*;
    &[
        SearchTargetSpec {
            page: General,
            title_key: "settings.general.interface_language",
            description_key: None,
            ratio: 0.02,
        },
        SearchTargetSpec {
            page: General,
            title_key: "settings.general.global_notifications",
            description_key: Some("settings.general.global_notifications_description"),
            ratio: 0.12,
        },
        SearchTargetSpec {
            page: General,
            title_key: "settings.notifications.title",
            description_key: Some("settings.notifications.search_description"),
            ratio: 0.22,
        },
        SearchTargetSpec {
            page: General,
            title_key: "settings.general.launch_at_login",
            description_key: Some("settings.general.launch_at_login_description"),
            ratio: 0.32,
        },
        SearchTargetSpec {
            page: General,
            title_key: "settings.general.run_as_administrator",
            description_key: Some("settings.general.run_as_administrator_description"),
            ratio: 0.42,
        },
        SearchTargetSpec {
            page: Clipboard,
            title_key: "settings.clipboard.mouse_left_behavior",
            description_key: Some("settings.clipboard.mouse_left_behavior_description"),
            ratio: 0.02,
        },
        SearchTargetSpec {
            page: Clipboard,
            title_key: "settings.clipboard.mouse_middle_behavior",
            description_key: Some("settings.clipboard.mouse_middle_behavior_description"),
            ratio: 0.09,
        },
        SearchTargetSpec {
            page: Clipboard,
            title_key: "settings.clipboard.edge_hide",
            description_key: Some("settings.clipboard.edge_hide_description"),
            ratio: 0.18,
        },
        SearchTargetSpec {
            page: Clipboard,
            title_key: "settings.clipboard.edge_hide_show_edge",
            description_key: Some("settings.clipboard.edge_hide_show_edge_description"),
            ratio: 0.24,
        },
        SearchTargetSpec {
            page: Clipboard,
            title_key: "settings.clipboard.hover_preview",
            description_key: Some("settings.clipboard.hover_preview_description"),
            ratio: 0.30,
        },
        SearchTargetSpec {
            page: Clipboard,
            title_key: "settings.clipboard.auto_scroll_to_top",
            description_key: Some("settings.clipboard.auto_scroll_to_top_description"),
            ratio: 0.46,
        },
        SearchTargetSpec {
            page: Clipboard,
            title_key: "settings.clipboard.auto_focus_search",
            description_key: Some("settings.clipboard.auto_focus_search_description"),
            ratio: 0.52,
        },
        SearchTargetSpec {
            page: Clipboard,
            title_key: "settings.clipboard.auto_clear_search",
            description_key: Some("settings.clipboard.auto_clear_search_description"),
            ratio: 0.58,
        },
        SearchTargetSpec {
            page: Clipboard,
            title_key: "settings.clipboard.confirm_favorite_removal",
            description_key: Some("settings.clipboard.confirm_favorite_removal_description"),
            ratio: 0.64,
        },
        SearchTargetSpec {
            page: Clipboard,
            title_key: "settings.clipboard.window_position",
            description_key: Some("settings.clipboard.window_position_description"),
            ratio: 0.72,
        },
        SearchTargetSpec {
            page: Clipboard,
            title_key: "settings.clipboard.display_priority",
            description_key: Some("settings.clipboard.display_priority_description"),
            ratio: 0.82,
        },
        SearchTargetSpec {
            page: Clipboard,
            title_key: "settings.clipboard.preview_priority",
            description_key: Some("settings.clipboard.preview_priority_description"),
            ratio: 0.90,
        },
        SearchTargetSpec {
            page: Clipboard,
            title_key: "settings.clipboard.preview_types",
            description_key: Some("settings.clipboard.preview_types_description"),
            ratio: 0.93,
        },
        SearchTargetSpec {
            page: Clipboard,
            title_key: "settings.clipboard.search_engines",
            description_key: Some("settings.clipboard.search_engines_description"),
            ratio: 0.96,
        },
        SearchTargetSpec {
            page: Clipboard,
            title_key: "settings.clipboard.capture_formats",
            description_key: Some("settings.clipboard.capture_formats_description"),
            ratio: 0.04,
        },
        SearchTargetSpec {
            page: Clipboard,
            title_key: "settings.clipboard.capture_all_formats",
            description_key: Some("settings.clipboard.capture_all_formats_description"),
            ratio: 0.04,
        },
        SearchTargetSpec {
            page: Clipboard,
            title_key: "settings.clipboard.move_pasted_to_first_in_clipboard",
            description_key: Some(
                "settings.clipboard.move_pasted_to_first_in_clipboard_description",
            ),
            ratio: 0.36,
        },
        SearchTargetSpec {
            page: Clipboard,
            title_key: "settings.clipboard.move_pasted_to_first_in_favorites",
            description_key: Some(
                "settings.clipboard.move_pasted_to_first_in_favorites_description",
            ),
            ratio: 0.41,
        },
        SearchTargetSpec {
            page: ApplicationFilter,
            title_key: "settings.application_filter.enabled",
            description_key: Some("settings.application_filter.enabled_description"),
            ratio: 0.02,
        },
        SearchTargetSpec {
            page: ApplicationFilter,
            title_key: "settings.application_filter.scope",
            description_key: Some("settings.application_filter.scope_description"),
            ratio: 0.15,
        },
        SearchTargetSpec {
            page: ApplicationFilter,
            title_key: "settings.application_filter.disable_clipboard",
            description_key: Some("settings.application_filter.disable_clipboard_description"),
            ratio: 0.20,
        },
        SearchTargetSpec {
            page: ApplicationFilter,
            title_key: "settings.application_filter.disable_hotkeys",
            description_key: Some("settings.application_filter.disable_hotkeys_description"),
            ratio: 0.24,
        },
        SearchTargetSpec {
            page: ApplicationFilter,
            title_key: "settings.application_filter.disable_edge_hide",
            description_key: Some("settings.application_filter.disable_edge_hide_description"),
            ratio: 0.28,
        },
        SearchTargetSpec {
            page: ApplicationFilter,
            title_key: "settings.application_filter.manual",
            description_key: Some("settings.application_filter.manual_description"),
            ratio: 0.38,
        },
        SearchTargetSpec {
            page: QuickPaste,
            title_key: "settings.quick_paste.visible_items",
            description_key: Some("settings.quick_paste.visible_items_description"),
            ratio: 0.02,
        },
        SearchTargetSpec {
            page: QuickPaste,
            title_key: "settings.quick_paste.window_width",
            description_key: Some("settings.quick_paste.window_width_description"),
            ratio: 0.18,
        },
        SearchTargetSpec {
            page: QuickPaste,
            title_key: "settings.quick_paste.select_with_up_down",
            description_key: Some("settings.quick_paste.select_with_up_down_description"),
            ratio: 0.34,
        },
        SearchTargetSpec {
            page: QuickPaste,
            title_key: "settings.quick_paste.switch_list_with_left_right",
            description_key: Some("settings.quick_paste.switch_list_with_left_right_description"),
            ratio: 0.48,
        },
        SearchTargetSpec {
            page: QuickPaste,
            title_key: "settings.quick_paste.keep_last_position",
            description_key: Some("settings.quick_paste.keep_last_position_description"),
            ratio: 0.62,
        },
        SearchTargetSpec {
            page: Sound,
            title_key: "settings.sound.copy_effect",
            description_key: Some("settings.sound.effect_description"),
            ratio: 0.02,
        },
        SearchTargetSpec {
            page: Sound,
            title_key: "settings.sound.copy_volume",
            description_key: Some("settings.sound.volume_description"),
            ratio: 0.14,
        },
        SearchTargetSpec {
            page: Sound,
            title_key: "settings.sound.paste_effect",
            description_key: Some("settings.sound.effect_description"),
            ratio: 0.26,
        },
        SearchTargetSpec {
            page: Sound,
            title_key: "settings.sound.paste_volume",
            description_key: Some("settings.sound.volume_description"),
            ratio: 0.38,
        },
        SearchTargetSpec {
            page: Sound,
            title_key: "settings.sound.quick_paste_scroll_effect",
            description_key: Some("settings.sound.effect_description"),
            ratio: 0.50,
        },
        SearchTargetSpec {
            page: Sound,
            title_key: "settings.sound.quick_paste_scroll_volume",
            description_key: Some("settings.sound.volume_description"),
            ratio: 0.62,
        },
        SearchTargetSpec {
            page: Shortcuts,
            title_key: "settings.shortcuts.toggle_main_window",
            description_key: None,
            ratio: 0.02,
        },
        SearchTargetSpec {
            page: Shortcuts,
            title_key: "settings.shortcuts.middle_mouse.enabled",
            description_key: Some("settings.shortcuts.middle_mouse.description"),
            ratio: 0.06,
        },
        SearchTargetSpec {
            page: Shortcuts,
            title_key: "settings.shortcuts.middle_mouse.threshold",
            description_key: Some("settings.shortcuts.middle_mouse.description"),
            ratio: 0.10,
        },
        SearchTargetSpec {
            page: Shortcuts,
            title_key: "settings.shortcuts.open_settings",
            description_key: None,
            ratio: 0.08,
        },
        SearchTargetSpec {
            page: Shortcuts,
            title_key: "settings.shortcuts.toggle_clipboard_monitor",
            description_key: None,
            ratio: 0.14,
        },
        SearchTargetSpec {
            page: Shortcuts,
            title_key: "settings.shortcuts.plain_text_paste",
            description_key: None,
            ratio: 0.20,
        },
        SearchTargetSpec {
            page: Shortcuts,
            title_key: "settings.shortcuts.quick_paste",
            description_key: None,
            ratio: 0.26,
        },
        SearchTargetSpec {
            page: Shortcuts,
            title_key: "settings.shortcuts.quick_paste_1_to_9",
            description_key: None,
            ratio: 0.34,
        },
        SearchTargetSpec {
            page: Shortcuts,
            title_key: "settings.shortcuts.global_section",
            description_key: Some("settings.shortcuts.global_description"),
            ratio: 0.00,
        },
        SearchTargetSpec {
            page: Shortcuts,
            title_key: "settings.shortcuts.session_section",
            description_key: Some("settings.shortcuts.session_description"),
            ratio: 0.54,
        },
        SearchTargetSpec {
            page: Shortcuts,
            title_key: "settings.shortcuts.select_previous_item",
            description_key: None,
            ratio: 0.62,
        },
        SearchTargetSpec {
            page: Shortcuts,
            title_key: "settings.shortcuts.select_next_item",
            description_key: None,
            ratio: 0.66,
        },
        SearchTargetSpec {
            page: Shortcuts,
            title_key: "settings.shortcuts.select_first_item",
            description_key: None,
            ratio: 0.70,
        },
        SearchTargetSpec {
            page: Shortcuts,
            title_key: "settings.shortcuts.select_last_item",
            description_key: None,
            ratio: 0.74,
        },
        SearchTargetSpec {
            page: Shortcuts,
            title_key: "settings.shortcuts.previous_tab",
            description_key: None,
            ratio: 0.78,
        },
        SearchTargetSpec {
            page: Shortcuts,
            title_key: "settings.shortcuts.next_tab",
            description_key: None,
            ratio: 0.82,
        },
        SearchTargetSpec {
            page: Shortcuts,
            title_key: "settings.shortcuts.toggle_window_fixed",
            description_key: None,
            ratio: 0.86,
        },
        SearchTargetSpec {
            page: Shortcuts,
            title_key: "settings.shortcuts.focus_search",
            description_key: None,
            ratio: 0.90,
        },
        SearchTargetSpec {
            page: Shortcuts,
            title_key: "settings.shortcuts.execute_paste",
            description_key: None,
            ratio: 0.92,
        },
        SearchTargetSpec {
            page: Shortcuts,
            title_key: "settings.shortcuts.toggle_favorite",
            description_key: None,
            ratio: 0.94,
        },
        SearchTargetSpec {
            page: Shortcuts,
            title_key: "settings.shortcuts.toggle_pinned",
            description_key: None,
            ratio: 0.96,
        },
        SearchTargetSpec {
            page: Shortcuts,
            title_key: "settings.shortcuts.delete_item",
            description_key: None,
            ratio: 0.98,
        },
        SearchTargetSpec {
            page: Shortcuts,
            title_key: "settings.shortcuts.hide_window",
            description_key: None,
            ratio: 1.00,
        },
        SearchTargetSpec {
            page: Personalization,
            title_key: "settings.personalization.appearance_mode",
            description_key: Some("settings.personalization.appearance_mode_description"),
            ratio: 0.02,
        },
        SearchTargetSpec {
            page: Personalization,
            title_key: "settings.personalization.light_theme",
            description_key: Some("settings.personalization.light_theme_description"),
            ratio: 0.08,
        },
        SearchTargetSpec {
            page: Personalization,
            title_key: "settings.personalization.dark_theme",
            description_key: Some("settings.personalization.dark_theme_description"),
            ratio: 0.14,
        },
        SearchTargetSpec {
            page: Personalization,
            title_key: "settings.personalization.ui_scale",
            description_key: Some("settings.personalization.ui_scale_description"),
            ratio: 0.18,
        },
        SearchTargetSpec {
            page: Personalization,
            title_key: "settings.personalization.app_font",
            description_key: Some("settings.personalization.app_font_description"),
            ratio: 0.20,
        },
        SearchTargetSpec {
            page: Personalization,
            title_key: "settings.personalization.titlebar_position",
            description_key: Some("settings.personalization.titlebar_position_description"),
            ratio: 0.22,
        },
        SearchTargetSpec {
            page: Personalization,
            title_key: "settings.personalization.tab_bar",
            description_key: None,
            ratio: 0.27,
        },
        SearchTargetSpec {
            page: Personalization,
            title_key: "settings.item_display.title",
            description_key: None,
            ratio: 0.29,
        },
        SearchTargetSpec {
            page: Personalization,
            title_key: "settings.personalization.preview_information",
            description_key: Some("settings.personalization.preview_information_description"),
            ratio: 0.30,
        },
        SearchTargetSpec {
            page: Personalization,
            title_key: "settings.personalization.animations",
            description_key: Some("settings.personalization.animations_description"),
            ratio: 0.31,
        },
        SearchTargetSpec {
            page: Personalization,
            title_key: "settings.personalization.gallery_gif_autoplay",
            description_key: Some("settings.personalization.gallery_gif_autoplay_description"),
            ratio: 0.32,
        },
        SearchTargetSpec {
            page: Personalization,
            title_key: "settings.personalization.window_material",
            description_key: Some("settings.personalization.window_material_description"),
            ratio: 0.32,
        },
        SearchTargetSpec {
            page: Personalization,
            title_key: "settings.personalization.window_material_opacity",
            description_key: Some("settings.personalization.window_material_opacity_description"),
            ratio: 0.40,
        },
        SearchTargetSpec {
            page: Personalization,
            title_key: "settings.personalization.background_image",
            description_key: Some("settings.personalization.background_image_description"),
            ratio: 0.50,
        },
        SearchTargetSpec {
            page: Personalization,
            title_key: "settings.personalization.background_image_blur",
            description_key: Some("settings.personalization.background_image_blur_description"),
            ratio: 0.58,
        },
        SearchTargetSpec {
            page: Personalization,
            title_key: "settings.personalization.window_material_brightness",
            description_key: Some(
                "settings.personalization.window_material_brightness_description",
            ),
            ratio: 0.64,
        },
        SearchTargetSpec {
            page: Personalization,
            title_key: "settings.personalization.list_item_style",
            description_key: Some("settings.personalization.list_item_style_description"),
            ratio: 0.72,
        },
        SearchTargetSpec {
            page: Personalization,
            title_key: "settings.personalization.card_item_gap",
            description_key: Some("settings.personalization.card_item_gap_description"),
            ratio: 0.78,
        },
        SearchTargetSpec {
            page: Personalization,
            title_key: "settings.personalization.list_item_height",
            description_key: Some("settings.personalization.list_item_height_description"),
            ratio: 0.84,
        },
        SearchTargetSpec {
            page: Personalization,
            title_key: "settings.personalization.adaptive_max_lines",
            description_key: Some("settings.personalization.adaptive_max_lines_description"),
            ratio: 0.90,
        },
        SearchTargetSpec {
            page: Personalization,
            title_key: "settings.personalization.only_show_notes",
            description_key: Some("settings.personalization.only_show_notes_description"),
            ratio: 0.93,
        },
        SearchTargetSpec {
            page: DataManagement,
            title_key: "settings.data_management.data_location",
            description_key: Some("settings.data_management.data_location_description"),
            ratio: 0.02,
        },
        SearchTargetSpec {
            page: DataManagement,
            title_key: "settings.data_management.retention",
            description_key: Some("settings.data_management.retention_description"),
            ratio: 0.14,
        },
        SearchTargetSpec {
            page: DataManagement,
            title_key: "settings.data_management.max_items",
            description_key: Some("settings.data_management.max_items_description"),
            ratio: 0.22,
        },
        SearchTargetSpec {
            page: DataManagement,
            title_key: "settings.data_management.cleanup_on_startup",
            description_key: Some("settings.data_management.cleanup_on_startup_description"),
            ratio: 0.30,
        },
        SearchTargetSpec {
            page: DataManagement,
            title_key: "settings.data_management.stats_title",
            description_key: Some("settings.data_management.stats_description"),
            ratio: 0.46,
        },
        SearchTargetSpec {
            page: DataManagement,
            title_key: "settings.data_management.actions_title",
            description_key: Some("settings.data_management.actions_description"),
            ratio: 0.78,
        },
        SearchTargetSpec {
            page: DataManagement,
            title_key: "settings.data_management.cleanup",
            description_key: None,
            ratio: 0.82,
        },
        SearchTargetSpec {
            page: DataManagement,
            title_key: "settings.data_management.export",
            description_key: None,
            ratio: 0.84,
        },
        SearchTargetSpec {
            page: DataManagement,
            title_key: "settings.data_management.import",
            description_key: None,
            ratio: 0.86,
        },
        SearchTargetSpec {
            page: DataManagement,
            title_key: "settings.data_management.clear_all_history",
            description_key: None,
            ratio: 0.86,
        },
        SearchTargetSpec {
            page: DataManagement,
            title_key: "settings.data_management.reset_application",
            description_key: None,
            ratio: 0.86,
        },
        SearchTargetSpec {
            page: DataManagement,
            title_key: "settings.data_management.open_directory",
            description_key: None,
            ratio: 0.88,
        },
        SearchTargetSpec {
            page: Sound,
            title_key: "settings.sound.external.title",
            description_key: Some("settings.sound.external.description"),
            ratio: 0.76,
        },
    ]
}

fn search_targets(translator: &i18n::Translator, query: &str) -> Vec<SearchTarget> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    search_target_specs()
        .iter()
        .filter_map(|spec| {
            let title = translator.translate(spec.title_key);
            let description = spec
                .description_key
                .map(|key| translator.translate(key))
                .unwrap_or_default();
            let page_title = translator.translate(spec.page.title_key());
            let haystack = format!("{title} {description} {page_title}").to_lowercase();
            haystack.contains(&query).then_some(SearchTarget {
                page: spec.page,
                title,
                description,
                ratio: spec.ratio,
            })
        })
        .collect()
}

struct SettingsWindowView {
    active_page: SettingsPage,
    focus_handle: FocusHandle,
    settings_search_input: Entity<InputState>,
    search_results_visible: bool,
    search_results_scroll_handle: ScrollHandle,
    page_scroll_handles: [ScrollHandle; 9],
    pending_search_scroll: Option<(SettingsPage, f32)>,
    highlighted_search_anchor: Option<ScrollAnchor>,
    highlighted_search: Option<String>,
    highlighted_search_page: Option<SettingsPage>,
    search_highlight_visible: bool,
    search_highlight_generation: u64,
    language_error: Option<String>,
    clipboard_error: Option<String>,
    quick_paste_error: Option<String>,
    sound_error: Option<String>,
    appearance_error: Option<String>,
    tab_bar_edit_symbols: bool,
    shortcut_error: Option<String>,
    data_stats: Option<DataManagementStats>,
    data_stats_loading: bool,
    data_stats_policy: Option<HistoryConfig>,
    data_stats_request_id: u64,
    data_stats_scope: DataStatsScope,
    data_busy: bool,
    data_error: Option<String>,
    data_message: Option<String>,
    legacy_migration: Option<std::sync::Arc<crate::services::legacy_migration::PreparedMigration>>,
    legacy_source: Option<std::path::PathBuf>,
    legacy_options: crate::services::legacy_migration::MigrationOptions,
    legacy_progress: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
    legacy_report: Option<crate::services::legacy_migration::MigrationReport>,
    legacy_error: Option<String>,
    history_retention_input: Entity<InputState>,
    history_max_items_input: Entity<InputState>,
    application_filter_input: Entity<InputState>,
    update_proxy_input: Entity<InputState>,
    update_proxy_mode: crate::config::UpdateProxyMode,
    update_error: Option<String>,
    application_filter_apps: Vec<crate::services::application_filter::ApplicationInfo>,
    application_filter_loading: bool,
    application_filter_initialized: bool,
    application_filter_request_id: u64,
    application_filter_error: Option<String>,
    selected_shortcut_category: ShortcutCategory,
    shortcut_scroll_handle: gpui::ScrollHandle,
    recording_shortcut: bool,
    recording_open_settings: bool,
    recording_toggle_clipboard_monitor: bool,
    recording_plain_text_paste: bool,
    recording_quick_paste: bool,
    recording_session_shortcut: Option<crate::config::SessionShortcutId>,
    recording_focus_handle: FocusHandle,
    adaptive_max_lines_slider: Entity<SliderState>,
    middle_mouse_threshold_slider: Entity<SliderState>,
    wheel_mouse_threshold_slider: Entity<SliderState>,
    window_material_opacity_slider: Entity<SliderState>,
    background_image_blur_slider: Entity<SliderState>,
    window_material_brightness_slider: Entity<SliderState>,
    paste_volume_slider: Entity<SliderState>,
    quick_paste_scroll_volume_slider: Entity<SliderState>,
    search_engine_name_input: Entity<InputState>,
    clipboard_format_input: Entity<InputState>,
    /// 收藏轮盘外观：扇区/悬停/中心色输入框。
    wheel_sector_color_input: Entity<InputState>,
    wheel_hover_color_input: Entity<InputState>,
    wheel_center_color_input: Entity<InputState>,
    clipboard_formats_expanded: bool,
    search_engine_url_input: Entity<InputState>,
    /// 待添加引擎已选择的图标本地路径；为空则用内置品牌图标或通用图标。
    pending_search_engine_icon: Option<String>,
    /// 搜索引擎管理区是否展开（折叠时只显示标题行节省空间）。
    search_engines_expanded: bool,
    copy_volume_slider: Entity<SliderState>,
    page_hover_delay_slider: Entity<SliderState>,
    filter_hover_delay_slider: Entity<SliderState>,
    _adaptive_max_lines_subscription: Subscription,
    _middle_mouse_threshold_subscription: Subscription,
    _wheel_mouse_threshold_subscription: Subscription,
    _window_material_opacity_subscription: Subscription,
    _background_image_blur_subscription: Subscription,
    _window_material_brightness_subscription: Subscription,
    _paste_volume_subscription: Subscription,
    _quick_paste_scroll_volume_subscription: Subscription,
    _copy_volume_subscription: Subscription,
    _page_hover_delay_subscription: Subscription,
    _filter_hover_delay_subscription: Subscription,
    _history_retention_subscription: Subscription,
    _history_max_items_subscription: Subscription,
    _wheel_sector_color_subscription: Subscription,
    _wheel_hover_color_subscription: Subscription,
    _wheel_center_color_subscription: Subscription,
    _shortcut_recording_blur_subscription: Subscription,
    _settings_search_subscription: Subscription,
}

impl SettingsWindowView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let middle_mouse_threshold = config_runtime::shortcuts_config(cx)
            .middle_mouse
            .hold_threshold_ms;
        let middle_mouse_threshold_slider = cx.new(|_| {
            SliderState::new()
                .min(10.0)
                .max(1000.0)
                .step(1.0)
                .default_value(f32::from(middle_mouse_threshold))
        });
        let middle_mouse_threshold_subscription = cx.subscribe_in(
            &middle_mouse_threshold_slider,
            window,
            |view, _, event: &SliderEvent, _, cx| {
                let SliderEvent::Change(value) = event else {
                    return;
                };
                let mut config = config_runtime::shortcuts_config(cx).middle_mouse;
                let threshold = value.end().round().clamp(10.0, 1000.0) as u16;
                if !config.enabled || config.hold_threshold_ms == threshold {
                    return;
                }
                config.hold_threshold_ms = threshold;
                let error = hotkey::set_middle_mouse_config(config, cx)
                    .err()
                    .map(|error| {
                        tracing::error!("保存鼠标中键长按阈值失败：{error}");
                        i18n::translator(cx).translate("settings.shortcuts.save_error")
                    });
                view.shortcut_error = notify_settings_error(SettingsPage::Shortcuts, error, cx);
                cx.notify();
            },
        );
        let wheel_mouse_threshold = config_runtime::shortcuts_config(cx)
            .wheel_mouse
            .hold_threshold_ms;
        let wheel_mouse_threshold_slider = cx.new(|_| {
            SliderState::new()
                .min(10.0)
                .max(1000.0)
                .step(1.0)
                .default_value(f32::from(wheel_mouse_threshold))
        });
        let wheel_mouse_threshold_subscription = cx.subscribe_in(
            &wheel_mouse_threshold_slider,
            window,
            |view, _, event: &SliderEvent, _, cx| {
                let SliderEvent::Change(value) = event else {
                    return;
                };
                let mut config = config_runtime::shortcuts_config(cx).wheel_mouse;
                let threshold = value.end().round().clamp(10.0, 1000.0) as u16;
                if !config.enabled || config.hold_threshold_ms == threshold {
                    return;
                }
                config.hold_threshold_ms = threshold;
                let error = hotkey::set_wheel_mouse_config(config, cx)
                    .err()
                    .map(|error| {
                        tracing::error!("保存侧键轮盘按住阈值失败：{error}");
                        i18n::translator(cx).translate("settings.shortcuts.save_error")
                    });
                view.shortcut_error = notify_settings_error(SettingsPage::Shortcuts, error, cx);
                cx.notify();
            },
        );
        let adaptive_max_lines = config_runtime::appearance_config(cx).adaptive_max_lines;
        let adaptive_max_lines_slider = cx.new(|_| {
            SliderState::new()
                .min(f32::from(ADAPTIVE_MAX_LINES_MIN))
                .max(f32::from(ADAPTIVE_MAX_LINES_MAX))
                .step(1.0)
                .default_value(f32::from(adaptive_max_lines))
        });
        let window_material_opacity = config_runtime::appearance_config(cx).window_material_opacity;
        let window_material_opacity_slider = cx.new(|_| {
            SliderState::new()
                .min(WINDOW_MATERIAL_OPACITY_MIN)
                .max(WINDOW_MATERIAL_OPACITY_MAX)
                .step(0.05)
                .default_value(window_material_opacity)
        });
        let background_image_blur = config_runtime::appearance_config(cx).background_image_blur;
        let background_image_blur_slider = cx.new(|_| {
            SliderState::new()
                .min(BACKGROUND_IMAGE_BLUR_MIN)
                .max(BACKGROUND_IMAGE_BLUR_MAX)
                .step(1.0)
                .default_value(background_image_blur)
        });
        let window_material_brightness = crate::window_material::current_brightness(cx);
        let window_material_brightness_slider = cx.new(|_| {
            SliderState::new()
                .min(WINDOW_MATERIAL_BRIGHTNESS_MIN)
                .max(WINDOW_MATERIAL_BRIGHTNESS_MAX)
                .step(0.05)
                .default_value(window_material_brightness)
        });
        let sound_config = config_runtime::sound_config(cx);
        let paste_volume_slider = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(f32::from(SOUND_VOLUME_MAX))
                .step(1.0)
                .default_value(f32::from(sound_config.paste.volume))
        });
        let quick_paste_scroll_volume_slider = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(f32::from(SOUND_VOLUME_MAX))
                .step(1.0)
                .default_value(f32::from(sound_config.quick_paste_scroll.volume))
        });
        let copy_volume_slider = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(f32::from(SOUND_VOLUME_MAX))
                .step(1.0)
                .default_value(f32::from(sound_config.copy.volume))
        });
        let clipboard_config = config_runtime::clipboard_config(cx);
        let page_hover_delay_slider = cx.new(|_| {
            SliderState::new()
                .max(f32::from(ClipboardConfig::TAB_BAR_HOVER_DELAY_MAX_MS))
                .min(f32::from(ClipboardConfig::TAB_BAR_HOVER_DELAY_MIN_MS))
                .step(10.0)
                .default_value(f32::from(clipboard_config.page_hover_delay_ms))
        });
        let filter_hover_delay_slider = cx.new(|_| {
            SliderState::new()
                .max(f32::from(ClipboardConfig::TAB_BAR_HOVER_DELAY_MAX_MS))
                .min(f32::from(ClipboardConfig::TAB_BAR_HOVER_DELAY_MIN_MS))
                .step(10.0)
                .default_value(f32::from(clipboard_config.filter_hover_delay_ms))
        });
        let history_config = config_runtime::history_config(cx);
        let unlimited_placeholder =
            i18n::translator(cx).translate("settings.data_management.unlimited");
        let history_retention_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(unlimited_placeholder.clone())
                .default_value(optional_limit_text(history_config.retention_days))
                .min(0.0)
                .step(1.0)
        });
        let history_max_items_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(unlimited_placeholder.clone())
                .default_value(optional_limit_text(history_config.max_items))
                .min(0.0)
                .step(1.0)
        });
        let application_filter_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(
                i18n::translator(cx).translate("settings.application_filter.manual_placeholder"),
            )
        });
        let update_proxy_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("http://127.0.0.1:7890")
                .default_value(config_runtime::update_config(cx).proxy_url)
        });
        let settings_search_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(i18n::translator(cx).translate("settings.search.placeholder"))
        });
        let settings_search_subscription = cx.subscribe(
            &settings_search_input,
            |view, input, event: &InputEvent, cx| match event {
                InputEvent::Focus => {
                    view.search_results_visible = !input.read(cx).value().trim().is_empty();
                    cx.notify();
                }
                InputEvent::Change => {
                    view.clear_search_highlight();
                    view.search_results_visible = !input.read(cx).value().trim().is_empty();
                    cx.notify();
                }
                InputEvent::PressEnter {
                    secondary: false,
                    shift: false,
                } => {
                    let query = input.read(cx).value().to_string();
                    if let Some(target) = search_targets(&i18n::translator(cx), &query)
                        .into_iter()
                        .next()
                    {
                        view.select_search_result(target, cx);
                    }
                }
                _ => {}
            },
        );
        let history_retention_subscription = cx.subscribe_in(
            &history_retention_input,
            window,
            |view, input, event: &InputEvent, _, cx| {
                if view.data_busy || !matches!(event, InputEvent::Change) {
                    return;
                }
                let Some(value) = parse_optional_limit(input.read(cx).value().as_ref()) else {
                    return;
                };
                let mut config = config_runtime::history_config(cx);
                if config.retention_days == value {
                    return;
                }
                config.retention_days = value;
                match config_runtime::set_history_config(config, cx) {
                    Ok(_) => view.data_error = None,
                    Err(error) => {
                        tracing::error!("保存历史保留时间失败：{error}");
                        view.data_error = notify_settings_error(
                            SettingsPage::DataManagement,
                            Some(
                                i18n::translator(cx)
                                    .translate("settings.data_management.history_save_error"),
                            ),
                            cx,
                        );
                    }
                }
                cx.notify();
            },
        );
        let history_max_items_subscription = cx.subscribe_in(
            &history_max_items_input,
            window,
            |view, input, event: &InputEvent, _, cx| {
                if view.data_busy || !matches!(event, InputEvent::Change) {
                    return;
                }
                let Some(value) = parse_optional_limit(input.read(cx).value().as_ref()) else {
                    return;
                };
                let mut config = config_runtime::history_config(cx);
                if config.max_items == value {
                    return;
                }
                config.max_items = value;
                match config_runtime::set_history_config(config, cx) {
                    Ok(_) => view.data_error = None,
                    Err(error) => {
                        tracing::error!("保存最大历史条数失败：{error}");
                        view.data_error = notify_settings_error(
                            SettingsPage::DataManagement,
                            Some(
                                i18n::translator(cx)
                                    .translate("settings.data_management.history_save_error"),
                            ),
                            cx,
                        );
                    }
                }
                cx.notify();
            },
        );
        let save_error = i18n::translator(cx).translate("settings.personalization.save_error");
        let adaptive_max_lines_subscription = cx.subscribe_in(
            &adaptive_max_lines_slider,
            window,
            move |view, _, event: &SliderEvent, _, cx| {
                let SliderEvent::Change(value) = event else {
                    return;
                };
                let adaptive_max_lines = value.end().round().clamp(
                    f32::from(ADAPTIVE_MAX_LINES_MIN),
                    f32::from(ADAPTIVE_MAX_LINES_MAX),
                ) as u8;
                pages::persist_appearance_change_in_view(
                    view,
                    "自适应最大行数",
                    move |appearance| appearance.adaptive_max_lines = adaptive_max_lines,
                    save_error.clone(),
                    cx,
                );
            },
        );
        let material_save_error =
            i18n::translator(cx).translate("settings.personalization.save_error");
        let window_material_opacity_subscription = cx.subscribe_in(
            &window_material_opacity_slider,
            window,
            move |view, _, event: &SliderEvent, _, cx| {
                let SliderEvent::Change(value) = event else {
                    return;
                };
                let opacity = value
                    .end()
                    .clamp(WINDOW_MATERIAL_OPACITY_MIN, WINDOW_MATERIAL_OPACITY_MAX);
                pages::persist_appearance_change_in_view(
                    view,
                    "窗口材质不透明度",
                    move |appearance| appearance.window_material_opacity = opacity,
                    material_save_error.clone(),
                    cx,
                );
            },
        );
        let blur_save_error = i18n::translator(cx).translate("settings.personalization.save_error");
        let background_image_blur_subscription = cx.subscribe_in(
            &background_image_blur_slider,
            window,
            move |view, _, event: &SliderEvent, _, cx| {
                let SliderEvent::Change(value) = event else {
                    return;
                };
                let blur = value
                    .end()
                    .clamp(BACKGROUND_IMAGE_BLUR_MIN, BACKGROUND_IMAGE_BLUR_MAX);
                pages::persist_appearance_change_in_view(
                    view,
                    "背景图模糊程度",
                    move |appearance| appearance.background_image_blur = blur,
                    blur_save_error.clone(),
                    cx,
                );
            },
        );
        let brightness_save_error =
            i18n::translator(cx).translate("settings.personalization.save_error");
        let window_material_brightness_subscription = cx.subscribe_in(
            &window_material_brightness_slider,
            window,
            move |view, _, event: &SliderEvent, _, cx| {
                let SliderEvent::Change(value) = event else {
                    return;
                };
                let brightness = value.end().clamp(
                    WINDOW_MATERIAL_BRIGHTNESS_MIN,
                    WINDOW_MATERIAL_BRIGHTNESS_MAX,
                );
                pages::persist_appearance_change_in_view(
                    view,
                    "窗口材质明暗度",
                    move |appearance| appearance.window_material_brightness = Some(brightness),
                    brightness_save_error.clone(),
                    cx,
                );
            },
        );
        let sound_save_error = i18n::translator(cx).translate("settings.sound.save_error");
        let paste_volume_subscription = cx.subscribe_in(
            &paste_volume_slider,
            window,
            move |view, _, event: &SliderEvent, _, cx| {
                let SliderEvent::Change(value) = event else {
                    return;
                };
                let volume = value.end().round().clamp(0.0, f32::from(SOUND_VOLUME_MAX)) as u8;
                pages::persist_sound_change_in_view(
                    view,
                    "粘贴音效音量",
                    move |sound| sound.paste.volume = volume,
                    sound_save_error.clone(),
                    cx,
                );
            },
        );
        let sound_save_error = i18n::translator(cx).translate("settings.sound.save_error");
        let copy_volume_subscription = cx.subscribe_in(
            &copy_volume_slider,
            window,
            move |view, _, event: &SliderEvent, _, cx| {
                let SliderEvent::Change(value) = event else {
                    return;
                };
                let volume = value.end().round().clamp(0.0, f32::from(SOUND_VOLUME_MAX)) as u8;
                pages::persist_sound_change_in_view(
                    view,
                    "复制音效音量",
                    move |sound| sound.copy.volume = volume,
                    sound_save_error.clone(),
                    cx,
                );
            },
        );
        let sound_save_error = i18n::translator(cx).translate("settings.sound.save_error");
        let quick_paste_scroll_volume_subscription = cx.subscribe_in(
            &quick_paste_scroll_volume_slider,
            window,
            move |view, _, event: &SliderEvent, _, cx| {
                let SliderEvent::Change(value) = event else {
                    return;
                };
                let volume = value.end().round().clamp(0.0, f32::from(SOUND_VOLUME_MAX)) as u8;
                pages::persist_sound_change_in_view(
                    view,
                    "便捷粘贴滚动音效音量",
                    move |sound| sound.quick_paste_scroll.volume = volume,
                    sound_save_error.clone(),
                    cx,
                );
            },
        );
        let page_hover_delay_subscription = clipboard_hover_delay_subscription(
            &page_hover_delay_slider,
            window,
            cx,
            |clipboard, delay| clipboard.page_hover_delay_ms = delay,
        );
        let filter_hover_delay_subscription = clipboard_hover_delay_subscription(
            &filter_hover_delay_slider,
            window,
            cx,
            |clipboard, delay| clipboard.filter_hover_delay_ms = delay,
        );
        let clipboard_format_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(
                i18n::translator(cx).translate("settings.clipboard.native_format_placeholder"),
            )
        });
        let search_engine_name_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(
                i18n::translator(cx).translate("settings.clipboard.search_engine_name_placeholder"),
            )
        });
        let search_engine_url_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(
                i18n::translator(cx).translate("settings.clipboard.search_engine_url_placeholder"),
            )
        });
        let wheel_appearance = config_runtime::appearance_config(cx).wheel_appearance;
        let wheel_sector_color_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(wheel_appearance.sector_color.clone())
        });
        let wheel_hover_color_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(wheel_appearance.hover_color.clone())
        });
        let wheel_center_color_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(wheel_appearance.center_color.clone())
        });
        // 三个颜色输入：回车时写入 appearance.wheelAppearance（避免每敲一个字符就写盘）。
        let wheel_color_save_error =
            i18n::translator(cx).translate("settings.personalization.save_error");
        let wheel_sector_input_for_save = wheel_sector_color_input.clone();
        let sector_save_error = wheel_color_save_error.clone();
        let wheel_sector_subscription = cx.subscribe_in(
            &wheel_sector_input_for_save,
            window,
            move |view, input, event: &InputEvent, _window, cx| {
                if !matches!(event, InputEvent::PressEnter { .. }) {
                    return;
                }
                let raw = input.read(cx).value().to_string();
                save_wheel_color(
                    view,
                    raw,
                    |wheel| &mut wheel.sector_color,
                    "轮盘扇区颜色",
                    sector_save_error.clone(),
                    cx,
                );
            },
        );
        let wheel_hover_input_for_save = wheel_hover_color_input.clone();
        let hover_save_error = wheel_color_save_error.clone();
        let wheel_hover_subscription = cx.subscribe_in(
            &wheel_hover_input_for_save,
            window,
            move |view, input, event: &InputEvent, _window, cx| {
                if !matches!(event, InputEvent::PressEnter { .. }) {
                    return;
                }
                let raw = input.read(cx).value().to_string();
                save_wheel_color(
                    view,
                    raw,
                    |wheel| &mut wheel.hover_color,
                    "轮盘悬停颜色",
                    hover_save_error.clone(),
                    cx,
                );
            },
        );
        let wheel_center_input_for_save = wheel_center_color_input.clone();
        let center_save_error = wheel_color_save_error.clone();
        let wheel_center_subscription = cx.subscribe_in(
            &wheel_center_input_for_save,
            window,
            move |view, input, event: &InputEvent, _window, cx| {
                if !matches!(event, InputEvent::PressEnter { .. }) {
                    return;
                }
                let raw = input.read(cx).value().to_string();
                save_wheel_color(
                    view,
                    raw,
                    |wheel| &mut wheel.center_color,
                    "轮盘中心颜色",
                    center_save_error.clone(),
                    cx,
                );
            },
        );
        let recording_focus_handle = cx.focus_handle();
        let search_results_scroll_handle = ScrollHandle::new();
        let shortcut_scroll_handle = ScrollHandle::new();
        let page_scroll_handles = std::array::from_fn(|index| {
            if index == SettingsPage::Shortcuts.index() {
                shortcut_scroll_handle.clone()
            } else {
                ScrollHandle::new()
            }
        });
        let shortcut_recording_blur_subscription =
            cx.on_blur(&recording_focus_handle, window, |view, _, cx| {
                if !view.recording_shortcut {
                    return;
                }
                if let Err(error) = hotkey::cancel_toggle_main_window_recording(cx) {
                    tracing::error!("快捷键录制失焦后恢复原快捷键失败：{error}");
                    view.shortcut_error = notify_settings_error(
                        SettingsPage::Shortcuts,
                        Some(i18n::translator(cx).translate("settings.shortcuts.save_error")),
                        cx,
                    );
                }
                view.recording_shortcut = false;
                view.recording_open_settings = false;
                view.recording_toggle_clipboard_monitor = false;
                view.recording_plain_text_paste = false;
                view.recording_quick_paste = false;
                view.recording_session_shortcut = None;
                cx.notify();
            });
        Self {
            active_page: SettingsPage::General,
            focus_handle: cx.focus_handle(),
            settings_search_input,
            search_results_visible: false,
            search_results_scroll_handle,
            page_scroll_handles,
            pending_search_scroll: None,
            highlighted_search_anchor: None,
            highlighted_search: None,
            highlighted_search_page: None,
            search_highlight_visible: false,
            search_highlight_generation: 0,
            language_error: None,
            clipboard_error: None,
            quick_paste_error: None,
            sound_error: None,
            appearance_error: None,
            tab_bar_edit_symbols: false,
            shortcut_error: None,
            data_stats: None,
            data_stats_loading: false,
            data_stats_policy: None,
            data_stats_request_id: 0,
            data_stats_scope: DataStatsScope::History,
            data_busy: false,
            data_error: None,
            data_message: None,
            legacy_migration: None,
            legacy_source: None,
            legacy_options: crate::services::legacy_migration::MigrationOptions::default(),
            legacy_progress: None,
            legacy_report: None,
            legacy_error: None,
            history_retention_input,
            history_max_items_input,
            application_filter_input,
            update_proxy_input,
            update_proxy_mode: config_runtime::update_config(cx).proxy_mode,
            update_error: None,
            application_filter_apps: Vec::new(),
            application_filter_loading: false,
            application_filter_initialized: false,
            application_filter_request_id: 0,
            application_filter_error: None,
            selected_shortcut_category: ShortcutCategory::Global,
            shortcut_scroll_handle,
            recording_shortcut: false,
            recording_open_settings: false,
            recording_toggle_clipboard_monitor: false,
            recording_plain_text_paste: false,
            recording_quick_paste: false,
            recording_session_shortcut: None,
            recording_focus_handle,
            adaptive_max_lines_slider,
            middle_mouse_threshold_slider,
            wheel_mouse_threshold_slider,
            window_material_opacity_slider,
            background_image_blur_slider,
            window_material_brightness_slider,
            paste_volume_slider,
            quick_paste_scroll_volume_slider,
            copy_volume_slider,
            page_hover_delay_slider,
            filter_hover_delay_slider,
            search_engine_name_input,
            clipboard_format_input,
            wheel_sector_color_input,
            wheel_hover_color_input,
            wheel_center_color_input,
            clipboard_formats_expanded: false,
            search_engine_url_input,
            pending_search_engine_icon: None,
            search_engines_expanded: false,
            _adaptive_max_lines_subscription: adaptive_max_lines_subscription,
            _middle_mouse_threshold_subscription: middle_mouse_threshold_subscription,
            _wheel_mouse_threshold_subscription: wheel_mouse_threshold_subscription,
            _window_material_opacity_subscription: window_material_opacity_subscription,
            _background_image_blur_subscription: background_image_blur_subscription,
            _window_material_brightness_subscription: window_material_brightness_subscription,
            _paste_volume_subscription: paste_volume_subscription,
            _quick_paste_scroll_volume_subscription: quick_paste_scroll_volume_subscription,
            _copy_volume_subscription: copy_volume_subscription,
            _page_hover_delay_subscription: page_hover_delay_subscription,
            _filter_hover_delay_subscription: filter_hover_delay_subscription,
            _history_retention_subscription: history_retention_subscription,
            _history_max_items_subscription: history_max_items_subscription,
            _wheel_sector_color_subscription: wheel_sector_subscription,
            _wheel_hover_color_subscription: wheel_hover_subscription,
            _wheel_center_color_subscription: wheel_center_subscription,
            _shortcut_recording_blur_subscription: shortcut_recording_blur_subscription,
            _settings_search_subscription: settings_search_subscription,
        }
    }

    pub(super) fn setting_row(
        &self,
        theme: gpui_component::ThemeColor,
        title: String,
        description: Option<gpui::SharedString>,
        control: impl IntoElement,
    ) -> AnyElement {
        self.highlight_element(
            theme,
            title.clone(),
            components::setting_row(theme, title, description, control),
        )
    }

    pub(super) fn compact_setting_row(
        &self,
        theme: gpui_component::ThemeColor,
        title: String,
        control: impl IntoElement,
    ) -> AnyElement {
        self.highlight_element(
            theme,
            title.clone(),
            components::compact_setting_row(theme, title, control),
        )
    }

    pub(super) fn highlight_element(
        &self,
        theme: gpui_component::ThemeColor,
        title: String,
        element: impl IntoElement,
    ) -> AnyElement {
        let highlighted = self.highlighted_search_page == Some(self.active_page)
            && self.highlighted_search.as_deref() == Some(title.as_str());
        if !highlighted {
            return element.into_any_element();
        }
        div()
            .id(format!("settings-search-target-{}", title))
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .relative()
            .anchor_scroll(self.highlighted_search_anchor.clone())
            .child(element)
            .when(self.search_highlight_visible, |this| {
                this.child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(px(6.0))
                        .border_2()
                        .border_color(theme.primary),
                )
            })
            .into_any_element()
    }

    fn clear_search_highlight(&mut self) {
        self.search_highlight_generation = self.search_highlight_generation.wrapping_add(1);
        self.highlighted_search = None;
        self.highlighted_search_page = None;
        self.highlighted_search_anchor = None;
        self.search_highlight_visible = false;
    }

    fn schedule_search_highlight_hide(&self, cx: &mut Context<Self>) {
        let generation = self.search_highlight_generation;
        cx.spawn(async move |view, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(SETTINGS_SEARCH_HIGHLIGHT_DURATION_MS))
                .await;
            let _ = view.update(cx, |view, cx| {
                if view.search_highlight_generation == generation {
                    view.clear_search_highlight();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn select_search_result(&mut self, target: SearchTarget, cx: &mut Context<Self>) {
        self.active_page = target.page;
        self.search_results_visible = false;
        self.pending_search_scroll = Some((target.page, target.ratio));
        self.search_highlight_generation = self.search_highlight_generation.wrapping_add(1);
        self.highlighted_search = Some(target.title);
        self.highlighted_search_page = Some(target.page);
        self.search_highlight_visible = false;
        self.highlighted_search_anchor = Some(ScrollAnchor::for_handle(
            self.page_scroll_handles[target.page.index()].clone(),
        ));
        cx.notify();
    }

    fn sync_history_inputs(&self, window: &mut Window, cx: &mut Context<Self>) {
        let config = config_runtime::history_config(cx);
        sync_limit_input(
            &self.history_retention_input,
            optional_limit_text(config.retention_days),
            window,
            cx,
        );
        sync_limit_input(
            &self.history_max_items_input,
            optional_limit_text(config.max_items),
            window,
            cx,
        );
    }

    /// 轮盘颜色输入框与配置保持同步（重置/外部修改后回填）。
    fn sync_wheel_color_inputs(&self, window: &mut Window, cx: &mut Context<Self>) {
        let wheel = config_runtime::appearance_config(cx).wheel_appearance;
        for (input, value) in [
            (&self.wheel_sector_color_input, wheel.sector_color),
            (&self.wheel_hover_color_input, wheel.hover_color),
            (&self.wheel_center_color_input, wheel.center_color),
        ] {
            input.update(cx, |state, cx| {
                if state.value().as_ref() != value.as_str() {
                    state.set_value(value.clone(), window, cx);
                }
            });
        }
    }

    pub(crate) fn refresh_application_filter_apps(&mut self, cx: &mut Context<Self>) {
        if self.application_filter_loading {
            return;
        }
        self.application_filter_loading = true;
        self.application_filter_initialized = true;
        self.application_filter_request_id = self.application_filter_request_id.wrapping_add(1);
        let request_id = self.application_filter_request_id;
        cx.spawn(async move |this, cx| {
            let applications = cx
                .background_spawn(async {
                    crate::services::application_filter::enumerate_running_applications()
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.application_filter_request_id != request_id {
                    return;
                }
                view.application_filter_loading = false;
                view.application_filter_apps = applications;
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn refresh_data_stats(&mut self, cx: &mut Context<Self>) {
        let policy = config_runtime::history_config(cx);
        if self.data_stats_loading && self.data_stats_policy.as_ref() == Some(&policy) {
            return;
        }
        let service = cx.read_global::<crate::services::data_management::DataManagementService, _>(
            |service, _| service.clone(),
        );
        self.data_stats_request_id = self.data_stats_request_id.wrapping_add(1);
        let request_id = self.data_stats_request_id;
        self.data_stats_policy = Some(policy.clone());
        self.data_stats_loading = true;
        let translator = i18n::translator(cx);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { service.stats(&policy) })
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.data_stats_request_id != request_id {
                    return;
                }
                view.data_stats_loading = false;
                match result {
                    Ok(stats) => {
                        view.data_stats = Some(stats);
                        view.data_error = None;
                    }
                    Err(error) => {
                        tracing::error!("读取数据统计失败：{error}");
                        view.data_error = notify_settings_error(
                            SettingsPage::DataManagement,
                            Some(translator.translate("settings.data_management.stats_error")),
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn sync_adaptive_max_lines_slider(&self, window: &mut Window, cx: &mut Context<Self>) {
        let adaptive_max_lines =
            f32::from(config_runtime::appearance_config(cx).adaptive_max_lines);
        self.adaptive_max_lines_slider.update(cx, |slider, cx| {
            if slider.value() != SliderValue::Single(adaptive_max_lines) {
                slider.set_value(adaptive_max_lines, window, cx);
            }
        });
    }

    fn sync_window_material_opacity_slider(&self, window: &mut Window, cx: &mut Context<Self>) {
        let opacity = config_runtime::appearance_config(cx).window_material_opacity;
        self.window_material_opacity_slider
            .update(cx, |slider, cx| {
                if slider.value() != SliderValue::Single(opacity) {
                    slider.set_value(opacity, window, cx);
                }
            });
    }

    fn sync_background_image_blur_slider(&self, window: &mut Window, cx: &mut Context<Self>) {
        let blur = config_runtime::appearance_config(cx).background_image_blur;
        self.background_image_blur_slider.update(cx, |slider, cx| {
            if slider.value() != SliderValue::Single(blur) {
                slider.set_value(blur, window, cx);
            }
        });
    }

    fn sync_window_material_brightness_slider(&self, window: &mut Window, cx: &mut Context<Self>) {
        let brightness = crate::window_material::current_brightness(cx);
        self.window_material_brightness_slider
            .update(cx, |slider, cx| {
                if slider.value() != SliderValue::Single(brightness) {
                    slider.set_value(brightness, window, cx);
                }
            });
    }

    fn sync_sound_sliders(&self, window: &mut Window, cx: &mut Context<Self>) {
        let sound = config_runtime::sound_config(cx);
        for (slider, volume) in [
            (&self.paste_volume_slider, sound.paste.volume),
            (&self.copy_volume_slider, sound.copy.volume),
            (
                &self.quick_paste_scroll_volume_slider,
                sound.quick_paste_scroll.volume,
            ),
        ] {
            let volume = f32::from(volume);
            slider.update(cx, |slider, cx| {
                if slider.value() != SliderValue::Single(volume) {
                    slider.set_value(volume, window, cx);
                }
            });
        }
    }
}

fn optional_limit_text(value: Option<u32>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

pub(in crate::windows::settings) fn parse_optional_limit(value: &str) -> Option<Option<u32>> {
    let value = value.trim();
    if value.is_empty() || value == "none" {
        Some(None)
    } else {
        value.parse().ok().map(Some)
    }
}

fn sync_limit_input(
    input: &Entity<InputState>,
    value: String,
    window: &mut Window,
    cx: &mut Context<SettingsWindowView>,
) {
    input.update(cx, |input, cx| {
        if input.value().as_ref() != value {
            input.set_value(value, window, cx);
        }
    });
}

/// 轮盘颜色输入回车保存：把输入框文本归一化后写入 appearance.wheelAppearance 对应字段。
/// 输入非法（既非空也非合法十六进制）时提示错误不保存。
fn save_wheel_color(
    view: &mut SettingsWindowView,
    raw: String,
    field: fn(&mut crate::config::WheelAppearanceConfig) -> &mut String,
    field_name: &'static str,
    save_error: String,
    cx: &mut Context<SettingsWindowView>,
) {
    let normalized = crate::config::normalize_wheel_color(&raw);
    if normalized.is_empty() && !raw.trim().trim_start_matches('#').is_empty() {
        // 输入了内容但格式非法：提示并回退为当前配置值。
        view.appearance_error =
            notify_settings_error(SettingsPage::Personalization, Some(save_error), cx);
        cx.notify();
        return;
    }
    let mut appearance = config_runtime::appearance_config(cx);
    *field(&mut appearance.wheel_appearance) = normalized;
    let error_message = match config_runtime::set_appearance_config(appearance, cx) {
        Ok(_) => None,
        Err(error) => {
            tracing::error!("保存{field_name}失败：{error}");
            Some(i18n::translator(cx).translate("settings.personalization.save_error"))
        }
    };
    view.appearance_error = notify_settings_error(SettingsPage::Personalization, error_message, cx);
    cx.notify();
}

impl Focusable for SettingsWindowView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SettingsWindowView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let history_config = config_runtime::history_config(cx);
        if self.active_page == SettingsPage::ApplicationFilter
            && !self.application_filter_initialized
        {
            self.refresh_application_filter_apps(cx);
        }
        if self.active_page == SettingsPage::DataManagement
            && self.data_stats_policy.as_ref() != Some(&history_config)
        {
            self.refresh_data_stats(cx);
        }
        self.sync_history_inputs(window, cx);
        self.sync_wheel_color_inputs(window, cx);
        self.sync_adaptive_max_lines_slider(window, cx);
        let threshold = f32::from(
            config_runtime::shortcuts_config(cx)
                .middle_mouse
                .hold_threshold_ms,
        );
        self.middle_mouse_threshold_slider.update(cx, |slider, cx| {
            if slider.value() != SliderValue::Single(threshold) {
                slider.set_value(threshold, window, cx);
            }
        });
        let wheel_threshold = f32::from(
            config_runtime::shortcuts_config(cx)
                .wheel_mouse
                .hold_threshold_ms,
        );
        self.wheel_mouse_threshold_slider.update(cx, |slider, cx| {
            if slider.value() != SliderValue::Single(wheel_threshold) {
                slider.set_value(wheel_threshold, window, cx);
            }
        });
        self.sync_window_material_opacity_slider(window, cx);
        self.sync_background_image_blur_slider(window, cx);
        self.sync_window_material_brightness_slider(window, cx);
        self.sync_sound_sliders(window, cx);
        if let Some((page, ratio)) = self.pending_search_scroll.take() {
            if let Some(anchor) = self.highlighted_search_anchor.clone() {
                anchor.scroll_to(window, cx);
            } else {
                let handle = self.page_scroll_handles[page.index()].clone();
                window.on_next_frame(move |_, _| {
                    let max_offset = handle.max_offset();
                    handle.set_offset(gpui::point(px(0.0), -max_offset.y * ratio));
                });
            }
            if let (Some(highlighted_page), Some(highlighted_title)) = (
                self.highlighted_search_page,
                self.highlighted_search.clone(),
            ) {
                let highlighted_generation = self.search_highlight_generation;
                let view = cx.entity().clone();
                window.on_next_frame(move |_, cx| {
                    let _ = view.update(cx, |view, cx| {
                        if view.search_highlight_generation == highlighted_generation
                            && view.highlighted_search_page == Some(highlighted_page)
                            && view.highlighted_search.as_deref()
                                == Some(highlighted_title.as_str())
                        {
                            view.search_highlight_visible = true;
                            view.schedule_search_highlight_hide(cx);
                            cx.notify();
                        }
                    });
                });
            }
        }
        v_flex()
            .size_full()
            .relative()
            .bg(cx.theme().colors.background)
            .track_focus(&self.focus_handle)
            .child(self.render_title_bar(window, cx))
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .min_h_0()
                    .flex()
                    .overflow_hidden()
                    .child(self.render_sidebar(cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .overflow_hidden()
                            .child(self.render_content(cx)),
                    ),
            )
            .child(self.render_search_results(cx))
    }
}
