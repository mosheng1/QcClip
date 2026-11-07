use super::*;
use crate::services::sync::ReceivedChange;
use crate::windows::list_order::nearest_survivor;

/// 拖拽/加载阻塞时的轮询间隔。50ms 级唤醒会持续抢拖拽动画帧。
const BLOCKED_REFRESH_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

#[cfg(test)]
mod tests;

struct UpdateBatch {
    changes: Vec<ReceivedChange>,
    tab: TabId,
    generation: u64,
    visibility_generation: u64,
    tag_generation: u64,
    previous_order: Vec<HistoryItemId>,
    ids: Vec<HistoryItemId>,
}

struct UpdateData {
    snapshot: Option<ListOrderSnapshot>,
    records: Vec<HistoryListRecord>,
    tags: Option<Vec<Tag>>,
}

enum NextUpdate {
    Stop,
    Wait,
    Load(UpdateBatch, Task<Result<UpdateData, String>>),
}

fn anchor_in_layout(
    order: &[HistoryItemId],
    visible_ids: Option<&HashSet<HistoryItemId>>,
    axis_offset: Pixels,
    leading_padding: Pixels,
    item_offset: impl Fn(usize) -> Option<Pixels>,
) -> Option<(HistoryItemId, Pixels)> {
    let viewport_start = -(axis_offset + leading_padding);
    let positions = order
        .iter()
        .enumerate()
        .filter(|(_, id)| visible_ids.is_none_or(|visible| visible.contains(id)))
        .filter_map(|(index, id)| Some((*id, item_offset(index)?, item_offset(index + 1)?)))
        .collect::<Vec<_>>();
    positions
        .iter()
        .find(|(_, _, end)| *end > viewport_start)
        .or_else(|| positions.last())
        .map(|(id, origin, _)| (*id, *origin))
}

impl MainWindow {
    pub(crate) fn refresh_missing_visibility(&mut self, cx: &mut Context<Self>) {
        if let Some(panes) = self.split_panes.clone() {
            for pane in panes {
                pane.update(cx, |pane, cx| pane.refresh_missing_visibility(cx));
            }
            return;
        }
        if self.active_tab == TabId::Emoji {
            return;
        }
        self.missing_visibility_dirty = true;
        self.missing_visibility_generation = self.missing_visibility_generation.wrapping_add(1);
        let visibility_generation = self.missing_visibility_generation;
        self.missing_visibility_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok(next) = this.update(cx, |this, cx| {
                    if this.missing_visibility_generation != visibility_generation {
                        return None;
                    }
                    if this.sync_update_blocked() {
                        return Some(None);
                    }
                    let tab = this.active_tab;
                    let query_generation = this.query_generation;
                    let previous_order = this.list_order_ids.clone();
                    let loaded_ids: HashSet<_> = this.loaded_items.keys().copied().collect();
                    let selected_id = this.selected_item_id;
                    let viewport_index = this
                        .pending_list_anchor
                        .as_ref()
                        .map(|anchor| anchor.history_id)
                        .or_else(|| {
                            this.visible_list_anchor(None)
                                .map(|anchor| anchor.history_id)
                        })
                        .and_then(|id| previous_order.iter().position(|candidate| *candidate == id))
                        .unwrap_or_default();
                    let query = this.page_query(0, MAX_QUERY_PAGE_SIZE);
                    let policy = crate::config_runtime::history_config(cx);
                    let history = this.history_repository.clone();
                    let favorites = this.favorite_repository.clone();
                    let task = cx.background_spawn(async move {
                        let snapshot = if tab == TabId::Favorites {
                            favorites.ordered_snapshot_scoped(&query, &policy)?
                        } else {
                            history.ordered_snapshot_scoped(&query, &policy)?
                        };
                        if snapshot.ids == previous_order {
                            return Ok::<_, HistoryQueryError>((snapshot, Vec::new(), Vec::new()));
                        }
                        let visible_ids: HashSet<_> = snapshot.ids.iter().copied().collect();
                        let anchor_index =
                            nearest_survivor(&previous_order, viewport_index, &visible_ids)
                                .and_then(|id| {
                                    snapshot.ids.iter().position(|candidate| *candidate == id)
                                })
                                .unwrap_or_default();
                        let window_start = anchor_index.saturating_sub(MAX_QUERY_PAGE_SIZE / 4);
                        let window_end =
                            (window_start + MAX_QUERY_PAGE_SIZE).min(snapshot.ids.len());
                        let mut fetch_ids = snapshot.ids[window_start..window_end]
                            .iter()
                            .copied()
                            .filter(|id| !loaded_ids.contains(id))
                            .collect::<Vec<_>>();
                        let selected_id = selected_id.and_then(|id| {
                            if visible_ids.contains(&id) {
                                Some(id)
                            } else {
                                previous_order
                                    .iter()
                                    .position(|candidate| *candidate == id)
                                    .and_then(|index| {
                                        nearest_survivor(&previous_order, index, &visible_ids)
                                    })
                                    .or_else(|| snapshot.ids.first().copied())
                            }
                        });
                        if let Some(selected_id) = selected_id.filter(|id| {
                            visible_ids.contains(id)
                                && !loaded_ids.contains(id)
                                && !fetch_ids.contains(id)
                        }) {
                            fetch_ids.push(selected_id);
                        }
                        let mut records = Vec::new();
                        for chunk in fetch_ids.chunks(MAX_QUERY_PAGE_SIZE) {
                            records.extend(if tab == TabId::Favorites {
                                favorites.query_by_ids_scoped(&query, chunk, &policy)?
                            } else {
                                history.query_by_ids_scoped(&query, chunk, &policy)?
                            });
                        }
                        Ok::<_, HistoryQueryError>((snapshot, fetch_ids, records))
                    });
                    Some(Some((
                        tab,
                        query_generation,
                        this.list_order_ids.clone(),
                        task,
                    )))
                }) else {
                    break;
                };
                let Some(next) = next else { break };
                let Some((tab, query_generation, previous_order, task)) = next else {
                    cx.background_executor()
                        .timer(BLOCKED_REFRESH_POLL_INTERVAL)
                        .await;
                    continue;
                };
                let result = task.await;
                let Ok(retry) = this.update(cx, |this, cx| {
                    if this.missing_visibility_generation != visibility_generation
                        || this.query_generation != query_generation
                        || this.active_tab != tab
                    {
                        return false;
                    }
                    if this.list_order_ids != previous_order || this.sync_update_blocked() {
                        return true;
                    }
                    match result {
                        Ok((snapshot, fetch_ids, records)) => {
                            if snapshot.ids != previous_order {
                                this.apply_sync_records(&fetch_ids, records, snapshot, cx);
                                cx.notify();
                            }
                            this.missing_visibility_dirty = false;
                        }
                        Err(HistoryQueryError::Database(
                            crate::services::database::DatabaseError::DataSwitching,
                        )) => {
                            // The database is closed during a data switch. Keep the
                            // dirty flag and let the refresh after the switch retry.
                            tracing::debug!("失效文件列表更新等待数据切换完成");
                        }
                        Err(error) => tracing::error!("失效文件列表局部更新失败：{error}"),
                    }
                    false
                }) else {
                    break;
                };
                if !retry {
                    break;
                }
            }
            let _ = this.update(cx, |this, _| {
                if this.missing_visibility_generation == visibility_generation {
                    this.missing_visibility_task = None;
                }
            });
        }));
    }

    pub(crate) fn retry_missing_visibility(&mut self, cx: &mut Context<Self>) {
        if let Some(panes) = self.split_panes.clone() {
            for pane in panes {
                pane.update(cx, |pane, cx| pane.retry_missing_visibility(cx));
            }
            return;
        }
        if self.missing_visibility_dirty && self.missing_visibility_task.is_none() {
            self.refresh_missing_visibility(cx);
        }
    }

    pub(crate) fn receive_sync_changes(
        &mut self,
        changes: Vec<ReceivedChange>,
        cx: &mut Context<Self>,
    ) {
        if let Some(panes) = self.split_panes.clone() {
            for pane in panes {
                pane.update(cx, |pane, cx| {
                    pane.receive_sync_changes(changes.clone(), cx)
                });
            }
            self.refresh_tags(cx);
            return;
        }
        for change in changes {
            self.sync_changes
                .entry(change.item_id)
                .and_modify(|pending| pending.merge(change))
                .or_insert(change);
        }
        if self.sync_refresh_task.is_some() || self.sync_changes.is_empty() {
            return;
        }
        self.sync_refresh_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok(next) = this.update(cx, |this, cx| this.next_sync_update(cx)) else {
                    break;
                };
                match next {
                    NextUpdate::Stop => break,
                    NextUpdate::Wait => {
                        cx.background_executor()
                            .timer(BLOCKED_REFRESH_POLL_INTERVAL)
                            .await;
                    }
                    NextUpdate::Load(batch, task) => {
                        let result = task.await;
                        let _ = this.update(cx, |this, cx| {
                            if batch.generation != this.query_generation
                                || batch.visibility_generation != this.missing_visibility_generation
                                || batch.tab != this.active_tab
                                || batch.previous_order != this.list_order_ids
                                || batch.tag_generation != this.tags_query_generation
                                || this.sync_update_blocked()
                            {
                                // 查询期间切页或本地列表改变时，合并重查，不能用旧结果覆盖新状态。
                                for change in batch.changes {
                                    this.sync_changes
                                        .entry(change.item_id)
                                        .and_modify(|pending| {
                                            pending.tags_changed |= change.tags_changed
                                        })
                                        .or_insert(change);
                                }
                                return;
                            }
                            match result {
                                Ok(data) => {
                                    if let Some(tags) = data.tags {
                                        let existing: HashSet<_> =
                                            this.tags.iter().map(|tag| tag.id).collect();
                                        this.tags.extend(
                                            tags.into_iter()
                                                .filter(|tag| !existing.contains(&tag.id)),
                                        );
                                        sort_tags(&mut this.tags);
                                    }
                                    if let Some(snapshot) = data.snapshot {
                                        this.apply_sync_records(
                                            &batch.ids,
                                            data.records,
                                            snapshot,
                                            cx,
                                        );
                                    }
                                    cx.notify();
                                }
                                Err(error) => tracing::error!("同步内容局部更新失败：{error}"),
                            }
                        });
                    }
                }
            }
        }));
    }

    fn sync_update_blocked(&self) -> bool {
        self.list_status == ListStatus::Loading
            || self.is_loading_more
            || self.list_drag_source_id.is_some()
            || self.list_reorder_commit_pending
            || self.tag_drag_source_id.is_some()
            || self.tags_loading
    }

    fn next_sync_update(&mut self, cx: &mut Context<Self>) -> NextUpdate {
        if self.sync_changes.is_empty() {
            self.sync_refresh_task = None;
            return NextUpdate::Stop;
        }
        if self.sync_update_blocked() {
            return NextUpdate::Wait;
        }
        let changes: Vec<_> = std::mem::take(&mut self.sync_changes)
            .into_values()
            .collect();
        let ids: Vec<_> = changes
            .iter()
            .filter_map(|change| match self.active_tab {
                TabId::Clipboard => change.history_id,
                TabId::Favorites => change.favorite_id,
                TabId::Emoji => None,
            })
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let load_tags = changes.iter().any(|change| change.tags_changed);
        let batch = UpdateBatch {
            changes,
            tab: self.active_tab,
            generation: self.query_generation,
            visibility_generation: self.missing_visibility_generation,
            tag_generation: self.tags_query_generation,
            previous_order: self.list_order_ids.clone(),
            ids: ids.clone(),
        };
        let mut query = self.page_query(0, MAX_QUERY_PAGE_SIZE);
        query.limit = MAX_QUERY_PAGE_SIZE as u32;
        let policy = crate::config_runtime::history_config(cx);
        let history = self.history_repository.clone();
        let favorites = self.favorite_repository.clone();
        let tags = self.tag_repository.clone();
        let tab = batch.tab;
        let task = cx.background_spawn(async move {
            let snapshot = if ids.is_empty() {
                None
            } else if tab == TabId::Favorites {
                Some(
                    favorites
                        .ordered_snapshot_scoped(&query, &policy)
                        .map_err(|e| e.to_string())?,
                )
            } else {
                Some(
                    history
                        .ordered_snapshot_scoped(&query, &policy)
                        .map_err(|e| e.to_string())?,
                )
            };
            let mut records = Vec::new();
            for chunk in ids.chunks(MAX_QUERY_PAGE_SIZE) {
                records.extend(if tab == TabId::Favorites {
                    favorites
                        .query_by_ids_scoped(&query, chunk, &policy)
                        .map_err(|e| e.to_string())?
                } else {
                    history
                        .query_by_ids_scoped(&query, chunk, &policy)
                        .map_err(|e| e.to_string())?
                });
            }
            let tags = if load_tags {
                Some(tags.list().map_err(|e| e.to_string())?)
            } else {
                None
            };
            Ok(UpdateData {
                snapshot,
                records,
                tags,
            })
        });
        NextUpdate::Load(batch, task)
    }

    fn apply_sync_records(
        &mut self,
        changed_ids: &[HistoryItemId],
        records: Vec<HistoryListRecord>,
        snapshot: ListOrderSnapshot,
        cx: &mut Context<Self>,
    ) {
        let changed_ids: HashSet<_> = changed_ids.iter().copied().collect();
        let visible_ids: HashSet<_> = snapshot.ids.iter().copied().collect();
        if self.preview_hover_source.is_some_and(|source| {
            let id = match source {
                PreviewSource::History(id) | PreviewSource::Favorite(id) => id,
            };
            !visible_ids.contains(&id) || changed_ids.contains(&id)
        }) {
            self.cancel_hover_preview(cx);
        }
        let order_changed = self.list_order_ids != snapshot.ids;
        let selected_position = self
            .selected_item_id
            .and_then(|id| self.list_order_positions.get(&id).copied());
        let selected_fallback = selected_position
            .and_then(|index| nearest_survivor(&self.list_order_ids, index, &visible_ids));
        if order_changed {
            self.pending_list_anchor = self.visible_list_anchor(Some(&visible_ids));
        }
        let removed_items: Vec<_> = self
            .loaded_items
            .values()
            .filter(|item| !visible_ids.contains(&item.history_id))
            .map(|item| item.item_id)
            .collect();
        for item_id in removed_items {
            self.invalidate_image_cache_item(item_id);
        }
        self.items.retain(|item| {
            visible_ids.contains(&item.history_id) && !changed_ids.contains(&item.history_id)
        });
        self.loaded_items
            .retain(|id, _| visible_ids.contains(id) && !changed_ids.contains(id));
        for record in records {
            if !visible_ids.contains(&record.history_id) {
                continue;
            }
            let item = record_to_list_item(record);
            self.adaptive_item_heights.remove(&item.history_id);
            self.invalidate_image_cache_item(item.item_id);
            self.loaded_items.insert(item.history_id, item.clone());
            self.items.push(item);
        }
        self.list_order_ids = snapshot.ids;
        self.list_heatmap.replace(snapshot.paste_counts);
        self.rebuild_list_order_positions();
        self.items.sort_by_key(|item| {
            self.list_order_positions
                .get(&item.history_id)
                .copied()
                .unwrap_or(usize::MAX)
        });
        self.total_item_count = self.list_order_ids.len();
        self.adaptive_item_heights
            .retain(|id, _| visible_ids.contains(id));
        self.list_item_load_animations
            .retain(|id, _| visible_ids.contains(id));
        self.has_more_items = self.loaded_items.len() < self.total_item_count;
        refresh_shortcuts_for_order(&mut self.items, &self.list_order_ids);
        self.sync_loaded_shortcuts();
        if self
            .selected_item_id
            .is_some_and(|id| !visible_ids.contains(&id))
        {
            self.selected_item_id =
                selected_fallback.or_else(|| self.list_order_ids.first().copied());
        }
        self.reconcile_selection();
        self.list_status = if self.total_item_count == 0 {
            ListStatus::Empty
        } else {
            ListStatus::Ready
        };
        if order_changed {
            self.list_layout_generation = self.list_layout_generation.wrapping_add(1);
            self.list_layout_cache = None;
            self.list_smooth_scroll.reset();
        } else {
            self.invalidate_list_layout_cache();
        }
    }

    fn visible_list_anchor(
        &self,
        visible_ids: Option<&HashSet<HistoryItemId>>,
    ) -> Option<PendingListAnchor> {
        let pending = self.pending_list_anchor.as_ref();
        let layout_order_ids = pending
            .map(|anchor| anchor.layout_order_ids.clone())
            .unwrap_or_else(|| self.list_order_ids.clone());
        let horizontal = pending
            .map(|anchor| anchor.horizontal)
            .unwrap_or(self.last_rendered_list_horizontal);
        let axis_offset = pending.map(|anchor| anchor.axis_offset).unwrap_or_else(|| {
            let offset = self.list_scroll_handle.offset();
            if horizontal { offset.x } else { offset.y }
        });
        let leading_padding = pending
            .map(|anchor| anchor.leading_padding)
            .unwrap_or(self.last_rendered_list_leading_padding);
        let (history_id, origin) = anchor_in_layout(
            &layout_order_ids,
            visible_ids,
            axis_offset,
            leading_padding,
            |index| self.list_scroll_handle.item_offset(index),
        )?;
        Some(PendingListAnchor {
            history_id,
            screen_axis: origin + axis_offset,
            horizontal,
            layout_order_ids,
            axis_offset,
            leading_padding,
        })
    }
}
