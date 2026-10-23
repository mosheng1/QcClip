use std::{
    collections::HashMap,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{
    OptionalExtension, Transaction, TransactionBehavior, params, params_from_iter, types::Value,
};
use thiserror::Error;
use uuid::Uuid;

use crate::domain::clipboard::ClipboardItemId;
use crate::services::storage::RawBlobStore;

use super::{
    Database,
    models::{LIST_ORDER_BASE, LIST_ORDER_GAP},
    query::{
        HistoryContentFilter, HistoryListRecord, HistoryQuery, HistoryQueryError,
        ListOrderSnapshot, PasteFilter, append_search_filter, append_tag_filter,
        collect_order_snapshot, load_preview_options, parse_application_icon_source,
        parse_content_kind, parse_tag_ids, preview_text_expression,
    },
};

#[derive(Clone, Debug)]
pub struct FavoriteRepository {
    database: Database,
    blob_store: Option<RawBlobStore>,
}

#[derive(Debug, Error)]
pub enum FavoriteWriteError {
    #[error("无法连接收藏数据库：{0}")]
    Database(#[from] super::DatabaseError),
    #[error("添加收藏失败：{0}")]
    Sql(#[from] rusqlite::Error),
}

#[derive(Debug, Error)]
pub enum FavoriteReorderError {
    #[error("无法连接收藏数据库：{0}")]
    Database(#[from] super::DatabaseError),
    #[error("调整收藏排序失败：{0}")]
    Sql(#[from] rusqlite::Error),
    #[error("收藏排序值超出支持范围")]
    SortOrderOverflow,
}

#[derive(Debug, Error)]
pub enum FavoriteDeleteError {
    #[error("无法连接收藏数据库：{0}")]
    Database(#[from] super::DatabaseError),
    #[error("取消收藏失败：{0}")]
    Sql(#[from] rusqlite::Error),
}

impl FavoriteRepository {
    pub fn new(database: Database) -> Self {
        Self {
            database,
            blob_store: None,
        }
    }

    pub fn with_blob_store(database: Database, blob_store: RawBlobStore) -> Self {
        Self {
            database,
            blob_store: Some(blob_store),
        }
    }

    pub fn add_to_favorites(&self, item_id: ClipboardItemId) -> Result<bool, FavoriteWriteError> {
        let mut connection = self.database.connect_write()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        if transaction
            .query_row(
                "SELECT 1 FROM favorites WHERE item_id = ?1",
                [item_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            transaction.commit()?;
            return Ok(false);
        }

        let next_sort_order: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(sort_order), -1) + ?1
             FROM favorites WHERE is_pinned = 0",
            [LIST_ORDER_GAP],
            |row| row.get(0),
        )?;
        let timestamp = current_timestamp_millis();
        let inserted = transaction.execute(
            "INSERT INTO favorites (uuid, item_id, is_pinned, sort_order, added_at, updated_at)
             VALUES (?1, ?2, 0, ?3, ?4, ?4)",
            params![
                Uuid::new_v4().to_string(),
                item_id,
                next_sort_order,
                timestamp
            ],
        )?;
        transaction.commit()?;
        Ok(inserted > 0)
    }

    pub fn reorder(
        &self,
        favorite_id: i64,
        target_favorite_id: i64,
    ) -> Result<bool, FavoriteReorderError> {
        if favorite_id <= 0 || target_favorite_id <= 0 || favorite_id == target_favorite_id {
            return Ok(false);
        }

        let mut connection = self.database.connect_write()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let source = transaction
            .query_row(
                "SELECT is_pinned, sort_order FROM favorites WHERE id = ?1",
                [favorite_id],
                |row| Ok((row.get::<_, bool>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        let Some((source_partition, source_order)) = source else {
            transaction.commit()?;
            return Ok(false);
        };
        let target = transaction
            .query_row(
                "SELECT is_pinned, sort_order FROM favorites WHERE id = ?1",
                [target_favorite_id],
                |row| Ok((row.get::<_, bool>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        let Some((target_partition, target_order)) = target else {
            transaction.commit()?;
            return Ok(false);
        };
        if target_partition != source_partition {
            transaction.commit()?;
            return Ok(false);
        }
        shift_favorite_order(
            &transaction,
            source_partition,
            favorite_id,
            target_favorite_id,
            source_order,
            target_order,
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn move_item_to_top(&self, item_id: ClipboardItemId) -> Result<bool, FavoriteReorderError> {
        let mut connection = self.database.connect_write()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let source = transaction
            .query_row(
                "SELECT id, sort_order FROM favorites
                 WHERE item_id = ?1 AND is_pinned = 0",
                [item_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        let Some((favorite_id, source_order)) = source else {
            transaction.commit()?;
            return Ok(false);
        };
        let target = transaction
            .query_row(
                "SELECT id, sort_order FROM favorites
                 WHERE is_pinned = 0 ORDER BY sort_order DESC, id DESC LIMIT 1",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        let Some((target_favorite_id, target_order)) = target else {
            transaction.commit()?;
            return Ok(false);
        };
        if favorite_id == target_favorite_id {
            transaction.commit()?;
            return Ok(false);
        }
        shift_favorite_order(
            &transaction,
            false,
            favorite_id,
            target_favorite_id,
            source_order,
            target_order,
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn set_pinned(
        &self,
        favorite_id: i64,
        is_pinned: bool,
    ) -> Result<bool, FavoriteReorderError> {
        if favorite_id <= 0 {
            return Ok(false);
        }

        let mut connection = self.database.connect_write()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(source_partition) = transaction
            .query_row(
                "SELECT is_pinned FROM favorites WHERE id = ?1",
                [favorite_id],
                |row| row.get::<_, bool>(0),
            )
            .optional()?
        else {
            transaction.commit()?;
            return Ok(false);
        };
        if source_partition == is_pinned {
            transaction.commit()?;
            return Ok(false);
        }

        let target_order: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(sort_order), -1) + ?1
             FROM favorites WHERE is_pinned = ?2",
            params![LIST_ORDER_GAP, is_pinned],
            |row| row.get(0),
        )?;
        let timestamp = current_timestamp_millis();
        transaction.execute(
            "UPDATE favorites
             SET is_pinned = ?1, sort_order = ?2, updated_at = ?3
             WHERE id = ?4",
            params![is_pinned, target_order, timestamp, favorite_id],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn remove_from_favorites(&self, favorite_id: i64) -> Result<bool, FavoriteDeleteError> {
        let mut connection = self.database.connect_write()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let deleted = transaction.execute("DELETE FROM favorites WHERE id = ?1", [favorite_id])?;
        transaction.commit()?;
        Ok(deleted > 0)
    }

    pub fn remove_many_from_favorites(
        &self,
        favorite_ids: &[i64],
    ) -> Result<usize, FavoriteDeleteError> {
        if favorite_ids.is_empty() {
            return Ok(0);
        }

        let mut connection = self.database.connect_write()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut removed = 0;
        for favorite_id in favorite_ids {
            removed += transaction.execute("DELETE FROM favorites WHERE id = ?1", [favorite_id])?;
        }
        transaction.commit()?;
        Ok(removed)
    }

    pub fn favorite_id_for_item(
        &self,
        item_id: ClipboardItemId,
    ) -> Result<Option<i64>, HistoryQueryError> {
        let connection = self.database.connect()?;
        connection
            .query_row(
                "SELECT id FROM favorites WHERE item_id = ?1",
                [item_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(HistoryQueryError::from)
    }

    pub fn count(&self, query: &HistoryQuery) -> Result<usize, HistoryQueryError> {
        let connection = self.database.connect()?;
        let mut conditions = vec!["i.capture_state = 'ready'".to_owned()];
        let mut parameters = Vec::new();
        if let Some(search) = query
            .search
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            append_search_filter(&mut conditions, &mut parameters, search);
        }
        if !query.content_filters.is_empty() {
            let mut filters: Vec<HistoryContentFilter> =
                query.content_filters.iter().copied().collect();
            filters.sort_unstable();
            conditions.push(format!(
                "EXISTS (
                    SELECT 1 FROM clipboard_formats AS f_filter
                    WHERE f_filter.item_id = i.id
                      AND f_filter.preview_content_kind IN ({})
                )",
                vec!["?"; filters.len()].join(", ")
            ));
            parameters.extend(
                filters
                    .into_iter()
                    .map(|filter| Value::Text(filter.content_kind().as_str().to_owned())),
            );
        }
        match query.paste_filter {
            Some(PasteFilter::Unpasted) => conditions.push("f.paste_count = 0".to_owned()),
            Some(PasteFilter::Pasted) => conditions.push("f.paste_count > 0".to_owned()),
            None => {}
        }
        append_tag_filter(&mut conditions, &mut parameters, query);

        let sql = format!(
            "SELECT COUNT(*)
             FROM favorites AS f
             JOIN clipboard_items AS i ON i.id = f.item_id
             LEFT JOIN clipboard_history AS h ON h.item_id = f.item_id
             WHERE {}",
            conditions.join(" AND ")
        );
        let count: i64 =
            connection.query_row(&sql, params_from_iter(parameters.iter()), |row| row.get(0))?;
        usize::try_from(count)
            .map_err(|_| HistoryQueryError::InvalidNonNegativeValue("favorite_count".to_owned()))
    }

    pub fn find_by_id(
        &self,
        favorite_id: i64,
        query: &HistoryQuery,
    ) -> Result<Option<HistoryListRecord>, HistoryQueryError> {
        let mut query = query.clone();
        query.offset = 0;
        query.limit = 1;
        Ok(self.query_internal(&query, Some(favorite_id), None)?.pop())
    }

    pub fn pinned_count(&self, query: &HistoryQuery) -> Result<usize, HistoryQueryError> {
        let connection = self.database.connect()?;
        let mut conditions = vec![
            "i.capture_state = 'ready'".to_owned(),
            "f.is_pinned = 1".to_owned(),
        ];
        let mut parameters = Vec::new();
        if let Some(search) = query
            .search
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            append_search_filter(&mut conditions, &mut parameters, search);
        }
        if !query.content_filters.is_empty() {
            let mut filters: Vec<_> = query.content_filters.iter().copied().collect();
            filters.sort_unstable();
            conditions.push(format!(
                "EXISTS (
                    SELECT 1 FROM clipboard_formats AS f_filter
                    WHERE f_filter.item_id = i.id
                      AND f_filter.preview_content_kind IN ({})
                )",
                vec!["?"; filters.len()].join(", ")
            ));
            parameters.extend(
                filters
                    .into_iter()
                    .map(|filter| Value::Text(filter.content_kind().as_str().to_owned())),
            );
        }
        match query.paste_filter {
            Some(PasteFilter::Unpasted) => conditions.push("f.paste_count = 0".to_owned()),
            Some(PasteFilter::Pasted) => conditions.push("f.paste_count > 0".to_owned()),
            None => {}
        }
        append_tag_filter(&mut conditions, &mut parameters, query);
        let sql = format!(
            "SELECT COUNT(*)
             FROM favorites AS f
             JOIN clipboard_items AS i ON i.id = f.item_id
             LEFT JOIN clipboard_history AS h ON h.item_id = f.item_id
             WHERE {}",
            conditions.join(" AND ")
        );
        let count: i64 =
            connection.query_row(&sql, params_from_iter(parameters.iter()), |row| row.get(0))?;
        usize::try_from(count).map_err(|_| {
            HistoryQueryError::InvalidNonNegativeValue("pinned_favorite_count".to_owned())
        })
    }

    pub fn item_ids_for_favorite_ids(
        &self,
        favorite_ids: &[i64],
    ) -> Result<HashMap<i64, ClipboardItemId>, HistoryQueryError> {
        let connection = self.database.connect()?;
        let mut statement = connection.prepare("SELECT item_id FROM favorites WHERE id = ?1")?;
        let mut item_ids = HashMap::with_capacity(favorite_ids.len());
        for favorite_id in favorite_ids {
            if let Some(item_id) = statement
                .query_row([favorite_id], |row| row.get(0))
                .optional()?
            {
                item_ids.insert(*favorite_id, item_id);
            }
        }
        Ok(item_ids)
    }

    pub fn query(&self, query: &HistoryQuery) -> Result<Vec<HistoryListRecord>, HistoryQueryError> {
        self.query_internal(query, None, None)
    }

    pub fn query_by_ids(
        &self,
        query: &HistoryQuery,
        favorite_ids: &[i64],
    ) -> Result<Vec<HistoryListRecord>, HistoryQueryError> {
        if favorite_ids.is_empty() {
            return Ok(Vec::new());
        }
        self.query_internal(query, None, Some(favorite_ids))
    }

    pub fn ordered_ids(&self, query: &HistoryQuery) -> Result<Vec<i64>, HistoryQueryError> {
        Ok(self.query_order(query, false)?.ids)
    }

    pub fn ordered_snapshot(
        &self,
        query: &HistoryQuery,
    ) -> Result<ListOrderSnapshot, HistoryQueryError> {
        self.query_order(query, true)
    }

    fn query_order(
        &self,
        query: &HistoryQuery,
        include_heat: bool,
    ) -> Result<ListOrderSnapshot, HistoryQueryError> {
        // 只读取排序和热度元数据，避免加载完整预览文本及原始内容。
        let connection = self.database.connect()?;
        let mut conditions = vec!["i.capture_state = 'ready'".to_owned()];
        let mut parameters = Vec::new();

        if let Some(search) = query
            .search
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            append_search_filter(&mut conditions, &mut parameters, search);
        }
        if !query.content_filters.is_empty() {
            let mut filters: Vec<HistoryContentFilter> =
                query.content_filters.iter().copied().collect();
            filters.sort_unstable();
            conditions.push(format!(
                "EXISTS (
                    SELECT 1 FROM clipboard_formats AS f_filter
                    WHERE f_filter.item_id = i.id
                      AND f_filter.preview_content_kind IN ({})
                )",
                vec!["?"; filters.len()].join(", ")
            ));
            parameters.extend(
                filters
                    .into_iter()
                    .map(|filter| Value::Text(filter.content_kind().as_str().to_owned())),
            );
        }
        match query.paste_filter {
            Some(PasteFilter::Unpasted) => conditions.push("f.paste_count = 0".to_owned()),
            Some(PasteFilter::Pasted) => conditions.push("f.paste_count > 0".to_owned()),
            None => {}
        }
        append_tag_filter(&mut conditions, &mut parameters, query);

        let history_join = query
            .search
            .as_deref()
            .map(str::trim)
            .is_some_and(|value| !value.is_empty())
            .then_some("LEFT JOIN clipboard_history AS h ON h.item_id = f.item_id\n             ")
            .unwrap_or_default();
        let columns = if include_heat {
            "f.id, f.paste_count"
        } else {
            "f.id"
        };
        let sql = format!(
            "SELECT {columns}
             FROM favorites AS f
             JOIN clipboard_items AS i ON i.id = f.item_id
             {history_join}WHERE {}
             ORDER BY f.is_pinned DESC,
                      f.sort_order DESC,
                      f.added_at DESC,
                      f.id DESC",
            conditions.join(" AND ")
        );
        let mut statement = connection.prepare(&sql)?;
        collect_order_snapshot(
            statement.query(params_from_iter(parameters.iter()))?,
            include_heat,
        )
    }

    fn query_internal(
        &self,
        query: &HistoryQuery,
        favorite_id: Option<i64>,
        favorite_ids: Option<&[i64]>,
    ) -> Result<Vec<HistoryListRecord>, HistoryQueryError> {
        if query.limit == 0 {
            return Ok(Vec::new());
        }
        let connection = self.database.connect_read()?;
        let mut conditions = vec!["i.capture_state = 'ready'".to_owned()];
        let mut parameters = Vec::new();
        if let Some(search) = query
            .search
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            append_search_filter(&mut conditions, &mut parameters, search);
        }
        if !query.content_filters.is_empty() {
            let mut filters: Vec<HistoryContentFilter> =
                query.content_filters.iter().copied().collect();
            filters.sort_unstable();
            conditions.push(format!(
                "EXISTS (
                    SELECT 1 FROM clipboard_formats AS f_filter
                    WHERE f_filter.item_id = i.id
                      AND f_filter.preview_content_kind IN ({})
                )",
                vec!["?"; filters.len()].join(", ")
            ));
            parameters.extend(
                filters
                    .into_iter()
                    .map(|filter| Value::Text(filter.content_kind().as_str().to_owned())),
            );
        }
        match query.paste_filter {
            Some(PasteFilter::Unpasted) => conditions.push("f.paste_count = 0".to_owned()),
            Some(PasteFilter::Pasted) => conditions.push("f.paste_count > 0".to_owned()),
            None => {}
        }
        append_tag_filter(&mut conditions, &mut parameters, query);
        if let Some(favorite_id) = favorite_id {
            conditions.push("f.id = ?".to_owned());
            parameters.push(Value::Integer(favorite_id));
        }
        if let Some(favorite_ids) = favorite_ids {
            let placeholders = vec!["?"; favorite_ids.len()].join(", ");
            conditions.push(format!("f.id IN ({placeholders})"));
            parameters.extend(favorite_ids.iter().copied().map(Value::Integer));
        }
        let limit = favorite_ids
            .map(|favorite_ids| favorite_ids.len())
            .unwrap_or_else(|| query.limit.min(500) as usize);
        let offset = if favorite_ids.is_some() {
            0
        } else {
            query.offset as usize
        };
        parameters.push(Value::Integer(i64::try_from(limit).unwrap_or(i64::MAX)));
        parameters.push(Value::Integer(i64::try_from(offset).unwrap_or(i64::MAX)));
        let preview_text = preview_text_expression("i.preview_text", query.preview_char_limit);

        let sql = format!(
            "SELECT
                f.id,
                f.item_id,
                {preview_text},
                COALESCE(i.preview_char_count, 0),
                i.content_kind,
                f.added_at,
                f.paste_count,
                f.is_pinned,
                f.sort_order,
                h.last_source_app_name,
                a.sha256,
                a.byte_length,
                a.relative_path,
                COALESCE((
                    SELECT GROUP_CONCAT(cit.tag_id, ',')
                    FROM clipboard_item_tags AS cit
                    WHERE cit.item_id = i.id
                ), ''),
                COALESCE(n.note_text, '')
             FROM favorites AS f
             JOIN clipboard_items AS i ON i.id = f.item_id
             LEFT JOIN clipboard_history AS h ON h.item_id = f.item_id
             LEFT JOIN application_icon_assets AS a ON a.id = h.last_source_app_icon_id
             LEFT JOIN clipboard_item_notes AS n ON n.item_id = i.id
             WHERE {}
             ORDER BY f.is_pinned DESC,
                      f.sort_order DESC,
                      f.added_at DESC,
                      f.id DESC
             LIMIT ? OFFSET ?",
            conditions.join(" AND ")
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(parameters.iter()), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, bool>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<Vec<u8>>>(10)?,
                row.get::<_, Option<i64>>(11)?,
                row.get::<_, Option<String>>(12)?,
                row.get::<_, String>(13)?,
                row.get::<_, String>(14)?,
            ))
        })?;
        let mut records = Vec::new();
        for row in rows {
            let (
                favorite_id,
                item_id,
                preview_text,
                preview_char_count,
                content_kind,
                last_seen_at,
                paste_count,
                is_pinned,
                sort_order,
                source_app_name,
                source_app_icon_hash,
                source_app_icon_byte_length,
                source_app_icon_path,
                tag_ids,
                note_text,
            ) = row?;
            let loaded_preview_options = load_preview_options(
                &connection,
                item_id,
                query.preview_char_limit,
                query.load_image_sources,
                query.load_html_content,
                query.html_content_char_limit,
                self.blob_store.as_ref(),
            )?;
            let selected_preview = loaded_preview_options.visible.first();
            records.push(HistoryListRecord {
                history_id: favorite_id,
                item_id,
                note_text,
                preview_text: selected_preview
                    .map(|preview| preview.text.clone())
                    .unwrap_or(preview_text),
                preview_char_count: selected_preview
                    .map(|preview| preview.char_count)
                    .unwrap_or(usize::try_from(preview_char_count).map_err(|_| {
                        HistoryQueryError::InvalidNonNegativeValue("preview_char_count".to_owned())
                    })?),
                preview_options: loaded_preview_options.visible,
                all_format_options: loaded_preview_options.all,
                content_kind: parse_content_kind(&content_kind)?,
                favorite_id: Some(favorite_id),
                tag_ids: parse_tag_ids(&tag_ids)?,
                last_seen_at,
                paste_count: u64::try_from(paste_count).map_err(|_| {
                    HistoryQueryError::InvalidNonNegativeValue("paste_count".to_owned())
                })?,
                is_pinned,
                sort_order,
                source_app_name,
                source_app_icon: parse_application_icon_source(
                    source_app_icon_hash,
                    source_app_icon_byte_length,
                    source_app_icon_path,
                )?,
            });
        }
        Ok(records)
    }
}

fn current_timestamp_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn shift_favorite_order(
    transaction: &Transaction<'_>,
    is_pinned: bool,
    favorite_id: i64,
    target_favorite_id: i64,
    source_order: i64,
    target_order: i64,
) -> Result<(), FavoriteReorderError> {
    if source_order == target_order {
        return Ok(());
    }
    let mut destination_order =
        favorite_order_between(transaction, is_pinned, source_order, target_order)?;
    if destination_order.is_none() {
        rebalance_favorite_order(transaction, is_pinned)?;
        let (source_order, target_order): (i64, i64) = transaction.query_row(
            "SELECT source.sort_order, target.sort_order
             FROM favorites AS source, favorites AS target
             WHERE source.id = ?1 AND target.id = ?2",
            params![favorite_id, target_favorite_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        destination_order =
            favorite_order_between(transaction, is_pinned, source_order, target_order)?;
    }
    let destination_order = destination_order.ok_or(FavoriteReorderError::SortOrderOverflow)?;
    let timestamp = current_timestamp_millis();
    transaction.execute(
        "UPDATE favorites SET sort_order = ?1, updated_at = ?2 WHERE id = ?3",
        params![destination_order, timestamp, favorite_id],
    )?;
    Ok(())
}

fn favorite_order_between(
    transaction: &Transaction<'_>,
    is_pinned: bool,
    source_order: i64,
    target_order: i64,
) -> Result<Option<i64>, FavoriteReorderError> {
    let (lower, upper) = if source_order < target_order {
        let previous_order = transaction
            .query_row(
                "SELECT sort_order FROM favorites
                 WHERE is_pinned = ?1 AND sort_order > ?2
                 ORDER BY sort_order ASC LIMIT 1",
                params![is_pinned, target_order],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let Some(previous_order) = previous_order else {
            return target_order
                .checked_add(LIST_ORDER_GAP)
                .map(Some)
                .ok_or(FavoriteReorderError::SortOrderOverflow);
        };
        (target_order, previous_order)
    } else {
        let next_order = transaction
            .query_row(
                "SELECT sort_order FROM favorites
                 WHERE is_pinned = ?1 AND sort_order < ?2
                 ORDER BY sort_order DESC LIMIT 1",
                params![is_pinned, target_order],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let Some(next_order) = next_order else {
            let Some(previous_order) = target_order.checked_sub(LIST_ORDER_GAP) else {
                return Ok(None);
            };
            return Ok(Some(previous_order));
        };
        (next_order, target_order)
    };
    if upper.saturating_sub(lower) <= 1 {
        return Ok(None);
    }
    Ok(Some(lower + (upper - lower) / 2))
}

fn rebalance_favorite_order(
    transaction: &Transaction<'_>,
    is_pinned: bool,
) -> Result<(), FavoriteReorderError> {
    let mut statement = transaction
        .prepare("SELECT id FROM favorites WHERE is_pinned = ?1 ORDER BY sort_order DESC")?;
    let ordered_ids = statement
        .query_map([is_pinned], |row| row.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    if ordered_ids.is_empty() {
        return Ok(());
    }
    let max_order: i64 = transaction.query_row(
        "SELECT MAX(sort_order) FROM favorites WHERE is_pinned = ?1",
        [is_pinned],
        |row| row.get(0),
    )?;
    let row_count =
        i64::try_from(ordered_ids.len()).map_err(|_| FavoriteReorderError::SortOrderOverflow)?;
    let temporary_offset = max_order
        .checked_add(LIST_ORDER_BASE)
        .and_then(|value| value.checked_add(row_count))
        .and_then(|value| value.checked_add(1))
        .and_then(|value| value.checked_mul(LIST_ORDER_GAP))
        .ok_or(FavoriteReorderError::SortOrderOverflow)?;
    let temporary_max = max_order
        .checked_add(temporary_offset)
        .ok_or(FavoriteReorderError::SortOrderOverflow)?;
    let final_max = LIST_ORDER_BASE
        .checked_add(row_count)
        .and_then(|value| value.checked_mul(LIST_ORDER_GAP))
        .ok_or(FavoriteReorderError::SortOrderOverflow)?;
    if temporary_offset <= max_order
        || temporary_max < temporary_offset
        || final_max >= temporary_offset
    {
        return Err(FavoriteReorderError::SortOrderOverflow);
    }
    transaction.execute(
        "UPDATE favorites SET sort_order = sort_order + ?1 WHERE is_pinned = ?2",
        params![temporary_offset, is_pinned],
    )?;
    let timestamp = current_timestamp_millis();
    for (index, favorite_id) in ordered_ids.iter().rev().enumerate() {
        let reverse_index =
            i64::try_from(index).map_err(|_| FavoriteReorderError::SortOrderOverflow)?;
        let sort_order = LIST_ORDER_BASE
            .checked_add(reverse_index)
            .and_then(|value| value.checked_mul(LIST_ORDER_GAP))
            .ok_or(FavoriteReorderError::SortOrderOverflow)?;
        transaction.execute(
            "UPDATE favorites SET sort_order = ?1, updated_at = ?2 WHERE id = ?3",
            params![sort_order, timestamp, favorite_id],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::services::{
        capture::{CaptureService, mock::MockClipboardSource},
        storage::RawBlobStore,
    };

    use super::*;

    #[test]
    fn favorite_query_uses_favorite_sort_order_and_filters() {
        let directory = tempfile::tempdir().expect("无法创建测试临时目录");
        let database =
            Database::open(directory.path().join("test.sqlite3")).expect("无法创建测试数据库");
        let store = RawBlobStore::new(directory.path()).expect("无法创建测试文件存储");
        let capture = CaptureService::new(database.clone(), store);
        let first = capture
            .store_captured_clipboard(&MockClipboardSource::text("第一个").capture())
            .expect("无法保存第一个内容");
        let second = capture
            .store_captured_clipboard(&MockClipboardSource::text("第二个").capture())
            .expect("无法保存第二个内容");
        let connection = database.connect().expect("无法连接数据库");
        let item_ids: Vec<i64> = connection
            .prepare("SELECT item_id FROM clipboard_history ORDER BY id")
            .expect("无法准备查询")
            .query_map([], |row| row.get(0))
            .expect("无法查询内容")
            .collect::<Result<_, _>>()
            .expect("无法读取内容");
        connection
            .execute(
                "INSERT INTO favorites (uuid, item_id, is_pinned, sort_order, added_at, updated_at)
                 VALUES ('favorite-1', ?1, 0, 0, 1, 1), ('favorite-2', ?2, 0, 1, 2, 2)",
                [item_ids[0], item_ids[1]],
            )
            .expect("无法插入收藏");
        let favorite_repository = FavoriteRepository::new(database);
        let records = favorite_repository
            .query(&HistoryQuery::default())
            .expect("无法查询收藏");
        assert_eq!(
            favorite_repository
                .count(&HistoryQuery::default())
                .expect("无法统计收藏"),
            2
        );
        assert_eq!(
            records
                .iter()
                .map(|record| record.item_id)
                .collect::<Vec<_>>(),
            vec![second, first]
        );
        assert_eq!(
            favorite_repository
                .ordered_ids(&HistoryQuery::default())
                .expect("收藏 ID 排序失败"),
            vec![2, 1]
        );
        connection
            .execute("UPDATE favorites SET paste_count = 23 WHERE id = 2", [])
            .expect("无法更新收藏粘贴次数");
        let snapshot = favorite_repository
            .ordered_snapshot(&HistoryQuery::default())
            .expect("无法读取收藏热度快照");
        assert_eq!(snapshot.ids, vec![2, 1]);
        assert_eq!(snapshot.paste_counts, vec![23, 0]);
        let filtered = favorite_repository
            .ordered_snapshot(&HistoryQuery {
                paste_filter: Some(PasteFilter::Pasted),
                ..HistoryQuery::default()
            })
            .expect("无法筛选收藏热度");
        assert_eq!(filtered.ids, vec![2]);
        assert_eq!(filtered.paste_counts, vec![23]);

        let limited_query = HistoryQuery {
            preview_char_limit: Some(2),
            ..HistoryQuery::default()
        };
        let limited_records = favorite_repository
            .query(&limited_query)
            .expect("无法查询裁剪后的收藏预览");
        assert!(limited_records.iter().all(|record| {
            record.preview_text.chars().count() == 2
                && record.preview_char_count == 3
                && record
                    .preview_options
                    .iter()
                    .all(|preview| preview.text.chars().count() == 2 && preview.char_count == 3)
        }));
    }

    #[test]
    fn reorder_moves_favorite_and_rewrites_sort_order() {
        let directory = tempfile::tempdir().expect("无法创建临时目录");
        let database =
            Database::open(directory.path().join("test.sqlite3")).expect("无法创建测试数据库");
        let store = RawBlobStore::new(directory.path()).expect("无法创建测试文件存储");
        let capture = CaptureService::new(database.clone(), store);
        let first = capture
            .store_captured_clipboard(&MockClipboardSource::text("第一条").capture())
            .expect("无法保存第一个内容");
        let second = capture
            .store_captured_clipboard(&MockClipboardSource::text("第二条").capture())
            .expect("无法保存第二个内容");
        let third = capture
            .store_captured_clipboard(&MockClipboardSource::text("第三条").capture())
            .expect("无法保存第三个内容");
        let repository = FavoriteRepository::new(database);
        repository
            .add_to_favorites(first)
            .expect("无法收藏第一个内容");
        repository
            .add_to_favorites(second)
            .expect("无法收藏第二个内容");
        repository
            .add_to_favorites(third)
            .expect("无法收藏第三个内容");
        let first_favorite = repository
            .favorite_id_for_item(first)
            .expect("无法查询第一个收藏")
            .expect("第一个收藏不存在");
        let third_favorite = repository
            .favorite_id_for_item(third)
            .expect("无法查询第三个收藏")
            .expect("第三个收藏不存在");

        assert!(
            repository
                .reorder(first_favorite, third_favorite)
                .expect("收藏排序失败")
        );
        let records = repository
            .query(&HistoryQuery::default())
            .expect("查询重排后的收藏失败");
        assert_eq!(
            records
                .iter()
                .map(|record| record.item_id)
                .collect::<Vec<_>>(),
            vec![first, third, second]
        );
        let sort_orders = records
            .iter()
            .map(|record| record.sort_order)
            .collect::<Vec<_>>();
        assert!(
            sort_orders.windows(2).all(|pair| pair[0] > pair[1]),
            "收藏排序应保持降序：{sort_orders:?}"
        );
    }

    #[test]
    fn set_pinned_moves_favorite_to_the_top_of_the_target_partition() {
        let directory = tempfile::tempdir().expect("无法创建测试临时目录");
        let database =
            Database::open(directory.path().join("test.sqlite3")).expect("无法创建测试数据库");
        let store = RawBlobStore::new(directory.path()).expect("无法创建测试文件存储");
        let capture = CaptureService::new(database.clone(), store);
        let first = capture
            .store_captured_clipboard(&MockClipboardSource::text("第一条收藏").capture())
            .expect("无法保存第一条收藏");
        let second = capture
            .store_captured_clipboard(&MockClipboardSource::text("第二条收藏").capture())
            .expect("无法保存第二条收藏");
        let third = capture
            .store_captured_clipboard(&MockClipboardSource::text("第三条收藏").capture())
            .expect("无法保存第三条收藏");
        let repository = FavoriteRepository::new(database.clone());
        for item_id in [first, second, third] {
            assert!(
                repository
                    .add_to_favorites(item_id)
                    .expect("无法添加测试收藏")
            );
        }
        let favorite_ids = database
            .connect()
            .expect("无法连接测试数据库")
            .prepare(
                "SELECT id FROM favorites
                 ORDER BY sort_order DESC",
            )
            .expect("无法查询收藏 ID")
            .query_map([], |row| row.get::<_, i64>(0))
            .expect("无法读取收藏 ID")
            .collect::<Result<Vec<_>, _>>()
            .expect("无法收集收藏 ID");

        assert!(
            repository
                .set_pinned(favorite_ids[0], true)
                .expect("收藏置顶失败")
        );
        assert!(
            repository
                .set_pinned(favorite_ids[1], true)
                .expect("第二条收藏置顶失败")
        );

        let records = repository
            .query(&HistoryQuery::default())
            .expect("查询置顶收藏失败");
        assert_eq!(
            records
                .iter()
                .map(|record| (record.item_id, record.is_pinned))
                .collect::<Vec<_>>(),
            vec![(second, true), (third, true), (first, false)]
        );
        for is_pinned in [true, false] {
            let sort_orders = records
                .iter()
                .filter(|record| record.is_pinned == is_pinned)
                .map(|record| record.sort_order)
                .collect::<Vec<_>>();
            assert!(
                sort_orders.windows(2).all(|pair| pair[0] > pair[1]),
                "同一分区内收藏排序应保持降序：{sort_orders:?}"
            );
        }
    }

    #[test]
    fn favorite_search_matches_source_application_and_preserves_count() {
        let directory = tempfile::tempdir().expect("无法创建临时目录");
        let database =
            Database::open(directory.path().join("test.sqlite3")).expect("无法创建测试数据库");
        let store = RawBlobStore::new(directory.path()).expect("无法创建测试文件存储");
        let capture = CaptureService::new(database.clone(), store);
        let mut captured = MockClipboardSource::text("收藏来源搜索").capture();
        captured.source_application.name = Some("测试编辑器".to_owned());
        let history_id = capture
            .store_captured_clipboard(&captured)
            .expect("无法保存测试内容");
        let item_id: i64 = database
            .connect()
            .expect("无法连接测试数据库")
            .query_row(
                "SELECT item_id FROM clipboard_history WHERE id = ?1",
                [history_id],
                |row| row.get(0),
            )
            .expect("无法查询测试内容 ID");
        let repository = FavoriteRepository::new(database.clone());
        repository
            .add_to_favorites(item_id)
            .expect("无法添加测试收藏");

        let query = HistoryQuery {
            search: Some("编辑器".to_owned()),
            ..HistoryQuery::default()
        };
        let records = repository.query(&query).expect("收藏来源应用搜索失败");
        assert_eq!(records.len(), 1);
        assert_eq!(repository.count(&query).expect("收藏来源应用计数失败"), 1);
    }

    #[test]
    fn favorite_query_filters_tags_and_uses_favorite_sort_order() {
        let directory = tempfile::tempdir().expect("无法创建临时目录");
        let database =
            Database::open(directory.path().join("test.sqlite3")).expect("无法创建测试数据库");
        let store = RawBlobStore::new(directory.path()).expect("无法创建测试文件存储");
        let capture = CaptureService::new(database.clone(), store);
        let first = capture
            .store_captured_clipboard(&MockClipboardSource::text("第一个").capture())
            .expect("无法保存第一个内容");
        let second = capture
            .store_captured_clipboard(&MockClipboardSource::text("第二个").capture())
            .expect("无法保存第二个内容");
        let favorite_repository = FavoriteRepository::new(database.clone());
        favorite_repository
            .add_to_favorites(first)
            .expect("无法收藏第一个内容");
        favorite_repository
            .add_to_favorites(second)
            .expect("无法收藏第二个内容");
        let tag_repository = super::super::TagRepository::new(database.clone());
        let work_tag = tag_repository
            .create("工作", "#1677FF")
            .expect("无法创建工作标签");
        let code_tag = tag_repository
            .create("代码", "#00FF00")
            .expect("无法创建代码标签");
        let connection = database.connect().expect("无法连接数据库");
        connection
            .execute(
                "INSERT INTO clipboard_item_tags (
                    uuid, item_id, tag_id, created_at, updated_at
                ) VALUES (?1, ?2, ?5, 1, 1), (?3, ?4, ?5, 1, 1),
                         (?6, ?2, ?7, 1, 1)",
                params![
                    Uuid::new_v4().to_string(),
                    first,
                    Uuid::new_v4().to_string(),
                    second,
                    work_tag.id,
                    Uuid::new_v4().to_string(),
                    code_tag.id,
                ],
            )
            .expect("无法添加内容工作标签");

        let query = HistoryQuery {
            tag_ids: vec![work_tag.id],
            ..HistoryQuery::default()
        };
        assert_eq!(
            favorite_repository.count(&query).expect("无法统计标签收藏"),
            2
        );
        assert_eq!(
            favorite_repository
                .query(&query)
                .expect("无法查询标签收藏")
                .iter()
                .map(|record| record.item_id)
                .collect::<Vec<_>>(),
            vec![second, first]
        );

        let query = HistoryQuery {
            tag_ids: vec![work_tag.id, code_tag.id],
            ..HistoryQuery::default()
        };
        assert_eq!(
            favorite_repository
                .count(&query)
                .expect("无法统计任意标签收藏"),
            2
        );
        assert_eq!(
            favorite_repository
                .query(&query)
                .expect("无法查询任意标签收藏")
                .iter()
                .map(|record| record.item_id)
                .collect::<Vec<_>>(),
            vec![second, first]
        );

        let query = HistoryQuery {
            tag_ids: vec![work_tag.id, code_tag.id],
            tag_match_all: true,
            ..HistoryQuery::default()
        };
        assert_eq!(
            favorite_repository
                .count(&query)
                .expect("无法统计全部标签收藏"),
            1
        );
        assert_eq!(
            favorite_repository
                .query(&query)
                .expect("无法查询全部标签收藏")
                .iter()
                .map(|record| record.item_id)
                .collect::<Vec<_>>(),
            vec![first]
        );
    }

    #[test]
    fn adding_favorite_is_idempotent_and_uses_single_collection_order() {
        let directory = tempfile::tempdir().expect("无法创建测试临时目录");
        let database =
            Database::open(directory.path().join("test.sqlite3")).expect("无法创建测试数据库");
        let store = RawBlobStore::new(directory.path()).expect("无法创建测试文件存储");
        let capture = CaptureService::new(database.clone(), store);
        let item_id = capture
            .store_captured_clipboard(&MockClipboardSource::text("测试收藏").capture())
            .expect("无法保存测试内容");
        let favorite_repository = FavoriteRepository::new(database.clone());

        assert!(
            favorite_repository
                .add_to_favorites(item_id)
                .expect("无法添加收藏")
        );
        let connection = database.connect().expect("无法连接数据库");
        let sort_order_before: i64 = connection
            .query_row(
                "SELECT sort_order FROM favorites WHERE item_id = ?1",
                [item_id],
                |row| row.get(0),
            )
            .expect("无法查询收藏顺序");
        assert!(
            !favorite_repository
                .add_to_favorites(item_id)
                .expect("重复添加收藏失败")
        );

        let (favorite_count, tag_relation_count, sort_order): (i64, i64, i64) = connection
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM favorites WHERE item_id = ?1),
                    (SELECT COUNT(*) FROM clipboard_item_tags WHERE item_id = ?1),
                    (SELECT sort_order FROM favorites WHERE item_id = ?1)",
                [item_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("无法验证收藏关系");
        assert_eq!(favorite_count, 1);
        assert_eq!(tag_relation_count, 0);
        assert_eq!(sort_order, sort_order_before);
    }

    #[test]
    fn adding_item_tag_is_idempotent_and_shared_by_history_and_favorites() {
        let directory = tempfile::tempdir().expect("无法创建测试临时目录");
        let database =
            Database::open(directory.path().join("test.sqlite3")).expect("无法创建测试数据库");
        let store = RawBlobStore::new(directory.path()).expect("无法创建测试文件存储");
        let capture = CaptureService::new(database.clone(), store.clone());
        let history_id = capture
            .store_captured_clipboard(&MockClipboardSource::text("标签收藏测试").capture())
            .expect("无法保存测试内容");
        let favorite_repository = FavoriteRepository::new(database.clone());
        let tag_repository = super::super::TagRepository::new(database.clone());
        let item_id: i64 = database
            .connect()
            .expect("无法连接数据库")
            .query_row(
                "SELECT item_id FROM clipboard_history WHERE id = ?1",
                [history_id],
                |row| row.get(0),
            )
            .expect("无法查询测试内容 ID");
        let work_tag = tag_repository
            .create("工作", "#1677FF")
            .expect("无法创建工作标签");
        let code_tag = tag_repository
            .create("代码", "#00FF00")
            .expect("无法创建代码标签");

        let missing_tag = tag_repository
            .add_to_item(item_id, 999)
            .expect_err("不存在的标签不应写入内容");
        assert!(matches!(
            missing_tag,
            super::super::TagItemWriteError::Sql(_)
        ));
        let connection = database.connect().expect("无法连接数据库");
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM favorites WHERE item_id = ?1",
                    [item_id],
                    |row| row.get::<_, i64>(0),
                )
                .expect("无法验证标签写入结果"),
            0
        );
        drop(connection);

        assert!(
            tag_repository
                .add_to_item(item_id, work_tag.id)
                .expect("无法添加工作标签")
        );
        assert!(
            !tag_repository
                .add_to_item(item_id, work_tag.id)
                .expect("重复添加工作标签失败")
        );
        assert!(
            tag_repository
                .add_to_item(item_id, code_tag.id)
                .expect("无法添加代码标签")
        );

        let history_repository =
            super::super::HistoryRepository::new(database.clone(), store.clone());
        let work_query = HistoryQuery {
            tag_ids: vec![work_tag.id],
            ..HistoryQuery::default()
        };
        assert_eq!(
            history_repository
                .count(&work_query)
                .expect("无法统计历史标签"),
            1
        );
        assert_eq!(
            history_repository
                .query(&work_query)
                .expect("无法查询历史标签")
                .iter()
                .map(|record| record.item_id)
                .collect::<Vec<_>>(),
            vec![item_id]
        );
        let all_tags_query = HistoryQuery {
            tag_ids: vec![work_tag.id, code_tag.id],
            tag_match_all: true,
            ..HistoryQuery::default()
        };
        assert_eq!(
            history_repository
                .count(&all_tags_query)
                .expect("无法统计历史全部标签"),
            1
        );
        let history_record = history_repository
            .query(&HistoryQuery::default())
            .expect("无法查询标签关系");
        let history_record = history_record
            .into_iter()
            .find(|record| record.item_id == item_id)
            .expect("历史记录不存在");
        assert!(history_record.favorite_id.is_none());
        assert_eq!(history_record.tag_ids, vec![work_tag.id, code_tag.id]);

        favorite_repository
            .add_to_favorites(item_id)
            .expect("无法添加测试收藏");
        let favorite_record = favorite_repository
            .query(&HistoryQuery::default())
            .expect("无法查询收藏标签关系")
            .into_iter()
            .find(|record| record.item_id == item_id)
            .expect("收藏记录不存在");
        assert_eq!(favorite_record.tag_ids, vec![work_tag.id, code_tag.id]);
    }

    #[test]
    fn removing_from_tag_deletes_only_current_relation_and_preserves_other_tags() {
        let directory = tempfile::tempdir().expect("无法创建测试临时目录");
        let database =
            Database::open(directory.path().join("test.sqlite3")).expect("无法创建测试数据库");
        let store = RawBlobStore::new(directory.path()).expect("无法创建测试文件存储");
        let capture = CaptureService::new(database.clone(), store);
        let item_id = capture
            .store_captured_clipboard(&MockClipboardSource::text("移除标签测试").capture())
            .expect("无法保存测试内容");
        let tag_repository = super::super::TagRepository::new(database.clone());
        let first_tag = tag_repository
            .create("工作", "#1677FF")
            .expect("无法创建第一个标签");
        let second_tag = tag_repository
            .create("代码", "#00FF00")
            .expect("无法创建第二个标签");
        tag_repository
            .add_to_item(item_id, first_tag.id)
            .expect("无法添加第一个标签");
        tag_repository
            .add_to_item(item_id, second_tag.id)
            .expect("无法添加第二个标签");
        let favorite_repository = FavoriteRepository::new(database.clone());
        favorite_repository
            .add_to_favorites(item_id)
            .expect("无法添加测试收藏");

        assert!(
            tag_repository
                .remove_from_item(item_id, first_tag.id)
                .expect("无法移除第一个标签")
        );
        assert!(
            !tag_repository
                .remove_from_item(item_id, first_tag.id)
                .expect("重复移除第一个标签失败")
        );

        let connection = database.connect().expect("无法连接数据库");
        let counts: (i64, i64, i64) = connection
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM favorites WHERE item_id = ?1),
                    (SELECT COUNT(*) FROM clipboard_item_tags
                     WHERE item_id = ?1 AND tag_id = ?2),
                    (SELECT COUNT(*) FROM clipboard_item_tags
                     WHERE item_id = ?1 AND tag_id = ?3)",
                params![item_id, first_tag.id, second_tag.id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("无法验证标签移除结果");
        assert_eq!(counts, (1, 0, 1));
    }

    #[test]
    fn removing_favorite_deletes_only_favorite_relation() {
        let directory = tempfile::tempdir().expect("无法创建测试临时目录");
        let database =
            Database::open(directory.path().join("test.sqlite3")).expect("无法创建测试数据库");
        let store = RawBlobStore::new(directory.path()).expect("无法创建测试文件存储");
        let capture = CaptureService::new(database.clone(), store);
        let item_id = capture
            .store_captured_clipboard(&MockClipboardSource::text("取消收藏测试").capture())
            .expect("无法保存测试内容");
        let favorite_repository = FavoriteRepository::new(database.clone());
        let tag_repository = super::super::TagRepository::new(database.clone());
        let tag = tag_repository
            .create("工作", "#1677FF")
            .expect("无法创建测试标签");
        tag_repository
            .add_to_item(item_id, tag.id)
            .expect("无法添加测试内容标签");
        assert!(
            favorite_repository
                .add_to_favorites(item_id)
                .expect("无法添加测试收藏")
        );
        let favorite_id = favorite_repository
            .favorite_id_for_item(item_id)
            .expect("无法读取收藏 ID")
            .expect("测试收藏不存在");

        assert!(
            favorite_repository
                .remove_from_favorites(favorite_id)
                .expect("无法取消收藏")
        );
        assert!(
            !favorite_repository
                .remove_from_favorites(favorite_id)
                .expect("重复取消收藏失败")
        );

        let connection = database.connect().expect("无法连接数据库");
        let counts: (i64, i64, i64, i64) = connection
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM favorites WHERE id = ?1),
                    (SELECT COUNT(*) FROM clipboard_item_tags
                     WHERE item_id = ?2 AND tag_id = ?3),
                    (SELECT COUNT(*) FROM clipboard_history WHERE item_id = ?2),
                    (SELECT COUNT(*) FROM clipboard_items WHERE id = ?2)",
                params![favorite_id, item_id, tag.id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("无法验证取消收藏结果");
        assert_eq!(counts, (0, 1, 1, 1));
    }
}
