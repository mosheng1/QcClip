use serde::{Deserialize, Serialize};

use super::ClipboardWindowPosition;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClipboardListLayout {
    #[default]
    Single,
    SideBySide,
    Stacked,
}

impl ClipboardListLayout {
    pub const ALL: [Self; 3] = [Self::Single, Self::SideBySide, Self::Stacked];

    pub const fn effective(self, position: ClipboardWindowPosition) -> Self {
        if position.is_screen_edge() {
            Self::Single
        } else {
            self
        }
    }

    pub const fn code(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::SideBySide => "side-by-side",
            Self::Stacked => "stacked",
        }
    }

    pub const fn label_key(self) -> &'static str {
        match self {
            Self::Single => "settings.clipboard.list_layout.single",
            Self::SideBySide => "settings.clipboard.list_layout.side_by_side",
            Self::Stacked => "settings.clipboard.list_layout.stacked",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn published_configs_default_to_single_and_split_preferences_round_trip() {
        let old =
            super::super::load::from_str(r#"{"clipboard":{"windowPosition":"remember"}}"#).unwrap();
        assert_eq!(
            old.config.clipboard.list_layout,
            ClipboardListLayout::Single
        );
        for layout in ClipboardListLayout::ALL {
            let json = format!(r#"{{"clipboard":{{"listLayout":"{}"}}}}"#, layout.code());
            let loaded = super::super::load::from_str(&json).unwrap();
            assert_eq!(loaded.config.clipboard.list_layout, layout);
            let saved = serde_json::to_value(&loaded.config).unwrap();
            assert_eq!(saved["clipboard"]["listLayout"], layout.code());
        }
        let invalid =
            super::super::load::from_str(r#"{"clipboard":{"listLayout":"invalid"}}"#).unwrap();
        assert_eq!(
            invalid.config.clipboard.list_layout,
            ClipboardListLayout::Single
        );
        assert!(!invalid.warnings.is_empty());
    }

    #[test]
    fn screen_edges_suspend_split_layout_without_changing_preference() {
        for layout in ClipboardListLayout::ALL {
            for position in ClipboardWindowPosition::ALL {
                assert_eq!(
                    layout.effective(position),
                    if position.is_screen_edge() {
                        ClipboardListLayout::Single
                    } else {
                        layout
                    }
                );
            }
        }
    }
}
