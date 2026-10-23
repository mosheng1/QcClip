use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use thiserror::Error;
use uuid::Uuid;

use crate::domain::{
    capture::{
        CUSTOM_TEXT_FORMAT_KEY, CUSTOM_TEXT_FORMAT_NAME, CUSTOM_TEXT_FORMAT_NAMESPACE,
        CapturedClipboard, FormatCaptureState, FormatKind, RawFormat, RestoreState,
        SourceApplication,
    },
    clipboard::{ClipboardItemId, HistoryItemId},
};

use super::{
    application_icon::{ApplicationIconStore, upsert_application_icon},
    capture::{generate_preview, hash_format_set, select_default_preview},
    database::{Database, DatabaseError, LIST_ORDER_GAP},
    storage::{RawBlob, RawBlobError, RawBlobStore},
};

const DEFAULT_INLINE_THRESHOLD: usize = 64 * 1024;

#[derive(Clone, Debug)]
pub struct CustomTextService {
    database: Database,
    blob_store: RawBlobStore,
    application_icon_store: ApplicationIconStore,
    inline_threshold: usize,
}

#[derive(Debug, Error)]
pub enum CustomTextError {
    #[error("无法连接数据库：{0}")]
    Database(#[from] DatabaseError),
    #[error("自定义文本事务失败：{0}")]
    Sql(#[from] rusqlite::Error),
    #[error("自定义文本原始数据存储失败：{0}")]
    Storage(#[from] RawBlobError),
    #[error("自定义文本预览失败：{0}")]
    Preview(#[from] super::capture::CaptureError),
    #[error("内容不存在或尚未完成：{0}")]
    InvalidItem(String),
    #[error("数据库中的自定义文本格式无效：{0}")]
    InvalidFormat(String),
}

#[derive(Clone, Debug)]
struct StoredFormat {
    format_order: u32,
    format_namespace: String,
    format_key: String,
    format_name: String,
    native_format_id: Option<u32>,
    format_kind: FormatKind,
    bytes: Option<Vec<u8>>,
    capture_state: FormatCaptureState,
    restore_state: RestoreState,
    restore_error_code: Option<String>,
    restore_error_message: Option<String>,
    mime_type: Option<String>,
    encoding: Option<String>,
    error_code: Option<String>,
    error_message: Option<String>,
}

impl StoredFormat {
    fn into_raw_format(self) -> Option<RawFormat> {
        if !self.capture_state.has_data() {
            return None;
        }
        Some(RawFormat {
            format_namespace: self.format_namespace,
            format_key: self.format_key,
            format_name: self.format_name,
            native_format_id: self.native_format_id,
            format_kind: self.format_kind,
            bytes: self.bytes,
            format_order: self.format_order,
            capture_state: self.capture_state,
            restore_state: self.restore_state,
            restore_error_code: self.restore_error_code,
            restore_error_message: self.restore_error_message,
            error_code: self.error_code,
            error_message: self.error_message,
            mime_type: self.mime_type,
            encoding: self.encoding,
        })
    }
}

impl CustomTextService {
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

    pub fn save_for_item(&self, item_id: i64, text: &str) -> Result<(), CustomTextError> {
        self.save_for_item_internal(item_id, text, None)
    }

    pub fn save_for_item_with_tags(
        &self,
        item_id: i64,
        text: &str,
        tag_ids: &[i64],
    ) -> Result<(), CustomTextError> {
        self.save_for_item_internal(item_id, text, Some(tag_ids))
    }

    fn save_for_item_internal(
        &self,
        item_id: i64,
        text: &str,
        tag_ids: Option<&[i64]>,
    ) -> Result<(), CustomTextError> {
        let bytes = encode_text(text);
        let mut connection = self.database.connect_rewrite()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = load_formats(&transaction, &self.blob_store, item_id)?;
        if existing.is_empty() {
            return Err(CustomTextError::InvalidItem(format!(
                "内容 ID {item_id} 不存在"
            )));
        }
        let state: String = transaction.query_row(
            "SELECT capture_state FROM clipboard_items WHERE id = ?1",
            [item_id],
            |row| row.get(0),
        )?;
        if state != "ready" {
            return Err(CustomTextError::InvalidItem(format!(
                "内容 ID {item_id} 当前状态为 {state}"
            )));
        }

        let custom_order = existing
            .iter()
            .find(|format| {
                format.format_namespace == CUSTOM_TEXT_FORMAT_NAMESPACE
                    && format.format_key == CUSTOM_TEXT_FORMAT_KEY
            })
            .map(|format| format.format_order)
            .unwrap_or_else(|| {
                existing
                    .iter()
                    .map(|format| format.format_order)
                    .max()
                    .unwrap_or(0)
                    .saturating_add(1)
            });
        let mut formats = existing
            .into_iter()
            .filter(|format| {
                !(format.format_namespace == CUSTOM_TEXT_FORMAT_NAMESPACE
                    && format.format_key == CUSTOM_TEXT_FORMAT_KEY)
            })
            .collect::<Vec<_>>();
        formats.push(StoredFormat {
            format_order: custom_order,
            format_namespace: CUSTOM_TEXT_FORMAT_NAMESPACE.to_owned(),
            format_key: CUSTOM_TEXT_FORMAT_KEY.to_owned(),
            format_name: CUSTOM_TEXT_FORMAT_NAME.to_owned(),
            native_format_id: None,
            format_kind: FormatKind::Text,
            bytes: Some(bytes.clone()),
            capture_state: FormatCaptureState::Generated,
            restore_state: RestoreState::Restorable,
            restore_error_code: None,
            restore_error_message: None,
            mime_type: None,
            encoding: Some("utf-16le".to_owned()),
            error_code: None,
            error_message: None,
        });
        let captured = CapturedClipboard {
            formats: formats
                .iter()
                .cloned()
                .filter_map(StoredFormat::into_raw_format)
                .collect(),
            primary_format_identity: Some((
                CUSTOM_TEXT_FORMAT_NAMESPACE.to_owned(),
                CUSTOM_TEXT_FORMAT_KEY.to_owned(),
            )),
            preview_candidate: None,
            source_application: Default::default(),
        };
        let generated_preview = generate_preview(&captured)?;
        let custom_preview = generated_preview
            .formats
            .iter()
            .find(|preview| {
                preview.format_namespace == CUSTOM_TEXT_FORMAT_NAMESPACE
                    && preview.format_key == CUSTOM_TEXT_FORMAT_KEY
            })
            .ok_or_else(|| CustomTextError::InvalidFormat("无法生成文本预览".to_owned()))?;
        // 编辑后的自定义文本即使为空，也必须继续作为默认展示内容；不能让默认选择器
        // 因“空内容”过滤掉它并回退到原始 HTML 或纯文本。
        let default_preview = custom_preview;
        let content_hash = hash_format_set(formats.iter().filter_map(|format| {
            format.bytes.as_deref().map(|bytes| {
                (
                    format.format_namespace.as_str(),
                    format.format_key.as_str(),
                    bytes,
                )
            })
        }));
        let total_bytes = formats
            .iter()
            .filter_map(|format| format.bytes.as_ref())
            .map(|bytes| i64::try_from(bytes.len()).unwrap_or(i64::MAX))
            .sum::<i64>();
        let timestamp = current_timestamp_millis();
        let custom_blob = if bytes.len() > self.inline_threshold {
            let pending = self.blob_store.write_pending(&bytes)?;
            Some(self.blob_store.commit_pending(&pending)?)
        } else {
            None
        };

        transaction.execute(
            "UPDATE clipboard_formats
             SET is_primary = 0
             WHERE item_id = ?1",
            [item_id],
        )?;
        let existing_custom_id: Option<i64> = transaction
            .query_row(
                "SELECT id FROM clipboard_formats
                 WHERE item_id = ?1 AND format_namespace = ?2 AND format_key = ?3",
                params![
                    item_id,
                    CUSTOM_TEXT_FORMAT_NAMESPACE,
                    CUSTOM_TEXT_FORMAT_KEY
                ],
                |row| row.get(0),
            )
            .optional()?;
        let (storage_kind, inline_data, blob_id, byte_length) = if let Some(blob) = custom_blob {
            let blob_id = upsert_binary_blob(&transaction, &blob, timestamp)?;
            ("external", None, Some(blob_id), bytes.len() as i64)
        } else {
            ("inline", Some(bytes.as_slice()), None, bytes.len() as i64)
        };
        if let Some(format_id) = existing_custom_id {
            transaction.execute(
                "UPDATE clipboard_formats
                 SET format_order = ?1, format_name = ?2, native_format_id = NULL,
                     format_kind = 'text', is_primary = 1, storage_kind = ?3,
                     inline_data = ?4, blob_id = ?5, byte_length = ?6, data_hash = ?7,
                     restore_state = 'restorable', restore_error_code = NULL,
                     restore_error_message = NULL, mime_type = NULL, encoding = 'utf-16le',
                     preview_text = ?8, preview_char_count = ?9,
                     preview_content_kind = ?10, capture_state = 'generated',
                     error_code = NULL, error_message = NULL,
                     image_width = NULL, image_height = NULL
                 WHERE id = ?11",
                params![
                    i64::from(custom_order),
                    CUSTOM_TEXT_FORMAT_NAME,
                    storage_kind,
                    inline_data,
                    blob_id,
                    byte_length,
                    sha256(&bytes).as_slice(),
                    custom_preview.text,
                    custom_preview.char_count as i64,
                    custom_preview.content_kind.as_str(),
                    format_id,
                ],
            )?;
        } else {
            transaction.execute(
                "INSERT INTO clipboard_formats (
                    uuid, item_id, format_order, format_namespace, format_key,
                    format_name, format_kind, is_primary, storage_kind, inline_data,
                    blob_id, byte_length, data_hash, restore_state, encoding,
                    capture_state, preview_text, preview_char_count,
                    preview_content_kind, created_at
                 ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, 'text', 1, ?7, ?8, ?9, ?10, ?11,
                    'restorable', 'utf-16le', 'generated', ?12, ?13, ?14, ?15
                 )",
                params![
                    Uuid::new_v4().to_string(),
                    item_id,
                    i64::from(custom_order),
                    CUSTOM_TEXT_FORMAT_NAMESPACE,
                    CUSTOM_TEXT_FORMAT_KEY,
                    CUSTOM_TEXT_FORMAT_NAME,
                    storage_kind,
                    inline_data,
                    blob_id,
                    byte_length,
                    sha256(&bytes).as_slice(),
                    custom_preview.text,
                    custom_preview.char_count as i64,
                    custom_preview.content_kind.as_str(),
                    timestamp,
                ],
            )?;
        }
        transaction.execute(
            "UPDATE clipboard_items
             SET content_hash = ?1, content_kind = ?2, preview_text = ?3,
                 preview_char_count = ?4, format_count = ?5, total_bytes = ?6,
                 updated_at = ?7, capture_state = 'ready'
             WHERE id = ?8",
            params![
                content_hash.as_slice(),
                generated_preview.content_kind.as_str(),
                default_preview.text,
                default_preview.char_count as i64,
                formats.len() as i64,
                total_bytes,
                timestamp,
                item_id,
            ],
        )?;
        if let Some(tag_ids) = tag_ids {
            replace_item_tags(&transaction, item_id, tag_ids)?;
        }
        cleanup_unreferenced_blobs(&transaction, &self.blob_store)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn can_restore_original(&self, item_id: i64) -> Result<bool, CustomTextError> {
        let connection = self.database.connect()?;
        let can_restore = connection.query_row(
            "SELECT EXISTS(
                 SELECT 1
                 FROM clipboard_formats AS custom
                 WHERE custom.item_id = ?1
                   AND custom.format_namespace = ?2
                   AND custom.format_key = ?3
             ) AND EXISTS(
                 SELECT 1
                 FROM clipboard_formats AS original
                 WHERE original.item_id = ?1
                   AND NOT (
                       original.format_namespace = ?2
                       AND original.format_key = ?3
                   )
                   AND original.capture_state IN ('captured', 'generated')
                   AND original.restore_state = 'restorable'
             )",
            params![
                item_id,
                CUSTOM_TEXT_FORMAT_NAMESPACE,
                CUSTOM_TEXT_FORMAT_KEY,
            ],
            |row| row.get(0),
        )?;
        Ok(can_restore)
    }

    pub fn remove_custom_text(&self, item_id: i64) -> Result<(), CustomTextError> {
        let mut connection = self.database.connect_rewrite()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = load_formats(&transaction, &self.blob_store, item_id)?;
        if existing.is_empty() {
            return Err(CustomTextError::InvalidItem(format!(
                "内容 ID {item_id} 不存在"
            )));
        }
        let has_custom_text = existing.iter().any(|format| {
            format.format_namespace == CUSTOM_TEXT_FORMAT_NAMESPACE
                && format.format_key == CUSTOM_TEXT_FORMAT_KEY
        });
        if !has_custom_text {
            return Err(CustomTextError::InvalidItem(
                "当前内容没有自定义文本格式".to_owned(),
            ));
        }

        let formats = existing
            .into_iter()
            .filter(|format| {
                !(format.format_namespace == CUSTOM_TEXT_FORMAT_NAMESPACE
                    && format.format_key == CUSTOM_TEXT_FORMAT_KEY)
            })
            .collect::<Vec<_>>();
        if formats.is_empty() {
            return Err(CustomTextError::InvalidItem(
                "当前内容没有可恢复的原始格式".to_owned(),
            ));
        }

        let captured = CapturedClipboard {
            formats: formats
                .iter()
                .cloned()
                .filter_map(StoredFormat::into_raw_format)
                .collect(),
            primary_format_identity: None,
            preview_candidate: None,
            source_application: Default::default(),
        };
        let generated_preview = generate_preview(&captured)?;
        let default_preview = select_default_preview(&captured, &generated_preview.formats)
            .or_else(|| generated_preview.formats.first())
            .ok_or_else(|| CustomTextError::InvalidFormat("无法选择原始格式预览".to_owned()))?;
        let content_hash = hash_format_set(formats.iter().filter_map(|format| {
            format.bytes.as_deref().map(|bytes| {
                (
                    format.format_namespace.as_str(),
                    format.format_key.as_str(),
                    bytes,
                )
            })
        }));
        let total_bytes = formats
            .iter()
            .filter_map(|format| format.bytes.as_ref())
            .map(|bytes| i64::try_from(bytes.len()).unwrap_or(i64::MAX))
            .sum::<i64>();
        let restored_identity = (
            default_preview.format_namespace.as_str(),
            default_preview.format_key.as_str(),
        );
        let timestamp = current_timestamp_millis();

        transaction.execute(
            "UPDATE clipboard_formats SET is_primary = 0 WHERE item_id = ?1",
            [item_id],
        )?;
        transaction.execute(
            "DELETE FROM clipboard_formats
             WHERE item_id = ?1 AND format_namespace = ?2 AND format_key = ?3",
            params![
                item_id,
                CUSTOM_TEXT_FORMAT_NAMESPACE,
                CUSTOM_TEXT_FORMAT_KEY,
            ],
        )?;
        let restored = transaction.execute(
            "UPDATE clipboard_formats
             SET is_primary = 1
             WHERE item_id = ?1 AND format_namespace = ?2 AND format_key = ?3
               AND capture_state IN ('captured', 'generated')
               AND restore_state = 'restorable'",
            params![item_id, restored_identity.0, restored_identity.1],
        )?;
        if restored != 1 {
            return Err(CustomTextError::InvalidFormat(
                "无法恢复原始格式主格式".to_owned(),
            ));
        }
        transaction.execute(
            "UPDATE clipboard_items
             SET content_hash = ?1, content_kind = ?2, preview_text = ?3,
                 preview_char_count = ?4, format_count = ?5, total_bytes = ?6,
                 updated_at = ?7, capture_state = 'ready'
             WHERE id = ?8",
            params![
                content_hash.as_slice(),
                generated_preview.content_kind.as_str(),
                default_preview.text,
                default_preview.char_count as i64,
                formats.len() as i64,
                total_bytes,
                timestamp,
                item_id,
            ],
        )?;
        cleanup_unreferenced_blobs(&transaction, &self.blob_store)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn create_manual_text(&self, text: &str) -> Result<HistoryItemId, CustomTextError> {
        self.create_manual_text_with_tags(text, &[])
    }

    pub fn create_manual_text_with_tags(
        &self,
        text: &str,
        tag_ids: &[i64],
    ) -> Result<HistoryItemId, CustomTextError> {
        self.create_manual_text_with_favorite_and_tags(text, false, tag_ids)
    }

    pub fn create_manual_text_and_favorite(
        &self,
        text: &str,
    ) -> Result<HistoryItemId, CustomTextError> {
        self.create_manual_text_and_favorite_with_tags(text, &[])
    }

    pub fn create_manual_text_and_favorite_with_tags(
        &self,
        text: &str,
        tag_ids: &[i64],
    ) -> Result<HistoryItemId, CustomTextError> {
        self.create_manual_text_with_favorite_and_tags(text, true, tag_ids)
    }

    /// 查找与 `text` 字节完全一致的手动文本条目，用于快捷收藏的幂等复用。
    pub fn find_manual_text_item(
        &self,
        text: &str,
    ) -> Result<Option<ClipboardItemId>, CustomTextError> {
        let bytes = encode_text(text);
        let content_hash = hash_format_set([(
            CUSTOM_TEXT_FORMAT_NAMESPACE,
            CUSTOM_TEXT_FORMAT_KEY,
            bytes.as_slice(),
        )]);
        let connection = self.database.connect_read()?;
        connection
            .query_row(
                "SELECT id
                 FROM clipboard_items
                 WHERE origin_kind = 'manual'
                   AND hash_version = ?1
                   AND content_hash = ?2
                 LIMIT 1",
                params![
                    super::capture::CONTENT_HASH_VERSION,
                    content_hash.as_slice()
                ],
                |row| row.get(0),
            )
            .optional()
            .map_err(CustomTextError::from)
    }

    fn create_manual_text_with_favorite_and_tags(
        &self,
        text: &str,
        add_to_favorites: bool,
        tag_ids: &[i64],
    ) -> Result<HistoryItemId, CustomTextError> {
        let bytes = encode_text(text);
        let source_application = current_source_application();
        let preview = generate_preview(&CapturedClipboard {
            formats: vec![custom_text_format(bytes.clone(), 0)],
            primary_format_identity: Some((
                CUSTOM_TEXT_FORMAT_NAMESPACE.to_owned(),
                CUSTOM_TEXT_FORMAT_KEY.to_owned(),
            )),
            preview_candidate: None,
            source_application: Default::default(),
        })?;
        let preview = preview
            .formats
            .into_iter()
            .next()
            .ok_or_else(|| CustomTextError::InvalidFormat("无法生成文本预览".to_owned()))?;
        let content_hash = hash_format_set([(
            CUSTOM_TEXT_FORMAT_NAMESPACE,
            CUSTOM_TEXT_FORMAT_KEY,
            bytes.as_slice(),
        )]);
        let timestamp = current_timestamp_millis();
        let mut connection = self.database.connect_write()?;
        let blob = if bytes.len() > self.inline_threshold {
            let pending = self.blob_store.write_pending(&bytes)?;
            Some(self.blob_store.commit_pending(&pending)?)
        } else {
            None
        };
        let prepared_icon = self
            .application_icon_store
            .resolve(&connection, &source_application);
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let source_app_icon_id =
            upsert_application_icon(&transaction, prepared_icon.as_ref(), timestamp)?;
        transaction.execute(
            "INSERT INTO clipboard_items (
                uuid, origin_kind, hash_version, content_hash, content_kind,
                capture_state, preview_text, preview_char_count, format_count,
                total_bytes, created_at, updated_at
             ) VALUES (?1, 'manual', ?2, ?3, ?4, 'pending', ?5, ?6, 0, 0, ?7, ?7)",
            params![
                Uuid::new_v4().to_string(),
                super::capture::CONTENT_HASH_VERSION,
                content_hash.as_slice(),
                preview.content_kind.as_str(),
                preview.text,
                preview.char_count as i64,
                timestamp,
            ],
        )?;
        let item_id = transaction.last_insert_rowid();
        let blob_id = blob
            .as_ref()
            .map(|blob| upsert_binary_blob(&transaction, blob, timestamp))
            .transpose()?;
        insert_custom_format(&transaction, item_id, &bytes, &preview, timestamp, blob_id)?;
        transaction.execute(
            "UPDATE clipboard_items
             SET capture_state = 'ready', format_count = 1, total_bytes = ?1
             WHERE id = ?2",
            params![bytes.len() as i64, item_id],
        )?;
        let sort_order: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(sort_order), -1) + ?1
             FROM clipboard_history WHERE is_pinned = 0",
            [LIST_ORDER_GAP],
            |row| row.get(0),
        )?;
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
                sort_order,
                source_application.name,
                source_application.path,
                source_app_icon_id,
            ],
        )?;
        let history_id = transaction.last_insert_rowid();
        if add_to_favorites {
            let favorite_sort_order: i64 = transaction.query_row(
                "SELECT COALESCE(MAX(sort_order), -1) + ?1
                 FROM favorites WHERE is_pinned = 0",
                [LIST_ORDER_GAP],
                |row| row.get(0),
            )?;
            transaction.execute(
                "INSERT INTO favorites (uuid, item_id, is_pinned, sort_order, added_at, updated_at)
                 VALUES (?1, ?2, 0, ?3, ?4, ?4)",
                params![
                    Uuid::new_v4().to_string(),
                    item_id,
                    favorite_sort_order,
                    timestamp,
                ],
            )?;
        }
        replace_item_tags(&transaction, item_id, tag_ids)?;
        cleanup_unreferenced_blobs(&transaction, &self.blob_store)?;
        transaction.commit()?;
        Ok(history_id)
    }
}

fn replace_item_tags(
    transaction: &rusqlite::Transaction<'_>,
    item_id: i64,
    tag_ids: &[i64],
) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "DELETE FROM clipboard_item_tags WHERE item_id = ?1",
        [item_id],
    )?;
    let timestamp = current_timestamp_millis();
    let mut unique_tag_ids = tag_ids.to_vec();
    unique_tag_ids.sort_unstable();
    unique_tag_ids.dedup();
    for tag_id in unique_tag_ids {
        transaction.execute(
            "INSERT INTO clipboard_item_tags (
                uuid, item_id, tag_id, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?4)",
            params![Uuid::new_v4().to_string(), item_id, tag_id, timestamp],
        )?;
    }
    Ok(())
}

fn custom_text_format(bytes: Vec<u8>, format_order: u32) -> RawFormat {
    RawFormat {
        format_namespace: CUSTOM_TEXT_FORMAT_NAMESPACE.to_owned(),
        format_key: CUSTOM_TEXT_FORMAT_KEY.to_owned(),
        format_name: CUSTOM_TEXT_FORMAT_NAME.to_owned(),
        native_format_id: None,
        format_kind: FormatKind::Text,
        bytes: Some(bytes),
        format_order,
        capture_state: FormatCaptureState::Generated,
        restore_state: RestoreState::Restorable,
        restore_error_code: None,
        restore_error_message: None,
        error_code: None,
        error_message: None,
        mime_type: None,
        encoding: Some("utf-16le".to_owned()),
    }
}

fn encode_text(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

fn current_source_application() -> SourceApplication {
    let path = std::env::current_exe().ok();
    let name = path
        .as_deref()
        .and_then(|path| path.file_stem())
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .or_else(|| Some(env!("CARGO_PKG_NAME").to_owned()));
    SourceApplication {
        name,
        path: path.map(|path| path.to_string_lossy().into_owned()),
    }
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).into()
}

fn current_timestamp_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn parse_format_kind(value: String) -> Result<FormatKind, CustomTextError> {
    FormatKind::from_str(&value)
        .ok_or_else(|| CustomTextError::InvalidFormat(format!("未知格式类型“{value}”")))
}

fn parse_capture_state(value: String) -> Result<FormatCaptureState, CustomTextError> {
    match value.as_str() {
        "captured" => Ok(FormatCaptureState::Captured),
        "generated" => Ok(FormatCaptureState::Generated),
        "unsupported" => Ok(FormatCaptureState::Unsupported),
        "failed" => Ok(FormatCaptureState::Failed),
        _ => Err(CustomTextError::InvalidFormat(format!(
            "未知捕获状态“{value}”"
        ))),
    }
}

fn parse_restore_state(value: String) -> Result<RestoreState, CustomTextError> {
    match value.as_str() {
        "pending" => Ok(RestoreState::Pending),
        "restorable" => Ok(RestoreState::Restorable),
        "not_restorable" => Ok(RestoreState::NotRestorable),
        "not_applicable" => Ok(RestoreState::NotApplicable),
        _ => Err(CustomTextError::InvalidFormat(format!(
            "未知恢复状态“{value}”"
        ))),
    }
}

fn load_formats(
    connection: &Connection,
    blob_store: &RawBlobStore,
    item_id: i64,
) -> Result<Vec<StoredFormat>, CustomTextError> {
    let mut statement = connection.prepare(
        "SELECT f.format_order, f.format_namespace, f.format_key, f.format_name,
                f.native_format_id, f.format_kind, f.storage_kind, f.inline_data,
                b.sha256, b.byte_length, b.relative_path, f.restore_state,
                f.capture_state, f.restore_error_code, f.restore_error_message,
                f.mime_type, f.encoding, f.error_code, f.error_message
         FROM clipboard_formats AS f
         LEFT JOIN binary_blobs AS b ON b.id = f.blob_id
         WHERE f.item_id = ?1
         ORDER BY f.format_order",
    )?;
    let rows = statement.query_map([item_id], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<i64>>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, Option<Vec<u8>>>(7)?,
            row.get::<_, Option<Vec<u8>>>(8)?,
            row.get::<_, Option<i64>>(9)?,
            row.get::<_, Option<String>>(10)?,
            row.get::<_, String>(11)?,
            row.get::<_, String>(12)?,
            row.get::<_, Option<String>>(13)?,
            row.get::<_, Option<String>>(14)?,
            row.get::<_, Option<String>>(15)?,
            row.get::<_, Option<String>>(16)?,
            row.get::<_, Option<String>>(17)?,
            row.get::<_, Option<String>>(18)?,
        ))
    })?;
    let mut formats = Vec::new();
    for row in rows {
        let (
            format_order,
            format_namespace,
            format_key,
            format_name,
            native_format_id,
            format_kind,
            storage_kind,
            inline_data,
            sha256,
            byte_length,
            relative_path,
            restore_state,
            capture_state,
            restore_error_code,
            restore_error_message,
            mime_type,
            encoding,
            error_code,
            error_message,
        ) = row?;
        let bytes = match storage_kind.as_str() {
            "inline" => inline_data,
            "external" => {
                let hash = sha256.ok_or_else(|| {
                    CustomTextError::InvalidFormat("外部格式缺少数据哈希".to_owned())
                })?;
                let hash: [u8; 32] = hash.try_into().map_err(|value: Vec<u8>| {
                    CustomTextError::InvalidFormat(format!("数据哈希长度为 {}", value.len()))
                })?;
                let byte_length = byte_length.ok_or_else(|| {
                    CustomTextError::InvalidFormat("外部格式缺少数据长度".to_owned())
                })?;
                let relative_path = relative_path.ok_or_else(|| {
                    CustomTextError::InvalidFormat("外部格式缺少文件路径".to_owned())
                })?;
                Some(blob_store.read(&RawBlob::from_record(
                    hash,
                    u64::try_from(byte_length).map_err(|_| {
                        CustomTextError::InvalidFormat("格式长度不能为负数".to_owned())
                    })?,
                    relative_path,
                ))?)
            }
            "none" => None,
            _ => return Err(CustomTextError::InvalidFormat("未知存储类型".to_owned())),
        };
        formats.push(StoredFormat {
            format_order: u32::try_from(format_order)
                .map_err(|_| CustomTextError::InvalidFormat("格式顺序不能为负数".to_owned()))?,
            format_namespace,
            format_key,
            format_name,
            native_format_id: native_format_id
                .map(|value| {
                    u32::try_from(value)
                        .map_err(|_| CustomTextError::InvalidFormat("原生格式 ID 无效".to_owned()))
                })
                .transpose()?,
            format_kind: parse_format_kind(format_kind)?,
            bytes,
            capture_state: parse_capture_state(capture_state)?,
            restore_state: parse_restore_state(restore_state)?,
            restore_error_code,
            restore_error_message,
            mime_type,
            encoding,
            error_code,
            error_message,
        });
    }
    Ok(formats)
}

fn upsert_binary_blob(
    transaction: &Transaction<'_>,
    blob: &RawBlob,
    timestamp: i64,
) -> Result<i64, CustomTextError> {
    let relative_path = blob.relative_path().to_string_lossy().replace('\\', "/");
    transaction.execute(
        "INSERT INTO binary_blobs (
            uuid, sha256, byte_length, relative_path, storage_state, created_at
         ) VALUES (?1, ?2, ?3, ?4, 'ready', ?5)
         ON CONFLICT(sha256) DO NOTHING",
        params![
            Uuid::new_v4().to_string(),
            blob.sha256().as_slice(),
            blob.byte_length() as i64,
            relative_path,
            timestamp,
        ],
    )?;
    Ok(transaction.query_row(
        "SELECT id FROM binary_blobs WHERE sha256 = ?1",
        [blob.sha256().as_slice()],
        |row| row.get(0),
    )?)
}

fn cleanup_unreferenced_blobs(
    transaction: &Transaction<'_>,
    blob_store: &RawBlobStore,
) -> Result<(), CustomTextError> {
    if !Database::maintenance_supported(transaction)? {
        return Ok(());
    }
    let mut statement = transaction.prepare(
        "SELECT b.id, b.sha256, b.byte_length, b.relative_path
         FROM binary_blobs AS b
         WHERE NOT EXISTS (
             SELECT 1 FROM clipboard_formats AS f WHERE f.blob_id = b.id
         )",
    )?;
    let blobs = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);

    for (id, hash, byte_length, relative_path) in blobs {
        let hash_length = hash.len();
        let sha256: [u8; 32] = hash.try_into().map_err(|_| {
            CustomTextError::InvalidFormat(format!("孤儿 blob 哈希长度为 {hash_length}"))
        })?;
        let byte_length = u64::try_from(byte_length)
            .map_err(|_| CustomTextError::InvalidFormat("孤儿 blob 长度不能为负数".to_owned()))?;
        let blob = RawBlob::from_record(sha256, byte_length, relative_path);
        blob_store.delete_if_unreferenced(&blob, 0)?;
        transaction.execute(
            "DELETE FROM binary_blobs
             WHERE id = ?1
               AND NOT EXISTS (
                   SELECT 1 FROM clipboard_formats WHERE blob_id = ?1
               )",
            [id],
        )?;
    }
    Ok(())
}

fn insert_custom_format(
    transaction: &Transaction<'_>,
    item_id: i64,
    bytes: &[u8],
    preview: &crate::domain::clipboard::FormatPreview,
    timestamp: i64,
    blob: Option<i64>,
) -> Result<(), CustomTextError> {
    let (storage_kind, inline_data) = if blob.is_some() {
        ("external", None)
    } else {
        ("inline", Some(bytes))
    };
    transaction.execute(
        "INSERT INTO clipboard_formats (
            uuid, item_id, format_order, format_namespace, format_key, format_name,
            format_kind, is_primary, storage_kind, inline_data, blob_id, byte_length,
            data_hash, restore_state, encoding, capture_state, preview_text,
            preview_char_count, preview_content_kind, created_at
         ) VALUES (?1, ?2, 0, ?3, ?4, ?5, 'text', 1, ?6, ?7, ?8, ?9, ?10,
                   'restorable', 'utf-16le', 'generated', ?11, ?12, ?13, ?14)",
        params![
            Uuid::new_v4().to_string(),
            item_id,
            CUSTOM_TEXT_FORMAT_NAMESPACE,
            CUSTOM_TEXT_FORMAT_KEY,
            CUSTOM_TEXT_FORMAT_NAME,
            storage_kind,
            inline_data,
            blob,
            bytes.len() as i64,
            sha256(bytes).as_slice(),
            preview.text,
            preview.char_count as i64,
            preview.content_kind.as_str(),
            timestamp,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use tempfile::TempDir;

    use super::*;
    use crate::domain::capture::{CapturedClipboard, FormatKind, RawFormat, SourceApplication};
    use crate::services::{
        capture::CaptureService,
        database::{Database, TagRepository},
    };

    struct TestContext {
        _directory: TempDir,
        database: Database,
        service: CustomTextService,
    }

    impl TestContext {
        fn new() -> Self {
            let directory = tempfile::tempdir().expect("无法创建测试目录");
            let database =
                Database::open(directory.path().join("test.sqlite3")).expect("无法创建测试数据库");
            let blob_store = RawBlobStore::new(directory.path()).expect("无法创建 blob 存储");
            let service = CustomTextService::new(database.clone(), blob_store);
            Self {
                _directory: directory,
                database,
                service,
            }
        }

        fn item_id(&self, history_id: HistoryItemId) -> i64 {
            self.database
                .connect()
                .expect("无法连接测试数据库")
                .query_row(
                    "SELECT item_id FROM clipboard_history WHERE id = ?1",
                    [history_id],
                    |row| row.get(0),
                )
                .expect("无法查询测试内容")
        }
    }

    fn unicode_text(text: &str) -> Vec<u8> {
        encode_text(text)
    }

    fn captured_text_with_html() -> CapturedClipboard {
        let text = RawFormat::captured(
            "windows",
            "CF_UNICODETEXT",
            "CF_UNICODETEXT",
            FormatKind::Text,
            unicode_text("原始文本"),
            0,
            true,
        );
        let html = RawFormat::captured(
            "windows",
            "registered:HTML Format",
            "HTML Format",
            FormatKind::Html,
            "<b>原始文本</b>".as_bytes().to_vec(),
            1,
            true,
        );
        CapturedClipboard {
            primary_format_identity: Some((text.format_namespace.clone(), text.format_key.clone())),
            formats: vec![text, html],
            preview_candidate: None,
            source_application: SourceApplication::default(),
        }
    }

    fn capture_item(context: &TestContext, captured: &CapturedClipboard) -> i64 {
        let store = RawBlobStore::new(
            context
                .database
                .path()
                .parent()
                .expect("测试数据库缺少父目录"),
        )
        .expect("无法创建捕获存储");
        let history_id = CaptureService::new(context.database.clone(), store)
            .store_captured_clipboard(captured)
            .expect("无法保存测试捕获内容");
        context.item_id(history_id)
    }

    fn query_format_count(
        connection: &Connection,
        item_id: i64,
        namespace: &str,
        key: &str,
    ) -> i64 {
        connection
            .query_row(
                "SELECT COUNT(*) FROM clipboard_formats
                 WHERE item_id = ?1 AND format_namespace = ?2 AND format_key = ?3",
                params![item_id, namespace, key],
                |row| row.get(0),
            )
            .expect("无法查询格式数量")
    }

    #[test]
    fn find_manual_text_item_matches_exact_encoded_bytes() {
        let context = TestContext::new();

        assert_eq!(
            context
                .service
                .find_manual_text_item("想法")
                .expect("无法查询手动文本"),
            None
        );

        let history_id = context
            .service
            .create_manual_text("想法")
            .expect("无法创建手动文本");
        let item_id = context.item_id(history_id);

        assert_eq!(
            context
                .service
                .find_manual_text_item("想法")
                .expect("无法查询手动文本"),
            Some(item_id)
        );
        assert_eq!(
            context
                .service
                .find_manual_text_item("想法 ")
                .expect("无法查询手动文本"),
            None
        );
    }

    #[test]
    fn editing_keeps_item_and_original_formats_but_updates_custom_text() {
        let context = TestContext::new();
        let item_id = capture_item(&context, &captured_text_with_html());

        context
            .service
            .save_for_item(item_id, "修改后的文本\n中文")
            .expect("无法保存编辑文本");
        let connection = context.database.connect().expect("无法连接测试数据库");
        let first_custom_id: i64 = connection
            .query_row(
                "SELECT id FROM clipboard_formats
                 WHERE item_id = ?1 AND format_namespace = 'qcclip' AND format_key = 'custom-text'",
                [item_id],
                |row| row.get(0),
            )
            .expect("缺少自定义文本格式");
        let original_count = query_format_count(&connection, item_id, "windows", "CF_UNICODETEXT");
        let html_count =
            query_format_count(&connection, item_id, "windows", "registered:HTML Format");
        let (primary, storage_kind, bytes, item_count, content_kind, preview): (
            i64,
            String,
            Vec<u8>,
            i64,
            String,
            String,
        ) = connection
            .query_row(
                "SELECT f.is_primary, f.storage_kind, f.inline_data,
                        i.format_count, i.content_kind, i.preview_text
                 FROM clipboard_formats AS f
                 JOIN clipboard_items AS i ON i.id = f.item_id
                 WHERE f.id = ?1",
                [first_custom_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .expect("无法查询编辑结果");

        assert_eq!(original_count, 1);
        assert_eq!(html_count, 1);
        assert_eq!(primary, 1);
        assert_eq!(storage_kind, "inline");
        assert_eq!(bytes, unicode_text("修改后的文本\n中文"));
        assert_eq!(item_count, 3);
        assert_eq!(content_kind, "mixed");
        assert_eq!(preview, "修改后的文本\n中文");

        context
            .service
            .save_for_item(item_id, "再次编辑")
            .expect("无法重复保存编辑文本");
        let (custom_count, second_custom_id): (i64, i64) = connection
            .query_row(
                "SELECT COUNT(*), MIN(id) FROM clipboard_formats
                 WHERE item_id = ?1 AND format_namespace = 'qcclip' AND format_key = 'custom-text'",
                [item_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("无法查询重复编辑结果");
        assert_eq!(custom_count, 1);
        assert_eq!(first_custom_id, second_custom_id);
    }

    #[test]
    fn restoring_original_text_removes_only_custom_format_and_rebuilds_defaults() {
        let context = TestContext::new();
        let item_id = capture_item(&context, &captured_text_with_html());
        context
            .service
            .save_for_item(item_id, "修改后的文本")
            .expect("无法保存自定义文本");
        assert!(
            context
                .service
                .can_restore_original(item_id)
                .expect("无法检查恢复状态")
        );

        context
            .service
            .remove_custom_text(item_id)
            .expect("无法恢复原始内容");
        let connection = context.database.connect().expect("无法连接测试数据库");
        let (custom_count, primary_count, item_count, preview, content_kind): (
            i64,
            i64,
            i64,
            String,
            String,
        ) = connection
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM clipboard_formats
                     WHERE item_id = ?1 AND format_namespace = 'qcclip'
                       AND format_key = 'custom-text'),
                    (SELECT COUNT(*) FROM clipboard_formats
                     WHERE item_id = ?1 AND is_primary = 1),
                    i.format_count, i.preview_text, i.content_kind
                 FROM clipboard_items AS i WHERE i.id = ?1",
                [item_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .expect("无法查询恢复结果");
        let primary_key: String = connection
            .query_row(
                "SELECT format_key FROM clipboard_formats
                 WHERE item_id = ?1 AND is_primary = 1",
                [item_id],
                |row| row.get(0),
            )
            .expect("无法查询恢复后的主格式");

        assert_eq!(custom_count, 0);
        assert_eq!(primary_count, 1);
        assert_eq!(item_count, 2);
        assert_eq!(preview, "原始文本");
        assert_eq!(content_kind, "mixed");
        assert_eq!(primary_key, "CF_UNICODETEXT");
        assert!(
            !context
                .service
                .can_restore_original(item_id)
                .expect("无法检查恢复后的状态")
        );
    }

    #[test]
    fn manual_text_cannot_restore_without_an_original_format() {
        let context = TestContext::new();
        let history_id = context
            .service
            .create_manual_text("手动文本")
            .expect("无法创建手动文本");
        let item_id = context.item_id(history_id);

        assert!(
            !context
                .service
                .can_restore_original(item_id)
                .expect("无法检查手动文本恢复状态")
        );
        assert!(context.service.remove_custom_text(item_id).is_err());
    }

    #[test]
    fn manual_text_creates_history_item_with_utf16_custom_format() {
        let context = TestContext::new();
        let history_id = context
            .service
            .create_manual_text("手动新增\n内容")
            .expect("无法创建手动文本");
        let connection = context.database.connect().expect("无法连接测试数据库");
        let item_id = context.item_id(history_id);
        let (origin, storage_kind, bytes): (String, String, Vec<u8>) = connection
            .query_row(
                "SELECT i.origin_kind, f.storage_kind, f.inline_data
                 FROM clipboard_items AS i
                 JOIN clipboard_formats AS f ON f.item_id = i.id
                 WHERE i.id = ?1",
                [item_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("无法查询手动文本");

        assert_eq!(origin, "manual");
        assert_eq!(storage_kind, "inline");
        assert_eq!(bytes, unicode_text("手动新增\n内容"));
        assert_eq!(
            query_format_count(&connection, item_id, "qcclip", "custom-text"),
            1
        );

        let (source_name, source_path): (Option<String>, Option<String>) = connection
            .query_row(
                "SELECT last_source_app_name, last_source_app_path
                 FROM clipboard_history WHERE id = ?1",
                [history_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("无法查询手动文本来源应用");
        let expected_source = current_source_application();
        assert_eq!(source_name, expected_source.name);
        assert_eq!(source_path, expected_source.path);
    }

    #[test]
    fn manual_text_can_create_a_favorite_in_the_same_transaction() {
        let context = TestContext::new();
        let tag = TagRepository::new(context.database.clone())
            .create("收藏标签", "#1677FF")
            .expect("无法创建测试标签");
        let history_id = context
            .service
            .create_manual_text_and_favorite_with_tags("新增收藏", &[tag.id])
            .expect("无法创建收藏文本");
        let item_id = context.item_id(history_id);
        let connection = context.database.connect().expect("无法连接测试数据库");
        let (favorite_count, tag_count): (i64, i64) = connection
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM favorites WHERE item_id = ?1),
                    (SELECT COUNT(*) FROM clipboard_item_tags
                     WHERE item_id = ?1 AND tag_id = ?2)",
                params![item_id, tag.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("无法查询新增收藏标签");

        assert_eq!(favorite_count, 1);
        assert_eq!(tag_count, 1);
    }

    #[test]
    fn saving_custom_text_replaces_item_tags_and_deduplicates_ids() {
        let context = TestContext::new();
        let tag_repository = TagRepository::new(context.database.clone());
        let first_tag = tag_repository
            .create("第一个标签", "#1677FF")
            .expect("无法创建第一个测试标签");
        let second_tag = tag_repository
            .create("第二个标签", "#00AA66")
            .expect("无法创建第二个测试标签");
        let item_id = capture_item(&context, &captured_text_with_html());

        context
            .service
            .save_for_item_with_tags(
                item_id,
                "带标签的文本",
                &[first_tag.id, first_tag.id, second_tag.id],
            )
            .expect("无法保存带标签的文本");

        let connection = context.database.connect().expect("无法连接测试数据库");
        let tag_ids = connection
            .prepare(
                "SELECT tag_id FROM clipboard_item_tags
                 WHERE item_id = ?1 ORDER BY tag_id",
            )
            .expect("无法准备标签查询")
            .query_map([item_id], |row| row.get::<_, i64>(0))
            .expect("无法查询内容标签")
            .collect::<Result<Vec<_>, _>>()
            .expect("无法读取内容标签");

        assert_eq!(tag_ids, vec![first_tag.id, second_tag.id]);
    }

    #[test]
    fn replacing_large_custom_text_cleans_the_old_external_blob() {
        let context = TestContext::new();
        let history_id = context
            .service
            .create_manual_text(&"大".repeat(40_000))
            .expect("无法创建大文本");
        let item_id = context.item_id(history_id);
        let connection = context.database.connect().expect("无法连接测试数据库");
        let external_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM clipboard_formats
                 WHERE item_id = ?1 AND storage_kind = 'external'",
                [item_id],
                |row| row.get(0),
            )
            .expect("无法查询外部格式");
        assert_eq!(external_count, 1);

        context
            .service
            .save_for_item(item_id, "短文本")
            .expect("无法替换大文本");
        let (inline_count, blob_count): (i64, i64) = connection
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM clipboard_formats
                     WHERE item_id = ?1 AND storage_kind = 'inline'),
                    (SELECT COUNT(*) FROM binary_blobs)",
                [item_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("无法查询 blob 清理结果");
        assert_eq!(inline_count, 1);
        assert_eq!(blob_count, 0);
    }
}
