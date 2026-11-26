use super::*;
use crate::{
    config::TabBarButton,
    config_runtime,
    windows::main::tabs::{CONTENT_FILTERS, MAIN_TABS, PASTE_FILTERS, SYMBOL_CATEGORIES},
};

impl MainWindow {
    pub(crate) fn cancel_tab_bar_hover(&mut self) {
        self.tab_bar_hover.cancel();
        self.tab_bar_hover_task = None;
        self.tab_bar_hover_hitbox = None;
    }

    pub(crate) fn suppress_tab_bar_hover(&mut self, button: TabBarButton) {
        self.tab_bar_hover.suppress(button);
        self.tab_bar_hover_task = None;
    }

    pub(crate) fn update_tab_bar_hover(
        &mut self,
        button: TabBarButton,
        hitbox: Option<gpui::HitboxId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let clipboard = config_runtime::clipboard_config(cx);
        let hover_delay = Duration::from_millis(u64::from(clipboard.hover_delay_ms(button)));
        let hovered = window.is_window_hovered()
            && !cx.has_active_drag()
            && clipboard.hover_switch_enabled(button)
            && hitbox.is_some_and(|hitbox| hitbox.is_hovered(window));
        if !hovered {
            if self.tab_bar_hover.leave(button) {
                self.tab_bar_hover_task = None;
                self.tab_bar_hover_hitbox = None;
            }
            if let Some(filter) = CONTENT_FILTERS
                .iter()
                .chain(PASTE_FILTERS.iter())
                .find(|filter| filter.id.layout_button() == button)
            {
                self.filters.set_hovered(filter.id, false);
            }
            return;
        }
        self.tab_bar_hover_hitbox = hitbox.map(|hitbox| (button, hitbox));
        if !self.tab_bar_hover.enter(button, Instant::now()) {
            return;
        }
        self.tab_bar_hover_task = Some(cx.spawn_in(window, async move |this, cx| {
            let mut delay = hover_delay;
            loop {
                cx.background_executor().timer(delay).await;
                let next_delay = this
                    .update_in(cx, |this, window, cx| {
                        let clipboard = config_runtime::clipboard_config(cx);
                        let remaining =
                            this.tab_bar_hover
                                .remaining(button, Instant::now(), hover_delay)?;
                        if !window.is_window_hovered()
                            || cx.has_active_drag()
                            || !clipboard.hover_switch_enabled(button)
                            || !config_runtime::appearance_config(cx)
                                .tab_bar
                                .is_visible(button)
                            || !this.tab_bar_hover_hitbox.is_some_and(|(current, hitbox)| {
                                current == button && hitbox.is_hovered(window)
                            })
                        {
                            this.cancel_tab_bar_hover();
                            return None;
                        }
                        if !remaining.is_zero() {
                            return Some(remaining);
                        }
                        this.tab_bar_hover
                            .take_ready(button, Instant::now(), hover_delay);
                        this.tab_bar_hover_task = None;
                        if let Some(tab) = MAIN_TABS
                            .iter()
                            .find(|tab| tab.id.layout_button() == button)
                        {
                            this.select_tab(tab.id, cx);
                        } else if let Some(filter) = CONTENT_FILTERS
                            .iter()
                            .chain(PASTE_FILTERS.iter())
                            .find(|filter| filter.id.layout_button() == button)
                        {
                            if this.filters.set_hovered(filter.id, true) {
                                this.refresh_history(cx);
                            }
                        } else if let Some(category) = SYMBOL_CATEGORIES
                            .iter()
                            .find(|category| category.id.layout_button() == button)
                        {
                            this.select_symbol_category(category.id, cx);
                        }
                        None
                    })
                    .ok()
                    .flatten();
                let Some(remaining) = next_delay else {
                    break;
                };
                delay = remaining;
            }
        }));
    }
}
