use crate::config::{TabBarButton, TabBarButtonGroup, TabBarLayout};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

#[derive(Default)]
pub struct TabBarHover {
    pointer_button: Option<TabBarButton>,
    pending: Option<(TabBarButton, Instant)>,
}

impl TabBarHover {
    pub fn pending_button(&self) -> Option<TabBarButton> {
        self.pending.map(|(button, _)| button)
    }

    pub fn enter(&mut self, button: TabBarButton, now: Instant) -> bool {
        if self.pointer_button == Some(button) {
            return false;
        }
        self.pointer_button = Some(button);
        self.pending = Some((button, now));
        true
    }

    pub fn leave(&mut self, button: TabBarButton) -> bool {
        if self.pointer_button != Some(button) {
            return false;
        }
        self.cancel();
        true
    }

    pub fn cancel(&mut self) {
        self.pointer_button = None;
        self.pending = None;
    }

    pub fn suppress(&mut self, button: TabBarButton) {
        self.pointer_button = Some(button);
        self.pending = None;
    }

    pub fn remaining(
        &self,
        button: TabBarButton,
        now: Instant,
        delay: Duration,
    ) -> Option<Duration> {
        self.pending
            .filter(|(pending, _)| *pending == button)
            .map(|(_, entered)| delay.saturating_sub(now.saturating_duration_since(entered)))
    }

    pub fn take_ready(&mut self, button: TabBarButton, now: Instant, delay: Duration) -> bool {
        if self.remaining(button, now, delay) != Some(Duration::ZERO) {
            return false;
        }
        self.pending = None;
        true
    }
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum TabId {
    Clipboard,
    Favorites,
    Emoji,
}

impl TabId {
    pub const fn layout_button(self) -> TabBarButton {
        match self {
            Self::Clipboard => TabBarButton::Clipboard,
            Self::Favorites => TabBarButton::Favorites,
            Self::Emoji => TabBarButton::Emoji,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TabDefinition {
    pub id: TabId,
    pub label_key: &'static str,
    pub icon: &'static str,
}

pub const MAIN_TABS: &[TabDefinition] = &[
    TabDefinition {
        id: TabId::Clipboard,
        label_key: "main.tab.clipboard",
        icon: "clipboard-text",
    },
    TabDefinition {
        id: TabId::Favorites,
        label_key: "main.tab.favorites",
        icon: "star",
    },
    TabDefinition {
        id: TabId::Emoji,
        label_key: "main.tab.emoji",
        icon: "mood-smile",
    },
];

#[repr(u32)]
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum SymbolCategoryId {
    Emoji,
    Symbols,
    Gallery,
}

impl SymbolCategoryId {
    pub const fn layout_button(self) -> TabBarButton {
        match self {
            Self::Emoji => TabBarButton::SymbolEmoji,
            Self::Symbols => TabBarButton::Symbols,
            Self::Gallery => TabBarButton::Gallery,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SymbolCategoryDefinition {
    pub id: SymbolCategoryId,
    pub label_key: &'static str,
    pub icon: &'static str,
}

pub const SYMBOL_CATEGORIES: &[SymbolCategoryDefinition] = &[
    SymbolCategoryDefinition {
        id: SymbolCategoryId::Emoji,
        label_key: "main.symbol.emoji",
        icon: "😀",
    },
    SymbolCategoryDefinition {
        id: SymbolCategoryId::Symbols,
        label_key: "main.symbol.symbols",
        icon: "math-symbols",
    },
    SymbolCategoryDefinition {
        id: SymbolCategoryId::Gallery,
        label_key: "main.symbol.gallery",
        icon: "photo-star",
    },
];

#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FilterId {
    Text,
    Image,
    File,
    Link,
    Unpasted,
    Pasted,
}

impl FilterId {
    pub const fn layout_button(self) -> TabBarButton {
        match self {
            Self::Text => TabBarButton::Text,
            Self::Image => TabBarButton::Image,
            Self::File => TabBarButton::File,
            Self::Link => TabBarButton::Link,
            Self::Unpasted => TabBarButton::Unpasted,
            Self::Pasted => TabBarButton::Pasted,
        }
    }
}

#[derive(Clone, Default)]
pub struct FilterSelection {
    selected: HashSet<FilterId>,
    hovered: Option<FilterId>,
    pointer_filter: Option<FilterId>,
}

impl FilterSelection {
    pub fn is_selected(&self, filter: FilterId) -> bool {
        self.selected.contains(&filter)
    }

    pub fn is_hovered(&self, filter: FilterId) -> bool {
        self.hovered == Some(filter)
    }

    pub fn toggle(&mut self, filter: FilterId) {
        if !self.selected.insert(filter) {
            self.selected.remove(&filter);
        }
        if self.hovered == Some(filter) {
            self.hovered = None;
        }
    }

    pub fn set_hovered(&mut self, filter: FilterId, hovered: bool) -> bool {
        if !hovered {
            if self.pointer_filter == Some(filter) {
                self.pointer_filter = None;
            }
            return false;
        }
        // 点击取消后，重复的悬停通知必须等鼠标重新移入才允许激活。
        if self.pointer_filter == Some(filter) {
            return false;
        }
        self.pointer_filter = Some(filter);
        self.hovered.replace(filter) != Some(filter)
    }

    pub fn clear_hovered(&mut self) -> bool {
        self.pointer_filter = None;
        self.hovered.take().is_some()
    }

    pub fn retain_visible(&mut self, layout: &TabBarLayout) -> bool {
        let count = self.selected.len();
        self.selected
            .retain(|filter| layout.is_visible(filter.layout_button()));
        self.pointer_filter = self
            .pointer_filter
            .filter(|filter| layout.is_visible(filter.layout_button()));
        let hover_changed = self
            .hovered
            .is_some_and(|filter| !layout.is_visible(filter.layout_button()))
            && self.clear_hovered();
        self.selected.len() != count || hover_changed
    }

    pub fn active_filters(&self) -> impl Iterator<Item = FilterId> + '_ {
        self.selected.iter().copied().chain(
            self.hovered
                .filter(|filter| !self.selected.contains(filter)),
        )
    }
}

pub fn ordered_tabs(layout: &TabBarLayout) -> Vec<&'static TabDefinition> {
    layout
        .buttons_in(TabBarButtonGroup::Pages)
        .filter(|button| layout.is_visible(*button))
        .filter_map(|button| {
            MAIN_TABS
                .iter()
                .find(|tab| tab.id.layout_button() == button)
        })
        .collect()
}

pub fn ordered_filters(layout: &TabBarLayout) -> Vec<&'static FilterDefinition> {
    layout
        .buttons_in(TabBarButtonGroup::Filters)
        .filter(|button| layout.is_visible(*button))
        .filter_map(|button| {
            CONTENT_FILTERS
                .iter()
                .chain(PASTE_FILTERS.iter())
                .find(|filter| filter.id.layout_button() == button)
        })
        .collect()
}

pub fn ordered_symbol_categories(layout: &TabBarLayout) -> Vec<&'static SymbolCategoryDefinition> {
    layout
        .buttons_in(TabBarButtonGroup::Symbols)
        .filter(|button| layout.is_visible(*button))
        .filter_map(|button| {
            SYMBOL_CATEGORIES
                .iter()
                .find(|category| category.id.layout_button() == button)
        })
        .collect()
}

#[derive(Clone, Copy, Debug)]
pub struct FilterDefinition {
    pub id: FilterId,
    pub label_key: &'static str,
    pub icon: &'static str,
}

pub const CONTENT_FILTERS: &[FilterDefinition] = &[
    FilterDefinition {
        id: FilterId::Text,
        label_key: "main.filter.text",
        icon: "file-text",
    },
    FilterDefinition {
        id: FilterId::Image,
        label_key: "main.filter.image",
        icon: "photo",
    },
    FilterDefinition {
        id: FilterId::File,
        label_key: "main.filter.file",
        icon: "folder",
    },
    FilterDefinition {
        id: FilterId::Link,
        label_key: "main.filter.link",
        icon: "link",
    },
];

pub const PASTE_FILTERS: &[FilterDefinition] = &[
    FilterDefinition {
        id: FilterId::Unpasted,
        label_key: "main.filter.unpasted",
        icon: "clipboard-x",
    },
    FilterDefinition {
        id: FilterId::Pasted,
        label_key: "main.filter.pasted",
        icon: "clipboard-check",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_tabs_and_filters_have_stable_unique_ids() {
        let tab_ids: Vec<_> = MAIN_TABS.iter().map(|tab| tab.id).collect();
        let filter_ids: Vec<_> = CONTENT_FILTERS
            .iter()
            .chain(PASTE_FILTERS.iter())
            .map(|filter| filter.id)
            .collect();

        assert_eq!(
            tab_ids,
            vec![TabId::Clipboard, TabId::Favorites, TabId::Emoji]
        );
        assert_eq!(filter_ids.len(), 6);
        for (index, id) in filter_ids.iter().enumerate() {
            assert!(!filter_ids[..index].contains(id));
        }

        let symbol_category_ids: Vec<_> = SYMBOL_CATEGORIES
            .iter()
            .map(|category| category.id)
            .collect();
        assert_eq!(
            symbol_category_ids,
            vec![
                SymbolCategoryId::Emoji,
                SymbolCategoryId::Symbols,
                SymbolCategoryId::Gallery,
            ]
        );
    }
}
