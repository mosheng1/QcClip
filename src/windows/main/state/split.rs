use super::*;
use crate::config::ClipboardListLayout;

#[cfg(test)]
mod tests;

impl MainWindow {
    pub(crate) fn split_layout(&self, cx: &App) -> ClipboardListLayout {
        let clipboard = crate::config_runtime::clipboard_config(cx);
        clipboard.list_layout.effective(clipboard.window_position)
    }

    pub(crate) fn sync_split_layout(&mut self, cx: &mut Context<Self>) {
        let split = self.split_layout(cx) != ClipboardListLayout::Single;
        if split && self.split_panes.is_none() {
            let panes = [TabId::Clipboard, TabId::Favorites].map(|tab| {
                cx.new(|cx| {
                    let mut pane = Self::with_preview_service(
                        self.history_repository.clone(),
                        self.favorite_repository.clone(),
                        self.tag_repository.clone(),
                        self.capture_service.clone(),
                        self.custom_text_service.clone(),
                        self.paste_service.clone(),
                        self.image_preview_service.clone(),
                        self.image_library.clone(),
                        None,
                        self.clipboard_write_suppressor.clone(),
                        cx,
                    );
                    pane.active_tab = tab;
                    pane.is_list_pane = true;
                    pane.pane_active = self.active_tab == tab;
                    pane
                })
            });
            for index in 0..2 {
                panes[index].update(cx, |pane, _| {
                    pane.pane_peer = Some(panes[1 - index].downgrade())
                });
            }
            self.cancel_hover_preview(cx);
            self.list_query_task = None;
            self.sync_refresh_task = None;
            self.sync_changes.clear();
            self.query_generation = self.query_generation.wrapping_add(1);
            self.items.clear();
            self.loaded_items.clear();
            self.list_order_ids.clear();
            self.list_order_positions.clear();
            self.clear_image_cache();
            self.clear_file_list_cache();
            self.split_panes = Some(panes);
            self.refresh_split_panes(cx);
        } else if !split && self.split_panes.is_some() {
            self.cancel_split_previews(cx);
            self.split_panes = None;
            self.refresh_history(cx);
        }
    }

    pub(super) fn refresh_split_panes(&self, cx: &mut Context<Self>) {
        let Some(panes) = self.split_panes.clone() else {
            return;
        };
        for pane in panes {
            pane.update(cx, |pane, cx| {
                pane.search_query = self.search_query.clone();
                pane.filters = self.filters.clone();
                pane.selected_tag_ids = self.selected_tag_ids.clone();
                pane.tags_match_all = self.tags_match_all;
                pane.tags = self.tags.clone();
                pane.refresh_history(cx);
            });
        }
    }

    pub(crate) fn sync_split_filters(&self, cx: &mut Context<Self>) {
        let Some(panes) = self.split_panes.clone() else {
            return;
        };
        for pane in panes {
            pane.update(cx, |pane, cx| {
                if pane.history_query() != self.history_query() {
                    pane.search_query = self.search_query.clone();
                    pane.filters = self.filters.clone();
                    pane.selected_tag_ids = self.selected_tag_ids.clone();
                    pane.tags_match_all = self.tags_match_all;
                    pane.refresh_history(cx);
                }
                if pane.tags != self.tags {
                    pane.tags = self.tags.clone();
                    cx.notify();
                }
            });
        }
    }

    pub(crate) fn activate_split_pane(&mut self, tab: TabId, cx: &mut Context<Self>) {
        if tab == TabId::Emoji
            || self.active_tab == tab
            || MainWindowService::blocking_operation(cx).is_some()
        {
            return;
        }
        self.active_tab = tab;
        if let Some(panes) = self.split_panes.clone() {
            for pane in panes {
                pane.update(cx, |pane, cx| {
                    pane.pane_active = pane.active_tab == tab;
                    if !pane.pane_active {
                        pane.cancel_hover_preview(cx);
                    }
                    cx.notify();
                });
            }
        }
        self.persist_state(cx);
        cx.notify();
    }

    pub(crate) fn active_list_entity(&self, cx: &App) -> Option<Entity<MainWindow>> {
        if self.split_layout(cx) == ClipboardListLayout::Single {
            return None;
        }
        let index = match self.active_tab {
            TabId::Clipboard => 0,
            TabId::Favorites => 1,
            TabId::Emoji => return None,
        };
        self.split_panes.as_ref().map(|panes| panes[index].clone())
    }

    pub(crate) fn cancel_split_previews(&self, cx: &mut Context<Self>) {
        if let Some(panes) = self.split_panes.clone() {
            for pane in panes {
                pane.update(cx, |pane, cx| pane.cancel_hover_preview(cx));
            }
        }
    }

    pub(super) fn refresh_peer_items(&self, ids: &[ClipboardItemId], cx: &mut Context<Self>) {
        if let Some(peer) = self.pane_peer.as_ref().and_then(|peer| peer.upgrade()) {
            peer.update(cx, |peer, cx| {
                for id in ids {
                    peer.refresh_saved_item(*id, cx);
                }
            });
        }
    }
}
