use super::*;

fn extend_multi_selection_range(
    selected_ids: &mut Vec<HistoryItemId>,
    order_ids: &[HistoryItemId],
    anchor_index: usize,
    target_index: usize,
    keep_existing: bool,
) {
    if !keep_existing {
        selected_ids.clear();
    }
    for offset in 0..=anchor_index.abs_diff(target_index) {
        let index = if anchor_index <= target_index {
            anchor_index + offset
        } else {
            anchor_index - offset
        };
        let Some(id) = order_ids.get(index).copied() else {
            continue;
        };
        if !selected_ids.contains(&id) {
            selected_ids.push(id);
        }
    }
}

impl MainWindow {
    pub fn select_item(&mut self, history_id: HistoryItemId) {
        self.keyboard_selection_mouse_move_sequence = None;
        if self.items.iter().any(|item| item.history_id == history_id) {
            self.selected_item_id = Some(history_id);
        }
        self.reconcile_selection();
    }

    pub(crate) fn select_list_item_with_modifiers(
        &mut self,
        history_id: HistoryItemId,
        modifiers: Modifiers,
    ) {
        self.keyboard_selection_mouse_move_sequence = None;
        if !self.list_order_positions.contains_key(&history_id) {
            return;
        }

        if modifiers.shift {
            let anchor = self
                .multi_selection_anchor
                .or(self.selected_item_id)
                .unwrap_or(history_id);
            let Some(anchor_index) = self.list_order_positions.get(&anchor).copied() else {
                self.multi_selected_item_ids.clear();
                self.multi_selected_item_ids.push(history_id);
                self.multi_selection_anchor = Some(history_id);
                self.selected_item_id = Some(history_id);
                self.reconcile_selection();
                return;
            };
            let target_index = self
                .list_order_positions
                .get(&history_id)
                .copied()
                .unwrap_or(anchor_index);
            extend_multi_selection_range(
                &mut self.multi_selected_item_ids,
                &self.list_order_ids,
                anchor_index,
                target_index,
                modifiers.control,
            );
            self.multi_selection_anchor = Some(anchor);
        } else if modifiers.control {
            if let Some(index) = self
                .multi_selected_item_ids
                .iter()
                .position(|id| *id == history_id)
            {
                self.multi_selected_item_ids.remove(index);
            } else {
                self.multi_selected_item_ids.push(history_id);
            }
            self.multi_selection_anchor =
                (!self.multi_selected_item_ids.is_empty()).then_some(history_id);
        } else {
            if let Some(index) = self
                .multi_selected_item_ids
                .iter()
                .position(|id| *id == history_id)
            {
                self.multi_selected_item_ids.remove(index);
            } else {
                self.multi_selected_item_ids.push(history_id);
            }
            self.multi_selection_anchor = if self.multi_selected_item_ids.is_empty() {
                None
            } else if self
                .multi_selection_anchor
                .is_some_and(|anchor| self.multi_selected_item_ids.contains(&anchor))
            {
                self.multi_selection_anchor
            } else {
                Some(history_id)
            };
        }

        self.selected_item_id = if self.multi_selected_item_ids.is_empty() {
            None
        } else {
            Some(history_id)
        };
        self.reconcile_selection();
    }

    pub(crate) fn clear_multi_selection(&mut self) {
        self.multi_selected_item_ids.clear();
        self.multi_selection_anchor = None;
        self.selected_item_id = None;
        self.keyboard_selection_mouse_move_sequence = None;
        self.reconcile_selection();
    }

    pub(crate) fn can_follow_mouse_hover(&mut self) -> bool {
        if keyboard_hover_is_suppressed(
            self.keyboard_selection_mouse_move_sequence,
            crate::platform::input::pointer_snapshot().map(|snapshot| snapshot.mouse_move_sequence),
        ) {
            return false;
        }
        if self.keyboard_selection_mouse_move_sequence.is_some() {
            self.keyboard_selection_mouse_move_sequence = None;
        }
        true
    }

    pub(crate) fn move_selection(
        &mut self,
        direction: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_tab == TabId::Emoji || self.list_order_ids.is_empty() {
            return;
        }
        let current_index = self
            .selected_item_id
            .and_then(|selected| self.list_order_positions.get(&selected).copied());
        let next_index = match current_index {
            Some(index) => index
                .saturating_add_signed(direction)
                .min(self.list_order_ids.len().saturating_sub(1)),
            None if direction < 0 => 0,
            None => 0,
        };
        let Some(history_id) = self.list_order_ids.get(next_index).copied() else {
            return;
        };
        self.select_list_item_at(next_index, history_id, window, cx);
    }

    pub(crate) fn select_list_boundary(
        &mut self,
        last: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_tab == TabId::Emoji || self.list_order_ids.is_empty() {
            return;
        }
        let index = if last {
            self.list_order_ids.len() - 1
        } else {
            0
        };
        let Some(history_id) = self.list_order_ids.get(index).copied() else {
            return;
        };
        self.select_list_item_at(index, history_id, window, cx);
    }

    pub(super) fn select_list_item_at(
        &mut self,
        index: usize,
        history_id: HistoryItemId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.keyboard_selection_mouse_move_sequence =
            crate::platform::input::pointer_snapshot().map(|snapshot| snapshot.mouse_move_sequence);
        self.selected_item_id = Some(history_id);
        self.reconcile_selection();
        self.list_scroll_handle
            .scroll_to_item(index, gpui::ScrollStrategy::Nearest);
        if self.loaded_items.contains_key(&history_id) {
            self.pending_selection_index = None;
        } else {
            self.pending_selection_index = Some(index);
            if !self.is_loading_more {
                self.load_page(cx, index, 1);
            }
        }
        window.refresh();
    }

    pub(crate) fn move_tab(&mut self, direction: isize, cx: &mut Context<Self>) {
        if self.split_panes.is_some() && self.active_tab != TabId::Emoji {
            self.keyboard_selection_mouse_move_sequence =
                crate::platform::input::pointer_snapshot()
                    .map(|snapshot| snapshot.mouse_move_sequence);
            self.activate_split_pane(
                if self.active_tab == TabId::Clipboard {
                    TabId::Favorites
                } else {
                    TabId::Clipboard
                },
                cx,
            );
            return;
        }
        let layout = crate::config_runtime::appearance_config(cx).tab_bar;
        let tabs = super::super::tabs::ordered_tabs(&layout);
        if tabs.is_empty() {
            return;
        }
        let index = tabs
            .iter()
            .position(|tab| tab.id == self.active_tab)
            .unwrap_or(0);
        let count = tabs.len();
        let next_index = (index as isize + direction).rem_euclid(count as isize) as usize;
        self.select_tab(tabs[next_index].id, cx);
    }

    pub(crate) fn paste_selected_item(&mut self, cx: &mut Context<Self>) {
        let Some(item) = self.selected_list_item() else {
            return;
        };
        self.paste_item(item.item_id, cx);
    }

    pub(crate) fn toggle_selected_favorite(&mut self, cx: &mut Context<Self>) {
        let Some(item) = self.selected_list_item() else {
            return;
        };
        if self.active_tab == TabId::Favorites {
            if let Some(favorite_id) = item.favorite_id {
                self.remove_from_favorites(favorite_id, item.history_id, cx);
            }
        } else if let Some(favorite_id) = item.favorite_id {
            self.remove_from_favorites(favorite_id, item.history_id, cx);
        } else {
            self.add_to_favorites(item.item_id, cx);
        }
    }

    pub(crate) fn favorite_removal_target_for_shortcut(
        &self,
        shortcut: crate::config::SessionShortcutId,
    ) -> Option<(i64, HistoryItemId)> {
        if !shortcut_uses_favorite_removal(shortcut, self.active_tab) {
            return None;
        }
        let item = self.selected_list_item()?;
        Some((item.favorite_id?, item.history_id))
    }

    pub(crate) fn toggle_selected_pinned(&mut self, cx: &mut Context<Self>) {
        let Some(item) = self.selected_list_item() else {
            return;
        };
        self.set_item_pinned(item.history_id, item.favorite_id, !item.is_pinned, cx);
    }

    /// 把选中条目贴到屏幕；`split` 为 true 时直接进入拆词，只对含纯文本的条目生效。
    pub(crate) fn pin_selected_to_screen(&mut self, split: bool, cx: &mut Context<Self>) {
        let Some(item) = self.selected_list_item() else {
            return;
        };
        if !split {
            crate::windows::sticky::StickyService::open(item.item_id, cx);
            return;
        }
        let has_text = item.all_format_options.iter().any(|preview| {
            preview.is_restorable && preview.role() == crate::domain::capture::FormatRole::PlainText
        });
        if has_text {
            crate::windows::sticky::StickyService::open_split(item.item_id, cx);
        }
    }

    pub(crate) fn delete_selected_item(&mut self, cx: &mut Context<Self>) {
        if self.active_tab == TabId::Favorites {
            return;
        }
        if let Some(item) = self.selected_list_item() {
            self.delete_item(item.history_id, cx);
        }
    }

    pub(super) fn selected_list_item(&self) -> Option<ClipboardListItem> {
        let selected = self.selected_item_id?;
        self.loaded_items
            .get(&selected)
            .or_else(|| self.items.iter().find(|item| item.history_id == selected))
            .cloned()
    }

    pub(crate) fn begin_hover_preview(&mut self, source: PreviewSource, cx: &mut Context<Self>) {
        if !crate::config_runtime::clipboard_config(cx).hover_preview_enabled {
            self.cancel_hover_preview(cx);
            return;
        }
        if !hover_preview_allowed(
            MenuService::has_open_menu(cx),
            MainWindowService::is_logically_visible(cx),
        ) {
            return;
        }
        if self.preview_hover_source == Some(source) {
            return;
        }
        self.preview_hover_source = Some(source);
        self.preview_hover_generation = self.preview_hover_generation.wrapping_add(1);
        PreviewWindowService::hide(cx);
    }

    pub(crate) fn show_hover_preview(
        &mut self,
        item: ClipboardListItem,
        source: PreviewSource,
        anchor: PreviewAnchor,
        main_window: Option<PreviewRect>,
        display_priority: ClipboardDisplayPriority,
        cx: &mut Context<Self>,
    ) {
        if !crate::config_runtime::clipboard_config(cx).hover_preview_enabled {
            self.cancel_hover_preview(cx);
            return;
        }
        if !hover_preview_allowed(
            MenuService::has_open_menu(cx),
            MainWindowService::is_logically_visible(cx),
        ) {
            return;
        }
        self.begin_hover_preview(source, cx);
        self.preview_hover_generation = self.preview_hover_generation.wrapping_add(1);
        let hover_generation = self.preview_hover_generation;
        let query_generation = self.query_generation;
        let active_tab = self.active_tab;
        let history_id = item.history_id;
        let favorite_id = item.favorite_id;
        let policy = crate::config_runtime::history_config(cx);
        let fallback_item = item;
        let history_repository = self.history_repository.clone();
        let favorite_repository = self.favorite_repository.clone();
        let mut query = HistoryQuery::default();
        query.load_image_sources = true;
        query.load_html_content = true;
        let task = cx.background_spawn(async move {
            let result = if active_tab == TabId::Favorites {
                favorite_id
                    .map(|favorite_id| favorite_repository.find_by_id(favorite_id, &query))
                    .unwrap_or(Ok(None))
            } else {
                history_repository.find_by_id_scoped(history_id, &query, &policy)
            };
            result.map(|record| record.map(record_to_list_item))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| {
                    if this.preview_hover_generation != hover_generation
                        || this.query_generation != query_generation
                        || this.active_tab != active_tab
                        || !hover_preview_allowed(
                            MenuService::has_open_menu(cx),
                            MainWindowService::is_logically_visible(cx),
                        )
                    {
                        return;
                    }
                    let item = match result {
                        Ok(Some(item)) => item,
                        _ => fallback_item,
                    };
                    PreviewWindowService::show(
                        item,
                        source,
                        anchor,
                        main_window,
                        display_priority,
                        cx,
                    );
                });
            }
        })
        .detach();
    }

    pub(crate) fn cancel_hover_preview(&mut self, cx: &mut Context<Self>) {
        self.preview_hover_source = None;
        self.preview_hover_generation = self.preview_hover_generation.wrapping_add(1);
        PreviewWindowService::hide(cx);
    }

    pub(crate) fn hide_hover_preview(&mut self, source: PreviewSource, cx: &mut Context<Self>) {
        if self.preview_hover_source != Some(source) {
            return;
        }
        self.preview_hover_source = None;
        self.preview_hover_generation = self.preview_hover_generation.wrapping_add(1);
        PreviewWindowService::hide(cx);
    }

    pub(crate) fn can_reorder_items(&self) -> bool {
        self.active_tab != TabId::Emoji
    }

    pub(crate) fn clear_list_drop_target(&mut self) {
        self.list_drop_target = None;
        self.list_hovered_item_id = None;
        self.list_reorder_animation = None;
    }

    pub(crate) fn clear_list_drag_hover(&mut self, item_id: HistoryItemId) {
        if self.list_hovered_item_id == Some(item_id) {
            self.list_hovered_item_id = None;
        }
    }

    pub(crate) fn is_list_drag_source(&self, history_id: HistoryItemId) -> bool {
        self.list_drag_source_id == Some(history_id)
    }

    pub(crate) fn finish_list_drag_preview(&mut self, cx: &mut Context<Self>) {
        let entity = cx.entity().clone();
        cx.defer(move |cx| {
            let _ = entity.update(cx, |this, cx| {
                if this.list_reorder_commit_pending {
                    return;
                }
                if this.cancel_reorder_preview() {
                    cx.notify();
                }
            });
        });
    }

    pub(crate) fn begin_reorder_drag(&mut self, source_id: HistoryItemId) {
        if !self.can_reorder_items() {
            return;
        }
        if self.list_drag_source_id.is_some() {
            return;
        }
        let Some(source_position) = self.list_order_positions.get(&source_id).copied() else {
            return;
        };
        self.list_drag_source_id = Some(source_id);
        self.list_drag_origin_position = Some(source_position);
        self.list_hovered_item_id = None;
        self.list_drop_target = None;
    }

    pub(crate) fn set_reorder_target(
        &mut self,
        target: ListDropTarget,
        estimated_row_height: Pixels,
        current_visual_offsets: &HashMap<HistoryItemId, Pixels>,
    ) -> bool {
        if !self.can_reorder_items()
            || self.list_reorder_commit_pending
            || target.source_id == target.target_id
        {
            return false;
        }

        if self.list_hovered_item_id == Some(target.target_id) {
            return false;
        }
        let Some(source_is_pinned) = self
            .loaded_items
            .get(&target.source_id)
            .map(|item| item.is_pinned)
        else {
            return false;
        };
        let Some(target_is_pinned) = self
            .loaded_items
            .get(&target.target_id)
            .map(|item| item.is_pinned)
        else {
            return false;
        };
        if source_is_pinned != target_is_pinned {
            return false;
        }
        if self.list_drag_source_id.is_none() {
            self.begin_reorder_drag(target.source_id);
        }
        if self.list_drag_source_id != Some(target.source_id) {
            return false;
        }
        let Some(target_position) = self.list_order_positions.get(&target.target_id).copied()
        else {
            return false;
        };
        let Some(source_position) = self.list_order_positions.get(&target.source_id).copied()
        else {
            return false;
        };
        // 目标位置是删除源项后的最终插入索引：无论向上还是向下，都是目标项当前索引。
        let destination_position = target_position;
        let source_height = if self
            .list_layout_cache
            .as_ref()
            .is_some_and(|cache| cache.key.horizontal)
        {
            estimated_row_height
        } else {
            self.adaptive_item_heights
                .get(&target.source_id)
                .copied()
                .unwrap_or(estimated_row_height)
        };
        let source_shift = if source_position < target_position {
            source_height
        } else {
            -source_height
        };
        let mut start_offsets = HashMap::new();
        for history_id in self.loaded_items.keys().copied() {
            let Some(current_position) = self.list_order_positions.get(&history_id).copied() else {
                continue;
            };
            let position_shift = if history_id != target.source_id
                && source_position < target_position
                && current_position > source_position
                && current_position <= target_position
            {
                source_shift
            } else if history_id != target.source_id
                && source_position > target_position
                && current_position >= target_position
                && current_position < source_position
            {
                source_shift
            } else {
                px(0.0)
            };
            let offset = current_visual_offsets
                .get(&history_id)
                .copied()
                .unwrap_or(px(0.0))
                + position_shift;
            if offset != px(0.0) && history_id != target.source_id {
                start_offsets.insert(history_id, offset);
            }
        }
        if !self.move_list_order_item_to_position(target.source_id, destination_position) {
            return false;
        }
        if !start_offsets.is_empty() {
            self.list_reorder_animation_generation =
                self.list_reorder_animation_generation.wrapping_add(1);
            self.list_reorder_animation = Some(ListReorderAnimation {
                generation: self.list_reorder_animation_generation,
                start_offsets,
                started_at: Instant::now(),
            });
        } else {
            self.list_reorder_animation = None;
        }

        self.list_hovered_item_id = Some(target.target_id);
        self.list_drop_target = Some(target);
        true
    }

    fn original_list_position_during_drag(&self, current_position: usize) -> Option<usize> {
        let source_id = self.list_drag_source_id?;
        let source_origin = self.list_drag_origin_position?;
        let source_position = self.list_order_positions.get(&source_id).copied()?;

        Some(
            if source_position < source_origin
                && current_position > source_position
                && current_position <= source_origin
            {
                current_position - 1
            } else if source_position > source_origin
                && current_position >= source_origin
                && current_position < source_position
            {
                current_position + 1
            } else {
                current_position
            },
        )
    }

    pub(crate) fn cancel_reorder_preview(&mut self) -> bool {
        if self.list_reorder_commit_pending {
            return false;
        }
        let had_drag = self.list_drag_source_id.is_some()
            || self.list_hovered_item_id.is_some()
            || self.list_drop_target.is_some();
        if let (Some(source_id), Some(origin_position)) =
            (self.list_drag_source_id, self.list_drag_origin_position)
        {
            self.move_list_order_item_to_position(source_id, origin_position);
        }
        self.list_drag_source_id = None;
        self.list_drag_origin_position = None;
        self.list_reorder_commit_pending = false;
        self.clear_list_drop_target();
        had_drag
    }

    pub(super) fn finish_reorder_preview(&mut self) {
        self.list_drag_source_id = None;
        self.list_drag_origin_position = None;
        self.list_reorder_commit_pending = false;
        self.clear_list_drop_target();
    }

    pub(crate) fn commit_reorder_preview(&mut self, cx: &mut Context<Self>) {
        let Some(drop_target) = self.list_drop_target else {
            if self.cancel_reorder_preview() {
                cx.notify();
            }
            return;
        };
        let source_id = drop_target.source_id;
        let source_position = self.list_order_positions.get(&source_id).copied();
        let target_id = source_position.and_then(|source_position| {
            let origin_position = self.list_drag_origin_position?;
            if source_position > origin_position {
                source_position
                    .checked_sub(1)
                    .and_then(|position| self.list_order_ids.get(position))
                    .copied()
            } else if source_position < origin_position {
                self.list_order_ids.get(source_position + 1).copied()
            } else {
                None
            }
        });
        if self.list_drag_source_id == Some(source_id) {
            if let Some(target_id) = target_id {
                self.reorder_items(source_id, target_id, cx);
                return;
            }
        }
        if self.cancel_reorder_preview() {
            cx.notify();
        }
    }

    pub(crate) fn reorder_items(
        &mut self,
        source_id: HistoryItemId,
        target_id: HistoryItemId,
        cx: &mut Context<Self>,
    ) {
        if !self.can_reorder_items() || source_id == target_id {
            if self.cancel_reorder_preview() {
                cx.notify();
            }
            return;
        }
        self.clear_list_drop_target();
        self.list_reorder_commit_pending = true;
        let active_tab = self.active_tab;
        let generation = self.query_generation;
        let history_repository = self.history_repository.clone();
        let favorite_repository = self.favorite_repository.clone();
        let reorder_task = cx.background_spawn(async move {
            if active_tab == TabId::Favorites {
                favorite_repository
                    .reorder(source_id, target_id)
                    .map_err(|error| error.to_string())
            } else {
                history_repository
                    .reorder(source_id, target_id)
                    .map_err(|error| error.to_string())
            }
        });
        cx.spawn(async move |this, cx| match reorder_task.await {
            Ok(true) => {
                if let Some(this) = this.upgrade() {
                    this.update(cx, |this, cx| {
                        if this.active_tab != active_tab || this.query_generation != generation {
                            return;
                        }
                        if this.list_drag_source_id != Some(source_id) {
                            this.refresh_history(cx);
                            return;
                        }
                        if this.finish_committed_list_reorder(source_id, target_id) {
                            this.finish_reorder_preview();
                            cx.notify();
                        } else {
                            this.refresh_history(cx);
                        }
                    });
                }
            }
            Ok(false) => {
                if let Some(this) = this.upgrade() {
                    this.update(cx, |this, cx| {
                        if this.active_tab != active_tab || this.query_generation != generation {
                            return;
                        }
                        if this.list_drag_source_id != Some(source_id) {
                            return;
                        }
                        this.list_reorder_commit_pending = false;
                        if this.cancel_reorder_preview() {
                            cx.notify();
                        }
                    });
                }
            }
            Err(error) => {
                tracing::error!("列表排序失败：{error}");
                if let Some(this) = this.upgrade() {
                    this.update(cx, |this, cx| {
                        if this.active_tab != active_tab || this.query_generation != generation {
                            return;
                        }
                        if this.list_drag_source_id != Some(source_id) {
                            return;
                        }
                        this.list_reorder_commit_pending = false;
                        if this.cancel_reorder_preview() {
                            cx.notify();
                        }
                    });
                }
            }
        })
        .detach();
    }

    fn finish_committed_list_reorder(
        &mut self,
        source_id: HistoryItemId,
        target_id: HistoryItemId,
    ) -> bool {
        let Some(source_position) = self.list_order_positions.get(&source_id).copied() else {
            return false;
        };
        let Some(target_position) = self.list_order_positions.get(&target_id).copied() else {
            return false;
        };
        let Some(target_origin) = self.original_list_position_during_drag(target_position) else {
            return false;
        };
        if source_position != target_origin {
            return false;
        }

        refresh_shortcuts_for_order(&mut self.items, &self.list_order_ids);
        self.sync_loaded_shortcuts();
        self.reconcile_selection();
        true
    }
}
