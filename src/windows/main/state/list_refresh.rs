use super::*;

#[derive(Clone, Copy)]
pub(super) enum HistoryRefreshReason {
    Capture,
    Update,
}

impl HistoryRefreshReason {
    pub(super) fn requires_snapshot(self, policy: &crate::config::HistoryConfig) -> bool {
        matches!(self, Self::Update)
            || policy.retention_days.is_some()
            || policy.max_items.is_some()
    }
}

pub(super) enum HistoryItemRefresh {
    Captured {
        record: Option<HistoryListRecord>,
        pinned_count: usize,
    },
    Snapshot {
        record: Option<HistoryListRecord>,
        snapshot: ListOrderSnapshot,
    },
}

pub(super) fn load_history_item_refresh(
    repository: &HistoryRepository,
    history_id: HistoryItemId,
    query: &HistoryQuery,
    policy: &crate::config::HistoryConfig,
    reason: HistoryRefreshReason,
) -> Result<HistoryItemRefresh, HistoryQueryError> {
    let record = repository.find_by_id_scoped(history_id, query, policy)?;
    if reason.requires_snapshot(policy) {
        return Ok(HistoryItemRefresh::Snapshot {
            record,
            snapshot: repository.ordered_snapshot_scoped(query, policy)?,
        });
    }
    let pinned_count = if record.is_some() {
        repository.pinned_count(query)?
    } else {
        0
    };
    Ok(HistoryItemRefresh::Captured {
        record,
        pinned_count,
    })
}

pub(super) fn apply_captured_list_order(
    order_ids: &mut Vec<HistoryItemId>,
    heatmap: &mut ListHeatmap,
    total: &mut usize,
    item: &ClipboardListItem,
    pinned_count: usize,
) -> (Option<usize>, usize) {
    let source_position = order_ids.iter().position(|id| *id == item.history_id);
    let target_position = if item.is_pinned { 0 } else { pinned_count };
    let destination_position;
    if let Some(source_position) = source_position {
        order_ids.remove(source_position);
        destination_position = target_position.min(order_ids.len());
        order_ids.insert(destination_position, item.history_id);
        heatmap.update(source_position, item.paste_count);
        heatmap.move_item(source_position, destination_position);
    } else {
        destination_position = target_position.min(order_ids.len());
        order_ids.insert(destination_position, item.history_id);
        heatmap.insert(destination_position, item.paste_count);
        *total = total.saturating_add(1);
    }
    (source_position, destination_position)
}

pub(super) fn replace_list_order(
    order_ids: &mut Vec<HistoryItemId>,
    heatmap: &mut ListHeatmap,
    total: &mut usize,
    requested_order: &[HistoryItemId],
    snapshot: ListOrderSnapshot,
) -> bool {
    // Another refresh may have applied a newer capture while this query was running.
    // Retry this item against the current order without clearing the loaded list.
    if order_ids.as_slice() != requested_order {
        return false;
    }
    *total = snapshot.ids.len();
    *order_ids = snapshot.ids;
    heatmap.replace(snapshot.paste_counts);
    true
}

/// 快照可能改变目标条目以外的保留范围；所有提前返回前都要清理这些缓存。
pub(super) fn retain_list_records_for_order(
    items: &mut Vec<ClipboardListItem>,
    loaded_items: &mut HashMap<HistoryItemId, ClipboardListItem>,
    order_positions: &HashMap<HistoryItemId, usize>,
) -> HashSet<ClipboardItemId> {
    let removed_item_ids = items
        .iter()
        .chain(loaded_items.values())
        .filter(|item| !order_positions.contains_key(&item.history_id))
        .map(|item| item.item_id)
        .collect();
    items.retain(|item| order_positions.contains_key(&item.history_id));
    loaded_items.retain(|history_id, _| order_positions.contains_key(history_id));
    for item in items.iter_mut().chain(loaded_items.values_mut()) {
        item.shortcut = format!("Ctrl+{}", order_positions[&item.history_id] + 1);
    }
    removed_item_ids
}

pub(super) fn reconcile_snapshot_selection(
    selected_item_id: &mut Option<HistoryItemId>,
    items: &mut [ClipboardListItem],
    order_positions: &HashMap<HistoryItemId, usize>,
) {
    if selected_item_id.is_some_and(|history_id| !order_positions.contains_key(&history_id)) {
        *selected_item_id = items
            .iter()
            .find(|item| order_positions.contains_key(&item.history_id))
            .map(|item| item.history_id);
    }
    for item in items {
        item.is_selected = Some(item.history_id) == *selected_item_id;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::HistoryConfig,
        services::{
            capture::{CaptureService, mock::MockClipboardSource},
            database::Database,
        },
    };

    struct TestList {
        _directory: tempfile::TempDir,
        database: Database,
        capture: CaptureService,
        repository: HistoryRepository,
        order_ids: Vec<HistoryItemId>,
        heatmap: ListHeatmap,
        total: usize,
    }

    impl TestList {
        fn new() -> Self {
            let directory = tempfile::tempdir().expect("创建测试目录");
            let database =
                Database::open(directory.path().join("test.sqlite3")).expect("创建测试数据库");
            let blobs = RawBlobStore::new(directory.path()).expect("创建测试存储");
            Self {
                _directory: directory,
                capture: CaptureService::new(database.clone(), blobs.clone()),
                repository: HistoryRepository::new(database.clone(), blobs),
                database,
                order_ids: Vec::new(),
                heatmap: ListHeatmap::default(),
                total: 0,
            }
        }

        fn capture(&self, text: &str) -> HistoryItemId {
            self.capture
                .store_captured_clipboard(&MockClipboardSource::text(text).capture())
                .expect("保存捕获")
        }

        fn load(&mut self, policy: &HistoryConfig) {
            let requested_order = self.order_ids.clone();
            let snapshot = self
                .repository
                .ordered_snapshot_scoped(&HistoryQuery::default(), policy)
                .expect("查询初始列表");
            replace_list_order(
                &mut self.order_ids,
                &mut self.heatmap,
                &mut self.total,
                &requested_order,
                snapshot,
            );
        }

        fn refresh(
            &mut self,
            history_id: HistoryItemId,
            policy: &HistoryConfig,
            reason: HistoryRefreshReason,
        ) {
            let requested_order = self.order_ids.clone();
            let refresh = load_history_item_refresh(
                &self.repository,
                history_id,
                &HistoryQuery::default(),
                policy,
                reason,
            )
            .expect("查询更新后的条目");
            match refresh {
                HistoryItemRefresh::Captured {
                    record: Some(record),
                    pinned_count,
                } => {
                    apply_captured_list_order(
                        &mut self.order_ids,
                        &mut self.heatmap,
                        &mut self.total,
                        &record_to_list_item(record),
                        pinned_count,
                    );
                }
                HistoryItemRefresh::Captured { record: None, .. } => panic!("测试条目应存在"),
                HistoryItemRefresh::Snapshot { snapshot, .. } => {
                    replace_list_order(
                        &mut self.order_ids,
                        &mut self.heatmap,
                        &mut self.total,
                        &requested_order,
                        snapshot,
                    );
                }
            }
        }
    }

    #[test]
    fn quick_capture_replaces_history_beyond_max_items() {
        let mut list = TestList::new();
        let policy = HistoryConfig {
            max_items: Some(1),
            ..HistoryConfig::default()
        };
        let first = list.capture("A");
        list.load(&policy);
        assert_eq!(list.order_ids, vec![first]);
        let second = list.capture("B");

        list.refresh(second, &policy, HistoryRefreshReason::Capture);

        assert_eq!(list.order_ids, vec![second]);
        assert_eq!(list.total, 1);
        assert_eq!(list.heatmap.buckets().len(), 1);
    }

    #[test]
    fn quick_capture_removes_history_that_expired_after_loading() {
        let mut list = TestList::new();
        let policy = HistoryConfig {
            retention_days: Some(1),
            ..HistoryConfig::default()
        };
        let first = list.capture("A");
        list.load(&policy);
        list.database
            .connect()
            .expect("连接数据库")
            .execute(
                "UPDATE clipboard_history SET first_seen_at = 0, last_seen_at = 0 WHERE id = ?1",
                [first],
            )
            .expect("令原记录超出保留期");
        let second = list.capture("B");

        list.refresh(second, &policy, HistoryRefreshReason::Capture);

        assert_eq!(list.order_ids, vec![second]);
        assert_eq!(list.total, 1);
    }

    #[test]
    fn saving_older_quick_favorite_preserves_newer_history_order() {
        let mut list = TestList::new();
        let policy = HistoryConfig::default();
        let first = list.capture("A");
        let second = list.capture("B");
        list.load(&policy);
        let item_id = list
            .repository
            .find_by_id(first, &HistoryQuery::default())
            .expect("查询原条目")
            .expect("原条目存在")
            .item_id;
        FavoriteRepository::new(list.database.clone())
            .add_to_favorites(item_id)
            .expect("确认收藏 A");

        list.refresh(first, &policy, HistoryRefreshReason::Update);

        assert_eq!(list.order_ids, vec![second, first]);
        assert_eq!(list.total, 2);
    }

    #[test]
    fn snapshot_prunes_out_of_scope_cache_and_preserves_actual_shortcut_positions() {
        let list = TestList::new();
        let first = list.capture("A");
        let second = list.capture("B");
        let third = list.capture("C");
        let first_record = list
            .repository
            .find_by_id(first, &HistoryQuery::default())
            .expect("查询 A")
            .expect("A 存在");
        let second_record = list
            .repository
            .find_by_id(second, &HistoryQuery::default())
            .expect("查询 B")
            .expect("B 存在");
        let removed_item_id = first_record.item_id;
        // 模拟滚动到非顶部，只加载 A、B 的缓存。
        let mut items = vec![
            record_to_list_item(second_record),
            record_to_list_item(first_record),
        ];
        let mut loaded = items
            .iter()
            .cloned()
            .map(|item| (item.history_id, item))
            .collect();
        let snapshot = list
            .repository
            .ordered_snapshot_scoped(
                &HistoryQuery::default(),
                &HistoryConfig {
                    max_items: Some(2),
                    ..HistoryConfig::default()
                },
            )
            .expect("查询保留范围");
        assert_eq!(snapshot.ids, vec![third, second]);
        let positions = snapshot
            .ids
            .iter()
            .enumerate()
            .map(|(index, id)| (*id, index))
            .collect();

        let removed = retain_list_records_for_order(&mut items, &mut loaded, &positions);

        assert_eq!(
            items.iter().map(|item| item.history_id).collect::<Vec<_>>(),
            vec![second]
        );
        assert_eq!(loaded.len(), 1);
        assert_eq!(items[0].shortcut, "Ctrl+2");
        assert_eq!(loaded[&second].shortcut, "Ctrl+2");
        assert_eq!(removed, HashSet::from([removed_item_id]));
    }

    #[test]
    fn filtered_out_capture_still_expires_the_previous_cached_match() {
        let list = TestList::new();
        let first = list.capture("A");
        let first_record = list
            .repository
            .find_by_id(first, &HistoryQuery::default())
            .expect("查询 A")
            .expect("A 存在");
        let mut items = vec![record_to_list_item(first_record)];
        let mut loaded = items
            .iter()
            .cloned()
            .map(|item| (item.history_id, item))
            .collect();
        let second = list.capture("B");
        let query = HistoryQuery {
            search: Some("A".to_owned()),
            ..HistoryQuery::default()
        };
        let policy = HistoryConfig {
            max_items: Some(1),
            ..HistoryConfig::default()
        };
        let refresh = load_history_item_refresh(
            &list.repository,
            second,
            &query,
            &policy,
            HistoryRefreshReason::Capture,
        )
        .expect("刷新当前搜索结果");
        let HistoryItemRefresh::Snapshot { record, snapshot } = refresh else {
            panic!("有限保留范围需要快照");
        };
        assert!(record.is_none());
        assert!(snapshot.ids.is_empty());
        let positions = snapshot
            .ids
            .iter()
            .enumerate()
            .map(|(index, id)| (*id, index))
            .collect();

        retain_list_records_for_order(&mut items, &mut loaded, &positions);

        assert!(items.is_empty());
        assert!(loaded.is_empty());
    }

    #[test]
    fn delayed_snapshot_preserves_new_capture_and_requery_includes_saved_content() {
        let mut list = TestList::new();
        let policy = HistoryConfig::default();
        let first = list.capture("A");
        list.load(&policy);
        let first_record = list
            .repository
            .find_by_id(first, &HistoryQuery::default())
            .expect("查询 A")
            .expect("A 存在");
        FavoriteRepository::new(list.database.clone())
            .add_to_favorites(first_record.item_id)
            .expect("收藏 A");
        CustomTextService::new(
            list.database.clone(),
            RawBlobStore::new(list._directory.path()).expect("打开测试存储"),
        )
        .save_for_item(first_record.item_id, "A edited")
        .expect("保存编辑内容");
        let requested_order = list.order_ids.clone();
        let HistoryItemRefresh::Snapshot { snapshot, .. } = load_history_item_refresh(
            &list.repository,
            first,
            &HistoryQuery::default(),
            &policy,
            HistoryRefreshReason::Update,
        )
        .expect("取得 A 更新后的快照") else {
            panic!("保存应返回快照");
        };
        assert_eq!(snapshot.ids, vec![first]);

        // A 的后台查询已返回，但回调延后；B 的捕获回调先应用。
        let second = list.capture("B");
        list.refresh(second, &policy, HistoryRefreshReason::Capture);
        let second_record = list
            .repository
            .find_by_id(second, &HistoryQuery::default())
            .expect("查询已加载的 B")
            .expect("B 存在");
        let mut items = vec![
            record_to_list_item(second_record),
            record_to_list_item(first_record.clone()),
        ];
        let mut loaded: HashMap<_, _> = items
            .iter()
            .cloned()
            .map(|item| (item.history_id, item))
            .collect();
        let applied = replace_list_order(
            &mut list.order_ids,
            &mut list.heatmap,
            &mut list.total,
            &requested_order,
            snapshot,
        );

        assert_eq!(list.order_ids, vec![second, first]);
        assert_eq!(list.total, 2);
        assert_eq!(list.heatmap.buckets().len(), 2);
        assert!(!applied, "冲突快照必须要求重查，不能覆盖较新的列表");

        // 重试原条目查询，沿用已加载的 B，不重新加载整个列表。
        let requested_order = list.order_ids.clone();
        let HistoryItemRefresh::Snapshot { record, snapshot } = load_history_item_refresh(
            &list.repository,
            first,
            &HistoryQuery::default(),
            &policy,
            HistoryRefreshReason::Update,
        )
        .expect("重试 A 的更新查询") else {
            panic!("重试保存仍应返回快照");
        };
        let record = record.expect("重试仍能读取 A");
        assert_eq!(record.preview_text, "A edited");
        assert!(record.favorite_id.is_some());
        assert!(replace_list_order(
            &mut list.order_ids,
            &mut list.heatmap,
            &mut list.total,
            &requested_order,
            snapshot,
        ));
        let positions = list
            .order_ids
            .iter()
            .enumerate()
            .map(|(index, history_id)| (*history_id, index))
            .collect();
        assert!(retain_list_records_for_order(&mut items, &mut loaded, &positions).is_empty());
        assert_eq!(list.order_ids, vec![second, first]);
        assert_eq!(list.total, 2);
        assert_eq!(list.heatmap.buckets().len(), 2);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[&second].preview_text, "B");
        assert_eq!(loaded[&second].shortcut, "Ctrl+1");
        assert_eq!(items[0].history_id, second);
        assert_eq!(items[0].preview_text, "B");
    }

    #[test]
    fn snapshot_reconciles_selection_when_saved_item_leaves_filter() {
        let list = TestList::new();
        let first = list.capture("A");
        let second = list.capture("B");
        let custom_text = CustomTextService::new(
            list.database.clone(),
            RawBlobStore::new(list._directory.path()).expect("打开测试存储"),
        );
        for (history_id, text) in [(first, "keep A"), (second, "keep B")] {
            let record = list
                .repository
                .find_by_id(history_id, &HistoryQuery::default())
                .expect("查询原捕获")
                .expect("原捕获存在");
            custom_text
                .save_for_item(record.item_id, text)
                .expect("创建匹配搜索的自定义文本");
        }
        let query = HistoryQuery {
            search: Some("keep".to_owned()),
            ..HistoryQuery::default()
        };
        let policy = HistoryConfig::default();
        let records = list
            .repository
            .query_by_ids_scoped(&query, &[second, first], &policy)
            .expect("查询原搜索结果");
        let saved_item_id = records
            .iter()
            .find(|record| record.history_id == second)
            .expect("搜索结果包含 B")
            .item_id;
        let cached_items = records
            .into_iter()
            .map(record_to_list_item)
            .collect::<Vec<_>>();
        custom_text
            .save_for_item(saved_item_id, "no longer matches")
            .expect("编辑 B 使其退出搜索结果");
        let HistoryItemRefresh::Snapshot { record, snapshot } = load_history_item_refresh(
            &list.repository,
            second,
            &query,
            &policy,
            HistoryRefreshReason::Update,
        )
        .expect("查询编辑后的搜索结果") else {
            panic!("保存应返回快照");
        };
        assert!(record.is_none());
        assert_eq!(snapshot.ids, vec![first]);
        let positions = snapshot
            .ids
            .iter()
            .enumerate()
            .map(|(index, history_id)| (*history_id, index))
            .collect();

        for (before, expected) in [
            (Some(second), Some(first)),
            (Some(first), Some(first)),
            (None, None),
        ] {
            let mut items = cached_items.clone();
            let mut loaded = items
                .iter()
                .cloned()
                .map(|item| (item.history_id, item))
                .collect();
            let mut selected = before;
            retain_list_records_for_order(&mut items, &mut loaded, &positions);

            reconcile_snapshot_selection(&mut selected, &mut items, &positions);

            assert_eq!(selected, expected, "快照清理前选择：{before:?}");
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].history_id, first);
            assert_eq!(items[0].is_selected, expected == Some(first));
            assert_eq!(items[0].shortcut, "Ctrl+1");
            assert_eq!(loaded.len(), 1);
        }
    }
}
