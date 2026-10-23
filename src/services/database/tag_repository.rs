use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{OptionalExtension, Row, TransactionBehavior, params};
use thiserror::Error;
use uuid::Uuid;

use crate::domain::clipboard::{ClipboardItemId, Tag};

use super::Database;

#[derive(Clone, Debug)]
pub struct TagRepository {
    database: Database,
}

#[derive(Debug, Error)]
pub enum TagQueryError {
    #[error("无法连接标签数据库：{0}")]
    Database(#[from] super::DatabaseError),
    #[error("查询标签失败：{0}")]
    Sql(#[from] rusqlite::Error),
}

#[derive(Debug, Error)]
pub enum TagCreateError {
    #[error("无法连接标签数据库：{0}")]
    Database(#[from] super::DatabaseError),
    #[error("标签名称不能为空")]
    EmptyName,
    #[error("标签名称不能超过 100 个字符")]
    NameTooLong,
    #[error("标签名称“{0}”已存在")]
    DuplicateName(String),
    #[error("标签颜色必须是 #RRGGBB 格式")]
    InvalidColor,
    #[error("标签排序值超出支持范围")]
    SortOrderOverflow,
    #[error("创建标签失败：{0}")]
    Sql(#[from] rusqlite::Error),
}

#[derive(Debug, Error)]
pub enum TagUpdateError {
    #[error("无法连接标签数据库：{0}")]
    Database(#[from] super::DatabaseError),
    #[error("标签名称不能为空")]
    EmptyName,
    #[error("标签名称不能超过 100 个字符")]
    NameTooLong,
    #[error("标签名称“{0}”已存在")]
    DuplicateName(String),
    #[error("标签颜色必须是 #RRGGBB 格式")]
    InvalidColor,
    #[error("未找到要修改的标签：{0}")]
    NotFound(i64),
    #[error("修改标签失败：{0}")]
    Sql(#[from] rusqlite::Error),
}

#[derive(Debug, Error)]
pub enum TagReorderError {
    #[error("无法连接标签数据库：{0}")]
    Database(#[from] super::DatabaseError),
    #[error("调整标签排序失败：{0}")]
    Sql(#[from] rusqlite::Error),
    #[error("标签排序值超出支持范围")]
    SortOrderOverflow,
}

#[derive(Debug, Error)]
pub enum TagDeleteError {
    #[error("无法连接标签数据库：{0}")]
    Database(#[from] super::DatabaseError),
    #[error("删除标签失败：{0}")]
    Sql(#[from] rusqlite::Error),
}

#[derive(Debug, Error)]
pub enum TagItemWriteError {
    #[error("无法连接标签数据库：{0}")]
    Database(#[from] super::DatabaseError),
    #[error("添加内容标签关系失败：{0}")]
    Sql(#[from] rusqlite::Error),
}

#[derive(Debug, Error)]
pub enum TagItemDeleteError {
    #[error("无法连接标签数据库：{0}")]
    Database(#[from] super::DatabaseError),
    #[error("移除内容标签关系失败：{0}")]
    Sql(#[from] rusqlite::Error),
}

impl TagRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub fn list(&self) -> Result<Vec<Tag>, TagQueryError> {
        let connection = self.database.connect()?;
        let mut statement = connection.prepare(
            "SELECT
                id,
                uuid,
                name,
                normalized_name,
                color,
                sort_order,
                created_at,
                updated_at
             FROM tags
             ORDER BY sort_order DESC, id DESC",
        )?;
        let rows = statement.query_map([], tag_from_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(TagQueryError::from)
    }

    pub fn item_tag_ids(&self, item_id: ClipboardItemId) -> Result<Vec<i64>, TagQueryError> {
        let connection = self.database.connect()?;
        let mut statement = connection.prepare(
            "SELECT tag_id
             FROM clipboard_item_tags
             WHERE item_id = ?1
             ORDER BY tag_id",
        )?;
        let rows = statement.query_map([item_id], |row| row.get::<_, i64>(0))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(TagQueryError::from)
    }

    pub fn create(&self, name: &str, color: &str) -> Result<Tag, TagCreateError> {
        let name = validate_name(name).map_err(|error| match error {
            NameError::Empty => TagCreateError::EmptyName,
            NameError::TooLong => TagCreateError::NameTooLong,
        })?;
        let color = normalize_color(color).ok_or(TagCreateError::InvalidColor)?;
        let normalized_name = normalize_name(name);

        let mut connection = self.database.connect_write()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if transaction
            .query_row(
                "SELECT 1 FROM tags WHERE normalized_name = ?1",
                [&normalized_name],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            return Err(TagCreateError::DuplicateName(name.to_owned()));
        }

        let timestamp = current_timestamp_millis();
        let uuid = Uuid::new_v4().to_string();
        let sort_order: i64 = transaction
            .query_row(
                "SELECT COALESCE(MAX(sort_order), -1) FROM tags",
                [],
                |row| row.get::<_, i64>(0),
            )?
            .checked_add(1)
            .ok_or(TagCreateError::SortOrderOverflow)?;
        transaction.execute(
            "INSERT INTO tags (
                uuid, name, normalized_name, color, sort_order, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![&uuid, name, normalized_name, color, sort_order, timestamp],
        )?;
        let id = transaction.last_insert_rowid();
        transaction.commit()?;

        Ok(Tag {
            id,
            uuid,
            name: name.to_owned(),
            normalized_name,
            color,
            sort_order,
            created_at: timestamp,
            updated_at: timestamp,
        })
    }

    pub fn update(&self, tag_id: i64, name: &str, color: &str) -> Result<Tag, TagUpdateError> {
        let name = validate_name(name).map_err(|error| match error {
            NameError::Empty => TagUpdateError::EmptyName,
            NameError::TooLong => TagUpdateError::NameTooLong,
        })?;
        let color = normalize_color(color).ok_or(TagUpdateError::InvalidColor)?;
        let normalized_name = normalize_name(name);

        let mut connection = self.database.connect_write()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if transaction
            .query_row("SELECT 1 FROM tags WHERE id = ?1", [tag_id], |_| Ok(()))
            .optional()?
            .is_none()
        {
            return Err(TagUpdateError::NotFound(tag_id));
        }
        if transaction
            .query_row(
                "SELECT 1
                 FROM tags
                 WHERE normalized_name = ?1 AND id <> ?2",
                params![&normalized_name, tag_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            return Err(TagUpdateError::DuplicateName(name.to_owned()));
        }

        let timestamp = current_timestamp_millis();
        transaction.execute(
            "UPDATE tags
             SET name = ?1, normalized_name = ?2, color = ?3, updated_at = ?4
             WHERE id = ?5",
            params![name, normalized_name, color, timestamp, tag_id],
        )?;
        let tag = transaction.query_row(
            "SELECT
                id,
                uuid,
                name,
                normalized_name,
                color,
                sort_order,
                created_at,
                updated_at
             FROM tags
             WHERE id = ?1",
            [tag_id],
            tag_from_row,
        )?;
        transaction.commit()?;
        Ok(tag)
    }

    pub fn reorder(&self, tag_id: i64, target_tag_id: i64) -> Result<bool, TagReorderError> {
        if tag_id <= 0 || target_tag_id <= 0 || tag_id == target_tag_id {
            return Ok(false);
        }

        let mut connection = self.database.connect_write()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let source_exists = transaction
            .query_row("SELECT 1 FROM tags WHERE id = ?1", [tag_id], |_| Ok(()))
            .optional()?
            .is_some();
        if !source_exists {
            transaction.commit()?;
            return Ok(false);
        }
        let target_exists = transaction
            .query_row("SELECT 1 FROM tags WHERE id = ?1", [target_tag_id], |_| {
                Ok(())
            })
            .optional()?
            .is_some();
        if !target_exists {
            transaction.commit()?;
            return Ok(false);
        }

        let mut ordered_ids = {
            let mut statement = transaction.prepare(
                "SELECT id
                 FROM tags
                 ORDER BY sort_order DESC, id DESC",
            )?;
            statement
                .query_map([], |row| row.get::<_, i64>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let Some(source_index) = ordered_ids.iter().position(|id| *id == tag_id) else {
            transaction.commit()?;
            return Ok(false);
        };
        ordered_ids.remove(source_index);
        let Some(target_index) = ordered_ids.iter().position(|id| *id == target_tag_id) else {
            transaction.commit()?;
            return Ok(false);
        };
        let insert_index = target_index + usize::from(source_index <= target_index);
        ordered_ids.insert(insert_index, tag_id);

        rewrite_tag_order(&transaction, &ordered_ids)?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn delete_many(&self, tag_ids: &[i64]) -> Result<usize, TagDeleteError> {
        if tag_ids.is_empty() {
            return Ok(0);
        }

        let mut connection = self.database.connect_write()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut deleted = 0;
        for tag_id in tag_ids {
            deleted += transaction.execute("DELETE FROM tags WHERE id = ?1", [tag_id])?;
        }
        transaction.commit()?;
        Ok(deleted)
    }

    pub fn add_to_item(
        &self,
        item_id: ClipboardItemId,
        tag_id: i64,
    ) -> Result<bool, TagItemWriteError> {
        let mut connection = self.database.connect_write()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let timestamp = current_timestamp_millis();
        let inserted = transaction.execute(
            "INSERT INTO clipboard_item_tags (
                uuid, item_id, tag_id, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(item_id, tag_id) DO NOTHING",
            params![Uuid::new_v4().to_string(), item_id, tag_id, timestamp],
        )?;
        transaction.commit()?;
        Ok(inserted > 0)
    }

    pub fn remove_from_item(
        &self,
        item_id: ClipboardItemId,
        tag_id: i64,
    ) -> Result<bool, TagItemDeleteError> {
        let mut connection = self.database.connect_write()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let deleted = transaction.execute(
            "DELETE FROM clipboard_item_tags
             WHERE item_id = ?1 AND tag_id = ?2",
            params![item_id, tag_id],
        )?;
        transaction.commit()?;
        Ok(deleted > 0)
    }
}

fn tag_from_row(row: &Row<'_>) -> rusqlite::Result<Tag> {
    Ok(Tag {
        id: row.get(0)?,
        uuid: row.get(1)?,
        name: row.get(2)?,
        normalized_name: row.get(3)?,
        color: row.get(4)?,
        sort_order: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

fn rewrite_tag_order(
    transaction: &rusqlite::Transaction<'_>,
    ordered_ids: &[i64],
) -> Result<(), TagReorderError> {
    let max_sort_order: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(sort_order), -1) FROM tags",
        [],
        |row| row.get(0),
    )?;
    let temporary_offset = max_sort_order
        .checked_add(1)
        .ok_or(TagReorderError::SortOrderOverflow)?;
    if max_sort_order > i64::MAX - temporary_offset {
        return Err(TagReorderError::SortOrderOverflow);
    }
    transaction.execute(
        "UPDATE tags SET sort_order = sort_order + ?1",
        [temporary_offset],
    )?;

    let timestamp = current_timestamp_millis();
    for (index, tag_id) in ordered_ids.iter().enumerate() {
        let sort_order = i64::try_from(ordered_ids.len() - index - 1)
            .map_err(|_| TagReorderError::SortOrderOverflow)?;
        transaction.execute(
            "UPDATE tags
             SET sort_order = ?1, updated_at = ?2
             WHERE id = ?3",
            params![sort_order, timestamp, tag_id],
        )?;
    }
    Ok(())
}

enum NameError {
    Empty,
    TooLong,
}

fn validate_name(name: &str) -> Result<&str, NameError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    if name.chars().count() > 100 {
        return Err(NameError::TooLong);
    }
    Ok(name)
}

fn normalize_name(name: &str) -> String {
    name.to_lowercase()
}

fn normalize_color(color: &str) -> Option<String> {
    let value = color.trim().strip_prefix('#').unwrap_or(color.trim());
    if value.len() != 6 || !value.chars().all(|character| character.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("#{value}").to_ascii_uppercase())
}

fn current_timestamp_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creating_tag_persists_color_and_identity() {
        let directory = tempfile::tempdir().expect("无法创建测试临时目录");
        let database =
            Database::open(directory.path().join("test.sqlite3")).expect("无法创建测试数据库");
        let repository = TagRepository::new(database);

        let tag = repository
            .create(" 工作 ", "#1677ff")
            .expect("无法创建测试标签");

        assert!(tag.id > 0);
        assert!(!tag.uuid.is_empty());
        assert_eq!(tag.name, "工作");
        assert_eq!(tag.normalized_name, "工作");
        assert_eq!(tag.color, "#1677FF");
        assert_eq!(repository.list().expect("无法查询标签"), vec![tag]);
    }

    #[test]
    fn listing_tags_returns_persisted_order_first() {
        let directory = tempfile::tempdir().expect("无法创建测试临时目录");
        let database =
            Database::open(directory.path().join("test.sqlite3")).expect("无法创建测试数据库");
        let repository = TagRepository::new(database.clone());
        let connection = database.connect().expect("无法连接测试数据库");

        connection
            .execute(
                "INSERT INTO tags (
                    uuid, name, normalized_name, color, sort_order, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, '#1677FF', ?4, ?5, ?5)",
                params!["tag-old", "旧标签", "旧标签", 0_i64, 100_i64],
            )
            .expect("无法插入旧标签");
        connection
            .execute(
                "INSERT INTO tags (
                    uuid, name, normalized_name, color, sort_order, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, '#00FF00', ?4, ?5, ?5)",
                params!["tag-new", "新标签", "新标签", 1_i64, 200_i64],
            )
            .expect("无法插入新标签");

        let names: Vec<_> = repository
            .list()
            .expect("无法查询标签")
            .into_iter()
            .map(|tag| tag.name)
            .collect();
        assert_eq!(names, vec!["新标签", "旧标签"]);
    }

    #[test]
    fn creating_tag_appends_to_the_end_of_the_persisted_order() {
        let directory = tempfile::tempdir().expect("无法创建测试临时目录");
        let database =
            Database::open(directory.path().join("test.sqlite3")).expect("无法创建测试数据库");
        let repository = TagRepository::new(database);

        let first = repository
            .create("第一个", "#1677FF")
            .expect("无法创建第一个测试标签");
        let second = repository
            .create("第二个", "#00FF00")
            .expect("无法创建第二个测试标签");

        assert_eq!(first.sort_order, 0);
        assert_eq!(second.sort_order, 1);
        assert_eq!(
            repository
                .list()
                .expect("无法查询标签")
                .into_iter()
                .map(|tag| tag.name)
                .collect::<Vec<_>>(),
            vec!["第二个", "第一个"]
        );
    }

    #[test]
    fn reordering_tags_rewrites_the_order_and_preserves_metadata() {
        let directory = tempfile::tempdir().expect("无法创建测试临时目录");
        let database =
            Database::open(directory.path().join("test.sqlite3")).expect("无法创建测试数据库");
        let repository = TagRepository::new(database);
        let first = repository
            .create("第一个", "#1677FF")
            .expect("无法创建第一个测试标签");
        let second = repository
            .create("第二个", "#00FF00")
            .expect("无法创建第二个测试标签");
        let third = repository
            .create("第三个", "#FF0000")
            .expect("无法创建第三个测试标签");

        assert!(
            repository
                .reorder(third.id, first.id)
                .expect("标签排序失败")
        );
        let tags = repository.list().expect("无法查询重排后的标签");
        assert_eq!(
            tags.iter().map(|tag| tag.name.as_str()).collect::<Vec<_>>(),
            vec!["第二个", "第一个", "第三个"]
        );
        assert_eq!(
            tags.iter().map(|tag| tag.sort_order).collect::<Vec<_>>(),
            vec![2, 1, 0]
        );
        assert_eq!(tags[1].uuid, first.uuid);
        assert_eq!(tags[0].uuid, second.uuid);
        assert_eq!(tags[2].uuid, third.uuid);
    }

    #[test]
    fn duplicate_tag_names_fail_after_normalization() {
        let directory = tempfile::tempdir().expect("无法创建测试临时目录");
        let database =
            Database::open(directory.path().join("test.sqlite3")).expect("无法创建测试数据库");
        let repository = TagRepository::new(database);
        repository
            .create("Rust", "#1677FF")
            .expect("无法创建测试标签");

        let error = repository
            .create(" rust ", "#00FF00")
            .expect_err("重复标签名称不应创建");
        assert!(matches!(
            error,
            TagCreateError::DuplicateName(name) if name == "rust"
        ));
    }

    #[test]
    fn updating_tag_preserves_identity_and_updates_metadata() {
        let directory = tempfile::tempdir().expect("无法创建测试临时目录");
        let database =
            Database::open(directory.path().join("test.sqlite3")).expect("无法创建测试数据库");
        let repository = TagRepository::new(database);
        let created = repository
            .create("工作", "#1677FF")
            .expect("无法创建测试标签");

        let updated = repository
            .update(created.id, "开发", "#00FF00")
            .expect("无法修改测试标签");

        assert_eq!(updated.id, created.id);
        assert_eq!(updated.uuid, created.uuid);
        assert_eq!(updated.name, "开发");
        assert_eq!(updated.normalized_name, "开发");
        assert_eq!(updated.color, "#00FF00");
        assert!(updated.updated_at >= created.updated_at);
    }

    #[test]
    fn deleting_tag_cascades_item_relations_but_preserves_favorite() {
        let directory = tempfile::tempdir().expect("无法创建测试临时目录");
        let database =
            Database::open(directory.path().join("test.sqlite3")).expect("无法创建测试数据库");
        let repository = TagRepository::new(database.clone());
        let tag = repository
            .create("工作", "#1677FF")
            .expect("无法创建测试标签");
        let connection = database.connect().expect("无法连接测试数据库");
        connection
            .execute(
                "INSERT INTO clipboard_items (
                    uuid, hash_version, content_hash, content_kind, capture_state,
                    created_at, updated_at
                 ) VALUES ('item-delete-tag', 1, zeroblob(32), 'text', 'pending', 1, 1)",
                [],
            )
            .expect("无法创建测试内容");
        let item_id = connection.last_insert_rowid();
        connection
            .execute(
                "INSERT INTO favorites (uuid, item_id, is_pinned, sort_order, added_at, updated_at)
                 VALUES ('favorite-delete-tag', ?1, 0, 0, 1, 1)",
                [item_id],
            )
            .expect("无法创建测试收藏");
        let favorite_id = connection.last_insert_rowid();
        connection
            .execute(
                "INSERT INTO clipboard_item_tags (
                    uuid, item_id, tag_id, created_at, updated_at
                 ) VALUES ('item-tag-delete', ?1, ?2, 1, 1)",
                [item_id, tag.id],
            )
            .expect("无法创建测试标签关系");

        assert_eq!(
            repository.delete_many(&[tag.id]).expect("无法删除测试标签"),
            1
        );
        assert_eq!(
            repository.delete_many(&[tag.id]).expect("重复删除标签失败"),
            0
        );
        let counts: (i64, i64) = connection
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM clipboard_item_tags WHERE tag_id = ?1),
                    (SELECT COUNT(*) FROM favorites WHERE id = ?2)",
                [tag.id, favorite_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("无法验证标签删除结果");
        assert_eq!(counts, (0, 1));
    }

    #[test]
    fn item_tag_ids_returns_sorted_unique_tag_ids() {
        let directory = tempfile::tempdir().expect("无法创建测试临时目录");
        let database =
            Database::open(directory.path().join("test.sqlite3")).expect("无法创建测试数据库");
        let repository = TagRepository::new(database.clone());
        let first = repository
            .create("工作", "#1677FF")
            .expect("无法创建第一个测试标签");
        let second = repository
            .create("开发", "#00FF00")
            .expect("无法创建第二个测试标签");
        let connection = database.connect().expect("无法连接测试数据库");
        connection
            .execute(
                "INSERT INTO clipboard_items (
                    uuid, hash_version, content_hash, content_kind, capture_state,
                    created_at, updated_at
                 ) VALUES ('item-tag-query', 1, zeroblob(32), 'text', 'pending', 1, 1)",
                [],
            )
            .expect("无法创建测试内容");
        let item_id = connection.last_insert_rowid();

        assert_eq!(
            repository.item_tag_ids(item_id).expect("无法查询空标签"),
            Vec::<i64>::new()
        );
        assert!(
            repository
                .add_to_item(item_id, second.id)
                .expect("无法添加第二个标签")
        );
        assert!(
            repository
                .add_to_item(item_id, first.id)
                .expect("无法添加第一个标签")
        );
        assert!(
            !repository
                .add_to_item(item_id, first.id)
                .expect("无法重复添加标签")
        );

        assert_eq!(
            repository.item_tag_ids(item_id).expect("无法查询标签"),
            {
                let mut expected = vec![first.id, second.id];
                expected.sort_unstable();
                expected
            }
        );
    }

    #[test]
    fn deleting_many_tags_is_atomic_and_reports_deleted_count() {
        let directory = tempfile::tempdir().expect("无法创建测试临时目录");
        let database =
            Database::open(directory.path().join("test.sqlite3")).expect("无法创建测试数据库");
        let repository = TagRepository::new(database);
        let first = repository
            .create("工作", "#1677FF")
            .expect("无法创建第一个测试标签");
        let second = repository
            .create("开发", "#00FF00")
            .expect("无法创建第二个测试标签");

        assert_eq!(
            repository
                .delete_many(&[first.id, second.id])
                .expect("无法批量删除测试标签"),
            2
        );
        assert!(repository.list().expect("无法查询标签").is_empty());
    }
}
