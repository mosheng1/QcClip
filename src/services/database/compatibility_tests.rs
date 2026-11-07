use rusqlite::Connection;

use super::query::HistoryQuery;
use super::{
    CURRENT_SCHEMA_VERSION, DATABASE_WRITE_VERSION, Database, HistoryRepository, TagRepository,
};
use crate::domain::{capture::FormatKind, clipboard::ContentKind};
use crate::services::{
    capture::{CaptureService, mock::MockClipboardSource},
    recovery::reconcile_startup,
    storage::RawBlobStore,
};

fn future_database(connection: &Connection, minimum_read: i32, minimum_write: i32) {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS database_compatibility (
            id INTEGER PRIMARY KEY CHECK(id = 1),
            minimum_read_version INTEGER NOT NULL,
            minimum_write_version INTEGER NOT NULL
         );
         DELETE FROM database_compatibility;",
        )
        .expect("无法准备兼容声明");
    connection
        .execute(
            "INSERT INTO database_compatibility VALUES (1, ?1, ?2)",
            [minimum_read, minimum_write],
        )
        .expect("无法写入兼容声明");
    let version: i32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("无法读取结构版本");
    connection
        .pragma_update(None, "user_version", version + 1)
        .expect("无法设置新版结构");
}

#[test]
fn future_database_does_not_install_current_capture_indexes() {
    let directory = tempfile::tempdir().expect("无法创建临时目录");
    let path = directory.path().join("future.sqlite3");
    let database = Database::open(&path).expect("无法创建数据库");
    let connection = database.connect_write().expect("无法连接数据库");
    connection
        .execute_batch(
            "DROP INDEX ix_clipboard_formats_text_merge;
         DROP INDEX ix_clipboard_formats_native_text_hash;",
        )
        .expect("无法准备新版数据库索引");
    future_database(&connection, 1, 1);
    drop(connection);
    let database = Database::open(path).expect("无法打开兼容数据库");
    let connection = database.connect().expect("无法连接兼容数据库");
    let indexes: i64 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='index' AND name IN
         ('ix_clipboard_formats_text_merge','ix_clipboard_formats_native_text_hash')",
            [],
            |row| row.get(0),
        )
        .expect("无法检查新版数据库索引");
    assert_eq!(indexes, 0);
}

#[test]
fn new_database_uses_current_schema_version() {
    let directory = tempfile::tempdir().expect("无法创建临时目录");
    let database = Database::open(directory.path().join("test.sqlite3")).expect("无法创建数据库");
    let connection = database.connect().expect("无法连接数据库");
    let version: i32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("无法读取结构版本");
    assert_eq!(version, CURRENT_SCHEMA_VERSION);
}

#[test]
fn opening_existing_database_adds_hidden_missing_items_table() {
    let directory = tempfile::tempdir().expect("无法创建临时目录");
    let path = directory.path().join("test.sqlite3");
    let database = Database::open(&path).expect("无法创建数据库");
    assert!(database.has_hidden_items_table());
    database
        .connect_write()
        .expect("无法连接数据库")
        .execute_batch("DROP TABLE hidden_missing_items;")
        .expect("无法移除隐藏表");
    drop(database);

    let reopened = Database::open(&path).expect("无法重新打开数据库");
    assert!(
        reopened.has_hidden_items_table(),
        "opening an existing schema must restore the additive hidden table"
    );
}

#[test]
fn compatible_future_schema_keeps_revision_and_unknown_columns() {
    let directory = tempfile::tempdir().expect("无法创建临时目录");
    let path = directory.path().join("test.sqlite3");
    let database = Database::open(&path).expect("无法创建数据库");
    let connection = database.connect().expect("无法连接数据库");
    connection
        .execute_batch(
            "ALTER TABLE tags ADD COLUMN future_value TEXT NOT NULL DEFAULT '保留';
         CREATE TABLE future_settings (value TEXT);
         INSERT INTO future_settings VALUES ('新版设置');",
        )
        .expect("无法添加扩展字段");
    future_database(&connection, 1, 1);
    drop(connection);

    let database = Database::open(&path).expect("兼容新版数据库应允许打开");
    let tags = TagRepository::new(database.clone());
    let tag = tags
        .create("兼容标签", "#1677FF")
        .expect("兼容新版数据库应允许写入");
    let connection = database.connect().expect("无法连接新版数据库");
    let value: String = connection
        .query_row(
            "SELECT future_value FROM tags WHERE id = ?1",
            [tag.id],
            |row| row.get(0),
        )
        .expect("无法查询扩展字段");
    assert_eq!(value, "保留");
    let version: i32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("无法查询结构版本");
    assert_eq!(version, CURRENT_SCHEMA_VERSION + 1);
    let value: String = connection
        .query_row("SELECT value FROM future_settings", [], |row| row.get(0))
        .expect("无法查询扩展表");
    assert_eq!(value, "新版设置");
    drop(connection);
    Database::open(&path).expect("再次打开兼容新版数据库失败");
}

#[test]
fn future_schema_can_be_read_only_without_losing_history() {
    let directory = tempfile::tempdir().expect("无法创建临时目录");
    let path = directory.path().join("test.sqlite3");
    let database = Database::open(&path).expect("无法创建数据库");
    let store = RawBlobStore::new(directory.path()).expect("无法创建存储");
    CaptureService::new(database.clone(), store.clone())
        .store_captured_clipboard(&MockClipboardSource::text("已有文本").capture())
        .expect("无法捕获测试内容");
    let connection = database.connect().expect("无法连接数据库");
    future_database(&connection, 1, DATABASE_WRITE_VERSION + 1);
    drop(connection);

    let database = Database::open(&path).expect("只读兼容数据库应允许打开");
    let history = HistoryRepository::new(database.clone(), store);
    let records = history
        .query(&HistoryQuery::default())
        .expect("只读模式应允许查询");
    assert_eq!(records.len(), 1);
    let connection = database.connect().expect("只读模式应允许连接");
    assert!(
        connection
            .is_readonly(rusqlite::MAIN_DB)
            .expect("无法检查只读状态")
    );
    assert!(
        connection
            .execute("DELETE FROM clipboard_history", [])
            .is_err()
    );
    assert!(
        TagRepository::new(database)
            .create("禁止写入", "#1677FF")
            .is_err()
    );
}

#[test]
fn incompatible_reader_is_rejected_without_changing_database() {
    let directory = tempfile::tempdir().expect("无法创建临时目录");
    let path = directory.path().join("test.sqlite3");
    let database = Database::open(&path).expect("无法创建数据库");
    let connection = database.connect().expect("无法连接数据库");
    future_database(&connection, 2, 2);
    drop(connection);
    assert!(Database::open(&path).is_err());
    let connection = Connection::open(&path).expect("无法核验数据库");
    let version: i32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("无法查询结构版本");
    assert_eq!(version, CURRENT_SCHEMA_VERSION + 1);
}

#[test]
fn added_required_column_without_default_falls_back_to_read_only() {
    let directory = tempfile::tempdir().expect("无法创建临时目录");
    let path = directory.path().join("test.sqlite3");
    let database = Database::open(&path).expect("无法创建数据库");
    let connection = database.connect().expect("无法连接数据库");
    connection
        .execute_batch("ALTER TABLE tags ADD COLUMN future_required TEXT NOT NULL;")
        .expect("无法模拟新版必填字段");
    future_database(&connection, 1, 1);
    drop(connection);
    let database = Database::open(&path).expect("核心结构可读时应允许查看");
    let connection = database.connect().expect("无法连接数据库");
    assert!(
        connection
            .is_readonly(rusqlite::MAIN_DB)
            .expect("无法检查只读状态")
    );
}

#[test]
fn future_schema_without_required_trigger_remains_readable() {
    let directory = tempfile::tempdir().expect("无法创建临时目录");
    let path = directory.path().join("test.sqlite3");
    let database = Database::open(&path).expect("无法创建数据库");
    let connection = database.connect().expect("无法连接数据库");
    connection
        .execute_batch("DROP TRIGGER ck_clipboard_items_ready;")
        .expect("无法模拟缺少写入触发器");
    future_database(&connection, 1, 1);
    drop(connection);
    let database = Database::open(&path).expect("写入规则缺失仍应允许读取");
    let connection = database.connect().expect("无法连接数据库");
    assert!(
        connection
            .is_readonly(rusqlite::MAIN_DB)
            .expect("无法检查只读状态")
    );
}

#[test]
fn performance_indexes_are_not_required_to_read_future_schema() {
    let directory = tempfile::tempdir().expect("无法创建临时目录");
    let path = directory.path().join("test.sqlite3");
    let database = Database::open(&path).expect("无法创建数据库");
    let connection = database.connect().expect("无法连接数据库");
    connection
        .execute_batch("DROP INDEX ix_clipboard_items_capture_state_id;")
        .expect("无法模拟索引改名");
    future_database(&connection, 1, 1);
    drop(connection);
    let database = Database::open(&path).expect("性能索引变化不应拒绝兼容数据库");
    TagRepository::new(database)
        .create("可写", "#1677FF")
        .expect("性能索引变化不应禁止写入");
}

#[test]
fn missing_core_column_is_rejected() {
    let directory = tempfile::tempdir().expect("无法创建临时目录");
    let path = directory.path().join("test.sqlite3");
    let database = Database::open(&path).expect("无法创建数据库");
    let connection = database.connect().expect("无法连接数据库");
    connection
        .execute_batch("ALTER TABLE clipboard_item_notes DROP COLUMN updated_at;")
        .expect("无法模拟缺少核心字段");
    future_database(&connection, 1, 1);
    drop(connection);
    assert!(Database::open(&path).is_err());
}

#[test]
fn unknown_types_do_not_break_the_page_or_change_original_values() {
    let directory = tempfile::tempdir().expect("无法创建临时目录");
    let database = Database::open(directory.path().join("test.sqlite3")).expect("无法创建数据库");
    let store = RawBlobStore::new(directory.path()).expect("无法创建存储");
    let capture = CaptureService::new(database.clone(), store.clone());
    capture
        .store_captured_clipboard(&MockClipboardSource::text("普通文本").capture())
        .expect("无法捕获普通文本");
    let history_id = capture
        .store_captured_clipboard(&MockClipboardSource::text("新版内容").capture())
        .expect("无法捕获新版内容");
    let connection = database.connect().expect("无法连接数据库");
    connection
        .execute_batch("PRAGMA ignore_check_constraints = ON;")
        .expect("无法模拟新版枚举");
    connection
        .execute(
            "UPDATE clipboard_items SET content_kind = 'future_content'
         WHERE id = (SELECT item_id FROM clipboard_history WHERE id = ?1)",
            [history_id],
        )
        .expect("无法模拟未知内容类型");
    connection
        .execute(
            "INSERT INTO clipboard_formats (
            uuid, item_id, format_order, format_namespace, format_key, format_name,
            format_kind, is_primary, storage_kind, inline_data, byte_length, data_hash,
            restore_state, preview_text, preview_char_count, preview_content_kind,
            capture_state, created_at
         )
         SELECT 'future-format', item_id, 100, 'future', 'new-format', '新版格式',
            'future_format', 0, storage_kind, inline_data, byte_length, data_hash,
            restore_state, '新版预览', 4, 'future_content', capture_state, created_at
         FROM clipboard_formats
         WHERE item_id = (SELECT item_id FROM clipboard_history WHERE id = ?1)
         LIMIT 1",
            [history_id],
        )
        .expect("无法模拟未知格式");
    connection.execute(
        "UPDATE clipboard_items
         SET format_count = (SELECT COUNT(*) FROM clipboard_formats WHERE item_id = clipboard_items.id),
             total_bytes = (SELECT SUM(byte_length) FROM clipboard_formats WHERE item_id = clipboard_items.id)
         WHERE id = (SELECT item_id FROM clipboard_history WHERE id = ?1)",
        [history_id],
    ).expect("无法更新测试统计");
    drop(connection);

    let records = HistoryRepository::new(database.clone(), store)
        .query(&HistoryQuery::default())
        .expect("未知类型不应使整页失败");
    assert_eq!(records.len(), 2);
    let record = records
        .iter()
        .find(|record| record.history_id == history_id)
        .expect("新版记录丢失");
    let preview = record
        .all_format_options
        .iter()
        .find(|preview| preview.format_key == "new-format")
        .expect("未知格式预览丢失");
    assert_eq!(preview.format_kind, FormatKind::Unknown);
    assert_eq!(preview.content_kind, ContentKind::Unknown);
    assert!(!preview.is_restorable);
    let connection = database.connect().expect("无法核验原始类型");
    let kind: String = connection
        .query_row(
            "SELECT format_kind FROM clipboard_formats WHERE uuid = 'future-format'",
            [],
            |row| row.get(0),
        )
        .expect("无法查询原始格式类型");
    assert_eq!(kind, "future_format");
}

#[test]
fn future_schema_skips_startup_recovery_and_resource_collection() {
    let directory = tempfile::tempdir().expect("无法创建临时目录");
    let database = Database::open(directory.path().join("test.sqlite3")).expect("无法创建数据库");
    let store = RawBlobStore::new(directory.path()).expect("无法创建存储");
    let pending = store
        .write_pending("新版未完成数据".as_bytes())
        .expect("无法创建未完成文件");
    let history_id = CaptureService::with_inline_threshold(database.clone(), store.clone(), 0)
        .store_captured_clipboard(&MockClipboardSource::text("外部数据").capture())
        .expect("无法捕获外部数据");
    let connection = database.connect().expect("无法连接数据库");
    connection
        .execute_batch(
            "CREATE TABLE future_blob_refs (
            blob_id INTEGER REFERENCES binary_blobs(id) ON DELETE CASCADE
         );
         INSERT INTO future_blob_refs SELECT id FROM binary_blobs;",
        )
        .expect("无法模拟新版资源引用");
    future_database(&connection, 1, 1);
    let relative_path: String = connection
        .query_row(
            "SELECT relative_path FROM binary_blobs LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("无法读取原始数据路径");
    drop(connection);

    let report = reconcile_startup(&database, &store).expect("兼容模式启动不应失败");
    assert_eq!(report.pending_files_removed, 0);
    assert_eq!(
        std::fs::read_dir(directory.path().join("data/.pending"))
            .unwrap()
            .count(),
        1
    );
    let history = HistoryRepository::new(database.clone(), store);
    history.delete(history_id).expect("删除历史关系失败");
    let report = history.cleanup_orphaned().expect("兼容模式应暂停资源清理");
    assert_eq!(report.orphan_blobs_removed, 0);
    assert!(directory.path().join(relative_path).exists());
    let connection = database.connect().expect("无法核验新版资源引用");
    let count: i64 = connection
        .query_row("SELECT COUNT(*) FROM future_blob_refs", [], |row| {
            row.get(0)
        })
        .expect("无法查询新版资源引用");
    assert_eq!(count, 1);
    drop(pending);
}

#[test]
fn list_read_connection_is_read_only_and_rechecks_compatibility() {
    let directory = tempfile::tempdir().expect("无法创建临时目录");
    let database = Database::open(directory.path().join("read.sqlite3")).expect("无法创建数据库");
    let reader = database.connect_read().expect("无法创建只读连接");
    assert!(
        reader
            .is_readonly(rusqlite::MAIN_DB)
            .expect("无法检查只读状态")
    );
    assert!(reader.execute("DELETE FROM tags", []).is_err());
    drop(reader);

    let writer = database.connect_write().expect("无法创建写入连接");
    future_database(&writer, 1, 2);
    assert!(database.connect_read().is_ok());
    writer
        .execute(
            "UPDATE database_compatibility SET minimum_read_version = 2",
            [],
        )
        .expect("无法修改读取版本");
    assert!(matches!(
        database.connect_read(),
        Err(super::DatabaseError::UnsupportedReadVersion { .. })
    ));
}

#[test]
fn list_read_connection_rejects_core_schema_changes_after_open() {
    let directory = tempfile::tempdir().expect("无法创建临时目录");
    let database = Database::open(directory.path().join("read.sqlite3")).expect("无法创建数据库");
    let writer = database.connect_write().expect("无法创建写入连接");
    writer
        .execute_batch("ALTER TABLE tags RENAME COLUMN name TO future_name;")
        .expect("无法模拟核心字段变化");
    assert!(matches!(
        database.connect_read(),
        Err(super::DatabaseError::Validation(_))
    ));
}
