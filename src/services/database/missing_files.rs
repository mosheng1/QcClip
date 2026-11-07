use thiserror::Error;

use crate::config::HistoryConfig;
use crate::domain::clipboard::ClipboardItemId;

use super::{Database, DatabaseError};

#[derive(Debug, Error)]
pub(crate) enum MissingFilesError {
    #[error("无法连接历史数据库：{0}")]
    Database(#[from] DatabaseError),
    #[error("更新失效文件隐藏状态失败：{0}")]
    Sql(#[from] rusqlite::Error),
    #[error("移动恢复条目失败：{0}")]
    Reorder(#[from] crate::services::database::HistoryReorderError),
}

impl Database {
    /// Discard imported hidden rows; the next scan rebuilds them for this machine.
    pub(crate) fn clear_hidden_missing_items(&self) -> Result<bool, MissingFilesError> {
        if !self.has_hidden_items_table() {
            return Ok(false);
        }
        let connection = self.connect_write()?;
        Ok(connection.execute("DELETE FROM hidden_missing_items", [])? > 0)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ScanFormat {
    pub namespace: String,
    pub kind: String,
    pub key: String,
    pub name: String,
    pub storage: String,
    pub inline_data: Option<Vec<u8>>,
    pub hash: Option<Vec<u8>>,
    pub length: i64,
    pub path: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ScanCandidate {
    pub item_id: ClipboardItemId,
    pub formats: Vec<ScanFormat>,
}

const SCAN_SQL: &str = "SELECT i.id,
        f.format_kind, f.format_key, f.format_name, f.storage_kind,
        CASE WHEN f.format_kind = 'file_list' THEN f.inline_data END,
        b.sha256, b.byte_length, b.relative_path,
        f.format_namespace
    FROM clipboard_items AS i
    LEFT JOIN clipboard_formats AS f ON f.item_id = i.id
    LEFT JOIN binary_blobs AS b ON b.id = f.blob_id
    WHERE i.capture_state = 'ready'
      AND (EXISTS (SELECT 1 FROM clipboard_history AS h WHERE h.item_id = i.id)
           OR EXISTS (SELECT 1 FROM favorites AS kept WHERE kept.item_id = i.id))
      AND EXISTS (SELECT 1 FROM clipboard_formats AS target
                  WHERE target.item_id = i.id AND target.format_kind IN ('image', 'file_list'))
    ORDER BY i.id, f.format_order";

/// Load the database snapshot used by the filesystem missing-file scan.
pub(crate) fn load_scan_candidates(
    database: &Database,
) -> Result<Vec<ScanCandidate>, DatabaseError> {
    let connection = database.connect_read()?;
    let mut statement = connection
        .prepare(SCAN_SQL)
        .map_err(|source| DatabaseError::Inspect { source })?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, ClipboardItemId>(0)?,
                if let Some(kind) = row.get::<_, Option<String>>(1)? {
                    Some(ScanFormat {
                        namespace: row.get::<_, Option<String>>(9)?.unwrap_or_default(),
                        kind,
                        key: row.get(2)?,
                        name: row.get(3)?,
                        storage: row.get(4)?,
                        inline_data: row.get(5)?,
                        hash: row.get(6)?,
                        length: row.get::<_, Option<i64>>(7)?.unwrap_or_default(),
                        path: row.get(8)?,
                    })
                } else {
                    None
                },
            ))
        })
        .map_err(|source| DatabaseError::Inspect { source })?;
    let mut candidates: Vec<ScanCandidate> = Vec::new();
    for row in rows {
        let (item_id, format) = row.map_err(|source| DatabaseError::Inspect { source })?;
        if candidates.last().is_none_or(|last| last.item_id != item_id) {
            candidates.push(ScanCandidate {
                item_id,
                formats: Vec::new(),
            });
        }
        if let Some(format) = format {
            candidates.last_mut().unwrap().formats.push(format);
        }
    }
    Ok(candidates)
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct MissingFilesUpdate {
    pub changed: bool,
    pub recovered: Vec<ClipboardItemId>,
}

/// Hide only the items recorded in `hidden_missing_items`. The condition works
/// against the row's own item id, so callers do not need to join items or
/// history just to apply hidden visibility.
pub(super) fn append_hidden_filter(
    conditions: &mut Vec<String>,
    database: &Database,
    policy: Option<&HistoryConfig>,
    item_id_expression: &str,
) {
    if !policy.is_some_and(|policy| policy.auto_hide_missing_files)
        || !database.has_hidden_items_table()
    {
        return;
    }
    conditions.push(format!(
        "NOT EXISTS (
            SELECT 1 FROM hidden_missing_items AS hidden_item
            WHERE hidden_item.item_id = {item_id_expression}
        )"
    ));
}
