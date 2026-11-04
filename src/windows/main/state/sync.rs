use super::*;
use crate::services::sync::ReceivedChange;

#[cfg(test)]
mod tests;

struct UpdateBatch {
    changes: Vec<ReceivedChange>,
    tab: TabId,
    generation: u64,
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

impl MainWindow {
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
                            .timer(Duration::from_millis(50))
                            .await;
                    }
                    NextUpdate::Load(batch, task) => {
                        let result = task.await;
                        let _ = this.update(cx, |this, cx| {
                            if batch.generation != this.query_generation
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
                                        this.apply_sync_records(&batch.ids, data.records, snapshot);
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
                        .ordered_snapshot(&query)
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
                        .query_by_ids(&query, chunk)
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
    ) {
        let changed_ids: HashSet<_> = changed_ids.iter().copied().collect();
        let visible_ids: HashSet<_> = snapshot.ids.iter().copied().collect();
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
        self.reconcile_selection();
        self.list_status = if self.total_item_count == 0 {
            ListStatus::Empty
        } else {
            ListStatus::Ready
        };
        self.invalidate_list_layout_cache();
    }
}
