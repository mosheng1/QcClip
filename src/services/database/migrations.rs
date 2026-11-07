use std::sync::OnceLock;

use rusqlite::{Connection, TransactionBehavior};

use super::{
    connection::DatabaseError,
    models::{CURRENT_SCHEMA_VERSION, DATABASE_READ_VERSION, DATABASE_WRITE_VERSION},
    repository::DatabaseRepository,
};

pub(super) const INITIAL_SCHEMA: &str = include_str!("schema.sql");

pub(super) fn ensure_capture_indexes(connection: &Connection) -> Result<(), DatabaseError> {
    connection
        .execute_batch(include_str!("capture_indexes.sql"))
        .map_err(|source| DatabaseError::Configure { source })
}

pub(super) fn ensure_missing_files_table(connection: &Connection) -> Result<(), DatabaseError> {
    connection
        .execute_batch(include_str!("missing_files.sql"))
        .map_err(|source| DatabaseError::Configure { source })
}

const REQUIRED_TABLES: &[&str] = &[
    "database_compatibility",
    "binary_blobs",
    "application_icon_assets",
    "application_icon_cache",
    "clipboard_items",
    "clipboard_formats",
    "clipboard_history",
    "favorites",
    "tags",
    "clipboard_item_tags",
    "clipboard_item_notes",
];

const REQUIRED_UNIQUE_INDEXES: &[&str] = &["ux_clipboard_formats_primary", "ux_tags_sort_order"];

const REQUIRED_TRIGGERS: &[&str] = &[
    "ck_clipboard_items_start_pending",
    "ck_clipboard_items_ready",
];

#[derive(Clone, Debug, Eq, PartialEq)]
struct Column {
    name: String,
    data_type: String,
    not_null: bool,
    default_value: Option<String>,
    primary_key: i32,
}

struct TableSchema {
    name: &'static str,
    columns: Vec<Column>,
    unique_keys: Vec<Vec<String>>,
    foreign_keys: Vec<(String, String, String, String, String)>,
}

static BASELINE_SCHEMA: OnceLock<Result<Vec<TableSchema>, String>> = OnceLock::new();

pub(super) fn migrate(connection: &mut Connection) -> Result<(), DatabaseError> {
    let version: i32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|source| DatabaseError::Inspect { source })?;
    if version == 0 {
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| DatabaseError::Migration { version: 1, source })?;
        let existing_tables: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        ).map_err(|source| DatabaseError::Inspect { source })?;
        if existing_tables != 0 {
            return Err(DatabaseError::Validation(
                "未标记版本的数据库已包含数据表，请先备份并核验结构，不能作为空库初始化".to_owned(),
            ));
        }
        transaction
            .execute_batch(INITIAL_SCHEMA)
            .map_err(|source| DatabaseError::Migration { version: 1, source })?;
        ensure_capture_indexes(&transaction)?;
        transaction
            .execute_batch(include_str!("sync.sql"))
            .map_err(|source| DatabaseError::Migration { version: 2, source })?;
        ensure_missing_files_table(&transaction)?;
        transaction
            .pragma_update(None, "user_version", CURRENT_SCHEMA_VERSION)
            .map_err(|source| DatabaseError::Migration { version: 1, source })?;
        validate_schema(&transaction)?;
        validate_write_schema(&transaction)?;
        transaction
            .commit()
            .map_err(|source| DatabaseError::Migration { version: 1, source })?;
    }
    if version == 1 {
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| DatabaseError::Migration { version: 2, source })?;
        let history_start = INITIAL_SCHEMA
            .find("CREATE TABLE clipboard_history (")
            .expect("缺少历史表定义");
        let history_end = INITIAL_SCHEMA[history_start..]
            .find("CREATE TABLE favorites (")
            .expect("缺少收藏表定义")
            + history_start;
        let history_schema = &INITIAL_SCHEMA[history_start..history_end];
        let table_end = history_schema.find(";").expect("历史表定义无效") + 1;
        let create =
            history_schema[..table_end].replacen("clipboard_history", "clipboard_history_v2", 1);
        let sql = format!(
            "{create}\nINSERT INTO clipboard_history_v2 SELECT * FROM clipboard_history;\nDROP TABLE clipboard_history;\nALTER TABLE clipboard_history_v2 RENAME TO clipboard_history;\n{}\n{}\nPRAGMA user_version = 2;",
            &history_schema[table_end..],
            include_str!("sync.sql")
        );
        transaction
            .execute_batch(&sql)
            .map_err(|source| DatabaseError::Migration { version: 2, source })?;
        ensure_missing_files_table(&transaction)?;
        validate_write_schema(&transaction)?;
        transaction
            .commit()
            .map_err(|source| DatabaseError::Migration { version: 2, source })?;
    }
    // 高版本只核验兼容性，不回写版本号，也不执行旧程序的初始化语句。
    validate_schema(connection)
}

pub(super) fn validate_schema(connection: &Connection) -> Result<(), DatabaseError> {
    let info = DatabaseRepository::new(connection).database_info()?;
    if info.minimum_read_version > DATABASE_READ_VERSION {
        return Err(DatabaseError::UnsupportedReadVersion {
            required: info.minimum_read_version,
            supported: DATABASE_READ_VERSION,
        });
    }
    for table in baseline_schema()? {
        let actual = columns(connection, table.name)?;
        for required in &table.columns {
            let Some(column) = actual.iter().find(|column| column.name == required.name) else {
                return Err(DatabaseError::Validation(format!(
                    "缺少核心字段“{}.{}”",
                    table.name, required.name,
                )));
            };
            if required.not_null && !column.not_null {
                return Err(DatabaseError::Validation(format!(
                    "核心字段“{}.{}”允许空值，与当前读取规则不兼容",
                    table.name, column.name,
                )));
            }
            if !column.data_type.eq_ignore_ascii_case(&required.data_type) {
                return Err(DatabaseError::Validation(format!(
                    "核心字段“{}.{}”类型应为 {}，实际为 {}",
                    table.name, column.name, required.data_type, column.data_type,
                )));
            }
        }
    }
    Ok(())
}

pub(super) fn validate_write_schema(connection: &Connection) -> Result<(), DatabaseError> {
    let info = DatabaseRepository::new(connection).database_info()?;
    if info.minimum_write_version > DATABASE_WRITE_VERSION {
        return Err(DatabaseError::UnsupportedWriteVersion {
            required: info.minimum_write_version,
            supported: DATABASE_WRITE_VERSION,
        });
    }
    for table in baseline_schema()? {
        for column in columns(connection, table.name)? {
            if let Some(required) = table
                .columns
                .iter()
                .find(|required| required.name == column.name)
            {
                if column.primary_key != required.primary_key
                    || column.default_value != required.default_value
                    || (column.not_null && !required.not_null)
                {
                    return Err(DatabaseError::Validation(format!(
                        "字段“{}.{}”的主键、默认值或必填约束与当前写入规则不兼容",
                        table.name, column.name,
                    )));
                }
            } else if column.not_null
                && column
                    .default_value
                    .as_deref()
                    .is_none_or(|value| value.trim().eq_ignore_ascii_case("null"))
                && column.primary_key == 0
            {
                return Err(DatabaseError::Validation(format!(
                    "新增必填字段“{}.{}”没有可用默认值，当前程序只能读取",
                    table.name, column.name,
                )));
            }
        }
        let actual_foreign_keys = foreign_keys(connection, table.name)?;
        for required in &table.foreign_keys {
            if !actual_foreign_keys.contains(required) {
                return Err(DatabaseError::Validation(format!(
                    "表“{}”缺少写入所需的外键规则：{}",
                    table.name, required.1,
                )));
            }
        }
        let actual_keys = unique_keys(connection, table.name)?;
        for key in &table.unique_keys {
            if !actual_keys.contains(key) {
                return Err(DatabaseError::Validation(format!(
                    "表“{}”缺少写入所需的唯一约束：{}",
                    table.name,
                    key.join(", "),
                )));
            }
        }
    }
    validate_objects(connection, "index", REQUIRED_UNIQUE_INDEXES)?;
    validate_objects(connection, "trigger", REQUIRED_TRIGGERS)
}

fn baseline_schema() -> Result<&'static [TableSchema], DatabaseError> {
    let result = BASELINE_SCHEMA.get_or_init(|| {
        let connection = Connection::open_in_memory().map_err(|error| error.to_string())?;
        connection
            .execute_batch(INITIAL_SCHEMA)
            .map_err(|error| error.to_string())?;
        REQUIRED_TABLES
            .iter()
            .map(|name| {
                Ok(TableSchema {
                    name,
                    columns: columns(&connection, name).map_err(|error| error.to_string())?,
                    unique_keys: unique_keys(&connection, name)
                        .map_err(|error| error.to_string())?,
                    foreign_keys: foreign_keys(&connection, name)
                        .map_err(|error| error.to_string())?,
                })
            })
            .collect()
    });
    match result {
        Ok(tables) => Ok(tables),
        Err(error) => Err(DatabaseError::Validation(format!(
            "无法加载首发数据库结构：{error}"
        ))),
    }
}

fn columns(connection: &Connection, table: &str) -> Result<Vec<Column>, DatabaseError> {
    let mut statement = connection
        .prepare("SELECT name, type, \"notnull\", dflt_value, pk FROM pragma_table_info(?1)")
        .map_err(|source| DatabaseError::Inspect { source })?;
    statement
        .query_map([table], |row| {
            Ok(Column {
                name: row.get(0)?,
                data_type: row.get(1)?,
                not_null: row.get(2)?,
                default_value: row.get(3)?,
                primary_key: row.get(4)?,
            })
        })
        .map_err(|source| DatabaseError::Inspect { source })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| DatabaseError::Inspect { source })
}

fn unique_keys(connection: &Connection, table: &str) -> Result<Vec<Vec<String>>, DatabaseError> {
    let mut statement = connection
        .prepare("SELECT name FROM pragma_index_list(?1) WHERE \"unique\" = 1 AND partial = 0")
        .map_err(|source| DatabaseError::Inspect { source })?;
    let names = statement
        .query_map([table], |row| row.get::<_, String>(0))
        .map_err(|source| DatabaseError::Inspect { source })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| DatabaseError::Inspect { source })?;
    names
        .iter()
        .map(|name| {
            let mut statement = connection
                .prepare("SELECT name FROM pragma_index_info(?1) ORDER BY seqno")
                .map_err(|source| DatabaseError::Inspect { source })?;
            statement
                .query_map([name], |row| row.get::<_, Option<String>>(0))
                .map_err(|source| DatabaseError::Inspect { source })?
                .map(|row| row.map(|name| name.unwrap_or_default()))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|source| DatabaseError::Inspect { source })
        })
        .collect()
}

fn foreign_keys(
    connection: &Connection,
    table: &str,
) -> Result<Vec<(String, String, String, String, String)>, DatabaseError> {
    let mut statement = connection.prepare(
        "SELECT \"table\", \"from\", \"to\", on_update, on_delete FROM pragma_foreign_key_list(?1)",
    ).map_err(|source| DatabaseError::Inspect { source })?;
    statement
        .query_map([table], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .map_err(|source| DatabaseError::Inspect { source })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| DatabaseError::Inspect { source })
}

fn validate_objects(
    connection: &Connection,
    object_type: &str,
    required_names: &[&str],
) -> Result<(), DatabaseError> {
    for name in required_names {
        let exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = ?1 AND name = ?2)",
                [object_type, name],
                |row| row.get(0),
            )
            .map_err(|source| DatabaseError::Inspect { source })?;
        if !exists {
            return Err(DatabaseError::Validation(format!(
                "缺少 {object_type} 对象“{name}”"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rusqlite::{Connection, params};
    use tempfile::TempDir;

    use super::*;
    use crate::services::database::{Database, repository::DatabaseRepository};

    struct TestDatabase {
        _directory: TempDir,
        database: Database,
    }

    impl TestDatabase {
        fn new() -> Self {
            let directory = tempfile::tempdir().expect("无法创建测试临时目录");
            let database =
                Database::open(directory.path().join("test.sqlite3")).expect("无法创建测试数据库");
            Self {
                _directory: directory,
                database,
            }
        }

        fn connect(&self) -> super::super::DatabaseConnection {
            self.database.connect().expect("无法连接测试数据库")
        }
    }

    fn insert_item(connection: &Connection, uuid: &str, hash_byte: u8) -> i64 {
        let hash = [hash_byte; 32];
        connection
            .execute(
                "INSERT INTO clipboard_items (
                uuid, hash_version, content_hash, content_kind, capture_state,
                created_at, updated_at
             ) VALUES (?1, 1, ?2, 'text', 'pending', 1, 1)",
                params![uuid, hash.as_slice()],
            )
            .expect("无法插入测试内容");
        connection.last_insert_rowid()
    }

    fn insert_history_and_favorite(connection: &Connection, item_id: i64) -> (i64, i64) {
        connection
            .execute(
                "INSERT INTO clipboard_history (
                uuid, item_id, first_seen_at, last_seen_at, sort_order, updated_at
             ) VALUES ('history-uuid', ?1, 1, 1, 0, 1)",
                [item_id],
            )
            .expect("无法插入测试历史");
        let history_id = connection.last_insert_rowid();
        connection
            .execute(
                "INSERT INTO favorites (uuid, item_id, is_pinned, sort_order, added_at, updated_at)
             VALUES ('favorite-uuid', ?1, 0, 0, 1, 1)",
                [item_id],
            )
            .expect("无法插入测试收藏");
        (history_id, connection.last_insert_rowid())
    }

    #[test]
    fn existing_database_installs_capture_indexes_once_without_changing_records() {
        let directory = tempfile::tempdir().expect("无法创建测试目录");
        let path = directory.path().join("existing.sqlite3");
        let database = Database::open(&path).expect("无法创建数据库");
        let connection = database.connect_write().expect("无法连接数据库");
        insert_item(&connection, "保留内容", 1);
        connection
            .execute_batch(
                "DROP INDEX ix_clipboard_formats_text_merge;
             DROP INDEX ix_clipboard_formats_native_text_hash;",
            )
            .expect("无法准备缺少索引的旧数据库");
        drop(connection);
        let reopened = Database::open(&path).expect("无法打开现有数据库");
        let connection = reopened.connect().expect("无法连接现有数据库");
        let indexes: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='index' AND name IN
             ('ix_clipboard_formats_text_merge','ix_clipboard_formats_native_text_hash')",
                [],
                |row| row.get(0),
            )
            .expect("无法检查新增索引");
        assert_eq!(indexes, 2);
        assert_eq!(
            connection
                .query_row("SELECT uuid FROM clipboard_items", [], |row| row
                    .get::<_, String>(0))
                .expect("无法读取原记录"),
            "保留内容"
        );
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i32>(0))
                .expect("无法读取数据库版本"),
            CURRENT_SCHEMA_VERSION
        );
        drop(connection);
        Database::open(path).expect("索引应支持重复初始化");
    }

    #[test]
    fn empty_database_creates_full_schema_and_repeated_initialization_is_safe() {
        let directory = tempfile::tempdir().expect("无法创建测试临时目录");
        let path = directory.path().join("nested").join("test.sqlite3");
        let first = Database::open(&path).expect("首次初始化数据库失败");
        let second = Database::open(&path).expect("重复初始化数据库失败");

        assert!(first.path().is_file());
        let connection = second.connect().expect("无法连接数据库");
        let info = DatabaseRepository::new(&connection)
            .database_info()
            .expect("无法读取数据库信息");
        assert_eq!(info.schema_version, CURRENT_SCHEMA_VERSION);
        assert!(info.foreign_keys_enabled);
        assert_eq!(info.journal_mode.to_ascii_lowercase(), "wal");
        assert_eq!(info.busy_timeout_millis, 5_000);

        let image_dimension_columns: i64 = connection
            .query_row(
                "SELECT COUNT(*)
                 FROM pragma_table_info('clipboard_formats')
                 WHERE name IN ('image_width', 'image_height')",
                [],
                |row| row.get(0),
            )
            .expect("无法检查图片尺寸字段");
        assert_eq!(image_dimension_columns, 2);

        let tag_sort_order_columns: i64 = connection
            .query_row(
                "SELECT COUNT(*)
                 FROM pragma_table_info('tags')
                 WHERE name = 'sort_order'",
                [],
                |row| row.get(0),
            )
            .expect("无法检查标签排序字段");
        assert_eq!(tag_sort_order_columns, 1);
    }

    #[test]
    fn foreign_keys_and_content_unique_constraint_are_enforced() {
        let test_database = TestDatabase::new();
        let connection = test_database.connect();
        let item_id = insert_item(&connection, "item-1", 1);
        let duplicate_uuid = connection.execute(
            "INSERT INTO clipboard_items (
                uuid, hash_version, content_hash, content_kind, capture_state,
                created_at, updated_at
             ) VALUES ('item-1', 1, ?1, 'text', 'pending', 1, 1)",
            params![[2_u8; 32].as_slice()],
        );
        let duplicate_hash = connection.execute(
            "INSERT INTO clipboard_items (
                uuid, hash_version, content_hash, content_kind, capture_state,
                created_at, updated_at
             ) VALUES ('item-2', 1, ?1, 'text', 'pending', 1, 1)",
            params![[1_u8; 32].as_slice()],
        );
        let missing_foreign_key = connection.execute(
            "INSERT INTO clipboard_history (
                uuid, item_id, first_seen_at, last_seen_at, sort_order, updated_at
             ) VALUES ('history-missing', 999, 1, 1, 0, 1)",
            [],
        );

        assert!(item_id > 0);
        assert!(duplicate_uuid.is_err());
        assert!(duplicate_hash.is_err());
        assert!(missing_foreign_key.is_err());
    }

    #[test]
    fn tag_schema_requires_normalized_name_and_valid_color() {
        let test_database = TestDatabase::new();
        let connection = test_database.connect();

        connection
            .execute(
                "INSERT INTO tags (
                    uuid, name, normalized_name, created_at, updated_at
                 ) VALUES ('tag-default-color', '默认颜色', '默认颜色', 1, 1)",
                [],
            )
            .expect("标签颜色默认值不应缺失");
        let default_color: String = connection
            .query_row(
                "SELECT color FROM tags WHERE uuid = 'tag-default-color'",
                [],
                |row| row.get(0),
            )
            .expect("无法读取标签默认颜色");
        assert_eq!(default_color, "#1677FF");

        let empty_normalized_name = connection.execute(
            "INSERT INTO tags (
                uuid, name, normalized_name, color, created_at, updated_at
             ) VALUES ('tag-empty-normalized', '空规范名', '   ', '#1677FF', 1, 1)",
            [],
        );
        let invalid_color = connection.execute(
            "INSERT INTO tags (
                uuid, name, normalized_name, color, created_at, updated_at
             ) VALUES ('tag-invalid-color', '无效颜色', '无效颜色', '#GGGGGG', 1, 1)",
            [],
        );

        assert!(empty_normalized_name.is_err());
        assert!(invalid_color.is_err());
    }

    #[test]
    fn format_identity_is_defined_by_namespace_and_key() {
        let test_database = TestDatabase::new();
        let connection = test_database.connect();
        let item_id = insert_item(&connection, "item-format-identity", 9);

        for (uuid, namespace, order, primary, byte, hash) in [
            (
                "format-identity-1",
                "windows",
                0_i64,
                1_i64,
                b"a".as_slice(),
                [1_u8; 32],
            ),
            (
                "format-identity-2",
                "mime",
                1_i64,
                0_i64,
                b"b".as_slice(),
                [2_u8; 32],
            ),
        ] {
            connection
                .execute(
                    "INSERT INTO clipboard_formats (
                        uuid, item_id, format_order, format_namespace, format_key, format_name,
                        format_kind, is_primary, storage_kind, inline_data, byte_length,
                        data_hash, restore_state, capture_state, created_at
                     ) VALUES (
                        ?1, ?2, ?3, ?4, 'same-key', '格式',
                        'text', ?5, 'inline', ?6, 1, ?7, 'restorable', 'captured', 1
                     )",
                    params![
                        uuid,
                        item_id,
                        order,
                        namespace,
                        primary,
                        byte,
                        hash.as_slice()
                    ],
                )
                .expect("不同命名空间的相同格式键不应冲突");
        }
    }

    #[test]
    fn content_without_valid_primary_format_cannot_become_ready() {
        let test_database = TestDatabase::new();
        let connection = test_database.connect();
        let item_id = insert_item(&connection, "item-ready", 3);

        let result = connection.execute(
            "UPDATE clipboard_items
             SET capture_state = 'ready', format_count = 0, total_bytes = 0
             WHERE id = ?1",
            [item_id],
        );

        assert!(result.is_err());
    }

    #[test]
    fn non_restorable_format_cannot_be_primary_format() {
        let test_database = TestDatabase::new();
        let connection = test_database.connect();
        let item_id = insert_item(&connection, "item-format", 4);
        let data = b"abc";
        let hash = [7_u8; 32];

        let result = connection.execute(
            "INSERT INTO clipboard_formats (
                uuid, item_id, format_order, format_namespace, format_key, format_name,
                format_kind, is_primary, storage_kind, inline_data, byte_length,
                data_hash, restore_state, capture_state, created_at
             ) VALUES (
                'format-uuid', ?1, 0, 'windows', 'text', 'CF_UNICODETEXT',
                'text', 1, 'inline', ?2, 3, ?3, 'not_restorable', 'captured', 1
             )",
            params![item_id, data.as_slice(), hash.as_slice()],
        );

        assert!(result.is_err());
    }

    #[test]
    fn preview_fields_must_be_present_together() {
        let test_database = TestDatabase::new();
        let connection = test_database.connect();
        let item_id = insert_item(&connection, "item-preview", 8);
        let data = b"abc";
        let hash = [8_u8; 32];

        let result = connection.execute(
            "INSERT INTO clipboard_formats (
                uuid, item_id, format_order, format_namespace, format_key, format_name,
                format_kind, is_primary, storage_kind, inline_data, byte_length,
                data_hash, restore_state, capture_state, preview_text, created_at
             ) VALUES (
                'format-preview', ?1, 0, 'windows', 'CF_UNICODETEXT', 'CF_UNICODETEXT',
                'text', 1, 'inline', ?2, 3, ?3, 'restorable', 'captured', 'abc', 1
             )",
            params![item_id, data.as_slice(), hash.as_slice()],
        );

        assert!(result.is_err());
    }

    #[test]
    fn clipboard_item_tag_relations_are_unique_and_tag_deletion_preserves_favorites() {
        let test_database = TestDatabase::new();
        let connection = test_database.connect();
        let item_id = insert_item(&connection, "item-tag", 5);
        let (_, favorite_id) = insert_history_and_favorite(&connection, item_id);
        connection
            .execute(
                "INSERT INTO tags (
                uuid, name, normalized_name, color, created_at, updated_at
             ) VALUES ('tag-uuid', '工作', '工作', '#1677FF', 1, 1)",
                [],
            )
            .expect("无法插入测试标签");
        let tag_id = connection.last_insert_rowid();
        connection
            .execute(
                "INSERT INTO clipboard_item_tags (
                uuid, item_id, tag_id, created_at, updated_at
             ) VALUES ('tag-item-1', ?1, ?2, 1, 1)",
                [item_id, tag_id],
            )
            .expect("无法插入测试内容标签关系");

        let duplicate = connection.execute(
            "INSERT INTO clipboard_item_tags (
                uuid, item_id, tag_id, created_at, updated_at
             ) VALUES ('tag-item-2', ?1, ?2, 1, 1)",
            [item_id, tag_id],
        );
        assert!(duplicate.is_err());

        connection
            .execute("DELETE FROM tags WHERE id = ?1", [tag_id])
            .expect("无法删除测试标签");
        let counts: (i64, i64) = connection
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM clipboard_item_tags WHERE tag_id = ?1),
                    (SELECT COUNT(*) FROM favorites WHERE id = ?2)",
                [tag_id, favorite_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("无法检查标签删除结果");
        assert_eq!(counts, (0, 1));
    }

    #[test]
    fn deleting_history_does_not_delete_favorited_content() {
        let test_database = TestDatabase::new();
        let connection = test_database.connect();
        let item_id = insert_item(&connection, "item-delete-history", 6);
        let (history_id, favorite_id) = insert_history_and_favorite(&connection, item_id);

        connection
            .execute("DELETE FROM clipboard_history WHERE id = ?1", [history_id])
            .expect("无法删除测试历史");

        let references: (bool, bool) = connection
            .query_row(
                "SELECT
                EXISTS(SELECT 1 FROM clipboard_items WHERE id = ?1),
                EXISTS(SELECT 1 FROM favorites WHERE id = ?2)",
                [item_id, favorite_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("无法检查内容和收藏");
        assert_eq!(references, (true, true));
    }
}
