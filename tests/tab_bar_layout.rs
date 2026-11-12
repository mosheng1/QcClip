#![allow(dead_code, unused_imports)]

#[path = "../src/config/mod.rs"]
mod config;
#[path = "../src/windows/main/tabs.rs"]
mod tabs;

use config::{TabBarButton, TabBarLayout};
use std::time::{Duration, Instant};
use tabs::{FilterId, FilterSelection, SymbolCategoryId, TabBarHover, TabId};

const TAB_BAR_HOVER_DELAY: Duration =
    Duration::from_millis(config::ClipboardConfig::TAB_BAR_HOVER_DELAY_DEFAULT_MS as u64);

#[test]
fn quickly_crossing_a_tab_button_does_not_activate_it() {
    let mut hover = TabBarHover::default();
    let start = Instant::now();
    assert!(hover.enter(TabBarButton::Favorites, start));
    assert!(!hover.take_ready(
        TabBarButton::Favorites,
        start + TAB_BAR_HOVER_DELAY / 2,
        TAB_BAR_HOVER_DELAY,
    ));
    assert!(hover.leave(TabBarButton::Favorites));
    assert!(!hover.take_ready(
        TabBarButton::Favorites,
        start + TAB_BAR_HOVER_DELAY,
        TAB_BAR_HOVER_DELAY,
    ));
}

#[test]
fn hover_dwell_delay_can_be_configured() {
    let mut hover = TabBarHover::default();
    let delay = Duration::from_millis(200);
    let start = Instant::now();
    assert!(hover.enter(TabBarButton::Favorites, start));
    assert!(!hover.take_ready(
        TabBarButton::Favorites,
        start + Duration::from_millis(199),
        delay
    ));
    assert!(hover.take_ready(
        TabBarButton::Favorites,
        start + Duration::from_millis(200),
        delay
    ));
}

#[test]
fn all_tab_button_groups_require_a_full_dwell_and_activate_only_once() {
    for button in TabBarButton::ALL {
        let mut hover = TabBarHover::default();
        let start = Instant::now();
        hover.enter(button, start);
        assert!(!hover.take_ready(
            button,
            start + TAB_BAR_HOVER_DELAY - Duration::from_millis(1),
            TAB_BAR_HOVER_DELAY,
        ));
        assert!(!hover.enter(button, start + TAB_BAR_HOVER_DELAY / 2));
        assert!(hover.take_ready(button, start + TAB_BAR_HOVER_DELAY, TAB_BAR_HOVER_DELAY,));
        assert!(!hover.take_ready(button, start + TAB_BAR_HOVER_DELAY, TAB_BAR_HOVER_DELAY,));
        assert!(!hover.enter(button, start + TAB_BAR_HOVER_DELAY * 2));
    }
}

#[test]
fn changing_hover_target_restarts_the_delay_and_ignores_old_leave_events() {
    let mut hover = TabBarHover::default();
    let start = Instant::now();
    hover.enter(TabBarButton::Text, start);
    hover.enter(TabBarButton::Image, start + Duration::from_millis(100));
    assert!(!hover.leave(TabBarButton::Text));
    assert!(!hover.take_ready(
        TabBarButton::Text,
        start + TAB_BAR_HOVER_DELAY,
        TAB_BAR_HOVER_DELAY,
    ));
    assert!(!hover.take_ready(
        TabBarButton::Image,
        start + TAB_BAR_HOVER_DELAY,
        TAB_BAR_HOVER_DELAY,
    ));
    assert!(hover.take_ready(
        TabBarButton::Image,
        start + Duration::from_millis(100) + TAB_BAR_HOVER_DELAY,
        TAB_BAR_HOVER_DELAY,
    ));
}

#[test]
fn leaving_and_reentering_the_same_button_requires_another_full_dwell() {
    let mut hover = TabBarHover::default();
    let start = Instant::now();
    hover.enter(TabBarButton::Favorites, start);
    hover.leave(TabBarButton::Favorites);
    hover.enter(TabBarButton::Favorites, start + Duration::from_millis(100));
    assert!(!hover.take_ready(
        TabBarButton::Favorites,
        start + TAB_BAR_HOVER_DELAY,
        TAB_BAR_HOVER_DELAY,
    ));
    assert!(hover.take_ready(
        TabBarButton::Favorites,
        start + Duration::from_millis(100) + TAB_BAR_HOVER_DELAY,
        TAB_BAR_HOVER_DELAY,
    ));
}

#[test]
fn cancelling_pending_hover_preserves_the_last_active_filter() {
    let mut filters = FilterSelection::default();
    filters.set_hovered(FilterId::Text, true);
    filters.set_hovered(FilterId::Text, false);
    let mut hover = TabBarHover::default();
    let start = Instant::now();
    hover.enter(TabBarButton::Image, start);
    hover.cancel();
    assert!(!hover.take_ready(
        TabBarButton::Image,
        start + TAB_BAR_HOVER_DELAY,
        TAB_BAR_HOVER_DELAY,
    ));
    assert!(filters.is_hovered(FilterId::Text));
    assert_eq!(
        filters.active_filters().collect::<Vec<_>>(),
        [FilterId::Text]
    );
}

#[test]
fn interrupted_hover_can_restart_without_leaving_the_button() {
    let mut hover = TabBarHover::default();
    let start = Instant::now();
    hover.enter(TabBarButton::Image, start);
    hover.cancel();
    let restarted = start + TAB_BAR_HOVER_DELAY;
    assert!(hover.enter(TabBarButton::Image, restarted));
    assert!(hover.take_ready(
        TabBarButton::Image,
        restarted + TAB_BAR_HOVER_DELAY,
        TAB_BAR_HOVER_DELAY,
    ));
}

#[test]
fn early_timer_wakeup_keeps_the_remaining_wait() {
    let mut hover = TabBarHover::default();
    let start = Instant::now();
    hover.enter(TabBarButton::Favorites, start);
    let early = start + TAB_BAR_HOVER_DELAY - Duration::from_millis(1);
    assert!(!hover.take_ready(TabBarButton::Favorites, early, TAB_BAR_HOVER_DELAY,));
    assert_eq!(
        hover.remaining(TabBarButton::Favorites, early, TAB_BAR_HOVER_DELAY),
        Some(Duration::from_millis(1)),
    );
    assert!(hover.take_ready(
        TabBarButton::Favorites,
        start + TAB_BAR_HOVER_DELAY,
        TAB_BAR_HOVER_DELAY,
    ));
    assert_eq!(
        hover.remaining(TabBarButton::Favorites, early, TAB_BAR_HOVER_DELAY),
        None
    );
}

#[test]
fn clicking_during_the_delay_suppresses_hover_until_pointer_leaves() {
    let mut hover = TabBarHover::default();
    let start = Instant::now();
    hover.enter(TabBarButton::Text, start);
    hover.suppress(TabBarButton::Text);
    assert!(!hover.take_ready(
        TabBarButton::Text,
        start + TAB_BAR_HOVER_DELAY,
        TAB_BAR_HOVER_DELAY,
    ));
    assert!(!hover.enter(TabBarButton::Text, start + TAB_BAR_HOVER_DELAY));
    assert!(hover.leave(TabBarButton::Text));
    assert!(hover.enter(TabBarButton::Text, start + TAB_BAR_HOVER_DELAY));
}

#[test]
fn completed_hover_ignores_other_buttons_leave_and_rearms_after_its_own_leave() {
    let mut hover = TabBarHover::default();
    let start = Instant::now();
    hover.enter(TabBarButton::Image, start);
    assert!(hover.take_ready(
        TabBarButton::Image,
        start + TAB_BAR_HOVER_DELAY,
        TAB_BAR_HOVER_DELAY,
    ));
    assert!(!hover.leave(TabBarButton::Text));
    assert!(!hover.enter(TabBarButton::Image, start + TAB_BAR_HOVER_DELAY));
    assert!(hover.leave(TabBarButton::Image));
    assert!(hover.enter(TabBarButton::Image, start + TAB_BAR_HOVER_DELAY));
}

#[test]
fn hover_switches_default_to_disabled_and_can_be_saved_independently() {
    let mut clipboard: config::ClipboardConfig = serde_json::from_str("{}").unwrap();
    assert!(!clipboard.page_hover_switch);
    assert!(!clipboard.filter_hover_switch);
    assert_eq!(clipboard.page_hover_delay_ms, 40);
    assert_eq!(clipboard.filter_hover_delay_ms, 40);
    clipboard.page_hover_switch = true;
    clipboard.filter_hover_switch = true;
    clipboard.page_hover_delay_ms = 300;
    clipboard.filter_hover_delay_ms = 500;
    let encoded = serde_json::to_string(&clipboard).unwrap();
    assert!(encoded.contains("\"pageHoverSwitch\":true"));
    assert!(encoded.contains("\"filterHoverSwitch\":true"));
    assert!(encoded.contains("\"pageHoverDelayMs\":300"));
    assert!(encoded.contains("\"filterHoverDelayMs\":500"));
    let reloaded: config::ClipboardConfig = serde_json::from_str(&encoded).unwrap();
    assert!(reloaded.page_hover_switch);
    assert!(reloaded.filter_hover_switch);
    assert_eq!(reloaded.page_hover_delay_ms, 300);
    assert_eq!(reloaded.filter_hover_delay_ms, 500);
}

#[test]
fn hover_switches_control_their_own_button_groups() {
    let mut clipboard = config::ClipboardConfig::default();
    clipboard.page_hover_switch = true;
    assert!(clipboard.hover_switch_enabled(TabBarButton::Favorites));
    assert!(!clipboard.hover_switch_enabled(TabBarButton::Symbols));
    assert!(!clipboard.hover_switch_enabled(TabBarButton::Tags));
    assert!(!clipboard.hover_switch_enabled(TabBarButton::Image));
    assert!(!clipboard.hover_switch_enabled(TabBarButton::Unpasted));

    clipboard.page_hover_switch = false;
    clipboard.filter_hover_switch = true;
    assert!(!clipboard.hover_switch_enabled(TabBarButton::Favorites));
    assert!(clipboard.hover_switch_enabled(TabBarButton::Symbols));
    assert!(clipboard.hover_switch_enabled(TabBarButton::Tags));
    assert!(clipboard.hover_switch_enabled(TabBarButton::Image));
    assert!(clipboard.hover_switch_enabled(TabBarButton::Unpasted));

    clipboard.page_hover_delay_ms = 120;
    clipboard.filter_hover_delay_ms = 260;
    assert_eq!(clipboard.hover_delay_ms(TabBarButton::Favorites), 120);
    assert_eq!(clipboard.hover_delay_ms(TabBarButton::Symbols), 260);
    assert_eq!(clipboard.hover_delay_ms(TabBarButton::Tags), 260);
    assert_eq!(clipboard.hover_delay_ms(TabBarButton::Image), 260);
}

#[test]
fn filter_selection_mode_defaults_to_multi_select_and_round_trips() {
    let loaded = config::load::from_str(r#"{"schemaVersion":1,"clipboard":{}}"#).unwrap();
    assert!(loaded.config.clipboard.filter_multi_select);
    assert!(loaded.warnings.is_empty());

    for multi_select in [false, true] {
        let mut config = config::AppConfig::default();
        config.clipboard.filter_multi_select = multi_select;
        let encoded = serde_json::to_string(&config).unwrap();
        let reloaded = config::load::from_str(&encoded).unwrap();
        assert_eq!(reloaded.config.clipboard.filter_multi_select, multi_select);
        assert!(reloaded.warnings.is_empty());
    }
}

#[test]
fn single_select_filters_replace_clicked_and_hovered_selections() {
    let mut filters = FilterSelection::default();
    filters.set_multi_select(false);
    filters.toggle(FilterId::Text);
    filters.toggle(FilterId::Image);
    assert_eq!(
        filters.active_filters().collect::<Vec<_>>(),
        [FilterId::Image]
    );
    filters.toggle(FilterId::Image);
    assert_eq!(filters.active_filters().count(), 0);

    filters.toggle(FilterId::Text);
    assert!(filters.set_hovered(FilterId::Image, true));
    assert!(!filters.is_selected(FilterId::Text));
    assert_eq!(
        filters.active_filters().collect::<Vec<_>>(),
        [FilterId::Image]
    );
    assert!(!filters.set_hovered(FilterId::Image, false));
    filters.toggle(FilterId::File);
    assert_eq!(
        filters.active_filters().collect::<Vec<_>>(),
        [FilterId::File]
    );
    filters.set_hovered(FilterId::File, true);
    filters.toggle(FilterId::File);
    assert!(!filters.set_hovered(FilterId::File, true));
    assert_eq!(filters.active_filters().count(), 0);
}

#[test]
fn changing_filter_mode_keeps_the_latest_active_selection() {
    let mut filters = FilterSelection::default();
    filters.toggle(FilterId::Text);
    filters.toggle(FilterId::Image);
    assert!(filters.set_multi_select(false));
    assert_eq!(
        filters.active_filters().collect::<Vec<_>>(),
        [FilterId::Image]
    );
    assert!(!filters.set_multi_select(false));

    assert!(!filters.set_multi_select(true));
    filters.toggle(FilterId::Text);
    filters.set_hovered(FilterId::File, true);
    assert_eq!(filters.active_filters().count(), 3);
    assert!(filters.set_multi_select(false));
    assert_eq!(
        filters.active_filters().collect::<Vec<_>>(),
        [FilterId::File]
    );
}

#[test]
fn hovered_filter_combines_with_clicked_filters_and_stays_active_on_leave() {
    let mut filters = FilterSelection::default();
    filters.toggle(FilterId::Text);
    filters.toggle(FilterId::Unpasted);
    assert!(filters.set_hovered(FilterId::Image, true));
    assert!(filters.is_selected(FilterId::Text));
    assert!(!filters.is_selected(FilterId::Image));
    assert!(filters.is_hovered(FilterId::Image));
    let active = filters
        .active_filters()
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        active,
        [FilterId::Text, FilterId::Unpasted, FilterId::Image].into()
    );
    assert!(!filters.set_hovered(FilterId::Image, false));
    assert!(filters.is_hovered(FilterId::Image));
    assert_eq!(filters.active_filters().count(), 3);
    assert!(filters.is_selected(FilterId::Text));
}

#[test]
fn hovering_another_filter_replaces_only_the_last_hovered_filter() {
    let mut filters = FilterSelection::default();
    filters.toggle(FilterId::Text);
    filters.set_hovered(FilterId::Image, true);
    filters.set_hovered(FilterId::File, true);
    assert!(!filters.set_hovered(FilterId::Image, false));
    assert!(!filters.is_hovered(FilterId::Image));
    assert!(filters.is_hovered(FilterId::File));
    assert_eq!(filters.active_filters().count(), 2);
    assert!(!filters.set_hovered(FilterId::File, false));
    assert!(filters.is_hovered(FilterId::File));
    assert_eq!(filters.active_filters().count(), 2);
    filters.clear_hovered();
    assert_eq!(
        filters.active_filters().collect::<Vec<_>>(),
        [FilterId::Text]
    );
}

#[test]
fn clicking_a_hovered_filter_converts_it_to_a_clicked_selection() {
    let mut filters = FilterSelection::default();
    filters.set_hovered(FilterId::Image, true);
    filters.toggle(FilterId::Image);
    assert!(filters.is_selected(FilterId::Image));
    assert!(!filters.is_hovered(FilterId::Image));
    assert_eq!(filters.active_filters().count(), 1);
    filters.toggle(FilterId::Image);
    assert!(!filters.is_selected(FilterId::Image));
    assert!(!filters.is_hovered(FilterId::Image));
    filters.set_hovered(FilterId::Image, false);
    assert_eq!(filters.active_filters().count(), 0);
}

#[test]
fn clicking_to_cancel_a_filter_does_not_reactivate_it_until_pointer_reenters() {
    let mut filters = FilterSelection::default();
    filters.toggle(FilterId::Text);
    filters.set_hovered(FilterId::Text, true);
    filters.toggle(FilterId::Text);
    assert!(!filters.is_selected(FilterId::Text));
    assert!(!filters.is_hovered(FilterId::Text));
    assert!(!filters.set_hovered(FilterId::Text, true));
    assert_eq!(filters.active_filters().count(), 0);

    assert!(!filters.set_hovered(FilterId::Text, false));
    assert!(filters.set_hovered(FilterId::Text, true));
    assert!(filters.is_hovered(FilterId::Text));
    assert!(!filters.is_selected(FilterId::Text));
}

#[test]
fn changing_clicked_filters_preserves_a_different_latched_hover_filter() {
    let mut filters = FilterSelection::default();
    filters.set_hovered(FilterId::Image, true);
    filters.set_hovered(FilterId::Image, false);
    filters.toggle(FilterId::Text);
    assert!(filters.is_hovered(FilterId::Image));
    assert!(filters.is_selected(FilterId::Text));
    assert_eq!(filters.active_filters().count(), 2);
    filters.toggle(FilterId::Text);
    assert_eq!(
        filters.active_filters().collect::<Vec<_>>(),
        [FilterId::Image]
    );
}

#[test]
fn hover_cleanup_and_hidden_buttons_preserve_visible_clicked_filters() {
    let mut filters = FilterSelection::default();
    filters.toggle(FilterId::Text);
    filters.toggle(FilterId::Image);
    filters.set_hovered(FilterId::File, true);
    assert!(filters.clear_hovered());
    assert!(!filters.clear_hovered());
    assert_eq!(filters.active_filters().count(), 2);

    let mut layout = TabBarLayout::default();
    layout.toggle(TabBarButton::Image);
    layout.toggle(TabBarButton::File);
    filters.set_hovered(FilterId::File, true);
    assert!(filters.retain_visible(&layout));
    assert_eq!(
        filters.active_filters().collect::<Vec<_>>(),
        [FilterId::Text]
    );
    assert!(!filters.retain_visible(&layout));
}

#[test]
fn main_window_uses_the_configured_order_and_visibility_for_each_group() {
    let mut layout = TabBarLayout::default();
    layout.move_button(TabBarButton::Emoji, TabBarButton::Clipboard);
    layout.move_button(TabBarButton::Pasted, TabBarButton::Text);
    layout.move_button(TabBarButton::Gallery, TabBarButton::SymbolEmoji);
    layout.toggle(TabBarButton::Favorites);
    layout.toggle(TabBarButton::Image);
    layout.toggle(TabBarButton::Symbols);
    assert_eq!(
        tabs::ordered_tabs(&layout)
            .iter()
            .map(|tab| tab.id)
            .collect::<Vec<_>>(),
        vec![TabId::Emoji, TabId::Clipboard]
    );
    assert_eq!(
        tabs::ordered_filters(&layout)
            .iter()
            .map(|filter| filter.id)
            .collect::<Vec<_>>(),
        vec![
            FilterId::Pasted,
            FilterId::Text,
            FilterId::File,
            FilterId::Link,
            FilterId::Unpasted
        ]
    );
    assert_eq!(
        tabs::ordered_symbol_categories(&layout)
            .iter()
            .map(|category| category.id)
            .collect::<Vec<_>>(),
        vec![SymbolCategoryId::Gallery, SymbolCategoryId::Emoji]
    );
}

#[test]
fn fully_hidden_groups_have_no_main_window_buttons() {
    let mut layout = TabBarLayout::default();
    for button in TabBarButton::ALL {
        layout.toggle(button);
    }
    assert!(tabs::ordered_tabs(&layout).is_empty());
    assert!(tabs::ordered_filters(&layout).is_empty());
    assert!(tabs::ordered_symbol_categories(&layout).is_empty());
}

#[test]
fn disabling_all_symbol_categories_also_disables_the_page_entry() {
    let mut layout = TabBarLayout::default();
    for button in [
        TabBarButton::SymbolEmoji,
        TabBarButton::Symbols,
        TabBarButton::Gallery,
    ] {
        layout.toggle(button);
    }
    assert!(!layout.is_visible(TabBarButton::Emoji));
    assert!(
        !tabs::ordered_tabs(&layout)
            .iter()
            .any(|tab| tab.id == TabId::Emoji)
    );

    layout.toggle(TabBarButton::Gallery);
    assert!(layout.is_visible(TabBarButton::Emoji));
    assert!(
        tabs::ordered_tabs(&layout)
            .iter()
            .any(|tab| tab.id == TabId::Emoji)
    );
}

#[test]
fn manually_hidden_page_stays_hidden_when_a_category_is_enabled() {
    let mut layout = TabBarLayout::default();
    layout.toggle(TabBarButton::Emoji);
    for button in [
        TabBarButton::SymbolEmoji,
        TabBarButton::Symbols,
        TabBarButton::Gallery,
    ] {
        layout.toggle(button);
    }
    layout.toggle(TabBarButton::Gallery);
    assert!(!layout.is_visible(TabBarButton::Emoji));
    layout.toggle(TabBarButton::Emoji);
    assert!(layout.is_visible(TabBarButton::Emoji));
}

#[test]
fn enabling_an_empty_symbol_page_restores_a_category_in_the_custom_order() {
    let mut layout = TabBarLayout::default();
    layout.move_button(TabBarButton::Gallery, TabBarButton::SymbolEmoji);
    for button in [
        TabBarButton::SymbolEmoji,
        TabBarButton::Symbols,
        TabBarButton::Gallery,
    ] {
        layout.toggle(button);
    }
    layout.toggle(TabBarButton::Emoji);
    assert!(layout.is_visible(TabBarButton::Emoji));
    assert_eq!(
        tabs::ordered_symbol_categories(&layout)
            .iter()
            .map(|category| category.id)
            .collect::<Vec<_>>(),
        vec![SymbolCategoryId::Gallery]
    );
}

#[test]
fn editor_and_main_window_use_the_same_icons_and_labels() {
    for tab in tabs::MAIN_TABS {
        assert_eq!(tab.id.layout_button().icon(), tab.icon);
        assert_eq!(tab.id.layout_button().label_key(), tab.label_key);
    }
    for filter in tabs::CONTENT_FILTERS.iter().chain(tabs::PASTE_FILTERS) {
        assert_eq!(filter.id.layout_button().icon(), filter.icon);
        assert_eq!(filter.id.layout_button().label_key(), filter.label_key);
    }
    for category in tabs::SYMBOL_CATEGORIES {
        assert_eq!(category.id.layout_button().label_key(), category.label_key);
        if category.id != SymbolCategoryId::Emoji {
            assert_eq!(category.id.layout_button().icon(), category.icon);
        }
    }
}
