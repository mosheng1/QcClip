use super::*;
use crate::config::{ClipboardListLayout, ClipboardWindowPosition};
use crate::state_runtime::PersistedMainWindowState;

#[cfg(test)]
mod tests;

const CENTER_SNAP_THRESHOLD: f32 = 0.01;
const CENTER_RELEASE_THRESHOLD: f32 = 0.015;

#[derive(Clone, Copy)]
pub(crate) struct SplitResize {
    layout: ClipboardListLayout,
    position: ClipboardWindowPosition,
    start: Pixels,
    start_ratio: f32,
    ratio: f32,
    available: Pixels,
}

impl MainWindow {
    pub(crate) fn split_layout(&self, cx: &App) -> ClipboardListLayout {
        let clipboard = crate::config_runtime::clipboard_config(cx);
        clipboard.effective_list_layout()
    }

    pub(crate) fn sync_split_layout(&mut self, cx: &mut Context<Self>) {
        let layout = self.split_layout(cx);
        let position = crate::config_runtime::clipboard_config(cx).window_position;
        if self.split_resize.is_some_and(|resize| {
            resize.layout != layout
                || resize.position.edge_layout_index() != position.edge_layout_index()
                || self.active_tab == TabId::Emoji
                || MainWindowService::blocking_operation(cx).is_some()
        }) {
            self.finish_split_resize(cx);
        }
        let split = layout != ClipboardListLayout::Single;
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
            self.split_content_bounds = None;
            self.refresh_history(cx);
        }
    }

    pub(crate) fn split_ratio(&self, cx: &App) -> f32 {
        let layout = self.split_layout(cx);
        self.split_resize
            .filter(|resize| resize.layout == layout)
            .map(|resize| resize.ratio)
            .unwrap_or_else(|| {
                state_runtime::state(cx).main_window.restored_list_ratio(
                    layout,
                    crate::config_runtime::clipboard_config(cx).window_position,
                )
            })
    }

    pub(crate) fn begin_split_resize(
        &mut self,
        position: Point<Pixels>,
        divider_size: Pixels,
        cx: &mut Context<Self>,
    ) {
        let layout = self.split_layout(cx);
        if layout == ClipboardListLayout::Single
            || self.split_panes.is_none()
            || self.active_tab == TabId::Emoji
            || MainWindowService::blocking_operation(cx).is_some()
        {
            return;
        }
        let Some(bounds) = self.split_content_bounds else {
            return;
        };
        let (start, available) = if layout == ClipboardListLayout::SideBySide {
            (position.x, bounds.size.width - divider_size)
        } else {
            (position.y, bounds.size.height - divider_size)
        };
        if available <= px(0.0) {
            return;
        }
        let ratio = self.split_ratio(cx);
        self.window_drag_start = None;
        self.cancel_hover_preview(cx);
        self.cancel_split_previews(cx);
        self.split_resize = Some(SplitResize {
            layout,
            position: crate::config_runtime::clipboard_config(cx).window_position,
            start,
            start_ratio: ratio,
            ratio,
            available,
        });
        cx.notify();
    }

    pub(crate) fn update_split_resize(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(resize) = self.split_resize.as_mut() else {
            return;
        };
        let position = if resize.layout == ClipboardListLayout::SideBySide {
            position.x
        } else {
            position.y
        };
        let ratio = (resize.start_ratio
            + f32::from(position - resize.start) / f32::from(resize.available))
        .clamp(
            PersistedMainWindowState::MIN_SPLIT_RATIO,
            PersistedMainWindowState::MAX_SPLIT_RATIO,
        );
        let center = PersistedMainWindowState::DEFAULT_SPLIT_RATIO;
        // 吸附后稍微扩大脱离范围，避免鼠标在阈值附近时分隔条反复跳动。
        let threshold = if resize.ratio == center {
            CENTER_RELEASE_THRESHOLD
        } else {
            CENTER_SNAP_THRESHOLD
        };
        let ratio = if (ratio - center).abs() <= threshold {
            center
        } else {
            ratio
        };
        if resize.ratio != ratio {
            resize.ratio = ratio;
            cx.notify();
        }
    }

    pub(crate) fn finish_split_resize(&mut self, cx: &mut Context<Self>) {
        if let Some(resize) = self.split_resize.take() {
            self.save_split_ratio(resize.layout, resize.position, resize.ratio, cx);
            cx.notify();
        }
    }

    pub(crate) fn reset_split_ratio(&mut self, cx: &mut Context<Self>) {
        let ratio = PersistedMainWindowState::DEFAULT_SPLIT_RATIO;
        if let Some(resize) = self.split_resize.as_mut() {
            resize.start_ratio = ratio;
            resize.ratio = ratio;
        } else {
            self.save_split_ratio(
                self.split_layout(cx),
                crate::config_runtime::clipboard_config(cx).window_position,
                ratio,
                cx,
            );
        }
        cx.notify();
    }

    fn save_split_ratio(
        &self,
        layout: ClipboardListLayout,
        position: ClipboardWindowPosition,
        ratio: f32,
        cx: &mut Context<Self>,
    ) {
        let index = match layout {
            ClipboardListLayout::Single => return,
            ClipboardListLayout::SideBySide => 0,
            ClipboardListLayout::Stacked => 1,
        };
        let mut state = state_runtime::state(cx);
        if let Some(edge) = position.edge_layout_index() {
            state.main_window.edge_split_ratios[edge][index] = ratio;
        } else {
            state.main_window.split_ratios[index] = ratio;
        }
        if let Err(error) = state_runtime::set_state(state, cx) {
            tracing::error!("保存主窗口分栏比例失败：{error}");
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
            || self.split_resize.is_some()
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

    pub(crate) fn list_pointer_interaction_active(&self, cx: &App) -> bool {
        self.split_resize.is_some()
            || self.heat_scrollbar.drag_origin.is_some()
            || self.split_panes.as_ref().is_some_and(|panes| {
                panes
                    .iter()
                    .any(|pane| pane.read(cx).heat_scrollbar.drag_origin.is_some())
            })
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
