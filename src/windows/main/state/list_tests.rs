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
        }
    }

    fn capture(&self, text: &str) -> HistoryItemId {
        self.capture
            .store_captured_clipboard(&MockClipboardSource::text(text).capture())
            .expect("保存捕获")
    }
}

#[test]
fn stale_image_failure_cannot_finish_a_restarted_load() {
    let key = ImagePreviewKey::new(42, "windows", "CF_HDROP");
    let mut loading = HashMap::from([(key.clone(), 1)]);
    loading.remove(&key); // 文件状态变化取消旧任务
    loading.insert(key.clone(), 2); // 恢复后重试同一格式

    assert!(!finish_image_load(&mut loading, &key, 1));
    assert_eq!(loading.get(&key), Some(&2));
    assert!(finish_image_load(&mut loading, &key, 2));
    assert!(!loading.contains_key(&key));
}

#[test]
fn saved_capture_resolves_the_row_for_the_active_tab() {
    let list = TestList::new();
    list.capture("unfavorited history");
    let history_id = list.capture("captured selection");
    let record = list
        .repository
        .find_by_id(history_id, &HistoryQuery::default())
        .unwrap()
        .unwrap();
    let favorites = FavoriteRepository::new(list.database.clone());
    favorites.add_to_favorites(record.item_id).unwrap();
    let favorite_id = favorites
        .favorite_id_for_item(record.item_id)
        .unwrap()
        .unwrap();
    assert_ne!(favorite_id, history_id);

    assert_eq!(
        saved_item_row_id(
            record.item_id,
            TabId::Favorites,
            &list.repository,
            &favorites
        )
        .unwrap(),
        Some(favorite_id)
    );
    assert_eq!(
        saved_item_row_id(
            record.item_id,
            TabId::Clipboard,
            &list.repository,
            &favorites
        )
        .unwrap(),
        Some(history_id)
    );
    assert_eq!(
        saved_item_row_id(record.item_id, TabId::Emoji, &list.repository, &favorites).unwrap(),
        None
    );
}

#[test]
fn saved_manual_favorite_without_history_resolves_only_the_favorite_row() {
    let list = TestList::new();
    list.capture("unfavorited history");
    let custom_text = CustomTextService::new(
        list.database.clone(),
        RawBlobStore::new(list._directory.path()).unwrap(),
    );
    let history_id = custom_text
        .create_manual_text_and_favorite_with_tags("manual", &[])
        .unwrap();
    let item_id = custom_text
        .find_manual_text_item("manual")
        .unwrap()
        .unwrap();
    list.database
        .connect()
        .unwrap()
        .execute("DELETE FROM clipboard_history WHERE id = ?1", [history_id])
        .unwrap();
    let favorites = FavoriteRepository::new(list.database.clone());
    let favorite_id = favorites.favorite_id_for_item(item_id).unwrap().unwrap();
    assert_ne!(favorite_id, item_id);

    assert_eq!(
        saved_item_row_id(item_id, TabId::Favorites, &list.repository, &favorites).unwrap(),
        Some(favorite_id)
    );
    assert_eq!(
        saved_item_row_id(item_id, TabId::Clipboard, &list.repository, &favorites).unwrap(),
        None
    );
}

#[test]
fn saving_an_older_quick_favorite_preserves_database_history_order() {
    let list = TestList::new();
    let first = list.capture("A");
    let second = list.capture("B");
    let query = HistoryQuery::default();
    let policy = HistoryConfig::default();
    let record = list.repository.find_by_id(first, &query).unwrap().unwrap();
    let before = list
        .repository
        .ordered_snapshot_scoped(&query, &policy)
        .unwrap();
    let favorites = FavoriteRepository::new(list.database.clone());

    CustomTextService::new(
        list.database.clone(),
        RawBlobStore::new(list._directory.path()).unwrap(),
    )
    .save_for_item_and_favorite_with_tags(record.item_id, "A edited", &[])
    .unwrap();

    let row_id = saved_item_row_id(
        record.item_id,
        TabId::Clipboard,
        &list.repository,
        &favorites,
    )
    .unwrap()
    .unwrap();
    let updated = list
        .repository
        .find_by_id_scoped(row_id, &query, &policy)
        .unwrap()
        .unwrap();
    let after = list
        .repository
        .ordered_snapshot_scoped(&query, &policy)
        .unwrap();
    assert_eq!(row_id, first);
    assert_eq!(updated.preview_text, "A edited");
    assert!(updated.favorite_id.is_some());
    assert_eq!(before.ids, vec![second, first]);
    assert_eq!(after.ids, before.ids);
}
