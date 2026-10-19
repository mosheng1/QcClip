use std::{
    collections::{HashMap, HashSet},
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use uuid::Uuid;

use crate::domain::{
    capture::{
        CapturedClipboard, FormatKind, RestoreState, classify_format_role, normalize_text_bytes,
    },
    clipboard::HistoryItemId,
};

use super::{
    CaptureError, generate_preview, hash_clipboard, hash_format_set, select_default_preview,
    text_merge::find_text_merge,
};
use crate::services::{
    application_icon::{ApplicationIconStore, upsert_application_icon},
    database::{Database, LIST_ORDER_GAP},
    storage::{
        RawBlob, RawBlobStore, image_extension_from_header, image_extension_rank,
        save_images_as_files,
    },
};

const DEFAULT_INLINE_THRESHOLD: usize = 64 * 1024;

#[derive(Clone, Debug)]
pub struct CaptureService {
    database: Database,
    blob_store: RawBlobStore,
    application_icon_store: ApplicationIconStore,
    inline_threshold: usize,
}

impl CaptureService {
    pub fn new(database: Database, blob_store: RawBlobStore) -> Self {
        let icon_root = database
            .path()
            .parent()
            .expect("数据库路径缺少应用数据目录")
            .to_path_buf();
        Self {
            database,
            blob_store,
            application_icon_store: ApplicationIconStore::new(icon_root),
            inline_threshold: DEFAULT_INLINE_THRESHOLD,
        }
    }

    #[cfg(test)]
    pub fn with_inline_threshold(
        database: Database,
        blob_store: RawBlobStore,
        inline_threshold: usize,
    ) -> Self {
        let icon_root = database
            .path()
            .parent()
            .expect("数据库路径缺少应用数据目录")
            .to_path_buf();
        Self {
            database,
            blob_store,
            application_icon_store: ApplicationIconStore::new(icon_root),
            inline_threshold,
        }
    }

    pub fn store_captured_clipboard(
        &self,
        captured: &CapturedClipboard,
    ) -> Result<HistoryItemId, CaptureError> {
        self.store_captured_clipboard_with_status(captured)
            .map(|result| result.history_id)
    }

    pub fn store_captured_clipboard_with_status(
        &self,
        captured: &CapturedClipboard,
    ) -> Result<CaptureStoreResult, CaptureError> {
        validate_formats(captured)?;
        let hashed = hash_clipboard(captured)?;
        select_primary_format(captured)?;
        let timestamp = current_timestamp_millis();
        let mut connection = self.database.connect_write()?;
        let prepared_icon = self
            .application_icon_store
            .resolve(&connection, &captured.source_application);
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let source_app_icon_id =
            upsert_application_icon(&transaction, prepared_icon.as_ref(), timestamp)?;

        let exact_existing = transaction
            .query_row(
                "SELECT id, capture_state
                 FROM clipboard_items
                 WHERE hash_version = ?1 AND content_hash = ?2",
                params![hashed.hash_version, hashed.content_hash.as_slice()],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let text_merge = find_text_merge(
            &transaction,
            captured,
            &self.blob_store,
            exact_existing.as_ref().map(|existing| existing.0),
            &hashed,
        )?;
        let captured = text_merge
            .as_ref()
            .and_then(|merged| merged.captured.as_ref())
            .unwrap_or(captured);
        let hashed = text_merge
            .as_ref()
            .and_then(|merged| merged.hashed.as_ref())
            .unwrap_or(&hashed);
        let existing = if text_merge
            .as_ref()
            .is_some_and(|merged| merged.formats_changed)
        {
            None
        } else if let Some(merged) = text_merge.as_ref() {
            transaction
                .query_row(
                    "SELECT id, capture_state FROM clipboard_items WHERE id = ?1",
                    [merged.item_id],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?
        } else {
            match exact_existing {
                Some(existing) => Some(existing),
                None => {
                    find_legacy_duplicate_item(&transaction, captured, hashed, &self.blob_store)?
                }
            }
        };
        if let Some((item_id, state)) = existing {
            if state != "ready" {
                return Err(CaptureError::InvalidFormat(format!(
                    "相同内容存在未完成的 {state} 记录，需先执行恢复"
                )));
            }
            let (history_id, is_new) = upsert_history(
                &transaction,
                item_id,
                captured,
                timestamp,
                source_app_icon_id,
            )?;
            transaction.commit()?;
            return Ok(CaptureStoreResult {
                history_id,
                is_new,
                formats_changed: false,
            });
        }

        let preview = generate_preview(captured)?;
        let default_preview = select_default_preview(captured, &preview.formats)
            .ok_or(CaptureError::NoCapturedFormats)?;
        let primary_index = select_primary_format(captured)?;
        let mut replaced_blob_ids = Vec::new();
        let mut reusable_blobs = HashMap::new();
        let item_id = if let Some(merged) = text_merge.as_ref() {
            let item_id = merged.item_id;
            {
                let mut statement = transaction.prepare(
                    "SELECT DISTINCT b.id, b.sha256, b.storage_state FROM clipboard_formats f
                     JOIN binary_blobs b ON b.id = f.blob_id
                     WHERE f.item_id = ?1",
                )?;
                for row in statement.query_map([item_id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })? {
                    let (blob_id, hash, state) = row?;
                    replaced_blob_ids.push(blob_id);
                    if state == "ready" {
                        reusable_blobs.insert(hash, blob_id);
                    }
                }
            }
            transaction.execute(
                "UPDATE clipboard_items SET capture_state = 'pending', hash_version = ?1,
                     content_hash = ?2, content_kind = ?3, preview_text = ?4,
                     preview_char_count = ?5, updated_at = ?6 WHERE id = ?7",
                params![
                    hashed.hash_version,
                    hashed.content_hash.as_slice(),
                    preview.content_kind.as_str(),
                    default_preview.text,
                    default_preview.char_count as i64,
                    timestamp,
                    item_id
                ],
            )?;
            transaction.execute(
                "DELETE FROM clipboard_formats WHERE item_id = ?1",
                [item_id],
            )?;
            item_id
        } else {
            transaction.execute(
                "INSERT INTO clipboard_items (
                uuid, origin_kind, hash_version, content_hash, content_kind,
                capture_state, preview_text, preview_char_count, format_count,
                total_bytes, created_at, updated_at
             ) VALUES (
                ?1, 'captured', ?2, ?3, ?4,
                'pending', ?5, ?6, 0, 0, ?7, ?7
             )",
                params![
                    Uuid::new_v4().to_string(),
                    hashed.hash_version,
                    hashed.content_hash.as_slice(),
                    preview.content_kind.as_str(),
                    default_preview.text,
                    default_preview.char_count as i64,
                    timestamp,
                ],
            )?;
            transaction.last_insert_rowid()
        };
        let mut total_bytes = 0_i64;
        let mut retained_blob_ids = HashSet::new();
        // 同一张图在剪贴板里往往同时提供 PNG、JPEG、WebP 等多种格式，
        // 只让优先级最高的一份落成明文图片，避免数据目录里出现同一张图的多个副本；
        // 其余格式照旧压缩保存，粘贴时各格式仍然齐全。
        let preferred_image_index = if save_images_as_files() {
            captured
                .formats
                .iter()
                .enumerate()
                .filter_map(|(index, format)| {
                    let extension = image_extension_from_header(format.bytes.as_deref()?)?;
                    Some((image_extension_rank(extension), index))
                })
                .min_by_key(|(rank, index)| (*rank, *index))
                .map(|(_, index)| index)
        } else {
            None
        };

        for (index, format) in captured.formats.iter().enumerate() {
            let is_primary = index == primary_index;
            let native_format_id = format.native_format_id.map(i64::from);
            let data_hash = hashed.format_hashes[index]
                .as_ref()
                .map(|hash| hash.as_slice());
            let format_preview = preview.formats.iter().find(|preview| {
                preview.format_namespace == format.format_namespace
                    && preview.format_key == format.format_key
            });
            let preview_text = format_preview.map(|preview| preview.text.as_str());
            let preview_char_count = format_preview.map(|preview| preview.char_count as i64);
            let preview_content_kind = format_preview.map(|preview| preview.content_kind.as_str());
            let image_width = format_preview.and_then(|preview| {
                preview
                    .image_dimensions
                    .map(|dimensions| i64::from(dimensions.width))
            });
            let image_height = format_preview.and_then(|preview| {
                preview
                    .image_dimensions
                    .map(|dimensions| i64::from(dimensions.height))
            });
            let (storage_kind, inline_data, blob_id, byte_length) = if format
                .capture_state
                .has_data()
            {
                let bytes = format.bytes.as_deref().ok_or_else(|| {
                    CaptureError::InvalidFormat(format!("格式“{}”缺少原始字节", format.format_name))
                })?;
                total_bytes = total_bytes.saturating_add(bytes.len() as i64);
                if bytes.len() <= self.inline_threshold {
                    ("inline", Some(bytes), None, bytes.len() as i64)
                } else if let Some(blob_id) = data_hash.and_then(|hash| reusable_blobs.get(hash)) {
                    // 已在加载合并候选时校验过原文件，同一事务内直接复用，避免重新压缩和落盘。
                    ("external", None, Some(*blob_id), bytes.len() as i64)
                } else {
                    // 只让优先级最高的图片格式落成明文，其余内容仍按原有方式压缩存储
                    let pending = match image_extension_from_header(bytes) {
                        Some(extension) if Some(index) == preferred_image_index => {
                            self.blob_store.write_pending_raw(bytes, extension)?
                        }
                        _ => self.blob_store.write_pending(bytes)?,
                    };
                    let blob = self.blob_store.commit_pending(&pending)?;
                    let blob_id = upsert_binary_blob(&transaction, &blob, timestamp)?;
                    ("external", None, Some(blob_id), bytes.len() as i64)
                }
            } else {
                ("none", None, None, 0)
            };

            if let Some(blob_id) = blob_id {
                retained_blob_ids.insert(blob_id);
            }

            transaction.execute(
                "INSERT INTO clipboard_formats (
                    uuid, item_id, format_order, format_namespace, format_key,
                    format_name, native_format_id, format_kind, is_primary,
                    storage_kind, inline_data, blob_id, byte_length, data_hash,
                    restore_state, restore_error_code, restore_error_message,
                    mime_type, encoding, capture_state, error_code, error_message,
                    preview_text, preview_char_count, preview_content_kind,
                    image_width, image_height, created_at
                 ) VALUES (
                    ?1, ?2, ?3, ?4, ?5,
                    ?6, ?7, ?8, ?9,
                    ?10, ?11, ?12, ?13, ?14,
                    ?15, ?16, ?17,
                    ?18, ?19, ?20, ?21, ?22,
                    ?23, ?24, ?25, ?26, ?27, ?28
                 )",
                params![
                    Uuid::new_v4().to_string(),
                    item_id,
                    i64::from(format.format_order),
                    format.format_namespace,
                    format.format_key,
                    format.format_name,
                    native_format_id,
                    format.format_kind.as_str(),
                    i64::from(is_primary),
                    storage_kind,
                    inline_data,
                    blob_id,
                    byte_length,
                    data_hash,
                    format.restore_state.as_str(),
                    format.restore_error_code,
                    format.restore_error_message,
                    format.mime_type,
                    format.encoding,
                    format.capture_state.as_str(),
                    format.error_code,
                    format.error_message,
                    preview_text,
                    preview_char_count,
                    preview_content_kind,
                    image_width,
                    image_height,
                    timestamp,
                ],
            )?;
        }

        transaction.execute(
            "UPDATE clipboard_items
             SET capture_state = 'ready', format_count = ?1, total_bytes = ?2, updated_at = ?3
             WHERE id = ?4",
            params![
                captured.formats.len() as i64,
                total_bytes,
                timestamp,
                item_id
            ],
        )?;
        let (history_id, is_new) = upsert_history(
            &transaction,
            item_id,
            captured,
            timestamp,
            source_app_icon_id,
        )?;
        transaction.commit()?;
        replaced_blob_ids.retain(|blob_id| !retained_blob_ids.contains(blob_id));
        if !replaced_blob_ids.is_empty()
            && let Err(error) = self.cleanup_replaced_blobs(&replaced_blob_ids)
        {
            tracing::warn!(%error, "合并后的旧格式文件清理失败，将由后续维护重试");
        }
        Ok(CaptureStoreResult {
            history_id,
            is_new,
            formats_changed: text_merge.is_some(),
        })
    }

    fn cleanup_replaced_blobs(&self, blob_ids: &[i64]) -> Result<(), CaptureError> {
        let mut connection = self.database.connect_write()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for blob_id in blob_ids {
            let record = transaction.query_row(
                "SELECT sha256, byte_length, relative_path FROM binary_blobs b
                 WHERE id = ?1 AND NOT EXISTS (SELECT 1 FROM clipboard_formats WHERE blob_id = b.id)",
                [blob_id],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?)),
            ).optional()?;
            let Some((hash, length, path)) = record else {
                continue;
            };
            let hash = hash
                .try_into()
                .map_err(|_| CaptureError::InvalidFormat("旧格式文件哈希长度无效".to_owned()))?;
            let length = u64::try_from(length)
                .map_err(|_| CaptureError::InvalidFormat("旧格式文件长度无效".to_owned()))?;
            self.blob_store
                .delete_if_unreferenced(&RawBlob::from_record(hash, length, path), 0)?;
            transaction.execute("DELETE FROM binary_blobs WHERE id = ?1", [blob_id])?;
        }
        transaction.commit()?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureStoreResult {
    pub history_id: HistoryItemId,
    pub is_new: bool,
    pub formats_changed: bool,
}

fn find_legacy_duplicate_item(
    transaction: &Transaction<'_>,
    captured: &CapturedClipboard,
    hashed: &super::HashedClipboard,
    blob_store: &RawBlobStore,
) -> Result<Option<(i64, String)>, CaptureError> {
    let Some(unicode_hash) = captured
        .formats
        .iter()
        .enumerate()
        .find(|(_, format)| {
            format.format_namespace == "windows"
                && format.format_key == "CF_UNICODETEXT"
                && format.capture_state.has_data()
        })
        .and_then(|(index, _)| {
            hashed.format_hashes[index]
                .as_ref()
                .map(|hash| hash.as_slice())
        })
    else {
        return Ok(None);
    };

    let candidates = {
        let mut statement = transaction.prepare(
            "SELECT i.id, i.capture_state
             FROM clipboard_items AS i
             JOIN clipboard_formats AS f ON f.item_id = i.id
             WHERE i.hash_version = ?1
               AND i.capture_state = 'ready'
               AND f.format_namespace = 'windows'
               AND f.format_key = 'CF_UNICODETEXT'
               AND f.data_hash = ?2
               AND f.capture_state IN ('captured', 'generated')
             ORDER BY i.id DESC",
        )?;
        statement
            .query_map(params![hashed.hash_version, unicode_hash], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };

    for (item_id, state) in candidates {
        let formats = {
            let mut statement = transaction.prepare(
                "SELECT f.format_namespace, f.format_key, f.format_name,
                        f.capture_state, f.storage_kind, f.inline_data,
                        b.sha256, b.byte_length, b.relative_path
                 FROM clipboard_formats AS f
                 LEFT JOIN binary_blobs AS b ON b.id = f.blob_id
                 WHERE f.item_id = ?1
                 ORDER BY f.format_order",
            )?;
            statement
                .query_map([item_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<Vec<u8>>>(5)?,
                        row.get::<_, Option<Vec<u8>>>(6)?,
                        row.get::<_, Option<i64>>(7)?,
                        row.get::<_, Option<String>>(8)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut hash_formats = Vec::new();
        let mut can_hash = true;
        for (
            namespace,
            key,
            name,
            capture_state,
            storage_kind,
            inline_data,
            blob_hash,
            blob_length,
            blob_path,
        ) in formats
        {
            if !matches!(capture_state.as_str(), "captured" | "generated")
                || classify_format_role(FormatKind::Unknown, &key, &name).is_internal()
            {
                continue;
            }
            let bytes = match storage_kind.as_str() {
                "inline" => inline_data,
                "external" => {
                    let (Some(hash), Some(length), Some(path)) =
                        (blob_hash, blob_length, blob_path)
                    else {
                        continue;
                    };
                    let Ok(hash) = <[u8; 32]>::try_from(hash) else {
                        continue;
                    };
                    let Ok(length) = u64::try_from(length) else {
                        continue;
                    };
                    blob_store
                        .read(&RawBlob::from_record(hash, length, path))
                        .ok()
                }
                _ => None,
            };
            let Some(bytes) = bytes else {
                can_hash = false;
                break;
            };
            let bytes = normalize_text_bytes(&namespace, &key, &bytes).to_vec();
            hash_formats.push((namespace, key, bytes));
        }

        if can_hash
            && !hash_formats.is_empty()
            && hash_format_set(hash_formats.iter().map(|(namespace, key, bytes)| {
                (namespace.as_str(), key.as_str(), bytes.as_slice())
            })) == hashed.content_hash
        {
            return Ok(Some((item_id, state)));
        }
    }

    Ok(None)
}

fn validate_formats(captured: &CapturedClipboard) -> Result<(), CaptureError> {
    let mut orders = HashSet::new();
    let mut identities = HashSet::new();
    for format in &captured.formats {
        if format.format_namespace.trim().is_empty()
            || format.format_key.trim().is_empty()
            || format.format_name.trim().is_empty()
        {
            return Err(CaptureError::InvalidFormat(
                "格式命名空间、稳定键和名称不能为空".to_owned(),
            ));
        }
        if !orders.insert(format.format_order) {
            return Err(CaptureError::InvalidFormat(format!(
                "格式顺序 {} 重复",
                format.format_order
            )));
        }
        if !identities.insert(format.stable_identity()) {
            return Err(CaptureError::InvalidFormat(format!(
                "格式身份“{}/{}”重复",
                format.format_namespace, format.format_key
            )));
        }
        match (format.capture_state.has_data(), format.bytes.is_some()) {
            (true, false) => {
                return Err(CaptureError::InvalidFormat(format!(
                    "格式“{}”标记为成功但没有原始字节",
                    format.format_name
                )));
            }
            (false, true) => {
                return Err(CaptureError::InvalidFormat(format!(
                    "格式“{}”读取失败却携带了原始字节",
                    format.format_name
                )));
            }
            _ => {}
        }
    }
    Ok(())
}

fn select_primary_format(captured: &CapturedClipboard) -> Result<usize, CaptureError> {
    let restorable = |format: &&crate::domain::capture::RawFormat| {
        format.capture_state.has_data()
            && format.bytes.is_some()
            && format.restore_state == RestoreState::Restorable
            && !classify_format_role(format.format_kind, &format.format_key, &format.format_name)
                .is_internal()
    };
    if let Some((primary_namespace, primary_key)) = captured.primary_format_identity.as_ref()
        && let Some((index, _)) = captured
            .formats
            .iter()
            .enumerate()
            .filter(|(_, format)| restorable(format))
            .find(|(_, format)| {
                format.format_namespace == *primary_namespace && format.format_key == *primary_key
            })
    {
        return Ok(index);
    }
    captured
        .formats
        .iter()
        .enumerate()
        .filter(|(_, format)| restorable(format))
        .min_by_key(|(_, format)| format.format_order)
        .map(|(index, _)| index)
        .ok_or(CaptureError::NoRestorableFormats)
}

fn upsert_binary_blob(
    transaction: &Transaction<'_>,
    blob: &RawBlob,
    timestamp: i64,
) -> Result<i64, CaptureError> {
    let relative_path = blob.relative_path().to_string_lossy().replace('\\', "/");
    transaction.execute(
        "INSERT INTO binary_blobs (
            uuid, sha256, byte_length, relative_path, storage_state, created_at
         ) VALUES (?1, ?2, ?3, ?4, 'pending', ?5)
         ON CONFLICT(sha256) DO NOTHING",
        params![
            Uuid::new_v4().to_string(),
            blob.sha256().as_slice(),
            blob.byte_length() as i64,
            relative_path,
            timestamp,
        ],
    )?;
    let (blob_id, stored_length): (i64, i64) = transaction.query_row(
        "SELECT id, byte_length FROM binary_blobs WHERE sha256 = ?1",
        [blob.sha256().as_slice()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if stored_length != blob.byte_length() as i64 {
        return Err(CaptureError::InvalidFormat(
            "相同原始字节哈希对应的文件长度不一致".to_owned(),
        ));
    }
    transaction.execute(
        "UPDATE binary_blobs
         SET relative_path = ?1, storage_state = 'ready', last_error = NULL,
             last_verified_at = ?2
         WHERE id = ?3",
        params![relative_path, timestamp, blob_id],
    )?;
    Ok(blob_id)
}

fn upsert_history(
    transaction: &Transaction<'_>,
    item_id: i64,
    captured: &CapturedClipboard,
    timestamp: i64,
    source_app_icon_id: Option<i64>,
) -> Result<(HistoryItemId, bool), CaptureError> {
    let existing: Option<(i64, bool)> = transaction
        .query_row(
            "SELECT id, is_pinned FROM clipboard_history WHERE item_id = ?1",
            [item_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let next_sort_order: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(sort_order), -1) + ?1
         FROM clipboard_history WHERE is_pinned = ?2",
        rusqlite::params![
            LIST_ORDER_GAP,
            existing.map(|(_, is_pinned)| is_pinned).unwrap_or(false)
        ],
        |row| row.get(0),
    )?;
    if let Some((history_id, _)) = existing {
        transaction.execute(
            "UPDATE clipboard_history
             SET last_seen_at = MAX(last_seen_at, ?1),
                 capture_count = capture_count + 1,
                 sort_order = ?2,
                 last_source_app_name = COALESCE(?3, last_source_app_name),
                 last_source_app_path = COALESCE(?4, last_source_app_path),
                 last_source_app_icon_id = CASE WHEN ?5 = 1 THEN ?6 ELSE last_source_app_icon_id END,
                 updated_at = ?1
             WHERE id = ?7",
            params![
                timestamp,
                next_sort_order,
                captured.source_application.name,
                captured.source_application.path,
                i64::from(
                    captured.source_application.name.is_some()
                        || captured.source_application.path.is_some(),
                ),
                source_app_icon_id,
                history_id,
            ],
        )?;
        return Ok((history_id, false));
    }

    transaction.execute(
        "INSERT INTO clipboard_history (
            uuid, item_id, first_seen_at, last_seen_at, capture_count,
            paste_count, is_pinned, sort_order, last_source_app_name,
            last_source_app_path, last_source_app_icon_id, updated_at
         ) VALUES (?1, ?2, ?3, ?3, 1, 0, 0, ?4, ?5, ?6, ?7, ?3)",
        params![
            Uuid::new_v4().to_string(),
            item_id,
            timestamp,
            next_sort_order,
            captured.source_application.name,
            captured.source_application.path,
            source_app_icon_id,
        ],
    )?;
    Ok((transaction.last_insert_rowid(), true))
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
    use crate::services::capture::mock::MockClipboardSource;

    struct TestServices {
        _directory: tempfile::TempDir,
        database: Database,
        blob_store: RawBlobStore,
        service: CaptureService,
    }

    impl TestServices {
        fn new(inline_threshold: usize) -> Self {
            let directory = tempfile::tempdir().expect("无法创建测试临时目录");
            let database =
                Database::open(directory.path().join("test.sqlite3")).expect("无法创建测试数据库");
            let blob_store = RawBlobStore::new(directory.path()).expect("无法创建测试文件存储");
            let service = CaptureService::with_inline_threshold(
                database.clone(),
                blob_store.clone(),
                inline_threshold,
            );
            Self {
                _directory: directory,
                database,
                blob_store,
                service,
            }
        }

        fn connect(&self) -> crate::services::database::DatabaseConnection {
            self.database.connect().expect("无法连接测试数据库")
        }
    }

    #[test]
    fn mock_capture_persists_external_files_and_database_records() {
        let services = TestServices::new(4);
        let captured = MockClipboardSource::text("hello").capture();
        let history_id = services
            .service
            .store_captured_clipboard(&captured)
            .expect("无法保存模拟捕获");
        let connection = services.connect();
        let item: (String, i64, i64, String) = connection
            .query_row(
                "SELECT i.capture_state, i.format_count, i.total_bytes, b.storage_state
             FROM clipboard_history h
             JOIN clipboard_items i ON i.id = h.item_id
             JOIN clipboard_formats f ON f.item_id = i.id
             JOIN binary_blobs b ON b.id = f.blob_id
             WHERE h.id = ?1",
                [history_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("无法查询持久化结果");

        assert_eq!(item, ("ready".to_owned(), 1, 5, "ready".to_owned()));
        let blob_record: (Vec<u8>, i64, String) = connection
            .query_row(
                "SELECT sha256, byte_length, relative_path FROM binary_blobs LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("无法查询文件索引");
        let blob = RawBlob::from_record(
            blob_record.0.try_into().expect("文件哈希长度错误"),
            blob_record.1 as u64,
            blob_record.2,
        );
        assert_eq!(
            services.blob_store.read(&blob).expect("无法读取外部文件"),
            b"hello"
        );
    }

    #[test]
    fn duplicate_mock_capture_reuses_content_and_history() {
        let services = TestServices::new(64);
        let mut first = MockClipboardSource::text("hello").capture();
        first.source_application.name = Some("应用 A".to_owned());
        let mut second = MockClipboardSource::text("hello").capture();
        second.source_application.name = Some("应用 B".to_owned());

        let first_id = services
            .service
            .store_captured_clipboard(&first)
            .expect("无法保存首次捕获");
        let second_id = services
            .service
            .store_captured_clipboard(&second)
            .expect("无法保存重复捕获");
        let connection = services.connect();
        let counts: (i64, i64, i64) = connection
            .query_row(
                "SELECT
                (SELECT COUNT(*) FROM clipboard_items),
                (SELECT COUNT(*) FROM clipboard_history),
                (SELECT capture_count FROM clipboard_history LIMIT 1)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("无法查询去重结果");

        assert_eq!(first_id, second_id);
        assert_eq!(counts, (1, 1, 2));
    }

    #[test]
    fn failed_formats_are_recorded_but_excluded_from_raw_byte_total() {
        let services = TestServices::new(64);
        let captured = MockClipboardSource::text_with_failed_image("hello").capture();
        services
            .service
            .store_captured_clipboard(&captured)
            .expect("无法保存带失败格式的捕获");
        let connection = services.connect();
        let values: (i64, i64, String, String) = connection
            .query_row(
                "SELECT i.format_count, i.total_bytes, f.storage_kind, f.capture_state
             FROM clipboard_items i
             JOIN clipboard_formats f ON f.item_id = i.id
             WHERE f.capture_state = 'failed'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("无法查询失败格式");

        assert_eq!(values, (2, 5, "none".to_owned(), "failed".to_owned()));
    }
}
