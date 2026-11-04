use serde::{Deserialize, Serialize};

use super::{ClipboardConfig, ClipboardWindowPosition};

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

impl ClipboardWindowPosition {
    /// 左右贴边与上下贴边分别记忆布局和分栏比例。
    pub const fn edge_layout_index(self) -> Option<usize> {
        match self {
            Self::ScreenLeftCenter | Self::ScreenRightCenter => Some(0),
            Self::ScreenTopCenter | Self::ScreenBottomCenter => Some(1),
            _ => None,
        }
    }
}

impl ClipboardConfig {
    pub fn effective_list_layout(&self) -> ClipboardListLayout {
        self.window_position
            .edge_layout_index()
            .map(|index| self.edge_list_layouts[index])
            .unwrap_or(self.list_layout)
    }

    pub fn set_list_layout(&mut self, layout: ClipboardListLayout) {
        if let Some(index) = self.window_position.edge_layout_index() {
            self.edge_list_layouts[index] = layout;
        } else {
            self.list_layout = layout;
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
    fn screen_edges_default_to_layouts_along_the_edge() {
        let mut clipboard = ClipboardConfig::default();
        for position in ClipboardWindowPosition::ALL {
            clipboard.window_position = position;
            let expected = match position.edge_layout_index() {
                Some(0) => ClipboardListLayout::Stacked,
                Some(1) => ClipboardListLayout::SideBySide,
                _ => ClipboardListLayout::Single,
            };
            assert_eq!(clipboard.effective_list_layout(), expected);
        }
    }

    #[test]
    fn normal_and_edge_layout_preferences_are_independent_and_round_trip() {
        let mut clipboard = ClipboardConfig::default();
        clipboard.set_list_layout(ClipboardListLayout::SideBySide);
        clipboard.window_position = ClipboardWindowPosition::ScreenLeftCenter;
        clipboard.set_list_layout(ClipboardListLayout::Single);
        clipboard.window_position = ClipboardWindowPosition::ScreenTopCenter;
        clipboard.set_list_layout(ClipboardListLayout::Stacked);
        let mut config = super::super::AppConfig::default();
        config.clipboard = clipboard;
        let json = serde_json::to_string(&config).unwrap();
        let mut loaded = super::super::load::from_str(&json)
            .unwrap()
            .config
            .clipboard;
        for position in ClipboardWindowPosition::ALL {
            loaded.window_position = position;
            let expected = match position.edge_layout_index() {
                Some(0) => ClipboardListLayout::Single,
                Some(1) => ClipboardListLayout::Stacked,
                _ => ClipboardListLayout::SideBySide,
            };
            assert_eq!(loaded.effective_list_layout(), expected);
        }
        let old = super::super::load::from_str(
            r#"{"clipboard":{"windowPosition":"screen-left-center"}}"#,
        )
        .unwrap();
        assert_eq!(
            old.config.clipboard.effective_list_layout(),
            ClipboardListLayout::Stacked
        );
        let invalid =
            super::super::load::from_str(r#"{"clipboard":{"edgeListLayouts":["invalid"]}}"#)
                .unwrap();
        assert_eq!(
            invalid.config.clipboard.edge_list_layouts,
            ClipboardConfig::default().edge_list_layouts
        );
        assert!(!invalid.warnings.is_empty());
    }
}
