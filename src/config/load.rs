use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use thiserror::Error;

use super::{
    AppConfig, AppearanceConfig, ApplicationFilterConfig, CURRENT_SCHEMA_VERSION, ClipboardConfig,
    GeneralConfig, HistoryConfig, NotificationConfig, QuickPasteConfig, SessionShortcutConfig,
    SessionShortcutId, ShortcutsConfig, SoundConfig, SoundEffectConfig,
    default_window_material_opacity,
};

#[derive(Debug, Error)]
pub enum ConfigLoadError {
    #[error("配置 JSON 格式无效：{0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("配置根节点必须是对象")]
    RootNotObject,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoadedConfig {
    pub config: AppConfig,
    pub warnings: Vec<String>,
}

pub fn from_str(content: &str) -> Result<LoadedConfig, ConfigLoadError> {
    let value = serde_json::from_str::<Value>(content)?;
    let Some(root) = value.as_object() else {
        return Err(ConfigLoadError::RootNotObject);
    };

    let mut warnings = Vec::new();
    read_schema_version(root, &mut warnings);

    let general = read_group(root, "general", &mut warnings, read_general);
    let clipboard = read_group(root, "clipboard", &mut warnings, read_clipboard);
    let application_filter = read_group(
        root,
        "applicationFilter",
        &mut warnings,
        read_application_filter,
    );
    let history = read_group(root, "history", &mut warnings, read_history);
    let quick_paste = read_group(root, "quickPaste", &mut warnings, read_quick_paste);
    let sound = read_group(root, "sound", &mut warnings, read_sound);
    let appearance = read_group(root, "appearance", &mut warnings, read_appearance);
    let shortcuts = read_group(root, "shortcuts", &mut warnings, read_shortcuts);
    let updates = read_group(root, "updates", &mut warnings, read_updates);

    Ok(LoadedConfig {
        config: AppConfig {
            schema_version: CURRENT_SCHEMA_VERSION,
            general,
            clipboard,
            application_filter,
            history,
            quick_paste,
            sound,
            appearance,
            shortcuts,
            updates,
        }
        .normalized(),
        warnings,
    })
}

fn read_updates(object: &Map<String, Value>, warnings: &mut Vec<String>) -> super::UpdateConfig {
    let default = super::UpdateConfig::default();
    super::UpdateConfig {
        frequency: read_field(
            object,
            "frequency",
            "updates.frequency",
            default.frequency,
            warnings,
        ),
        notification_enabled: read_field(
            object,
            "notificationEnabled",
            "updates.notificationEnabled",
            default.notification_enabled,
            warnings,
        ),
        proxy_mode: read_field(
            object,
            "proxyMode",
            "updates.proxyMode",
            default.proxy_mode,
            warnings,
        ),
        proxy_url: read_field(
            object,
            "proxyUrl",
            "updates.proxyUrl",
            default.proxy_url,
            warnings,
        ),
    }
}

fn read_application_filter(
    object: &Map<String, Value>,
    warnings: &mut Vec<String>,
) -> ApplicationFilterConfig {
    let default = ApplicationFilterConfig::default();
    ApplicationFilterConfig {
        enabled: read_field(
            object,
            "enabled",
            "applicationFilter.enabled",
            default.enabled,
            warnings,
        ),
        disable_clipboard_monitor: read_field(
            object,
            "disableClipboardMonitor",
            "applicationFilter.disableClipboardMonitor",
            default.disable_clipboard_monitor,
            warnings,
        ),
        disable_global_hotkeys: read_field(
            object,
            "disableGlobalHotkeys",
            "applicationFilter.disableGlobalHotkeys",
            default.disable_global_hotkeys,
            warnings,
        ),
        disable_edge_hide: read_field(
            object,
            "disableEdgeHide",
            "applicationFilter.disableEdgeHide",
            default.disable_edge_hide,
            warnings,
        ),
        applications: read_field(
            object,
            "applications",
            "applicationFilter.applications",
            default.applications,
            warnings,
        ),
    }
}

fn read_history(object: &Map<String, Value>, warnings: &mut Vec<String>) -> HistoryConfig {
    let default = HistoryConfig::default();
    HistoryConfig {
        retention_days: read_field(
            object,
            "retentionDays",
            "history.retentionDays",
            default.retention_days,
            warnings,
        ),
        max_items: read_field(
            object,
            "maxItems",
            "history.maxItems",
            default.max_items,
            warnings,
        ),
        cleanup_on_startup: read_field(
            object,
            "cleanupOnStartup",
            "history.cleanupOnStartup",
            default.cleanup_on_startup,
            warnings,
        ),
    }
}

fn read_schema_version(root: &Map<String, Value>, warnings: &mut Vec<String>) {
    let Some(value) = root.get("schemaVersion") else {
        return;
    };
    let Ok(version) = serde_json::from_value::<u32>(value.clone()) else {
        warnings.push("配置字段 schemaVersion 无效，已使用当前版本".to_owned());
        return;
    };
    if version != CURRENT_SCHEMA_VERSION {
        warnings.push(format!(
            "配置版本 {version} 与当前版本 {CURRENT_SCHEMA_VERSION} 不同，已按当前版本读取"
        ));
    }
}

fn read_group<T, F>(
    root: &Map<String, Value>,
    group_name: &str,
    warnings: &mut Vec<String>,
    reader: F,
) -> T
where
    T: Default,
    F: FnOnce(&Map<String, Value>, &mut Vec<String>) -> T,
{
    let Some(value) = root.get(group_name) else {
        return T::default();
    };
    let Some(object) = value.as_object() else {
        warnings.push(format!("配置分组 {group_name} 无效，已使用默认值"));
        return T::default();
    };
    reader(object, warnings)
}

fn read_general(object: &Map<String, Value>, warnings: &mut Vec<String>) -> GeneralConfig {
    let default = GeneralConfig::default();
    GeneralConfig {
        language: read_field(
            object,
            "language",
            "general.language",
            default.language,
            warnings,
        ),
        global_notification_enabled: read_field(
            object,
            "globalNotificationEnabled",
            "general.globalNotificationEnabled",
            default.global_notification_enabled,
            warnings,
        ),
        notifications: read_group(object, "notifications", warnings, read_notifications),
        launch_at_login: read_field(
            object,
            "launchAtLogin",
            "general.launchAtLogin",
            default.launch_at_login,
            warnings,
        ),
        run_as_administrator: read_field(
            object,
            "runAsAdministrator",
            "general.runAsAdministrator",
            default.run_as_administrator,
            warnings,
        ),
    }
}

fn read_notifications(
    object: &Map<String, Value>,
    warnings: &mut Vec<String>,
) -> NotificationConfig {
    let default = NotificationConfig::default();
    NotificationConfig {
        position: read_field(
            object,
            "position",
            "general.notifications.position",
            default.position,
            warnings,
        ),
        duration_ms: read_field(
            object,
            "durationMs",
            "general.notifications.durationMs",
            default.duration_ms,
            warnings,
        ),
        startup: read_field(
            object,
            "startup",
            "general.notifications.startup",
            default.startup,
            warnings,
        ),
        clipboard_preview: read_field(
            object,
            "clipboardPreview",
            "general.notifications.clipboardPreview",
            default.clipboard_preview,
            warnings,
        ),
        feature_status: read_field(
            object,
            "featureStatus",
            "general.notifications.featureStatus",
            default.feature_status,
            warnings,
        ),
        operation_result: read_field(
            object,
            "operationResult",
            "general.notifications.operationResult",
            default.operation_result,
            warnings,
        ),
        attention: read_field(
            object,
            "attention",
            "general.notifications.attention",
            default.attention,
            warnings,
        ),
    }
}

fn read_appearance(object: &Map<String, Value>, warnings: &mut Vec<String>) -> AppearanceConfig {
    let default = AppearanceConfig::default();
    let window_material = read_field(
        object,
        "windowMaterial",
        "appearance.windowMaterial",
        default.window_material,
        warnings,
    );
    let window_material_opacity = if object.contains_key("windowMaterialOpacity") {
        read_field(
            object,
            "windowMaterialOpacity",
            "appearance.windowMaterialOpacity",
            default_window_material_opacity(window_material),
            warnings,
        )
    } else {
        default_window_material_opacity(window_material)
    };
    AppearanceConfig {
        mode: read_field(object, "mode", "appearance.mode", default.mode, warnings),
        ui_scale_percent: read_field(
            object,
            "uiScalePercent",
            "appearance.uiScalePercent",
            default.ui_scale_percent,
            warnings,
        ),
        app_font_family: read_field(
            object,
            "appFontFamily",
            "appearance.appFontFamily",
            default.app_font_family,
            warnings,
        ),
        light_theme: read_field(
            object,
            "lightTheme",
            "appearance.lightTheme",
            default.light_theme,
            warnings,
        ),
        dark_theme: read_field(
            object,
            "darkTheme",
            "appearance.darkTheme",
            default.dark_theme,
            warnings,
        ),
        titlebar_position: read_field(
            object,
            "titlebarPosition",
            "appearance.titlebarPosition",
            default.titlebar_position,
            warnings,
        ),
        tab_bar: read_field(
            object,
            "tabBar",
            "appearance.tabBar",
            default.tab_bar,
            warnings,
        ),
        animations: read_field(
            object,
            "animations",
            "appearance.animations",
            default.animations,
            warnings,
        ),
        gallery_gif_autoplay: read_field(
            object,
            "galleryGifAutoplay",
            "appearance.galleryGifAutoplay",
            default.gallery_gif_autoplay,
            warnings,
        ),
        window_material,
        window_material_opacity,
        background_image_path: read_field(
            object,
            "backgroundImagePath",
            "appearance.backgroundImagePath",
            default.background_image_path,
            warnings,
        ),
        background_image_blur: read_field(
            object,
            "backgroundImageBlur",
            "appearance.backgroundImageBlur",
            default.background_image_blur,
            warnings,
        ),
        window_material_brightness: if object.contains_key("windowMaterialBrightness") {
            read_field(
                object,
                "windowMaterialBrightness",
                "appearance.windowMaterialBrightness",
                default.window_material_brightness,
                warnings,
            )
        } else {
            // 兼容旧版本仅供背景图使用的字段。
            read_field(
                object,
                "backgroundImageBrightness",
                "appearance.backgroundImageBrightness",
                default.window_material_brightness,
                warnings,
            )
        },
        list_item_style: read_field(
            object,
            "listItemStyle",
            "appearance.listItemStyle",
            default.list_item_style,
            warnings,
        ),
        card_item_gap: read_field(
            object,
            "cardItemGap",
            "appearance.cardItemGap",
            default.card_item_gap,
            warnings,
        ),
        list_item_height: read_field(
            object,
            "listItemHeight",
            "appearance.listItemHeight",
            default.list_item_height,
            warnings,
        ),
        adaptive_max_lines: read_field(
            object,
            "adaptiveMaxLines",
            "appearance.adaptiveMaxLines",
            default.adaptive_max_lines,
            warnings,
        ),
        only_show_notes: read_field(
            object,
            "onlyShowNotes",
            "appearance.onlyShowNotes",
            default.only_show_notes,
            warnings,
        ),
        item_display: read_group(object, "itemDisplay", warnings, |object, warnings| {
            let default = super::ItemDisplayConfig::default();
            super::ItemDisplayConfig {
                info_visibility: read_field(
                    object,
                    "infoVisibility",
                    "appearance.itemDisplay.infoVisibility",
                    default.info_visibility,
                    warnings,
                ),
                toolbar_visibility: read_field(
                    object,
                    "toolbarVisibility",
                    "appearance.itemDisplay.toolbarVisibility",
                    default.toolbar_visibility,
                    warnings,
                ),
                actions_visibility: read_field(
                    object,
                    "actionsVisibility",
                    "appearance.itemDisplay.actionsVisibility",
                    default.actions_visibility,
                    warnings,
                ),
                fields: read_field(
                    object,
                    "fields",
                    "appearance.itemDisplay.fields",
                    default.fields,
                    warnings,
                ),
            }
        }),
    }
}

fn read_clipboard(object: &Map<String, Value>, warnings: &mut Vec<String>) -> ClipboardConfig {
    let default = ClipboardConfig::default();
    let legacy_hover_switch = read_field(
        object,
        "tabBarHoverSwitch",
        "clipboard.tabBarHoverSwitch",
        default.page_hover_switch,
        warnings,
    );
    ClipboardConfig {
        mouse_left_behavior: read_field(
            object,
            "mouseLeftBehavior",
            "clipboard.mouseLeftBehavior",
            default.mouse_left_behavior,
            warnings,
        ),
        mouse_middle_behavior: read_field(
            object,
            "mouseMiddleBehavior",
            "clipboard.mouseMiddleBehavior",
            default.mouse_middle_behavior,
            warnings,
        ),
        display_priority: read_field(
            object,
            "displayPriority",
            "clipboard.displayPriority",
            default.display_priority,
            warnings,
        ),
        preview_priority: read_field(
            object,
            "previewPriority",
            "clipboard.previewPriority",
            default.preview_priority,
            warnings,
        ),
        hover_preview_enabled: read_field(
            object,
            "hoverPreviewEnabled",
            "clipboard.hoverPreviewEnabled",
            default.hover_preview_enabled,
            warnings,
        ),
        page_hover_switch: read_field(
            object,
            "pageHoverSwitch",
            "clipboard.pageHoverSwitch",
            legacy_hover_switch,
            warnings,
        ),
        filter_hover_switch: read_field(
            object,
            "filterHoverSwitch",
            "clipboard.filterHoverSwitch",
            legacy_hover_switch,
            warnings,
        ),
        page_hover_delay_ms: read_field(
            object,
            "pageHoverDelayMs",
            "clipboard.pageHoverDelayMs",
            default.page_hover_delay_ms,
            warnings,
        ),
        filter_hover_delay_ms: read_field(
            object,
            "filterHoverDelayMs",
            "clipboard.filterHoverDelayMs",
            default.filter_hover_delay_ms,
            warnings,
        ),
        move_pasted_to_first_in_clipboard: read_field(
            object,
            "movePastedToFirstInClipboard",
            "clipboard.movePastedToFirstInClipboard",
            default.move_pasted_to_first_in_clipboard,
            warnings,
        ),
        move_pasted_to_first_in_favorites: read_field(
            object,
            "movePastedToFirstInFavorites",
            "clipboard.movePastedToFirstInFavorites",
            default.move_pasted_to_first_in_favorites,
            warnings,
        ),
        auto_scroll_to_top: read_field(
            object,
            "autoScrollToTop",
            "clipboard.autoScrollToTop",
            default.auto_scroll_to_top,
            warnings,
        ),
        auto_focus_search: read_field(
            object,
            "autoFocusSearch",
            "clipboard.autoFocusSearch",
            default.auto_focus_search,
            warnings,
        ),
        auto_clear_search: read_field(
            object,
            "autoClearSearch",
            "clipboard.autoClearSearch",
            default.auto_clear_search,
            warnings,
        ),
        confirm_favorite_removal: read_field(
            object,
            "confirmFavoriteRemoval",
            "clipboard.confirmFavoriteRemoval",
            default.confirm_favorite_removal,
            warnings,
        ),
        clear_tags_on_tab_switch: read_field(
            object,
            "clearTagsOnTabSwitch",
            "clipboard.clearTagsOnTabSwitch",
            default.clear_tags_on_tab_switch,
            warnings,
        ),
        window_position: read_field(
            object,
            "windowPosition",
            "clipboard.windowPosition",
            default.window_position,
            warnings,
        ),
        edge_hide: read_field(
            object,
            "edgeHide",
            "clipboard.edgeHide",
            default.edge_hide,
            warnings,
        ),
        edge_hide_show_edge: read_field(
            object,
            "edgeHideShowEdge",
            "clipboard.edgeHideShowEdge",
            default.edge_hide_show_edge,
            warnings,
        ),
        save_images_as_files: read_field(
            object,
            "saveImagesAsFiles",
            "clipboard.saveImagesAsFiles",
            default.save_images_as_files,
            warnings,
        ),
        search_engines: read_field(
            object,
            "searchEngines",
            "clipboard.searchEngines",
            default.search_engines,
            warnings,
        ),
    }
}

fn read_sound(object: &Map<String, Value>, warnings: &mut Vec<String>) -> SoundConfig {
    let default = SoundConfig::default();
    SoundConfig {
        enabled: read_field(
            object,
            "enabled",
            "sound.enabled",
            default.enabled,
            warnings,
        ),
        paste: read_sound_effect(object, "paste", "sound.paste", default.paste, warnings),
        copy: read_sound_effect(object, "copy", "sound.copy", default.copy, warnings),
        quick_paste_scroll: read_sound_effect(
            object,
            "quickPasteScroll",
            "sound.quickPasteScroll",
            default.quick_paste_scroll,
            warnings,
        ),
    }
}

fn read_quick_paste(object: &Map<String, Value>, warnings: &mut Vec<String>) -> QuickPasteConfig {
    let default = QuickPasteConfig::default();
    QuickPasteConfig {
        visible_items: read_field(
            object,
            "visibleItems",
            "quickPaste.visibleItems",
            default.visible_items,
            warnings,
        ),
        window_width: read_field(
            object,
            "windowWidth",
            "quickPaste.windowWidth",
            default.window_width,
            warnings,
        ),
        select_with_up_down: read_field(
            object,
            "selectWithUpDown",
            "quickPaste.selectWithUpDown",
            default.select_with_up_down,
            warnings,
        ),
        switch_list_with_left_right: read_field(
            object,
            "switchListWithLeftRight",
            "quickPaste.switchListWithLeftRight",
            default.switch_list_with_left_right,
            warnings,
        ),
        keep_last_position: read_field(
            object,
            "keepLastPosition",
            "quickPaste.keepLastPosition",
            default.keep_last_position,
            warnings,
        ),
    }
}

fn read_sound_effect(
    object: &Map<String, Value>,
    field_name: &str,
    display_name: &str,
    default: SoundEffectConfig,
    warnings: &mut Vec<String>,
) -> SoundEffectConfig {
    let Some(value) = object.get(field_name) else {
        return default;
    };
    let Some(object) = value.as_object() else {
        warnings.push(format!("配置分组 {display_name} 无效，已使用默认值"));
        return default;
    };
    SoundEffectConfig {
        preset: read_field(
            object,
            "preset",
            &format!("{display_name}.preset"),
            default.preset,
            warnings,
        ),
        volume: read_field(
            object,
            "volume",
            &format!("{display_name}.volume"),
            default.volume,
            warnings,
        ),
    }
}

fn read_shortcuts(object: &Map<String, Value>, warnings: &mut Vec<String>) -> ShortcutsConfig {
    let default = ShortcutsConfig::default();
    ShortcutsConfig {
        toggle_main_window: read_field(
            object,
            "toggleMainWindow",
            "shortcuts.toggleMainWindow",
            default.toggle_main_window.clone(),
            warnings,
        ),
        middle_mouse: read_field(
            object,
            "middleMouse",
            "shortcuts.middleMouse",
            default.middle_mouse,
            warnings,
        ),
        open_settings: read_field(
            object,
            "openSettings",
            "shortcuts.openSettings",
            default.open_settings.clone(),
            warnings,
        ),
        toggle_clipboard_monitor: read_field(
            object,
            "toggleClipboardMonitor",
            "shortcuts.toggleClipboardMonitor",
            default.toggle_clipboard_monitor.clone(),
            warnings,
        ),
        plain_text_paste: read_field(
            object,
            "plainTextPaste",
            "shortcuts.plainTextPaste",
            default.plain_text_paste.clone(),
            warnings,
        ),
        quick_paste: read_field(
            object,
            "quickPaste",
            "shortcuts.quickPaste",
            default.quick_paste.clone(),
            warnings,
        ),
        quick_paste_1_to_9: read_field(
            object,
            "quickPaste1To9",
            "shortcuts.quickPaste1To9",
            default.quick_paste_1_to_9,
            warnings,
        ),
        select_previous_item: read_session_shortcut(
            object,
            SessionShortcutId::SelectPreviousItem,
            &default,
            warnings,
        ),
        select_next_item: read_session_shortcut(
            object,
            SessionShortcutId::SelectNextItem,
            &default,
            warnings,
        ),
        select_first_item: read_session_shortcut(
            object,
            SessionShortcutId::SelectFirstItem,
            &default,
            warnings,
        ),
        select_last_item: read_session_shortcut(
            object,
            SessionShortcutId::SelectLastItem,
            &default,
            warnings,
        ),
        previous_tab: read_session_shortcut(
            object,
            SessionShortcutId::PreviousTab,
            &default,
            warnings,
        ),
        next_tab: read_session_shortcut(object, SessionShortcutId::NextTab, &default, warnings),
        toggle_window_fixed: read_session_shortcut(
            object,
            SessionShortcutId::ToggleWindowFixed,
            &default,
            warnings,
        ),
        focus_search: read_session_shortcut(
            object,
            SessionShortcutId::FocusSearch,
            &default,
            warnings,
        ),
        execute_paste: read_session_shortcut(
            object,
            SessionShortcutId::ExecutePaste,
            &default,
            warnings,
        ),
        toggle_favorite: read_session_shortcut(
            object,
            SessionShortcutId::ToggleFavorite,
            &default,
            warnings,
        ),
        toggle_pinned: read_session_shortcut(
            object,
            SessionShortcutId::TogglePinned,
            &default,
            warnings,
        ),
        delete_item: read_session_shortcut(
            object,
            SessionShortcutId::DeleteItem,
            &default,
            warnings,
        ),
        hide_window: read_session_shortcut(
            object,
            SessionShortcutId::HideWindow,
            &default,
            warnings,
        ),
    }
}

fn read_session_shortcut(
    object: &Map<String, Value>,
    id: SessionShortcutId,
    defaults: &ShortcutsConfig,
    warnings: &mut Vec<String>,
) -> SessionShortcutConfig {
    read_field(
        object,
        id.config_key(),
        &format!("shortcuts.{}", id.config_key()),
        defaults.session(id).clone(),
        warnings,
    )
}

fn read_field<T>(
    object: &Map<String, Value>,
    field_name: &str,
    display_name: &str,
    default: T,
    warnings: &mut Vec<String>,
) -> T
where
    T: DeserializeOwned,
{
    let Some(value) = object.get(field_name) else {
        return default;
    };
    match serde_json::from_value(value.clone()) {
        Ok(value) => value,
        Err(_) => {
            warnings.push(format!("配置字段 {display_name} 无效，已使用默认值"));
            default
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn history_cleanup_on_startup_round_trips_and_defaults_to_disabled() {
        for content in ["{}", r#"{"history":{"retentionDays":7,"maxItems":100}}"#] {
            let loaded = super::from_str(content).expect("无法读取历史配置");
            assert!(!loaded.config.history.cleanup_on_startup);
            assert!(loaded.warnings.is_empty());
        }
        let loaded = super::from_str(r#"{"history":{"cleanupOnStartup":true}}"#)
            .expect("无法读取启动清理配置");
        assert!(loaded.config.history.cleanup_on_startup);
        let encoded = serde_json::to_string(&loaded.config).expect("无法保存启动清理配置");
        assert!(
            super::from_str(&encoded)
                .expect("无法读取保存的配置")
                .config
                .history
                .cleanup_on_startup
        );
        let invalid = super::from_str(r#"{"history":{"cleanupOnStartup":"invalid"}}"#)
            .expect("无法读取无效配置");
        assert!(!invalid.config.history.cleanup_on_startup);
        assert_eq!(invalid.warnings.len(), 1);
    }

    #[test]
    fn gallery_gif_autoplay_round_trips_and_defaults_to_disabled() {
        let old = super::from_str(r#"{"appearance":{"animations":{}}}"#).expect("无法读取已有配置");
        assert!(!old.config.appearance.gallery_gif_autoplay);
        let mut config = old.config;
        config.appearance.gallery_gif_autoplay = true;
        let encoded = serde_json::to_string(&config).expect("无法保存自动播放配置");
        assert!(
            super::from_str(&encoded)
                .expect("无法读取自动播放配置")
                .config
                .appearance
                .gallery_gif_autoplay
        );
        let invalid = super::from_str(r#"{"appearance":{"galleryGifAutoplay":"invalid"}}"#)
            .expect("无法读取无效配置");
        assert!(!invalid.config.appearance.gallery_gif_autoplay);
        assert_eq!(invalid.warnings.len(), 1);
    }

    #[test]
    fn note_display_setting_round_trips_and_defaults_to_disabled() {
        let mut config = super::super::AppConfig::default();
        assert!(
            !super::from_str("{}")
                .unwrap()
                .config
                .appearance
                .only_show_notes
        );
        config.appearance.only_show_notes = true;
        let json = serde_json::to_string(&config).unwrap();
        assert!(
            super::from_str(&json)
                .unwrap()
                .config
                .appearance
                .only_show_notes
        );
        let invalid = super::from_str(r#"{"appearance":{"onlyShowNotes":"invalid"}}"#).unwrap();
        assert!(!invalid.config.appearance.only_show_notes);
        assert_eq!(invalid.warnings.len(), 1);
    }

    #[test]
    fn update_config_round_trips_and_invalid_fields_use_defaults() {
        use super::super::{AppConfig, UpdateConfig, UpdateFrequency, UpdateProxyMode};
        let mut config = AppConfig::default();
        config.updates = UpdateConfig {
            frequency: UpdateFrequency::EveryThreeDays,
            notification_enabled: false,
            proxy_mode: UpdateProxyMode::Custom,
            proxy_url: "http://127.0.0.1:7890".to_owned(),
        };
        let encoded = serde_json::to_string(&config).unwrap();
        assert_eq!(
            super::from_str(&encoded).unwrap().config.updates,
            config.updates
        );
        let never = super::from_str(r#"{"updates":{"frequency":"never"}}"#).unwrap();
        assert_eq!(never.config.updates.frequency, UpdateFrequency::Never);
        assert!(never.warnings.is_empty());
        let loaded =
            super::from_str(r#"{"updates":{"frequency":"invalid","notificationEnabled":false}}"#)
                .unwrap();
        assert_eq!(loaded.config.updates.frequency, UpdateFrequency::Daily);
        assert!(!loaded.config.updates.notification_enabled);
        assert!(!loaded.warnings.is_empty());
    }

    use super::{ConfigLoadError, from_str};
    use crate::config::{
        AppearanceMode, CardItemGap, ClipboardDisplayPriority, ClipboardMouseLeftBehavior,
        ClipboardWindowPosition, InterfaceLanguage, ListItemHeight, ListItemStyle, ShortcutBinding,
        SoundPreset, TitlebarPosition, WindowMaterial,
    };

    #[test]
    fn sound_enabled_defaults_and_preserves_effects_when_disabled() {
        let old =
            from_str(r#"{"sound":{"copy":{"preset":"disabled"}}}"#).expect("无法加载旧音效配置");
        assert!(old.config.sound.enabled);
        assert_eq!(old.config.sound.copy.preset, SoundPreset::Disabled);
        assert!(old.warnings.is_empty());

        for enabled in [false, true] {
            let loaded = from_str(&format!(
                r#"{{"sound":{{"enabled":{enabled},"paste":{{"preset":"external:提示.wav","volume":35}},"copy":{{"preset":"disabled"}}}}}}"#,
            ))
            .expect("无法加载音效总开关配置");
            assert_eq!(loaded.config.sound.enabled, enabled);
            assert_eq!(
                loaded.config.sound.paste.preset,
                SoundPreset::External("提示.wav".to_owned())
            );
            assert_eq!(loaded.config.sound.paste.volume, 35);
            assert_eq!(loaded.config.sound.copy.preset, SoundPreset::Disabled);
            let saved = serde_json::to_string(&loaded.config).expect("无法保存音效配置");
            assert_eq!(
                from_str(&saved).expect("无法重新加载音效配置").config,
                loaded.config
            );
            assert!(loaded.warnings.is_empty());
        }

        let invalid = from_str(r#"{"sound":{"enabled":"false","copy":{"preset":"disabled"}}}"#)
            .expect("无法加载字段级容错音效配置");
        assert!(invalid.config.sound.enabled);
        assert_eq!(invalid.config.sound.copy.preset, SoundPreset::Disabled);
        assert_eq!(invalid.warnings.len(), 1);
        assert!(invalid.warnings[0].contains("sound.enabled"));
    }

    #[test]
    fn empty_object_returns_complete_defaults() {
        let loaded = from_str("{}").expect("空配置对象应当可以加载");

        assert!(!loaded.config.clipboard.clear_tags_on_tab_switch);
        assert_eq!(loaded.config.general.language, InterfaceLanguage::ZhCn);
        assert!(loaded.config.general.global_notification_enabled);
        assert_eq!(
            loaded.config.clipboard.display_priority,
            ClipboardDisplayPriority::TextHtmlImage
        );
        assert_eq!(loaded.config.sound.paste.preset, SoundPreset::CrispChime);
        assert_eq!(loaded.config.sound.copy.preset, SoundPreset::BrightPing);
        assert_eq!(loaded.config.appearance.mode, AppearanceMode::System);
        assert_eq!(
            loaded.config.appearance.titlebar_position,
            TitlebarPosition::Top
        );
        assert!(loaded.config.appearance.animations.window_visibility);
        assert_eq!(
            loaded.config.appearance.window_material,
            WindowMaterial::Disabled
        );
        assert_eq!(
            loaded.config.appearance.list_item_style,
            ListItemStyle::Card
        );
        assert_eq!(loaded.config.appearance.card_item_gap, CardItemGap::Eight);
        assert_eq!(
            loaded.config.appearance.list_item_height,
            ListItemHeight::Adaptive
        );
        assert_eq!(loaded.config.appearance.adaptive_max_lines, 8);
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn invalid_field_does_not_discard_other_valid_fields() {
        let loaded = from_str(
            r#"{
                "general": {"language": "invalid"},
                "appearance": {"mode": "dark", "lightTheme": "Custom Light"}
            }"#,
        )
        .expect("字段错误不应导致整份配置加载失败");

        assert_eq!(loaded.config.general.language, InterfaceLanguage::ZhCn);
        assert_eq!(loaded.config.appearance.mode, AppearanceMode::Dark);
        assert_eq!(loaded.config.appearance.light_theme, "Custom Light");
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.warnings[0].contains("general.language"));
    }

    #[test]
    fn invalid_group_does_not_discard_other_groups() {
        let loaded = from_str(
            r#"{
                "general": {"language": "en-US"},
                "appearance": "invalid"
            }"#,
        )
        .expect("分组错误不应导致整份配置加载失败");

        assert_eq!(loaded.config.general.language, InterfaceLanguage::EnUs);
        assert_eq!(loaded.config.appearance.mode, AppearanceMode::System);
        assert!(loaded.warnings[0].contains("appearance"));
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let loaded = from_str(
            r#"{
                "unknown": true,
                "general": {"language": "en-US", "future": 1}
            }"#,
        )
        .expect("未知字段不应影响配置加载");

        assert_eq!(loaded.config.general.language, InterfaceLanguage::EnUs);
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn malformed_json_and_non_object_roots_are_reported() {
        assert!(matches!(
            from_str("{"),
            Err(ConfigLoadError::InvalidJson(_))
        ));
        assert!(matches!(
            from_str("[]"),
            Err(ConfigLoadError::RootNotObject)
        ));
    }

    #[test]
    fn titlebar_position_is_loaded_from_appearance_config() {
        let loaded = from_str(r#"{"appearance":{"titlebarPosition":"right"}}"#)
            .expect("标题栏位置应当可以加载");

        assert_eq!(
            loaded.config.appearance.titlebar_position,
            TitlebarPosition::Right
        );
    }

    #[test]
    fn ui_scale_defaults_loads_and_normalizes_without_affecting_other_fields() {
        let default = from_str(r#"{"appearance":{"mode":"dark"}}"#).expect("无法加载旧配置");
        assert_eq!(default.config.appearance.ui_scale_percent, 100);
        for (input, expected) in [(0, 80), (125, 125), (65535, 200)] {
            let loaded = from_str(&format!(r#"{{"appearance":{{"uiScalePercent":{input}}}}}"#))
                .expect("无法加载缩放配置");
            assert_eq!(loaded.config.appearance.ui_scale_percent, expected);
        }
        let invalid = from_str(r#"{"appearance":{"uiScalePercent":"大","onlyShowNotes":true}}"#)
            .expect("无法加载字段级容错配置");
        assert_eq!(invalid.config.appearance.ui_scale_percent, 100);
        assert!(invalid.config.appearance.only_show_notes);
        assert_eq!(invalid.warnings.len(), 1);
        let json = serde_json::to_string(&default.config).expect("无法序列化缩放配置");
        assert_eq!(
            from_str(&json).expect("无法重新加载缩放配置").config,
            default.config
        );
    }

    #[test]
    fn app_font_defaults_normalizes_and_survives_round_trip() {
        let default = from_str(r#"{"appearance":{"mode":"dark"}}"#).expect("无法加载旧配置");
        assert_eq!(default.config.appearance.app_font_family, None);
        for (value, expected) in [
            (r#""  Microsoft YaHei  ""#, Some("Microsoft YaHei")),
            (r#""   ""#, None),
            ("null", None),
        ] {
            let loaded = from_str(&format!(r#"{{"appearance":{{"appFontFamily":{value}}}}}"#))
                .expect("无法加载字体配置");
            assert_eq!(
                loaded.config.appearance.app_font_family.as_deref(),
                expected
            );
            let json = serde_json::to_string(&loaded.config).expect("无法保存字体配置");
            assert_eq!(
                from_str(&json).expect("无法重新加载字体配置").config,
                loaded.config
            );
        }
        let invalid = from_str(r#"{"appearance":{"appFontFamily":123,"onlyShowNotes":true}}"#)
            .expect("无法加载字段级容错配置");
        assert_eq!(invalid.config.appearance.app_font_family, None);
        assert!(invalid.config.appearance.only_show_notes);
        assert_eq!(invalid.warnings.len(), 1);
    }

    #[test]
    fn tab_bar_layout_loads_normalizes_and_survives_round_trip() {
        use super::super::{TabBarButton, TabBarButtonGroup, TabBarLayout, TabBarSection};

        let old = from_str(r#"{"appearance":{"mode":"dark"}}"#).expect("无法加载旧配置");
        assert_eq!(old.config.appearance.tab_bar, TabBarLayout::default());
        let loaded = from_str(
            r#"{"appearance":{"tabBar":{
            "sections":["tags","filters","pages"],
            "buttons":["image","text","image"],
            "hidden":["favorites","text"]
        }}}"#,
        )
        .expect("无法加载 Tab 栏布局");
        assert!(loaded.warnings.is_empty());
        let layout = &loaded.config.appearance.tab_bar;
        assert_eq!(
            layout.sections,
            vec![
                TabBarSection::Tags,
                TabBarSection::Filters,
                TabBarSection::Pages
            ]
        );
        assert_eq!(
            layout.buttons_in(TabBarButtonGroup::Filters).next(),
            Some(TabBarButton::Image)
        );
        assert!(!layout.is_visible(TabBarButton::Favorites));
        assert_eq!(layout.buttons.len(), TabBarButton::ALL.len());
        let json = serde_json::to_string(&loaded.config).expect("无法保存 Tab 栏布局");
        assert_eq!(
            from_str(&json).expect("无法重新加载 Tab 栏布局").config,
            loaded.config
        );

        let invalid =
            from_str(r#"{"appearance":{"tabBar":{"buttons":["unknown"]},"onlyShowNotes":true}}"#)
                .expect("无法加载字段级容错配置");
        assert_eq!(invalid.config.appearance.tab_bar, TabBarLayout::default());
        assert!(invalid.config.appearance.only_show_notes);
        assert_eq!(invalid.warnings.len(), 1);
    }

    #[test]
    fn animation_config_is_loaded_from_appearance_config() {
        let loaded = from_str(
            r#"{"appearance":{"animations":{"windowVisibility":false,"iconHoverScale":false}}}"#,
        )
        .expect("窗口显示/隐藏动画设置应当可以加载");

        assert!(!loaded.config.appearance.animations.window_visibility);
        assert!(!loaded.config.appearance.animations.icon_hover_scale);
    }

    #[test]
    fn window_material_is_loaded_from_appearance_config() {
        let loaded = from_str(r#"{"appearance":{"windowMaterial":"acrylic"}}"#)
            .expect("窗口材质设置应当可以加载");

        assert_eq!(
            loaded.config.appearance.window_material,
            WindowMaterial::Acrylic
        );
    }

    #[test]
    fn window_material_opacity_is_loaded_from_appearance_config() {
        let loaded = from_str(r#"{"appearance":{"windowMaterialOpacity":0.5}}"#)
            .expect("窗口材质不透明度应当可以加载");

        assert_eq!(loaded.config.appearance.window_material_opacity, 0.5);
    }

    #[test]
    fn list_item_style_is_loaded_from_appearance_config() {
        let loaded =
            from_str(r#"{"appearance":{"listItemStyle":"card"}}"#).expect("列表项风格应当可以加载");

        assert_eq!(
            loaded.config.appearance.list_item_style,
            ListItemStyle::Card
        );
    }

    #[test]
    fn card_item_gap_is_loaded_from_appearance_config() {
        let loaded =
            from_str(r#"{"appearance":{"cardItemGap":"16px"}}"#).expect("卡片间距应当可以加载");

        assert_eq!(loaded.config.appearance.card_item_gap, CardItemGap::Sixteen);
    }

    #[test]
    fn list_item_height_is_loaded_from_appearance_config() {
        let loaded =
            from_str(r#"{"appearance":{"listItemHeight":"extra-small","adaptiveMaxLines":20}}"#)
                .expect("列表项行高应当可以加载");

        assert_eq!(
            loaded.config.appearance.list_item_height,
            ListItemHeight::ExtraSmall
        );
        assert_eq!(loaded.config.appearance.adaptive_max_lines, 15);
    }

    #[test]
    fn clipboard_display_priority_is_loaded_from_clipboard_config() {
        let loaded = from_str(r#"{"clipboard":{"displayPriority":"html-image-text"}}"#)
            .expect("剪贴板展示优先级应当可以加载");

        assert_eq!(
            loaded.config.clipboard.display_priority,
            ClipboardDisplayPriority::HtmlImageText
        );
    }

    #[test]
    fn clipboard_mouse_left_behavior_is_loaded_from_clipboard_config() {
        let loaded = from_str(r#"{"clipboard":{"mouseLeftBehavior":"double-copy"}}"#)
            .expect("剪贴板左键行为应当可以加载");

        assert_eq!(
            loaded.config.clipboard.mouse_left_behavior,
            ClipboardMouseLeftBehavior::DoubleCopy
        );
    }

    #[test]
    fn clipboard_mouse_left_behavior_plain_text_options_round_trip() {
        for code in ["single-plain-text-paste", "double-plain-text-paste"] {
            let json = serde_json::json!({
                "clipboard": { "mouseLeftBehavior": code }
            });
            let loaded = from_str(&json.to_string()).expect("纯文本粘贴左键行为应当可以加载");

            assert!(
                loaded.warnings.is_empty(),
                "加载纯文本粘贴左键行为不应产生警告"
            );
            let saved = serde_json::to_value(&loaded.config).expect("左键行为应当可以保存");
            assert_eq!(saved["clipboard"]["mouseLeftBehavior"], code);
        }
    }

    #[test]
    fn hover_switches_default_to_disabled_and_load_independently() {
        let loaded = from_str(r#"{"clipboard":{}}"#).expect("应当可以加载旧剪贴板设置");
        assert!(!loaded.config.clipboard.page_hover_switch);
        assert!(!loaded.config.clipboard.filter_hover_switch);
        assert!(loaded.warnings.is_empty());

        let loaded = from_str(r#"{"clipboard":{"pageHoverSwitch":true}}"#)
            .expect("应当可以加载页面栏悬停切换设置");
        assert!(loaded.config.clipboard.page_hover_switch);
        assert!(!loaded.config.clipboard.filter_hover_switch);
        assert!(loaded.warnings.is_empty());

        let loaded = from_str(r#"{"clipboard":{"filterHoverSwitch":true}}"#)
            .expect("应当可以加载筛选栏悬停切换设置");
        assert!(!loaded.config.clipboard.page_hover_switch);
        assert!(loaded.config.clipboard.filter_hover_switch);
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn hover_delays_default_load_independently_and_clamp() {
        let loaded = from_str(r#"{"clipboard":{}}"#).expect("应当可以加载默认悬停停留时间");
        assert_eq!(loaded.config.clipboard.page_hover_delay_ms, 40);
        assert_eq!(loaded.config.clipboard.filter_hover_delay_ms, 40);

        let loaded = from_str(r#"{"clipboard":{"pageHoverDelayMs":500}}"#)
            .expect("应当可以加载页面栏悬停停留时间");
        assert_eq!(loaded.config.clipboard.page_hover_delay_ms, 500);
        assert_eq!(loaded.config.clipboard.filter_hover_delay_ms, 40);

        let loaded = from_str(r#"{"clipboard":{"filterHoverDelayMs":300}}"#)
            .expect("应当可以加载筛选栏悬停停留时间");
        assert_eq!(loaded.config.clipboard.page_hover_delay_ms, 40);
        assert_eq!(loaded.config.clipboard.filter_hover_delay_ms, 300);

        let loaded =
            from_str(r#"{"clipboard":{"pageHoverDelayMs":5000,"filterHoverDelayMs":5000}}"#)
                .expect("超范围悬停停留时间应当被归一化");
        assert_eq!(loaded.config.clipboard.page_hover_delay_ms, 1000);
        assert_eq!(loaded.config.clipboard.filter_hover_delay_ms, 1000);
    }

    #[test]
    fn legacy_tab_bar_hover_switch_enables_both_bars() {
        let loaded = from_str(r#"{"clipboard":{"tabBarHoverSwitch":true}}"#)
            .expect("应当可以加载旧版悬停切换设置");
        assert!(loaded.config.clipboard.page_hover_switch);
        assert!(loaded.config.clipboard.filter_hover_switch);
        assert!(loaded.warnings.is_empty());

        let saved = serde_json::to_value(&loaded.config).expect("无法保存拆分的悬停切换设置");
        assert_eq!(saved["clipboard"]["pageHoverSwitch"], true);
        assert_eq!(saved["clipboard"]["filterHoverSwitch"], true);
    }

    #[test]
    fn clipboard_edge_hide_is_loaded_from_clipboard_config() {
        let loaded =
            from_str(r#"{"clipboard":{"edgeHide":true}}"#).expect("剪贴板贴边隐藏设置应当可以加载");

        assert!(loaded.config.clipboard.edge_hide);
    }

    #[test]
    fn clipboard_window_position_is_loaded_from_clipboard_config() {
        let loaded = from_str(r#"{"clipboard":{"windowPosition":"mouse-nearby"}}"#)
            .expect("剪贴板窗口显示位置应当可以加载");

        assert_eq!(
            loaded.config.clipboard.window_position,
            ClipboardWindowPosition::MouseNearby
        );
    }

    #[test]
    fn toggle_main_window_shortcut_is_loaded_from_shortcuts_config() {
        let loaded = from_str(
            r#"{
                "shortcuts": {
                    "toggleMainWindow": {
                        "control": true,
                        "alt": true,
                        "shift": false,
                        "platform": false,
                        "key": "h"
                    }
                }
            }"#,
        )
        .expect("显示/隐藏窗口快捷键应当可以加载");

        assert_eq!(
            loaded.config.shortcuts.toggle_main_window,
            Some(ShortcutBinding {
                control: true,
                alt: true,
                shift: false,
                platform: false,
                key: "h".to_owned(),
            })
        );
    }

    #[test]
    fn missing_shortcut_config_uses_ctrl_shift_x_as_default() {
        let loaded = from_str("{}").expect("空配置应当可以加载");

        assert_eq!(
            loaded.config.shortcuts.toggle_main_window,
            Some(ShortcutBinding::toggle_main_window_default())
        );
        assert_eq!(
            loaded.config.shortcuts.select_previous_item.binding,
            Some(ShortcutBinding {
                control: false,
                alt: false,
                shift: false,
                platform: false,
                key: "up".to_owned(),
            })
        );
        assert!(!loaded.config.shortcuts.select_previous_item.only_in_window);
    }

    #[test]
    fn middle_mouse_old_config_defaults_and_new_config_round_trips() {
        let old = from_str(r#"{"shortcuts":{"openSettings":null}}"#).unwrap();
        assert_eq!(
            old.config.shortcuts.middle_mouse,
            super::super::MiddleMouseShortcutConfig::default()
        );
        assert!(old.warnings.is_empty());

        let loaded = from_str(
            r#"{"shortcuts":{"middleMouse":{
            "enabled":true,"modifier":"control-shift","holdThresholdMs":350
        }}}"#,
        )
        .unwrap();
        let config = loaded.config.shortcuts.middle_mouse;
        assert!(config.enabled);
        assert_eq!(
            config.modifier,
            Some(super::super::QuickPasteShortcutModifier::ControlShift)
        );
        assert_eq!(config.hold_threshold_ms, 350);
        let saved = serde_json::to_string(&loaded.config).unwrap();
        assert_eq!(
            from_str(&saved).unwrap().config.shortcuts.middle_mouse,
            config
        );
    }

    #[test]
    fn middle_mouse_threshold_is_limited_to_supported_range() {
        for (input, expected) in [(0, 10), (10, 10), (1000, 1000), (1001, 1000)] {
            let loaded = from_str(&format!(
                r#"{{"shortcuts":{{"middleMouse":{{"holdThresholdMs":{input}}}}}}}"#,
            ))
            .unwrap();
            assert_eq!(
                loaded.config.shortcuts.middle_mouse.hold_threshold_ms,
                expected
            );
        }
    }

    #[test]
    fn session_shortcut_config_reads_window_scope_and_binding() {
        let loaded = from_str(
            r#"{
                "shortcuts": {
                    "focusSearch": {
                        "binding": {
                            "control": true,
                            "alt": false,
                            "shift": false,
                            "platform": false,
                            "key": "f"
                        },
                        "onlyInWindow": true
                    }
                }
            }"#,
        )
        .expect("会话级快捷键配置应当可以加载");

        let shortcut = &loaded.config.shortcuts.focus_search;
        assert_eq!(
            shortcut.binding,
            Some(ShortcutBinding {
                control: true,
                alt: false,
                shift: false,
                platform: false,
                key: "f".to_owned(),
            })
        );
        assert!(shortcut.only_in_window);
    }
}
