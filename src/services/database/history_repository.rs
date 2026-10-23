use rusqlite::{
    OptionalExtension, Transaction, TransactionBehavior, params_from_iter, types::Value,
};
use std::{
    collections::{HashMap, HashSet},
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;

use crate::config::HistoryConfig;
use crate::domain::clipboard::{ClipboardItemId, ContentKind, FormatPreview, HistoryItemId};
use crate::services::storage::{RawBlob, RawBlobStore};

use super::{
    Database,
    models::{LIST_ORDER_BASE, LIST_ORDER_GAP},
    query::{
        HistoryListRecord, HistoryQuery, HistoryQueryError, ListOrderSnapshot, PasteFilter,
        append_search_filter, append_tag_filter, append_tag_filter_for_item,
        collect_order_snapshot, load_preview_options, parse_application_icon_source,
        parse_content_kind, parse_tag_ids, preview_text_expression,
    },
};

#[derive(Clone, Debug)]
pub struct HistoryRepository {
    database: Database,
    blob_store: RawBlobStore,
}

#[derive(Debug, Error)]
pub enum HistoryDeleteError {
    #[error("无法连接历史数据库：{0}")]
    Database(#[from] super::DatabaseError),
    #[error("删除历史内容失败：{0}")]
    Sql(#[from] rusqlite::Error),
    #[error("删除历史内容后的数据清理失败：{0}")]
    Cleanup(#[from] HistoryCleanupError),
}

#[derive(Debug, Error)]
pub enum HistoryReorderError {
    #[error("无法连接历史数据库：{0}")]
    Database(#[from] super::DatabaseError),
    #[error("调整历史排序失败：{0}")]
    Sql(#[from] rusqlite::Error),
    #[error("历史排序值超出支持范围")]
    SortOrderOverflow,
}

#[derive(Debug, Error)]
pub enum HistoryCleanupError {
    #[error("无法连接历史数据库：{0}")]
    Database(#[from] super::DatabaseError),
    #[error("清理历史数据失败：{0}")]
    Sql(#[from] rusqlite::Error),
    #[error("清理原始数据文件失败：{0}")]
    Storage(#[from] crate::services::storage::RawBlobError),
    #[error("数据库中的原始数据哈希长度无效：{0} 字节")]
    InvalidHashLength(usize),
    #[error("历史记录数值无效：{0}")]
    InvalidCount(i64),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HistoryCleanupReport {
    pub history_items_removed: usize,
    pub orphan_items_removed: usize,
    pub orphan_blobs_removed: usize,
    pub orphan_blob_files_removed: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HistoryRetentionCounts {
    pub in_scope_items: usize,
    pub in_scope_unpinned_items: usize,
    pub over_limit_items: usize,
    pub pending_cleanup_items: usize,
}

impl HistoryRepository {
    pub fn note_repository(&self) -> super::NoteRepository {
        super::NoteRepository::new(self.database.clone())
    }

    pub fn new(database: Database, blob_store: RawBlobStore) -> Self {
        Self {
            database,
            blob_store,
        }
    }

    #[cfg(test)]
    pub fn query(&self, query: &HistoryQuery) -> Result<Vec<HistoryListRecord>, HistoryQueryError> {
        self.query_internal(query, None, None, None)
    }

    pub fn query_by_ids_scoped(
        &self,
        query: &HistoryQuery,
        history_ids: &[HistoryItemId],
        policy: &HistoryConfig,
    ) -> Result<Vec<HistoryListRecord>, HistoryQueryError> {
        if history_ids.is_empty() {
            return Ok(Vec::new());
        }
        self.query_internal(query, None, Some(history_ids), Some(policy))
    }

    #[cfg(test)]
    pub fn ordered_ids(
        &self,
        query: &HistoryQuery,
    ) -> Result<Vec<HistoryItemId>, HistoryQueryError> {
        Ok(self.query_order(query, None, false)?.ids)
    }

    pub fn ordered_ids_scoped(
        &self,
        query: &HistoryQuery,
        policy: &HistoryConfig,
    ) -> Result<Vec<HistoryItemId>, HistoryQueryError> {
        Ok(self.query_order(query, Some(policy), false)?.ids)
    }

    pub fn ordered_snapshot_scoped(
        &self,
        query: &HistoryQuery,
        policy: &HistoryConfig,
    ) -> Result<ListOrderSnapshot, HistoryQueryError> {
        self.query_order(query, Some(policy), true)
    }

    fn query_order(
        &self,
        query: &HistoryQuery,
        policy: Option<&HistoryConfig>,
        include_heat: bool,
    ) -> Result<ListOrderSnapshot, HistoryQueryError> {
        // 只读取排序和热度元数据，避免加载完整预览文本及原始内容。
        let connection = self.database.connect()?;
        let needs_item_join = query
            .search
            .as_deref()
            .map(str::trim)
            .is_some_and(|value| !value.is_empty());
        let mut conditions = vec![if needs_item_join {
            "i.capture_state = 'ready'".to_owned()
        } else {
            "h.item_id IN (
                SELECT i_ready.id
                FROM clipboard_items AS i_ready
                WHERE i_ready.capture_state = 'ready'
            )"
            .to_owned()
        }];
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
                    WHERE f_filter.item_id = h.item_id
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
            Some(PasteFilter::Unpasted) => conditions.push("h.paste_count = 0".to_owned()),
            Some(PasteFilter::Pasted) => conditions.push("h.paste_count > 0".to_owned()),
            None => {}
        }
        if !query.include_pinned {
            conditions.push("h.is_pinned = 0".to_owned());
        }
        append_history_scope_filter(&mut conditions, &mut parameters, policy);
        append_tag_filter_for_item(&mut conditions, &mut parameters, query, "h.item_id");

        let item_join = needs_item_join
            .then_some("JOIN clipboard_items AS i ON i.id = h.item_id\n             ")
            .unwrap_or_default();
        let columns = if include_heat {
            "h.id, h.paste_count"
        } else {
            "h.id"
        };
        let sql = format!(
            "SELECT {columns}
             FROM clipboard_history AS h
             {item_join}WHERE {}
             ORDER BY h.is_pinned DESC,
                      h.sort_order DESC,
                      h.last_seen_at DESC,
                      h.id DESC",
            conditions.join(" AND ")
        );
        let mut statement = connection.prepare(&sql)?;
        collect_order_snapshot(
            statement.query(params_from_iter(parameters.iter()))?,
            include_heat,
        )
    }

    #[cfg(test)]
    pub fn count(&self, query: &HistoryQuery) -> Result<usize, HistoryQueryError> {
        self.count_with_policy(query, None)
    }

    pub fn count_scoped(
        &self,
        query: &HistoryQuery,
        policy: &HistoryConfig,
    ) -> Result<usize, HistoryQueryError> {
        self.count_with_policy(query, Some(policy))
    }

    fn count_with_policy(
        &self,
        query: &HistoryQuery,
        policy: Option<&HistoryConfig>,
    ) -> Result<usize, HistoryQueryError> {
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
            Some(PasteFilter::Unpasted) => conditions.push("h.paste_count = 0".to_owned()),
            Some(PasteFilter::Pasted) => conditions.push("h.paste_count > 0".to_owned()),
            None => {}
        }
        if !query.include_pinned {
            conditions.push("h.is_pinned = 0".to_owned());
        }
        append_history_scope_filter(&mut conditions, &mut parameters, policy);
        append_tag_filter(&mut conditions, &mut parameters, query);

        let sql = format!(
            "SELECT COUNT(*)
             FROM clipboard_history AS h
             JOIN clipboard_items AS i ON i.id = h.item_id
             WHERE {}",
            conditions.join(" AND ")
        );
        let count: i64 =
            connection.query_row(&sql, params_from_iter(parameters.iter()), |row| row.get(0))?;
        usize::try_from(count)
            .map_err(|_| HistoryQueryError::InvalidNonNegativeValue("history_count".to_owned()))
    }

    pub fn find_by_id(
        &self,
        history_id: HistoryItemId,
        query: &HistoryQuery,
    ) -> Result<Option<HistoryListRecord>, HistoryQueryError> {
        let mut query = query.clone();
        query.offset = 0;
        query.limit = 1;
        Ok(self
            .query_internal(&query, Some(history_id), None, None)?
            .pop())
    }

    pub fn find_by_id_scoped(
        &self,
        history_id: HistoryItemId,
        query: &HistoryQuery,
        policy: &HistoryConfig,
    ) -> Result<Option<HistoryListRecord>, HistoryQueryError> {
        let mut query = query.clone();
        query.offset = 0;
        query.limit = 1;
        Ok(self
            .query_internal(&query, Some(history_id), None, Some(policy))?
            .pop())
    }

    pub fn pinned_count(&self, query: &HistoryQuery) -> Result<usize, HistoryQueryError> {
        let connection = self.database.connect()?;
        let mut conditions = vec![
            "i.capture_state = 'ready'".to_owned(),
            "h.is_pinned = 1".to_owned(),
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
        if matches!(query.paste_filter, Some(PasteFilter::Unpasted)) {
            conditions.push("h.paste_count = 0".to_owned());
        } else if matches!(query.paste_filter, Some(PasteFilter::Pasted)) {
            conditions.push("h.paste_count > 0".to_owned());
        }
        append_tag_filter(&mut conditions, &mut parameters, query);
        let sql = format!(
            "SELECT COUNT(*)
             FROM clipboard_history AS h
             JOIN clipboard_items AS i ON i.id = h.item_id
             WHERE {}",
            conditions.join(" AND ")
        );
        let count: i64 =
            connection.query_row(&sql, params_from_iter(parameters.iter()), |row| row.get(0))?;
        usize::try_from(count).map_err(|_| {
            HistoryQueryError::InvalidNonNegativeValue("pinned_history_count".to_owned())
        })
    }

    pub fn find_item_previews(
        &self,
        item_id: ClipboardItemId,
        query: &HistoryQuery,
    ) -> Result<Option<(ContentKind, Vec<FormatPreview>)>, HistoryQueryError> {
        let connection = self.database.connect()?;
        let content_kind = connection
            .query_row(
                "SELECT content_kind
                 FROM clipboard_items
                 WHERE id = ?1 AND capture_state = 'ready'",
                [item_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(content_kind) = content_kind else {
            return Ok(None);
        };
        let previews = load_preview_options(
            &connection,
            item_id,
            query.preview_char_limit,
            query.load_image_sources,
            query.load_html_content,
            query.html_content_char_limit,
            Some(&self.blob_store),
        )?;
        Ok(Some((parse_content_kind(&content_kind)?, previews.visible)))
    }

    pub fn item_ids_for_history_ids(
        &self,
        history_ids: &[HistoryItemId],
    ) -> Result<HashMap<HistoryItemId, ClipboardItemId>, HistoryQueryError> {
        let connection = self.database.connect()?;
        let mut statement =
            connection.prepare("SELECT item_id FROM clipboard_history WHERE id = ?1")?;
        let mut item_ids = HashMap::with_capacity(history_ids.len());
        for history_id in history_ids {
            if let Some(item_id) = statement
                .query_row([history_id], |row| row.get(0))
                .optional()?
            {
                item_ids.insert(*history_id, item_id);
            }
        }
        Ok(item_ids)
    }

    pub fn delete(&self, history_id: HistoryItemId) -> Result<bool, HistoryDeleteError> {
        let mut connection = self.database.connect_write()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(item_id) = transaction
            .query_row(
                "SELECT item_id FROM clipboard_history WHERE id = ?1",
                [history_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
        else {
            transaction.commit()?;
            return Ok(false);
        };

        transaction.execute("DELETE FROM clipboard_history WHERE id = ?1", [history_id])?;
        let cleanup_report =
            cleanup_orphaned_in_transaction(&transaction, &self.blob_store, Some(item_id))?;
        transaction.commit()?;
        log_cleanup_report(Some(history_id), cleanup_report);
        Ok(true)
    }

    pub fn delete_many(&self, history_ids: &[HistoryItemId]) -> Result<usize, HistoryDeleteError> {
        if history_ids.is_empty() {
            return Ok(0);
        }

        let mut connection = self.database.connect_write()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut affected_item_ids = HashSet::new();
        {
            let mut statement =
                transaction.prepare("SELECT item_id FROM clipboard_history WHERE id = ?1")?;
            for history_id in history_ids {
                if let Some(item_id) = statement
                    .query_row([history_id], |row| row.get::<_, i64>(0))
                    .optional()?
                {
                    affected_item_ids.insert(item_id);
                }
            }
        }
        let mut deleted = 0;
        for history_id in history_ids {
            deleted +=
                transaction.execute("DELETE FROM clipboard_history WHERE id = ?1", [history_id])?;
        }
        let affected_item_ids = affected_item_ids.into_iter().collect::<Vec<_>>();
        let cleanup_report = cleanup_orphaned_items_in_transaction(
            &transaction,
            &self.blob_store,
            &affected_item_ids,
            true,
        )?;
        transaction.commit()?;
        log_cleanup_report(None, cleanup_report);
        Ok(deleted)
    }

    pub fn reorder(
        &self,
        history_id: HistoryItemId,
        target_history_id: HistoryItemId,
    ) -> Result<bool, HistoryReorderError> {
        if history_id <= 0 || target_history_id <= 0 || history_id == target_history_id {
            return Ok(false);
        }

        let mut connection = self.database.connect_write()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let source = transaction
            .query_row(
                "SELECT is_pinned, sort_order FROM clipboard_history WHERE id = ?1",
                [history_id],
                |row| Ok((row.get::<_, bool>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        let Some((source_partition, source_order)) = source else {
            transaction.commit()?;
            return Ok(false);
        };
        let target = transaction
            .query_row(
                "SELECT is_pinned, sort_order FROM clipboard_history WHERE id = ?1",
                [target_history_id],
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
        shift_history_order(
            &transaction,
            source_partition,
            history_id,
            target_history_id,
            source_order,
            target_order,
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn move_item_to_top(&self, item_id: ClipboardItemId) -> Result<bool, HistoryReorderError> {
        let mut connection = self.database.connect_write()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let source = transaction
            .query_row(
                "SELECT id, sort_order FROM clipboard_history
                 WHERE item_id = ?1 AND is_pinned = 0",
                [item_id],
                |row| Ok((row.get::<_, HistoryItemId>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        let Some((history_id, source_order)) = source else {
            transaction.commit()?;
            return Ok(false);
        };
        let target = transaction
            .query_row(
                "SELECT id, sort_order FROM clipboard_history
                 WHERE is_pinned = 0 ORDER BY sort_order DESC, id DESC LIMIT 1",
                [],
                |row| Ok((row.get::<_, HistoryItemId>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        let Some((target_history_id, target_order)) = target else {
            transaction.commit()?;
            return Ok(false);
        };
        if history_id == target_history_id {
            transaction.commit()?;
            return Ok(false);
        }
        shift_history_order(
            &transaction,
            false,
            history_id,
            target_history_id,
            source_order,
            target_order,
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn set_pinned(
        &self,
        history_id: HistoryItemId,
        is_pinned: bool,
    ) -> Result<bool, HistoryReorderError> {
        if history_id <= 0 {
            return Ok(false);
        }

        let mut connection = self.database.connect_write()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(source_partition) = transaction
            .query_row(
                "SELECT is_pinned FROM clipboard_history WHERE id = ?1",
                [history_id],
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
             FROM clipboard_history WHERE is_pinned = ?2",
            rusqlite::params![LIST_ORDER_GAP, is_pinned],
            |row| row.get(0),
        )?;
        let timestamp = current_timestamp_millis();
        transaction.execute(
            "UPDATE clipboard_history
             SET is_pinned = ?1, sort_order = ?2, updated_at = ?3
             WHERE id = ?4",
            rusqlite::params![is_pinned, target_order, timestamp, history_id],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    #[cfg(test)]
    pub fn cleanup_orphaned(&self) -> Result<HistoryCleanupReport, HistoryCleanupError> {
        let mut connection = self.database.connect_write()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let report = cleanup_orphaned_in_transaction(&transaction, &self.blob_store, None)?;
        transaction.commit()?;
        log_cleanup_report(None, report);
        Ok(report)
    }

    pub fn clear_all(&self) -> Result<HistoryCleanupReport, HistoryCleanupError> {
        self.clear_history(false)
    }

    pub fn clear_unpinned(&self) -> Result<HistoryCleanupReport, HistoryCleanupError> {
        self.clear_history(true)
    }

    fn clear_history(
        &self,
        keep_pinned: bool,
    ) -> Result<HistoryCleanupReport, HistoryCleanupError> {
        let mut connection = self.database.connect_write()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !Database::maintenance_supported(&transaction)? {
            return Err(super::DatabaseError::UnsupportedRewrite.into());
        }
        let removed = transaction.execute(
            "DELETE FROM clipboard_history WHERE ?1 = 0 OR is_pinned = 0",
            [keep_pinned],
        )?;
        let mut report = cleanup_orphaned_in_transaction(&transaction, &self.blob_store, None)?;
        report.history_items_removed = removed;
        transaction.commit()?;
        log_cleanup_report(None, report);
        Ok(report)
    }

    pub fn retention_counts(
        &self,
        policy: &HistoryConfig,
    ) -> Result<HistoryRetentionCounts, HistoryCleanupError> {
        let connection = self.database.connect()?;
        let total: i64 = connection.query_row(
            "SELECT COUNT(*)
             FROM clipboard_history AS h
             JOIN clipboard_items AS i ON i.id = h.item_id
             WHERE i.capture_state = 'ready'",
            [],
            |row| row.get(0),
        )?;
        let scope_condition = history_scope_condition(policy);
        let pending = if let Some((scope, parameters)) = &scope_condition {
            connection.query_row(
                &format!(
                    "SELECT COUNT(*)
                     FROM clipboard_history AS h
                     JOIN clipboard_items AS i ON i.id = h.item_id
                     WHERE i.capture_state = 'ready' AND NOT ({scope})"
                ),
                params_from_iter(parameters.iter()),
                |row| row.get::<_, i64>(0),
            )?
        } else {
            0
        };
        let in_scope_unpinned: i64 = if let Some((scope, parameters)) = &scope_condition {
            connection.query_row(
                &format!(
                    "SELECT COUNT(*)
                     FROM clipboard_history AS h
                     JOIN clipboard_items AS i ON i.id = h.item_id
                     WHERE i.capture_state = 'ready'
                       AND h.is_pinned = 0
                       AND ({scope})"
                ),
                params_from_iter(parameters.iter()),
                |row| row.get(0),
            )?
        } else {
            connection.query_row(
                "SELECT COUNT(*)
                 FROM clipboard_history AS h
                 JOIN clipboard_items AS i ON i.id = h.item_id
                 WHERE i.capture_state = 'ready' AND h.is_pinned = 0",
                [],
                |row| row.get(0),
            )?
        };
        let over_limit: i64 = if let Some(limit) = policy.max_items {
            connection.query_row(
                "SELECT COUNT(*)
                 FROM (
                    SELECT ROW_NUMBER() OVER (
                        ORDER BY h.sort_order DESC,
                                 h.last_seen_at DESC,
                                 h.id DESC
                    ) AS retention_rank
                    FROM clipboard_history AS h
                    JOIN clipboard_items AS i ON i.id = h.item_id
                    WHERE i.capture_state = 'ready' AND h.is_pinned = 0
                 )
                 WHERE retention_rank > ?1",
                [i64::from(limit)],
                |row| row.get(0),
            )?
        } else {
            0
        };
        let total = usize::try_from(total).map_err(|_| HistoryCleanupError::InvalidCount(total))?;
        let pending =
            usize::try_from(pending).map_err(|_| HistoryCleanupError::InvalidCount(pending))?;
        let in_scope_unpinned = usize::try_from(in_scope_unpinned)
            .map_err(|_| HistoryCleanupError::InvalidCount(in_scope_unpinned))?;
        let over_limit = usize::try_from(over_limit)
            .map_err(|_| HistoryCleanupError::InvalidCount(over_limit))?;
        Ok(HistoryRetentionCounts {
            in_scope_items: total.saturating_sub(pending),
            in_scope_unpinned_items: in_scope_unpinned,
            over_limit_items: over_limit,
            pending_cleanup_items: pending,
        })
    }

    /// 手动清理或启用启动清理时，永久删除超出当前保留范围的历史。
    pub fn cleanup_out_of_scope(
        &self,
        policy: &HistoryConfig,
    ) -> Result<HistoryCleanupReport, HistoryCleanupError> {
        let mut connection = self.database.connect_write()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !Database::maintenance_supported(&transaction)? {
            return Ok(HistoryCleanupReport::default());
        }
        let mut report = HistoryCleanupReport::default();
        let pending = if let Some((scope, parameters)) = history_scope_condition(policy) {
            let mut statement = transaction.prepare(&format!(
                "SELECT h.id
                 FROM clipboard_history AS h
                 JOIN clipboard_items AS i ON i.id = h.item_id
                 WHERE i.capture_state = 'ready' AND NOT ({scope})
                 ORDER BY h.last_seen_at ASC, h.id ASC"
            ))?;
            statement
                .query_map(params_from_iter(parameters.iter()), |row| {
                    row.get::<_, i64>(0)
                })?
                .collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };

        for history_id in pending {
            transaction.execute("DELETE FROM clipboard_history WHERE id = ?1", [history_id])?;
            report.history_items_removed += 1;
        }

        let cleanup = cleanup_orphaned_in_transaction(&transaction, &self.blob_store, None)?;
        report.orphan_items_removed += cleanup.orphan_items_removed;
        report.orphan_blobs_removed += cleanup.orphan_blobs_removed;
        report.orphan_blob_files_removed += cleanup.orphan_blob_files_removed;
        transaction.commit()?;
        log_cleanup_report(None, report);
        Ok(report)
    }

    fn query_internal(
        &self,
        query: &HistoryQuery,
        history_id: Option<HistoryItemId>,
        history_ids: Option<&[HistoryItemId]>,
        policy: Option<&HistoryConfig>,
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
            Some(PasteFilter::Unpasted) => conditions.push("h.paste_count = 0".to_owned()),
            Some(PasteFilter::Pasted) => conditions.push("h.paste_count > 0".to_owned()),
            None => {}
        }
        if !query.include_pinned {
            conditions.push("h.is_pinned = 0".to_owned());
        }
        append_history_scope_filter(&mut conditions, &mut parameters, policy);
        append_tag_filter(&mut conditions, &mut parameters, query);
        if let Some(history_id) = history_id {
            conditions.push("h.id = ?".to_owned());
            parameters.push(Value::Integer(history_id));
        }
        if let Some(history_ids) = history_ids {
            let placeholders = vec!["?"; history_ids.len()].join(", ");
            conditions.push(format!("h.id IN ({placeholders})"));
            parameters.extend(history_ids.iter().copied().map(Value::Integer));
        }
        let limit = history_ids
            .map(|history_ids| history_ids.len())
            .unwrap_or_else(|| query.limit.min(500) as usize);
        let offset = if history_ids.is_some() {
            0
        } else {
            query.offset as usize
        };
        parameters.push(Value::Integer(i64::try_from(limit).unwrap_or(i64::MAX)));
        parameters.push(Value::Integer(i64::try_from(offset).unwrap_or(i64::MAX)));
        let preview_text = preview_text_expression("i.preview_text", query.preview_char_limit);

        let sql = format!(
            "SELECT
                h.id,
                h.item_id,
                {preview_text},
                COALESCE(i.preview_char_count, 0),
                i.content_kind,
                h.last_seen_at,
                h.paste_count,
                h.is_pinned,
                h.sort_order,
                h.last_source_app_name,
                a.sha256,
                a.byte_length,
                a.relative_path,
                f.id,
                COALESCE((
                    SELECT GROUP_CONCAT(cit.tag_id, ',')
                    FROM clipboard_item_tags AS cit
                    WHERE cit.item_id = i.id
                ), ''),
                COALESCE(n.note_text, '')
             FROM clipboard_history AS h
             JOIN clipboard_items AS i ON i.id = h.item_id
             LEFT JOIN favorites AS f ON f.item_id = h.item_id
             LEFT JOIN clipboard_item_notes AS n ON n.item_id = i.id
             LEFT JOIN application_icon_assets AS a ON a.id = h.last_source_app_icon_id
             WHERE {}
             ORDER BY h.is_pinned DESC,
                      h.sort_order DESC,
                      h.last_seen_at DESC,
                      h.id DESC
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
                row.get::<_, Option<i64>>(13)?,
                row.get::<_, String>(14)?,
                row.get::<_, String>(15)?,
            ))
        })?;
        let mut records = Vec::new();
        for row in rows {
            let (
                history_id,
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
                favorite_id,
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
                Some(&self.blob_store),
            )?;
            let selected_preview = loaded_preview_options.visible.first();
            records.push(HistoryListRecord {
                history_id,
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
                favorite_id,
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

fn append_history_scope_filter(
    conditions: &mut Vec<String>,
    parameters: &mut Vec<Value>,
    policy: Option<&HistoryConfig>,
) {
    let Some(policy) = policy else {
        return;
    };
    if let Some((condition, values)) = history_scope_condition(policy) {
        conditions.push(condition);
        parameters.extend(values);
    }
}

fn history_scope_condition(policy: &HistoryConfig) -> Option<(String, Vec<Value>)> {
    if policy.retention_days.is_none() && policy.max_items.is_none() {
        return None;
    }

    let mut active_conditions = Vec::new();
    let mut parameters = Vec::new();
    if let Some(days) = policy.retention_days {
        let cutoff =
            current_timestamp_millis().saturating_sub(i64::from(days).saturating_mul(86_400_000));
        active_conditions.push("h.last_seen_at >= ?".to_owned());
        parameters.push(Value::Integer(cutoff));
    }

    if let Some(limit) = policy.max_items {
        active_conditions.push(format!(
            "h.id IN (
                SELECT ranked.id
                FROM (
                    SELECT candidate.id,
                           ROW_NUMBER() OVER (
                               ORDER BY candidate.sort_order DESC,
                                        candidate.last_seen_at DESC,
                                        candidate.id DESC
                           ) AS retention_rank
                    FROM clipboard_history AS candidate
                    JOIN clipboard_items AS candidate_item
                      ON candidate_item.id = candidate.item_id
                    WHERE candidate.is_pinned = 0
                      AND candidate_item.capture_state = 'ready'
                ) AS ranked
                WHERE ranked.retention_rank <= ?
            )"
        ));
        parameters.push(Value::Integer(i64::from(limit)));
    }

    let ordinary_scope = active_conditions.join(" AND ");
    Some((
        format!("(h.is_pinned = 1 OR ({ordinary_scope}))"),
        parameters,
    ))
}

fn cleanup_orphaned_in_transaction(
    transaction: &Transaction<'_>,
    blob_store: &RawBlobStore,
    deleted_item_id: Option<i64>,
) -> Result<HistoryCleanupReport, HistoryCleanupError> {
    if !Database::maintenance_supported(transaction)? {
        return Ok(HistoryCleanupReport::default());
    }
    let mut report = HistoryCleanupReport::default();
    let mut orphan_item_ids = Vec::new();
    let mut targeted_blobs = HashMap::new();

    loop {
        orphan_item_ids.clear();
        if let Some(item_id) = deleted_item_id {
            let orphan_item_id = transaction
                .query_row(
                    "SELECT i.id
                     FROM clipboard_items AS i
                     WHERE i.id = ?1
                       AND i.capture_state = 'ready'
                       AND NOT EXISTS (
                         SELECT 1 FROM clipboard_history AS h WHERE h.item_id = i.id
                       )
                       AND NOT EXISTS (
                         SELECT 1 FROM favorites AS f WHERE f.item_id = i.id
                       )
                       AND NOT EXISTS (
                         SELECT 1 FROM clipboard_items AS child
                         WHERE child.parent_item_id = i.id
                       )",
                    [item_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            if let Some(orphan_item_id) = orphan_item_id {
                orphan_item_ids.push(orphan_item_id);
            }
        } else {
            let mut statement = transaction.prepare(
                "SELECT i.id
                 FROM clipboard_items AS i
                 WHERE i.capture_state = 'ready'
                   AND NOT EXISTS (
                     SELECT 1 FROM clipboard_history AS h WHERE h.item_id = i.id
                 )
                   AND NOT EXISTS (
                     SELECT 1 FROM favorites AS f WHERE f.item_id = i.id
                   )
                   AND NOT EXISTS (
                     SELECT 1 FROM clipboard_items AS child
                     WHERE child.parent_item_id = i.id
                 )",
            )?;
            let rows = statement.query_map([], |row| row.get::<_, i64>(0))?;
            orphan_item_ids.extend(rows.collect::<Result<Vec<_>, _>>()?);
        }

        if orphan_item_ids.is_empty() {
            break;
        }

        if deleted_item_id.is_some() {
            for (blob_id, blob) in load_blobs_for_items(transaction, &orphan_item_ids)? {
                targeted_blobs.entry(blob_id).or_insert(blob);
            }
        }

        for item_id in orphan_item_ids.iter().copied() {
            report.orphan_items_removed +=
                transaction.execute("DELETE FROM clipboard_items WHERE id = ?1", [item_id])?;
        }

        if deleted_item_id.is_some() {
            break;
        }
    }

    if deleted_item_id.is_some() {
        delete_unreferenced_blob_candidates(
            transaction,
            blob_store,
            targeted_blobs.into_iter().collect(),
            &mut report,
        )?;
    } else {
        let orphan_blobs = load_unreferenced_blobs(transaction)?;
        for (blob_id, blob) in orphan_blobs {
            if blob_store.delete_if_unreferenced(&blob, 0)? {
                report.orphan_blob_files_removed += 1;
            }
            report.orphan_blobs_removed += transaction.execute(
                "DELETE FROM binary_blobs
                 WHERE id = ?1
                   AND NOT EXISTS (
                       SELECT 1 FROM clipboard_formats WHERE blob_id = ?1
                   )",
                [blob_id],
            )?;
        }
    }

    Ok(report)
}

fn cleanup_orphaned_items_in_transaction(
    transaction: &Transaction<'_>,
    blob_store: &RawBlobStore,
    item_ids: &[ClipboardItemId],
    include_ancestors: bool,
) -> Result<HistoryCleanupReport, HistoryCleanupError> {
    if !Database::maintenance_supported(transaction)? {
        return Ok(HistoryCleanupReport::default());
    }
    let mut candidate_item_ids = item_ids.iter().copied().collect::<HashSet<_>>();
    if include_ancestors {
        let mut statement = transaction.prepare(
            "WITH RECURSIVE ancestors(id, parent_item_id) AS (
                 SELECT id, parent_item_id FROM clipboard_items WHERE id = ?1
                 UNION
                 SELECT parent.id, parent.parent_item_id
                 FROM clipboard_items AS parent
                 JOIN ancestors AS child ON parent.id = child.parent_item_id
             )
             SELECT id FROM ancestors",
        )?;
        for item_id in item_ids {
            let rows = statement.query_map([item_id], |row| row.get::<_, i64>(0))?;
            candidate_item_ids.extend(rows.collect::<Result<Vec<_>, _>>()?);
        }
    }

    let mut report = HistoryCleanupReport::default();
    loop {
        let orphan_item_ids = {
            let mut statement = transaction.prepare(
                "SELECT i.id
                 FROM clipboard_items AS i
                 WHERE i.id = ?1
                   AND i.capture_state = 'ready'
                   AND NOT EXISTS (
                     SELECT 1 FROM clipboard_history AS h WHERE h.item_id = i.id
                 )
                   AND NOT EXISTS (
                     SELECT 1 FROM favorites AS f WHERE f.item_id = i.id
                 )
                   AND NOT EXISTS (
                     SELECT 1 FROM clipboard_items AS child
                     WHERE child.parent_item_id = i.id
                 )",
            )?;
            let mut orphan_item_ids = Vec::new();
            for item_id in &candidate_item_ids {
                if let Some(orphan_item_id) = statement
                    .query_row([item_id], |row| row.get::<_, i64>(0))
                    .optional()?
                {
                    orphan_item_ids.push(orphan_item_id);
                }
            }
            orphan_item_ids
        };
        if orphan_item_ids.is_empty() {
            break;
        }

        let orphan_blobs = load_blobs_for_items(transaction, &orphan_item_ids)?;
        for item_id in orphan_item_ids {
            report.orphan_items_removed +=
                transaction.execute("DELETE FROM clipboard_items WHERE id = ?1", [item_id])?;
        }
        delete_unreferenced_blob_candidates(transaction, blob_store, orphan_blobs, &mut report)?;
    }

    Ok(report)
}

fn load_blobs_for_items(
    transaction: &Transaction<'_>,
    item_ids: &[ClipboardItemId],
) -> Result<Vec<(i64, RawBlob)>, HistoryCleanupError> {
    let mut blobs = HashMap::new();
    let mut statement = transaction.prepare(
        "SELECT DISTINCT b.id, b.sha256, b.byte_length, b.relative_path
         FROM binary_blobs AS b
         JOIN clipboard_formats AS f ON f.blob_id = b.id
         WHERE f.item_id = ?1",
    )?;
    for item_id in item_ids {
        let rows = statement.query_map([item_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (blob_id, hash, byte_length, relative_path) = row?;
            if blobs.contains_key(&blob_id) {
                continue;
            }
            let hash_length = hash.len();
            let sha256 = hash
                .try_into()
                .map_err(|_| HistoryCleanupError::InvalidHashLength(hash_length))?;
            blobs.insert(
                blob_id,
                RawBlob::from_record(
                    sha256,
                    u64::try_from(byte_length).unwrap_or(0),
                    relative_path,
                ),
            );
        }
    }
    Ok(blobs.into_iter().collect())
}

fn delete_unreferenced_blob_candidates(
    transaction: &Transaction<'_>,
    blob_store: &RawBlobStore,
    candidates: Vec<(i64, RawBlob)>,
    report: &mut HistoryCleanupReport,
) -> Result<(), HistoryCleanupError> {
    for (blob_id, blob) in candidates {
        let is_referenced: bool = transaction.query_row(
            "SELECT EXISTS (
                 SELECT 1 FROM clipboard_formats WHERE blob_id = ?1
             )",
            [blob_id],
            |row| row.get(0),
        )?;
        if is_referenced {
            continue;
        }
        if blob_store.delete_if_unreferenced(&blob, 0)? {
            report.orphan_blob_files_removed += 1;
        }
        report.orphan_blobs_removed += transaction.execute(
            "DELETE FROM binary_blobs
             WHERE id = ?1
               AND NOT EXISTS (
                   SELECT 1 FROM clipboard_formats WHERE blob_id = ?1
               )",
            [blob_id],
        )?;
    }
    Ok(())
}

fn log_cleanup_report(history_id: Option<HistoryItemId>, report: HistoryCleanupReport) {
    if report.history_items_removed == 0
        && report.orphan_items_removed == 0
        && report.orphan_blobs_removed == 0
        && report.orphan_blob_files_removed == 0
    {
        return;
    }

    tracing::debug!(
        history_id = ?history_id,
        history_items = report.history_items_removed,
        orphan_items = report.orphan_items_removed,
        orphan_blobs = report.orphan_blobs_removed,
        orphan_blob_files = report.orphan_blob_files_removed,
        "历史数据清理完成"
    );
}

fn load_unreferenced_blobs(
    transaction: &Transaction<'_>,
) -> Result<Vec<(i64, RawBlob)>, HistoryCleanupError> {
    let mut statement = transaction.prepare(
        "SELECT b.id, b.sha256, b.byte_length, b.relative_path
         FROM binary_blobs AS b
         WHERE NOT EXISTS (
             SELECT 1 FROM clipboard_formats AS f WHERE f.blob_id = b.id
         )",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, Vec<u8>>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let rows = rows.collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(id, hash, byte_length, relative_path)| {
            let hash_length = hash.len();
            let sha256 = hash
                .try_into()
                .map_err(|_| HistoryCleanupError::InvalidHashLength(hash_length))?;
            Ok((
                id,
                RawBlob::from_record(
                    sha256,
                    u64::try_from(byte_length).unwrap_or(0),
                    relative_path,
                ),
            ))
        })
        .collect()
}

fn shift_history_order(
    transaction: &Transaction<'_>,
    is_pinned: bool,
    history_id: HistoryItemId,
    target_history_id: HistoryItemId,
    source_order: i64,
    target_order: i64,
) -> Result<(), HistoryReorderError> {
    if source_order == target_order {
        return Ok(());
    }
    let mut destination_order =
        history_order_between(transaction, is_pinned, source_order, target_order)?;
    if destination_order.is_none() {
        rebalance_history_order(transaction, is_pinned)?;
        let (source_order, target_order): (i64, i64) = transaction.query_row(
            "SELECT source.sort_order, target.sort_order
             FROM clipboard_history AS source, clipboard_history AS target
             WHERE source.id = ?1 AND target.id = ?2",
            rusqlite::params![history_id, target_history_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        destination_order =
            history_order_between(transaction, is_pinned, source_order, target_order)?;
    }
    let destination_order = destination_order.ok_or(HistoryReorderError::SortOrderOverflow)?;
    let timestamp = current_timestamp_millis();
    transaction.execute(
        "UPDATE clipboard_history
         SET sort_order = ?1, updated_at = ?2
         WHERE id = ?3",
        rusqlite::params![destination_order, timestamp, history_id],
    )?;
    Ok(())
}

fn history_order_between(
    transaction: &Transaction<'_>,
    is_pinned: bool,
    source_order: i64,
    target_order: i64,
) -> Result<Option<i64>, HistoryReorderError> {
    let (lower, upper) = if source_order < target_order {
        let previous_order = transaction
            .query_row(
                "SELECT sort_order FROM clipboard_history
                 WHERE is_pinned = ?1 AND sort_order > ?2
                 ORDER BY sort_order ASC LIMIT 1",
                rusqlite::params![is_pinned, target_order],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let Some(previous_order) = previous_order else {
            return target_order
                .checked_add(LIST_ORDER_GAP)
                .map(Some)
                .ok_or(HistoryReorderError::SortOrderOverflow);
        };
        (target_order, previous_order)
    } else {
        let next_order = transaction
            .query_row(
                "SELECT sort_order FROM clipboard_history
                 WHERE is_pinned = ?1 AND sort_order < ?2
                 ORDER BY sort_order DESC LIMIT 1",
                rusqlite::params![is_pinned, target_order],
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

fn rebalance_history_order(
    transaction: &Transaction<'_>,
    is_pinned: bool,
) -> Result<(), HistoryReorderError> {
    let mut statement = transaction.prepare(
        "SELECT id FROM clipboard_history
         WHERE is_pinned = ?1 ORDER BY sort_order DESC",
    )?;
    let ordered_ids = statement
        .query_map([is_pinned], |row| row.get::<_, HistoryItemId>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    if ordered_ids.is_empty() {
        return Ok(());
    }
    let max_order: i64 = transaction.query_row(
        "SELECT MAX(sort_order) FROM clipboard_history WHERE is_pinned = ?1",
        [is_pinned],
        |row| row.get(0),
    )?;
    let row_count =
        i64::try_from(ordered_ids.len()).map_err(|_| HistoryReorderError::SortOrderOverflow)?;
    let temporary_offset = max_order
        .checked_add(LIST_ORDER_BASE)
        .and_then(|value| value.checked_add(row_count))
        .and_then(|value| value.checked_add(1))
        .and_then(|value| value.checked_mul(LIST_ORDER_GAP))
        .ok_or(HistoryReorderError::SortOrderOverflow)?;
    let temporary_max = max_order
        .checked_add(temporary_offset)
        .ok_or(HistoryReorderError::SortOrderOverflow)?;
    let final_max = LIST_ORDER_BASE
        .checked_add(row_count)
        .and_then(|value| value.checked_mul(LIST_ORDER_GAP))
        .ok_or(HistoryReorderError::SortOrderOverflow)?;
    if temporary_offset <= max_order
        || temporary_max < temporary_offset
        || final_max >= temporary_offset
    {
        return Err(HistoryReorderError::SortOrderOverflow);
    }
    transaction.execute(
        "UPDATE clipboard_history SET sort_order = sort_order + ?1 WHERE is_pinned = ?2",
        rusqlite::params![temporary_offset, is_pinned],
    )?;
    let timestamp = current_timestamp_millis();
    for (index, history_id) in ordered_ids.iter().rev().enumerate() {
        let reverse_index =
            i64::try_from(index).map_err(|_| HistoryReorderError::SortOrderOverflow)?;
        let sort_order = LIST_ORDER_BASE
            .checked_add(reverse_index)
            .and_then(|value| value.checked_mul(LIST_ORDER_GAP))
            .ok_or(HistoryReorderError::SortOrderOverflow)?;
        transaction.execute(
            "UPDATE clipboard_history
             SET sort_order = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![sort_order, timestamp, history_id],
        )?;
    }
    Ok(())
}

fn current_timestamp_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use crate::{
        domain::capture::{CapturedClipboard, FormatKind, RawFormat, SourceApplication},
        services::{
            capture::{CaptureService, mock::MockClipboardSource},
            database::TagRepository,
            storage::RawBlobStore,
        },
    };

    use super::*;
    use crate::services::database::query::{HistoryContentFilter, HistoryFilter};

    struct TestServices {
        _directory: tempfile::TempDir,
        database: Database,
        capture: CaptureService,
        repository: HistoryRepository,
    }

    impl TestServices {
        fn new() -> Self {
            let directory = tempfile::tempdir().expect("无法创建测试临时目录");
            let database =
                Database::open(directory.path().join("test.sqlite3")).expect("无法创建测试数据库");
            let store = RawBlobStore::new(directory.path()).expect("无法创建测试文件存储");
            let capture = CaptureService::new(database.clone(), store.clone());
            let repository = HistoryRepository::new(database.clone(), store);
            Self {
                _directory: directory,
                database,
                capture,
                repository,
            }
        }

        fn store(&self, captured: CapturedClipboard) -> i64 {
            self.capture
                .store_captured_clipboard(&captured)
                .expect("无法保存测试捕获")
        }
    }

    fn file_capture() -> CapturedClipboard {
        let format = RawFormat::captured(
            "mime",
            "application/x-file-list",
            "文件列表",
            FormatKind::FileList,
            b"C:/test.txt".to_vec(),
            0,
            true,
        );
        CapturedClipboard {
            primary_format_identity: Some((
                format.format_namespace.clone(),
                format.format_key.clone(),
            )),
            formats: vec![format],
            preview_candidate: Some("1 个文件 · test.txt".to_owned()),
            source_application: SourceApplication::default(),
        }
    }

    #[test]
    fn clear_unpinned_removes_regular_and_pending_history_but_preserves_pins_and_favorites() {
        let mut services = TestServices::new();
        services.capture = CaptureService::with_inline_threshold(
            services.database.clone(),
            services.repository.blob_store.clone(),
            0,
        );
        let pinned_id = services.store(MockClipboardSource::text("保留的置顶记录").capture());
        let favorite_id = services.store(MockClipboardSource::text("保留的收藏内容").capture());
        services.store(MockClipboardSource::text("删除的普通记录").capture());
        let pending_id = services.store(MockClipboardSource::text("删除的待清理记录").capture());
        services
            .repository
            .set_pinned(pinned_id, true)
            .expect("置顶测试记录失败");
        let connection = services.database.connect().expect("连接测试数据库失败");
        connection
            .execute(
                "UPDATE clipboard_history SET first_seen_at = 0, last_seen_at = 0 WHERE id = ?1",
                [pending_id],
            )
            .expect("设置待清理历史失败");
        let favorite_item_id: i64 = connection
            .query_row(
                "SELECT item_id FROM clipboard_history WHERE id = ?1",
                [favorite_id],
                |row| row.get(0),
            )
            .expect("读取收藏内容失败");
        drop(connection);
        super::super::FavoriteRepository::new(services.database.clone())
            .add_to_favorites(favorite_item_id)
            .expect("添加测试收藏失败");

        let report = services
            .repository
            .clear_unpinned()
            .expect("清理非置顶历史失败");

        assert_eq!(report.history_items_removed, 3);
        assert_eq!(report.orphan_items_removed, 2);
        assert_eq!(report.orphan_blob_files_removed, 2);
        let records = services
            .repository
            .query(&HistoryQuery::default())
            .expect("读取历史失败");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].history_id, pinned_id);
        let connection = services.database.connect().expect("连接测试数据库失败");
        let counts: (i64, i64) = connection
            .query_row(
                "SELECT (SELECT COUNT(*) FROM favorites), (SELECT COUNT(*) FROM clipboard_items)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("核验保留内容失败");
        assert_eq!(counts, (1, 2));
        let paths = connection
            .prepare("SELECT relative_path FROM binary_blobs")
            .expect("读取原始数据路径失败")
            .query_map([], |row| row.get::<_, String>(0))
            .expect("查询原始数据路径失败")
            .collect::<Result<Vec<_>, _>>()
            .expect("解析原始数据路径失败");
        assert_eq!(paths.len(), 2);
        assert!(
            paths
                .iter()
                .all(|path| services._directory.path().join(path).exists())
        );
        drop(connection);
        assert_eq!(
            services
                .repository
                .clear_unpinned()
                .expect("重复清理失败")
                .history_items_removed,
            0
        );
    }

    #[test]
    fn clear_all_removes_pinned_and_pending_history_but_preserves_favorites() {
        let mut services = TestServices::new();
        services.capture = CaptureService::with_inline_threshold(
            services.database.clone(),
            services.repository.blob_store.clone(),
            0,
        );
        let history_id = services.store(file_capture());
        let second_id = services.store(CapturedClipboard {
            formats: vec![RawFormat::captured(
                "mime",
                "text/plain",
                "文本",
                FormatKind::Text,
                b"clear-all-test".to_vec(),
                0,
                true,
            )],
            primary_format_identity: Some(("mime".to_owned(), "text/plain".to_owned())),
            preview_candidate: Some("clear-all-test".to_owned()),
            source_application: SourceApplication::default(),
        });
        let connection = services.database.connect().expect("连接测试数据库失败");
        connection
            .execute(
                "UPDATE clipboard_history SET is_pinned = 1 WHERE id = ?1",
                [history_id],
            )
            .expect("置顶测试记录失败");
        connection
            .execute(
                "UPDATE clipboard_history SET first_seen_at = 0, last_seen_at = 0 WHERE id = ?1",
                [second_id],
            )
            .expect("设置待清理历史失败");
        let item_id: i64 = connection
            .query_row(
                "SELECT item_id FROM clipboard_history WHERE id = ?1",
                [history_id],
                |row| row.get(0),
            )
            .expect("读取收藏内容失败");
        drop(connection);
        super::super::FavoriteRepository::new(services.database.clone())
            .add_to_favorites(item_id)
            .expect("添加测试收藏失败");

        let report = services.repository.clear_all().expect("清空历史失败");
        assert_eq!(report.history_items_removed, 2);
        assert_eq!(report.orphan_items_removed, 1);
        assert_eq!(report.orphan_blob_files_removed, 1);
        assert_eq!(
            services.repository.count(&HistoryQuery::default()).unwrap(),
            0
        );
        let connection = services.database.connect().unwrap();
        let counts: (i64, i64) = connection
            .query_row(
                "SELECT (SELECT COUNT(*) FROM favorites), (SELECT COUNT(*) FROM clipboard_items)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(counts, (1, 1));
        let favorite_blob_path: String = connection
            .query_row("SELECT relative_path FROM binary_blobs", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(services._directory.path().join(favorite_blob_path).exists());
        drop(connection);
        assert_eq!(
            services
                .repository
                .clear_all()
                .unwrap()
                .history_items_removed,
            0
        );
    }

    #[test]
    fn content_and_paste_filters_are_applied_in_database() {
        let services = TestServices::new();
        services.store(MockClipboardSource::text("普通文本").capture());
        let link_id = services.store(MockClipboardSource::text("https://example.com").capture());
        services.store(file_capture());
        services
            .database
            .connect()
            .expect("无法连接数据库")
            .execute(
                "UPDATE clipboard_history SET paste_count = 1 WHERE id = ?1",
                [link_id],
            )
            .expect("无法更新粘贴次数");

        let query = HistoryQuery::from_filters([HistoryFilter::Link, HistoryFilter::Pasted]);
        let records = services.repository.query(&query).expect("历史筛选失败");
        assert_eq!(records.len(), 1);
        assert_eq!(services.repository.count(&query).expect("历史计数失败"), 1);
        assert_eq!(
            records[0].content_kind,
            crate::domain::clipboard::ContentKind::Link
        );

        let query = HistoryQuery::from_filters([HistoryFilter::Text, HistoryFilter::Unpasted]);
        let records = services.repository.query(&query).expect("文本筛选失败");
        assert_eq!(records.len(), 1);
        assert_eq!(services.repository.count(&query).expect("文本计数失败"), 1);
        assert_eq!(records[0].preview_text, "普通文本");
    }

    #[test]
    fn shared_tag_filter_combines_with_history_search() {
        let services = TestServices::new();
        let tagged_history_id = services.store(MockClipboardSource::text("带标签的文本").capture());
        services.store(MockClipboardSource::text("没有标签的文本").capture());
        let item_id: i64 = services
            .database
            .connect()
            .expect("无法连接测试数据库")
            .query_row(
                "SELECT item_id FROM clipboard_history WHERE id = ?1",
                [tagged_history_id],
                |row| row.get(0),
            )
            .expect("无法查询标签内容 ID");
        let tag = TagRepository::new(services.database.clone())
            .create("工作", "#1677FF")
            .expect("无法创建测试标签");
        TagRepository::new(services.database.clone())
            .add_to_item(item_id, tag.id)
            .expect("无法添加测试内容标签");

        let query = HistoryQuery {
            search: Some("带标签".to_owned()),
            content_filters: [HistoryContentFilter::Text].into_iter().collect(),
            tag_ids: vec![tag.id],
            ..HistoryQuery::default()
        };
        let records = services.repository.query(&query).expect("历史组合筛选失败");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].item_id, item_id);
        assert_eq!(
            services.repository.count(&query).expect("历史组合计数失败"),
            1
        );

        let unmatched_query = HistoryQuery {
            search: Some("没有标签".to_owned()),
            tag_ids: vec![tag.id],
            ..HistoryQuery::default()
        };
        assert!(
            services
                .repository
                .query(&unmatched_query)
                .expect("历史不匹配组合筛选失败")
                .is_empty()
        );
        assert_eq!(
            services
                .repository
                .count(&unmatched_query)
                .expect("历史不匹配组合计数失败"),
            0
        );
    }

    #[test]
    fn search_matches_recent_source_application_name() {
        let services = TestServices::new();
        let mut captured = MockClipboardSource::text("来源搜索").capture();
        captured.source_application.name = Some("测试浏览器".to_owned());
        services.store(captured);

        let query = HistoryQuery {
            search: Some("浏览器".to_owned()),
            ..HistoryQuery::default()
        };
        let records = services.repository.query(&query).expect("来源应用搜索失败");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].source_app_name.as_deref(), Some("测试浏览器"));
        assert_eq!(
            services.repository.count(&query).expect("来源应用计数失败"),
            1
        );
    }

    #[test]
    fn query_preserves_all_format_previews_for_same_content() {
        let services = TestServices::new();
        let text = RawFormat::captured(
            "windows",
            "CF_UNICODETEXT",
            "CF_UNICODETEXT",
            FormatKind::Text,
            "测试".encode_utf16().flat_map(u16::to_le_bytes).collect(),
            0,
            true,
        );
        let html = RawFormat::captured(
            "windows",
            "registered:HTML Format",
            "HTML Format",
            FormatKind::Html,
            "<b>富文本关键词</b>".as_bytes().to_vec(),
            1,
            true,
        );
        let captured = CapturedClipboard {
            primary_format_identity: Some((text.format_namespace.clone(), text.format_key.clone())),
            formats: vec![text, html],
            preview_candidate: None,
            source_application: SourceApplication::default(),
        };
        services.store(captured);

        let records = services
            .repository
            .query(&HistoryQuery::default())
            .expect("查询多格式预览失败");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].preview_options.len(), 2);
        assert_eq!(records[0].preview_options[0].format_key, "CF_UNICODETEXT");
        assert_eq!(
            records[0].preview_options[1].format_key,
            "registered:HTML Format"
        );

        let text_records = services
            .repository
            .query(&HistoryQuery::from_filters([HistoryFilter::Text]))
            .expect("文本筛选多格式内容失败");
        assert_eq!(text_records.len(), 1);

        let search_records = services
            .repository
            .query(&HistoryQuery {
                search: Some("富文本关键词".to_owned()),
                ..HistoryQuery::default()
            })
            .expect("搜索格式预览失败");
        assert_eq!(search_records.len(), 1);
    }

    #[test]
    fn query_exposes_semantic_candidates_while_retaining_raw_format_records() {
        let services = TestServices::new();
        let unicode = RawFormat::captured(
            "windows",
            "CF_UNICODETEXT",
            "CF_UNICODETEXT",
            FormatKind::Text,
            "测试".encode_utf16().flat_map(u16::to_le_bytes).collect(),
            1,
            true,
        );
        let ansi = RawFormat::captured(
            "windows",
            "CF_TEXT",
            "CF_TEXT",
            FormatKind::Text,
            b"test".to_vec(),
            0,
            true,
        );
        let locale = RawFormat::captured(
            "windows",
            "CF_LOCALE",
            "CF_LOCALE",
            FormatKind::Unknown,
            16_u32.to_le_bytes().to_vec(),
            2,
            true,
        );
        let chromium = RawFormat::captured(
            "windows",
            "registered:Chromium internal source URL",
            "Chromium internal source URL",
            FormatKind::Custom,
            b"https://example.com".to_vec(),
            3,
            true,
        );
        let item_id = services.store(CapturedClipboard {
            primary_format_identity: Some((
                unicode.format_namespace.clone(),
                unicode.format_key.clone(),
            )),
            formats: vec![ansi, unicode, locale, chromium],
            preview_candidate: None,
            source_application: SourceApplication::default(),
        });

        let records = services
            .repository
            .query(&HistoryQuery::default())
            .expect("查询语义格式候选失败");
        assert_eq!(records[0].preview_options.len(), 1);
        assert_eq!(records[0].preview_options[0].format_key, "CF_UNICODETEXT");

        let list_query = HistoryQuery {
            load_image_sources: true,
            ..HistoryQuery::default()
        };
        let list_records = services
            .repository
            .query(&list_query)
            .expect("查询全部格式失败");
        assert_eq!(
            list_records[0]
                .all_format_options
                .iter()
                .map(|preview| preview.format_key.as_str())
                .collect::<Vec<_>>(),
            [
                "CF_TEXT",
                "CF_UNICODETEXT",
                "CF_LOCALE",
                "registered:Chromium internal source URL"
            ]
        );

        let connection = services.database.connect().expect("无法连接数据库");
        let raw_format_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM clipboard_formats WHERE item_id = (
                    SELECT item_id FROM clipboard_history WHERE id = ?1
                )",
                [item_id],
                |row| row.get(0),
            )
            .expect("无法查询原始格式数量");
        assert_eq!(raw_format_count, 4);
    }

    #[test]
    fn image_html_is_classified_as_image_and_excluded_from_text_filter() {
        let services = TestServices::new();
        let html = RawFormat::captured(
            "windows",
            "registered:HTML Format",
            "HTML Format",
            FormatKind::Html,
            br#"<img src="data:image/png;base64,abc">"#.to_vec(),
            0,
            true,
        );
        let image = RawFormat::captured(
            "windows",
            "registered:PNG",
            "PNG",
            FormatKind::Image,
            b"image-bytes".to_vec(),
            1,
            true,
        );
        let empty_text = RawFormat::captured(
            "windows",
            "CF_UNICODETEXT",
            "CF_UNICODETEXT",
            FormatKind::Text,
            vec![0, 0],
            2,
            true,
        );
        services.store(CapturedClipboard {
            primary_format_identity: Some((html.format_namespace.clone(), html.format_key.clone())),
            formats: vec![html, image, empty_text],
            preview_candidate: None,
            source_application: SourceApplication::default(),
        });

        let records = services
            .repository
            .query(&HistoryQuery::default())
            .expect("查询图片型 HTML 失败");
        assert_eq!(
            records[0].content_kind,
            crate::domain::clipboard::ContentKind::Image
        );
        assert_eq!(records[0].preview_options.len(), 1);
        assert_eq!(records[0].preview_options[0].format_key, "registered:PNG");

        let text_query = HistoryQuery::from_filters([HistoryFilter::Text]);
        assert!(
            services
                .repository
                .query(&text_query)
                .expect("文本筛选失败")
                .is_empty()
        );
        assert_eq!(
            services
                .repository
                .count(&text_query)
                .expect("文本计数失败"),
            0
        );

        let image_query = HistoryQuery::from_filters([HistoryFilter::Image]);
        assert_eq!(
            services
                .repository
                .query(&image_query)
                .expect("图片筛选失败")
                .len(),
            1
        );
    }

    #[test]
    fn query_truncates_primary_and_all_format_previews_to_limit() {
        let services = TestServices::new();
        let text = RawFormat::captured(
            "windows",
            "CF_UNICODETEXT",
            "CF_UNICODETEXT",
            FormatKind::Text,
            "abcdefghij"
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect(),
            0,
            true,
        );
        let html = RawFormat::captured(
            "windows",
            "registered:HTML Format",
            "HTML Format",
            FormatKind::Html,
            "<b>abcdefghij</b>".as_bytes().to_vec(),
            1,
            true,
        );
        services.store(CapturedClipboard {
            primary_format_identity: Some((text.format_namespace.clone(), text.format_key.clone())),
            formats: vec![text, html],
            preview_candidate: None,
            source_application: SourceApplication::default(),
        });

        let full = services
            .repository
            .query(&HistoryQuery::default())
            .expect("查询完整预览失败");
        assert_eq!(full[0].preview_text, "abcdefghij");
        assert_eq!(full[0].preview_char_count, 10);

        let limited_query = HistoryQuery {
            preview_char_limit: Some(3),
            ..HistoryQuery::default()
        };
        let limited = services
            .repository
            .query(&limited_query)
            .expect("查询裁剪预览失败");
        assert_eq!(limited[0].preview_text, "abc");
        assert_eq!(limited[0].preview_char_count, 10);
        assert_eq!(limited[0].preview_options.len(), 2);
        assert!(
            limited[0]
                .preview_options
                .iter()
                .all(|preview| preview.text == "abc" && preview.char_count == 10)
        );
    }

    #[test]
    fn raw_image_references_load_only_when_explicitly_requested() {
        let services = TestServices::new();
        let image = RawFormat::captured(
            "mime",
            "image/png",
            "PNG",
            FormatKind::Image,
            vec![137, 80, 78, 71, 13, 10, 26, 10],
            0,
            true,
        );
        services.store(CapturedClipboard {
            primary_format_identity: Some((
                image.format_namespace.clone(),
                image.format_key.clone(),
            )),
            formats: vec![image],
            preview_candidate: None,
            source_application: SourceApplication::default(),
        });

        let full = services
            .repository
            .query(&HistoryQuery::default())
            .expect("查询默认图片预览失败");
        assert!(full[0].preview_options[0].image_source.is_none());

        let list_query = HistoryQuery {
            load_image_sources: true,
            ..HistoryQuery::default()
        };
        let list = services
            .repository
            .query(&list_query)
            .expect("查询列表图片引用失败");
        assert!(list[0].preview_options[0].image_source.is_some());
    }

    #[test]
    fn list_query_keeps_image_fallback_but_exposes_one_user_image() {
        let services = TestServices::new();
        let png = RawFormat::captured(
            "windows",
            "registered:PNG",
            "PNG",
            FormatKind::Image,
            vec![1, 2, 3],
            0,
            true,
        );
        let dibv5 = RawFormat::captured(
            "windows",
            "CF_DIBV5",
            "CF_DIBV5",
            FormatKind::Image,
            vec![4, 5, 6],
            1,
            true,
        );
        let dib = RawFormat::captured(
            "windows",
            "CF_DIB",
            "CF_DIB",
            FormatKind::Image,
            vec![7, 8, 9],
            2,
            true,
        );
        services.store(CapturedClipboard {
            primary_format_identity: Some((png.format_namespace.clone(), png.format_key.clone())),
            formats: vec![png, dibv5, dib],
            preview_candidate: None,
            source_application: SourceApplication::default(),
        });

        let query = HistoryQuery {
            load_image_sources: true,
            ..HistoryQuery::default()
        };
        let record = &services
            .repository
            .query(&query)
            .expect("查询图片回退格式失败")[0];
        assert_eq!(
            record
                .preview_options
                .iter()
                .map(|preview| preview.format_key.as_str())
                .collect::<Vec<_>>(),
            ["registered:PNG", "CF_DIB", "CF_DIBV5"]
        );
        assert_eq!(record.all_format_options.len(), 3);
        assert_eq!(
            crate::domain::clipboard::select_user_visible_previews(&record.preview_options)
                .iter()
                .map(|preview| preview.format_key.as_str())
                .collect::<Vec<_>>(),
            ["registered:PNG"]
        );
    }

    #[test]
    fn query_orders_pinned_partition_and_sort_order_descending() {
        let services = TestServices::new();
        let first = services.store(MockClipboardSource::text("第一条").capture());
        let second = services.store(MockClipboardSource::text("第二条").capture());
        let third = services.store(MockClipboardSource::text("第三条").capture());
        let connection = services.database.connect().expect("无法连接数据库");
        connection
            .execute(
                "UPDATE clipboard_history SET is_pinned = 1, sort_order = 0 WHERE id = ?1",
                [first],
            )
            .expect("无法置顶测试记录");

        let records = services
            .repository
            .query(&HistoryQuery::default())
            .expect("历史排序失败");
        assert_eq!(
            records
                .iter()
                .map(|record| record.history_id)
                .collect::<Vec<_>>(),
            vec![first, third, second]
        );
        assert_eq!(
            services
                .repository
                .ordered_ids(&HistoryQuery::default())
                .expect("历史 ID 排序失败"),
            vec![first, third, second]
        );
        connection
            .execute(
                "UPDATE clipboard_history SET paste_count = 17 WHERE id = ?1",
                [third],
            )
            .expect("无法更新测试粘贴次数");
        let query = HistoryQuery {
            paste_filter: Some(PasteFilter::Pasted),
            ..HistoryQuery::default()
        };
        let snapshot = services
            .repository
            .ordered_snapshot_scoped(&query, &HistoryConfig::default())
            .expect("无法读取历史热度快照");
        assert_eq!(snapshot.ids, vec![third]);
        assert_eq!(snapshot.paste_counts, vec![17]);
    }

    #[test]
    fn reorder_moves_item_and_rewrites_only_its_partition() {
        let services = TestServices::new();
        let first = services.store(MockClipboardSource::text("第一条").capture());
        let second = services.store(MockClipboardSource::text("第二条").capture());
        let third = services.store(MockClipboardSource::text("第三条").capture());

        assert!(
            services
                .repository
                .reorder(first, third)
                .expect("历史排序失败")
        );

        let records = services
            .repository
            .query(&HistoryQuery::default())
            .expect("查询重排后的历史失败");
        assert_eq!(
            records
                .iter()
                .map(|record| record.history_id)
                .collect::<Vec<_>>(),
            vec![first, third, second]
        );
        let sort_orders = records
            .iter()
            .map(|record| record.sort_order)
            .collect::<Vec<_>>();
        assert!(
            sort_orders.windows(2).all(|pair| pair[0] > pair[1]),
            "历史排序应保持降序：{sort_orders:?}"
        );
    }

    #[test]
    fn reorder_does_not_cross_pinned_partition() {
        let services = TestServices::new();
        let first = services.store(MockClipboardSource::text("置顶项").capture());
        let second = services.store(MockClipboardSource::text("普通项").capture());
        services
            .database
            .connect()
            .expect("无法连接测试数据库")
            .execute(
                "UPDATE clipboard_history SET is_pinned = 1, sort_order = 0 WHERE id = ?1",
                [first],
            )
            .expect("无法置顶测试记录");

        assert!(
            !services
                .repository
                .reorder(second, first)
                .expect("跨分区排序不应失败")
        );
        let records = services
            .repository
            .query(&HistoryQuery::default())
            .expect("查询分区排序失败");
        assert_eq!(
            records
                .iter()
                .map(|record| record.history_id)
                .collect::<Vec<_>>(),
            vec![first, second]
        );
    }

    #[test]
    fn set_pinned_moves_history_to_the_top_of_the_target_partition() {
        let services = TestServices::new();
        let first = services.store(MockClipboardSource::text("第一条").capture());
        let second = services.store(MockClipboardSource::text("第二条").capture());
        let third = services.store(MockClipboardSource::text("第三条").capture());

        assert!(
            services
                .repository
                .set_pinned(second, true)
                .expect("历史置顶失败")
        );
        assert!(
            services
                .repository
                .set_pinned(third, true)
                .expect("第二条历史置顶失败")
        );

        let records = services
            .repository
            .query(&HistoryQuery::default())
            .expect("查询置顶历史失败");
        assert_eq!(
            records
                .iter()
                .map(|record| (record.history_id, record.is_pinned))
                .collect::<Vec<_>>(),
            vec![(third, true), (second, true), (first, false)]
        );
        for is_pinned in [true, false] {
            let sort_orders = records
                .iter()
                .filter(|record| record.is_pinned == is_pinned)
                .map(|record| record.sort_order)
                .collect::<Vec<_>>();
            assert!(
                sort_orders.windows(2).all(|pair| pair[0] > pair[1]),
                "同一分区内历史排序应保持降序：{sort_orders:?}"
            );
        }

        assert!(
            services
                .repository
                .set_pinned(third, false)
                .expect("取消历史置顶失败")
        );
        let records = services
            .repository
            .query(&HistoryQuery::default())
            .expect("查询取消置顶历史失败");
        assert_eq!(records[0].history_id, second);
        assert!(records[0].is_pinned);
    }

    #[test]
    fn search_treats_wildcards_as_literal_characters() {
        let services = TestServices::new();
        services.store(MockClipboardSource::text("进度 100%_完成").capture());
        services.store(MockClipboardSource::text("进度 1000 已完成").capture());
        let query = HistoryQuery {
            search: Some("100%_".to_owned()),
            ..HistoryQuery::default()
        };

        let records = services.repository.query(&query).expect("历史搜索失败");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].preview_text, "进度 100%_完成");
    }

    #[test]
    fn empty_result_is_not_database_error_and_can_exclude_pinned_items() {
        let services = TestServices::new();
        let history_id = services.store(MockClipboardSource::text("置顶内容").capture());
        services
            .database
            .connect()
            .expect("无法连接数据库")
            .execute(
                "UPDATE clipboard_history SET is_pinned = 1 WHERE id = ?1",
                [history_id],
            )
            .expect("无法置顶测试记录");
        let query = HistoryQuery {
            include_pinned: false,
            content_filters: [HistoryContentFilter::Image].into_iter().collect(),
            ..HistoryQuery::default()
        };

        assert!(
            services
                .repository
                .query(&query)
                .expect("空查询失败")
                .is_empty()
        );
    }

    #[test]
    fn deleting_unreferenced_history_cleans_formats_and_external_raw_files() {
        let directory = tempfile::tempdir().expect("无法创建测试临时目录");
        let database =
            Database::open(directory.path().join("test.sqlite3")).expect("无法创建测试数据库");
        let blob_store = RawBlobStore::new(directory.path()).expect("无法创建测试文件存储");
        let capture =
            CaptureService::with_inline_threshold(database.clone(), blob_store.clone(), 0);
        let repository = HistoryRepository::new(database.clone(), blob_store);
        let history_id = capture
            .store_captured_clipboard(&MockClipboardSource::text("待删除内容").capture())
            .expect("无法保存待删除内容");
        let (item_id, relative_path): (i64, String) = database
            .connect()
            .expect("无法连接测试数据库")
            .query_row(
                "SELECT h.item_id, b.relative_path
                 FROM clipboard_history AS h
                 JOIN clipboard_formats AS f ON f.item_id = h.item_id
                 JOIN binary_blobs AS b ON b.id = f.blob_id
                 WHERE h.id = ?1",
                [history_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("无法读取待删除内容");
        let blob_path = directory.path().join(relative_path);
        assert!(blob_path.is_file());

        assert!(repository.delete(history_id).expect("删除历史失败"));

        let connection = database.connect().expect("无法重新连接测试数据库");
        let counts: (i64, i64, i64, i64) = connection
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM clipboard_history WHERE id = ?1),
                    (SELECT COUNT(*) FROM clipboard_items WHERE id = ?2),
                    (SELECT COUNT(*) FROM clipboard_formats WHERE item_id = ?2),
                    (SELECT COUNT(*) FROM binary_blobs)",
                [history_id, item_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("无法验证删除结果");
        assert_eq!(counts, (0, 0, 0, 0));
        assert!(!blob_path.exists());
    }

    #[test]
    fn deleting_history_preserves_favorited_content() {
        let services = TestServices::new();
        let history_id = services.store(MockClipboardSource::text("收藏内容").capture());
        let connection = services.database.connect().expect("无法连接测试数据库");
        let item_id: i64 = connection
            .query_row(
                "SELECT item_id FROM clipboard_history WHERE id = ?1",
                [history_id],
                |row| row.get(0),
            )
            .expect("无法读取收藏内容");
        connection
            .execute(
                "INSERT INTO favorites (uuid, item_id, is_pinned, sort_order, added_at, updated_at)
                 VALUES ('favorite-delete-test', ?1, 0, 0, 1, 1)",
                [item_id],
            )
            .expect("无法创建测试收藏");

        assert!(
            services
                .repository
                .delete(history_id)
                .expect("删除历史失败")
        );

        let counts: (i64, i64, i64) = services
            .database
            .connect()
            .expect("无法重新连接测试数据库")
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM clipboard_history WHERE id = ?1),
                    (SELECT COUNT(*) FROM clipboard_items WHERE id = ?2),
                    (SELECT COUNT(*) FROM favorites WHERE item_id = ?2)",
                [history_id, item_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("无法验证收藏保留结果");
        assert_eq!(counts, (0, 1, 1));
    }

    #[test]
    fn startup_cleanup_removes_unreferenced_deleted_history_content() {
        let services = TestServices::new();
        let history_id = services.store(MockClipboardSource::text("遗留孤儿内容").capture());
        services
            .database
            .connect()
            .expect("无法连接测试数据库")
            .execute("DELETE FROM clipboard_history WHERE id = ?1", [history_id])
            .expect("无法模拟历史删除");

        let report = services
            .repository
            .cleanup_orphaned()
            .expect("启动孤儿清理失败");

        assert_eq!(report.orphan_items_removed, 1);
        let connection = services.database.connect().expect("无法重新连接测试数据库");
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM clipboard_items", [], |row| row.get(0))
            .expect("无法验证孤儿清理结果");
        assert_eq!(count, 0);
    }
}
