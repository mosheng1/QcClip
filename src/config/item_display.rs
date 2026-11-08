use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ItemRegionVisibility {
    Always,
    #[serde(alias = "active")]
    Hover,
    Hidden,
}

impl ItemRegionVisibility {
    pub const ALL: [Self; 3] = [Self::Always, Self::Hover, Self::Hidden];

    pub const fn is_visible(self, hovered: bool) -> bool {
        matches!(self, Self::Always) || (matches!(self, Self::Hover) && hovered)
    }

    pub const fn label_key(self) -> &'static str {
        match self {
            Self::Always => "settings.item_display.always",
            Self::Hover => "settings.item_display.hover",
            Self::Hidden => "settings.item_display.hidden",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ItemDisplayField {
    Time,
    Length,
    ImageSize,
    Source,
    Shortcut,
    Edit,
    PinToScreen,
    SplitWords,
    Note,
    PastePlain,
    OpenDefault,
    OpenBrowser,
    Favorite,
    Pin,
    Delete,
}

impl ItemDisplayField {
    pub const ALL: [Self; 15] = [
        Self::Time,
        Self::Length,
        Self::ImageSize,
        Self::Source,
        Self::Shortcut,
        Self::Edit,
        Self::PinToScreen,
        Self::SplitWords,
        Self::Note,
        Self::PastePlain,
        Self::OpenDefault,
        Self::OpenBrowser,
        Self::Favorite,
        Self::Pin,
        Self::Delete,
    ];

    /// 新增的操作按钮默认不显示，需要用户在设置里手动开启。
    pub const DEFAULT_HIDDEN: [Self; 2] = [Self::PinToScreen, Self::SplitWords];

    pub fn default_fields() -> Vec<Self> {
        Self::ALL
            .into_iter()
            .filter(|field| !Self::DEFAULT_HIDDEN.contains(field))
            .collect()
    }

    pub const fn label_key(self) -> &'static str {
        match self {
            Self::Time => "settings.item_display.time",
            Self::Length => "settings.item_display.length",
            Self::ImageSize => "settings.item_display.image_size",
            Self::Source => "settings.item_display.source",
            Self::Shortcut => "settings.item_display.shortcut",
            Self::Edit => "main.menu.edit",
            Self::PinToScreen => "main.menu.pin_to_screen",
            Self::SplitWords => "main.menu.split_words",
            Self::Note => "main.note.edit",
            Self::PastePlain => "main.menu.paste_as_plain_text",
            Self::OpenDefault => "main.menu.open_with_default",
            Self::OpenBrowser => "main.menu.open_in_browser",
            Self::Favorite => "main.menu.add_to_favorites",
            Self::Pin => "main.menu.pin",
            Self::Delete => "main.menu.delete",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ItemDisplayConfig {
    pub info_visibility: ItemRegionVisibility,
    pub toolbar_visibility: ItemRegionVisibility,
    pub actions_visibility: ItemRegionVisibility,
    pub fields: Vec<ItemDisplayField>,
}

impl Default for ItemDisplayConfig {
    fn default() -> Self {
        Self {
            info_visibility: ItemRegionVisibility::Always,
            toolbar_visibility: ItemRegionVisibility::Always,
            actions_visibility: ItemRegionVisibility::Hover,
            fields: ItemDisplayField::default_fields(),
        }
    }
}

impl ItemDisplayConfig {
    pub fn enabled(&self, field: ItemDisplayField) -> bool {
        self.fields.contains(&field)
    }

    pub fn set_enabled(&mut self, field: ItemDisplayField, enabled: bool) {
        self.fields.retain(|value| *value != field);
        if enabled {
            self.fields.push(field);
        }
    }

    pub fn shows_info_row(&self) -> bool {
        self.info_visibility != ItemRegionVisibility::Hidden
            && (self.enabled(ItemDisplayField::Time)
                || self.enabled(ItemDisplayField::Length)
                || self.enabled(ItemDisplayField::ImageSize))
    }

    pub fn preset(&self) -> Option<ItemDisplayPreset> {
        ItemDisplayPreset::ALL.into_iter().find(|preset| {
            let other = preset.config();
            self.info_visibility == other.info_visibility
                && self.toolbar_visibility == other.toolbar_visibility
                && self.actions_visibility == other.actions_visibility
                && ItemDisplayField::ALL
                    .into_iter()
                    .all(|field| self.enabled(field) == other.enabled(field))
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemDisplayPreset {
    Standard,
    Pure,
    Compact,
    Full,
}

impl ItemDisplayPreset {
    pub const ALL: [Self; 4] = [Self::Standard, Self::Pure, Self::Compact, Self::Full];

    pub const fn label_key(self) -> &'static str {
        match self {
            Self::Standard => "settings.item_display.standard",
            Self::Pure => "settings.item_display.pure",
            Self::Compact => "settings.item_display.compact",
            Self::Full => "settings.item_display.full",
        }
    }

    pub fn config(self) -> ItemDisplayConfig {
        use ItemDisplayField::*;
        use ItemRegionVisibility::*;
        match self {
            Self::Standard => ItemDisplayConfig::default(),
            Self::Pure => ItemDisplayConfig {
                info_visibility: Hidden,
                toolbar_visibility: Hidden,
                actions_visibility: Hidden,
                fields: ItemDisplayField::default_fields(),
            },
            Self::Compact => ItemDisplayConfig {
                info_visibility: Hidden,
                toolbar_visibility: Always,
                actions_visibility: Hover,
                fields: vec![Source, Shortcut, Favorite, Pin, Delete],
            },
            Self::Full => ItemDisplayConfig {
                actions_visibility: Always,
                ..ItemDisplayConfig::default()
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_and_custom_fields_are_distinguished() {
        for preset in ItemDisplayPreset::ALL {
            let mut config = preset.config();
            config.fields.reverse();
            assert_eq!(config.preset(), Some(preset));
        }
        let mut config = ItemDisplayPreset::Full.config();
        config.set_enabled(ItemDisplayField::Time, false);
        assert_eq!(config.preset(), None);
        config.set_enabled(ItemDisplayField::Time, true);
        assert_eq!(config.preset(), Some(ItemDisplayPreset::Full));
        config.actions_visibility = ItemRegionVisibility::Hover;
        assert_eq!(config.preset(), Some(ItemDisplayPreset::Standard));
    }

    #[test]
    fn sticky_actions_are_hidden_by_default_and_can_be_enabled() {
        let mut config = ItemDisplayConfig::default();
        assert!(!config.enabled(ItemDisplayField::PinToScreen));
        assert!(!config.enabled(ItemDisplayField::SplitWords));
        config.set_enabled(ItemDisplayField::SplitWords, true);
        assert!(config.enabled(ItemDisplayField::SplitWords));
        assert_eq!(config.preset(), None);
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("split-words"), "{json}");
    }

    #[test]
    fn identity_and_actions_have_independent_visibility() {
        let default = ItemDisplayConfig::default();
        assert!(default.toolbar_visibility.is_visible(false));
        assert!(!default.actions_visibility.is_visible(false));
        assert!(default.actions_visibility.is_visible(true));
        let modes = [
            (ItemRegionVisibility::Always, [true, true]),
            (ItemRegionVisibility::Hover, [false, true]),
            (ItemRegionVisibility::Hidden, [false, false]),
        ];
        for (identity, expected_identity) in modes {
            for (actions, expected_actions) in modes {
                let config = ItemDisplayConfig {
                    toolbar_visibility: identity,
                    actions_visibility: actions,
                    ..ItemDisplayConfig::default()
                };
                for (index, hovered) in [false, true].into_iter().enumerate() {
                    assert_eq!(
                        config.toolbar_visibility.is_visible(hovered),
                        expected_identity[index]
                    );
                    assert_eq!(
                        config.actions_visibility.is_visible(hovered),
                        expected_actions[index]
                    );
                }
            }
        }
        let pure = ItemDisplayPreset::Pure.config();
        assert!(!pure.toolbar_visibility.is_visible(true));
        assert!(!pure.actions_visibility.is_visible(true));
    }

    #[test]
    fn hidden_or_empty_information_does_not_reserve_a_row() {
        assert!(!ItemDisplayPreset::Pure.config().shows_info_row());
        let mut config = ItemDisplayConfig::default();
        config.set_enabled(ItemDisplayField::Time, false);
        config.set_enabled(ItemDisplayField::Length, false);
        assert!(config.shows_info_row());
        config.set_enabled(ItemDisplayField::ImageSize, false);
        assert!(!config.shows_info_row());
        config.set_enabled(ItemDisplayField::ImageSize, true);
        assert!(config.shows_info_row());
        config.info_visibility = ItemRegionVisibility::Hidden;
        assert!(!config.shows_info_row());
        config.set_enabled(ItemDisplayField::Time, true);
        config.info_visibility = ItemRegionVisibility::Hover;
        assert!(config.shows_info_row());
    }

    #[test]
    fn hover_visibility_accepts_legacy_active_and_saves_as_hover() {
        for value in ["active", "hover"] {
            let json = serde_json::json!({
                "appearance": {
                    "itemDisplay": {
                        "infoVisibility": value,
                        "toolbarVisibility": value,
                        "actionsVisibility": value,
                    }
                }
            });
            let loaded = crate::config::load::from_str(&json.to_string()).unwrap();
            assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
            let display = &loaded.config.appearance.item_display;
            assert_eq!(display.info_visibility, ItemRegionVisibility::Hover);
            assert_eq!(display.toolbar_visibility, ItemRegionVisibility::Hover);
            assert_eq!(display.actions_visibility, ItemRegionVisibility::Hover);

            let saved = serde_json::to_value(&loaded.config).unwrap();
            for field in ["infoVisibility", "toolbarVisibility", "actionsVisibility"] {
                assert_eq!(saved["appearance"]["itemDisplay"][field], "hover");
            }
            let reloaded = crate::config::load::from_str(&saved.to_string()).unwrap();
            assert!(reloaded.warnings.is_empty());
            assert_eq!(reloaded.config.appearance.item_display, *display);
        }
    }

    #[test]
    fn loading_is_field_tolerant_and_presets_round_trip() {
        let old = crate::config::load::from_str("{}").unwrap();
        assert_eq!(
            old.config.appearance.item_display,
            ItemDisplayConfig::default()
        );
        let loaded = crate::config::load::from_str(r#"{"appearance":{"itemDisplay":{
            "infoVisibility":"unknown","toolbarVisibility":"hidden","actionsVisibility":"always","fields":[]
        }}}"#).unwrap();
        assert_eq!(
            loaded.config.appearance.item_display.info_visibility,
            ItemRegionVisibility::Always
        );
        assert_eq!(
            loaded.config.appearance.item_display.toolbar_visibility,
            ItemRegionVisibility::Hidden
        );
        assert_eq!(
            loaded.config.appearance.item_display.actions_visibility,
            ItemRegionVisibility::Always
        );
        assert!(loaded.config.appearance.item_display.fields.is_empty());
        assert_eq!(loaded.warnings.len(), 1);
        let invalid_actions = crate::config::load::from_str(
            r#"{"appearance":{"itemDisplay":{
            "toolbarVisibility":"hidden","actionsVisibility":"unknown"
        }}}"#,
        )
        .unwrap();
        assert_eq!(
            invalid_actions
                .config
                .appearance
                .item_display
                .toolbar_visibility,
            ItemRegionVisibility::Hidden
        );
        assert_eq!(
            invalid_actions
                .config
                .appearance
                .item_display
                .actions_visibility,
            ItemRegionVisibility::Hover
        );
        assert_eq!(invalid_actions.warnings.len(), 1);
        for preset in ItemDisplayPreset::ALL {
            let mut config = crate::config::AppConfig::default();
            config.appearance.item_display = preset.config();
            let json = serde_json::to_string(&config).unwrap();
            let loaded = crate::config::load::from_str(&json).unwrap();
            assert!(loaded.warnings.is_empty());
            assert_eq!(loaded.config.appearance.item_display, preset.config());
        }
    }
}
