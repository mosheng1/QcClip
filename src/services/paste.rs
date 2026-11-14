use std::{
    io::Cursor,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use rusqlite::{Connection, OptionalExtension};
use thiserror::Error;

use crate::{
    domain::{
        capture::{FormatKind, FormatRole, classify_format_role},
        clipboard::{ClipboardItemId, HistoryItemId},
    },
    platform::{
        clipboard::{
            ClipboardPayload, ClipboardWriteSuppressor, copy_to_clipboard, paste_into_active_window,
        },
        image_temp_file::{
            ImageTempFile, create_openable_image_file, hdrop_bytes_for_paths,
            retain_image_temp_file,
        },
        shell_open::open_with_default_program,
    },
    services::{
        database::{
            Database, FavoriteReorderError, FavoriteRepository, HistoryReorderError,
            HistoryRepository,
        },
        image_preview::parse_file_list,
        storage::{RawBlob, RawBlobError, RawBlobStore},
    },
};

#[derive(Clone, Debug)]
pub struct PasteService {
    database: Database,
    blob_store: RawBlobStore,
    clipboard_write_suppressor: ClipboardWriteSuppressor,
}

struct LoadedPayload {
    payload: ClipboardPayload,
    preview_content_kind: Option<String>,
}

pub(crate) struct PreparedMergedPaste {
    item_ids: Vec<i64>,
    payloads: Vec<ClipboardPayload>,
    image_files: Vec<ImageTempFile>,
}

struct MergedPayload {
    payloads: Vec<ClipboardPayload>,
    image_files: Vec<ImageTempFile>,
}

/// 右键菜单“图片文件”使用的虚拟格式标识；命中时粘贴为 CF_HDROP 文件列表。
pub const IMAGE_FILE_FORMAT_KEY: &str = "qcclip-image-file";

/// 右键菜单“文件名称”使用的虚拟格式标识；命中时把文件列表中的文件名作为纯文本粘贴。
pub const FILE_NAME_FORMAT_KEY: &str = "qcclip-file-name";

/// 右键菜单“文件路径”使用的虚拟格式标识；命中时把文件列表中的完整路径作为纯文本粘贴。
pub const FILE_PATH_FORMAT_KEY: &str = "qcclip-file-path";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PasteOrderTarget {
    Clipboard,
    Favorites,
}

/// 粘贴成功并写入计数后发布的变更。界面订阅后按需就地同步，粘贴入口不需要主动刷新界面。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PastedItem {
    pub(crate) item_id: ClipboardItemId,
    /// 剪贴板历史记录 ID 和更新后的粘贴次数；内容不在历史中时为空。
    pub(crate) history: Option<(HistoryItemId, u64)>,
    /// 收藏记录 ID 和更新后的粘贴次数；内容未收藏时为空。
    pub(crate) favorite: Option<(i64, u64)>,
    /// 实际被移到首位的列表；未启用、条目已置顶或已在首位时为空。
    pub(crate) moved_to_first: Option<PasteOrderTarget>,
}

/// 全局格式粘贴开关，由配置运行时同步；关闭后优先使用纯文本，文件仍保留文件列表，指定格式粘贴不受影响。
static FORMATTED_PASTE_ENABLED: AtomicBool = AtomicBool::new(true);

pub(crate) fn set_formatted_paste_enabled(enabled: bool) {
    FORMATTED_PASTE_ENABLED.store(enabled, Ordering::Relaxed);
}

fn formatted_paste_enabled() -> bool {
    FORMATTED_PASTE_ENABLED.load(Ordering::Relaxed)
}

static PASTED_ITEM_SUBSCRIBERS: Mutex<Vec<async_channel::Sender<PastedItem>>> =
    Mutex::new(Vec::new());

/// 订阅粘贴计数变更。接收端被丢弃后，下一次发布时会自动移除该订阅。
pub(crate) fn subscribe_pasted_items() -> async_channel::Receiver<PastedItem> {
    let (sender, receiver) = async_channel::unbounded();
    lock_pasted_item_subscribers().push(sender);
    receiver
}

fn publish_pasted_item(event: PastedItem) {
    lock_pasted_item_subscribers().retain(|sender| sender.try_send(event).is_ok());
}

fn lock_pasted_item_subscribers()
-> std::sync::MutexGuard<'static, Vec<async_channel::Sender<PastedItem>>> {
    match PASTED_ITEM_SUBSCRIBERS.lock() {
        Ok(subscribers) => subscribers,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[derive(Debug, Error)]
pub enum PasteError {
    #[error("无法连接数据库：{0}")]
    Database(#[from] crate::services::database::DatabaseError),
    #[error("读取历史内容失败：{0}")]
    Sql(#[from] rusqlite::Error),
    #[error("调整历史排序失败：{0}")]
    HistoryReorder(#[from] HistoryReorderError),
    #[error("调整收藏排序失败：{0}")]
    FavoriteReorder(#[from] FavoriteReorderError),
    #[error("历史内容没有可恢复的格式")]
    NoRestorableFormat,
    #[error("原始数据哈希长度无效：{0} 字节")]
    InvalidHashLength(usize),
    #[error("原始数据长度无效")]
    InvalidByteLength,
    #[error("原生剪贴板格式 ID 无效：{0}")]
    InvalidNativeFormatId(i64),
    #[error("读取原始数据失败：{0}")]
    Storage(#[from] RawBlobError),
    #[error("写回系统剪贴板失败：{0}")]
    Clipboard(String),
    #[error("读取图库图片失败：{0}")]
    GalleryImage(String),
    #[error("无法合并所选内容：{0}")]
    Merge(String),
}

enum MergeSource {
    Files(Vec<PathBuf>),
    Image {
        image_file: ImageTempFile,
        html: String,
    },
    Text {
        text: String,
        html: Option<String>,
    },
}

impl PasteService {
    pub fn new(database: Database, blob_store: RawBlobStore) -> Self {
        Self::with_clipboard_write_suppressor(
            database,
            blob_store,
            ClipboardWriteSuppressor::default(),
        )
    }

    pub fn with_clipboard_write_suppressor(
        database: Database,
        blob_store: RawBlobStore,
        clipboard_write_suppressor: ClipboardWriteSuppressor,
    ) -> Self {
        Self {
            database,
            blob_store,
            clipboard_write_suppressor,
        }
    }

    pub(crate) fn paste_item(
        &self,
        item_id: i64,
        move_to_first: Option<PasteOrderTarget>,
    ) -> Result<(), PasteError> {
        if !formatted_paste_enabled() {
            return self.paste_item_plain_text(item_id, move_to_first);
        }
        self.paste_item_with_default_formats(item_id, move_to_first)
    }

    fn paste_item_with_default_formats(
        &self,
        item_id: i64,
        move_to_first: Option<PasteOrderTarget>,
    ) -> Result<(), PasteError> {
        let connection = self.database.connect()?;
        let payloads = self.load_default_payloads(&connection, item_id)?;

        paste_into_active_window(&payloads, &self.clipboard_write_suppressor)
            .map_err(PasteError::Clipboard)?;
        self.record_paste(&connection, item_id, move_to_first)
    }

    pub(crate) fn paste_item_format(
        &self,
        item_id: i64,
        format_namespace: &str,
        format_key: &str,
        move_to_first: Option<PasteOrderTarget>,
    ) -> Result<(), PasteError> {
        let connection = self.database.connect()?;
        let payloads = self.load_payloads(&connection, item_id)?;
        // “图片文件”是右键菜单提供的虚拟格式：只保留图片载荷，CF_HDROP 由写入剪贴板时统一附加。
        let payloads = if format_key == FILE_NAME_FORMAT_KEY || format_key == FILE_PATH_FORMAT_KEY {
            let text = file_list_text(&payloads, format_key == FILE_NAME_FORMAT_KEY)
                .ok_or(PasteError::NoRestorableFormat)?;
            vec![text_payload(&text)]
        } else if format_key == IMAGE_FILE_FORMAT_KEY {
            payloads
                .into_iter()
                .filter(|payload| payload_role(payload) == FormatRole::Image)
                .collect::<Vec<_>>()
        } else {
            let payload = payloads
                .iter()
                .find(|payload| {
                    payload.format_namespace == format_namespace && payload.format_key == format_key
                })
                .cloned()
                .ok_or(PasteError::NoRestorableFormat)?;
            let role = payload_role(&payload);
            if role == FormatRole::VirtualFile {
                payloads
                    .into_iter()
                    .filter(|candidate| payload_role(candidate) == role)
                    .collect::<Vec<_>>()
            } else {
                vec![payload]
            }
        };

        paste_into_active_window(&payloads, &self.clipboard_write_suppressor)
            .map_err(PasteError::Clipboard)?;
        self.record_paste(&connection, item_id, move_to_first)
    }

    pub fn copy_item(&self, item_id: i64) -> Result<(), PasteError> {
        let connection = self.database.connect()?;
        let payloads = self.load_default_payloads(&connection, item_id)?;
        copy_to_clipboard(&payloads, &self.clipboard_write_suppressor)
            .map_err(PasteError::Clipboard)
    }

    pub fn copy_merged_items(&self, item_ids: &[i64]) -> Result<(), PasteError> {
        let MergedPayload {
            payloads,
            image_files,
        } = self.load_merged_payloads(item_ids, true)?;
        let result = copy_to_clipboard(&payloads, &self.clipboard_write_suppressor);
        for image_file in image_files {
            retain_image_temp_file(image_file);
        }
        result.map_err(PasteError::Clipboard)
    }

    pub(crate) fn prepare_merged_paste(
        &self,
        item_ids: &[i64],
    ) -> Result<PreparedMergedPaste, PasteError> {
        let MergedPayload {
            payloads,
            image_files,
        } = self.load_merged_payloads(item_ids, true)?;
        Ok(PreparedMergedPaste {
            item_ids: item_ids.to_vec(),
            payloads,
            image_files,
        })
    }

    pub(crate) fn paste_prepared_merged_items(
        &self,
        prepared: PreparedMergedPaste,
        move_to_first: Option<PasteOrderTarget>,
    ) -> Result<(), PasteError> {
        let PreparedMergedPaste {
            item_ids,
            payloads,
            image_files,
        } = prepared;
        let payloads = if formatted_paste_enabled() {
            payloads
        } else {
            plain_text_merged_payloads(payloads, !image_files.is_empty())
        };
        let result = paste_into_active_window(&payloads, &self.clipboard_write_suppressor);
        for image_file in image_files {
            retain_image_temp_file(image_file);
        }
        result.map_err(PasteError::Clipboard)?;

        let connection = self.database.connect()?;
        if connection.is_readonly(rusqlite::MAIN_DB)? {
            return Ok(());
        }
        let mut events = Vec::with_capacity(item_ids.len());
        for item_id in &item_ids {
            events.extend(self.update_paste_counts(&connection, *item_id, None)?);
        }
        if let Some(target) = move_to_first {
            for event in events.iter_mut().rev() {
                if self.move_item_to_first(event.item_id, target)? {
                    event.moved_to_first = Some(target);
                }
            }
        }
        events.into_iter().for_each(publish_pasted_item);
        Ok(())
    }

    /// 将条目中优先级最高的可恢复纯文本格式粘贴到当前活动窗口；文件条目优先提供路径文本，并保留文件列表。
    pub(crate) fn paste_item_plain_text(
        &self,
        item_id: i64,
        move_to_first: Option<PasteOrderTarget>,
    ) -> Result<(), PasteError> {
        let connection = self.database.connect()?;
        let payloads = self.load_payloads(&connection, item_id)?;
        let Some(payloads) = file_list_paste_payloads(&payloads, true)
            .or_else(|| preferred_plain_text_payload(&payloads).map(|payload| vec![payload]))
        else {
            drop(connection);
            return self.paste_item_with_default_formats(item_id, move_to_first);
        };

        paste_into_active_window(&payloads, &self.clipboard_write_suppressor)
            .map_err(PasteError::Clipboard)?;
        self.record_paste(&connection, item_id, move_to_first)
    }

    /// 将历史排序中的首个剪贴板条目以纯文本粘贴到当前活动窗口。
    pub(crate) fn paste_latest_plain_text(
        &self,
        move_to_first: Option<PasteOrderTarget>,
    ) -> Result<ClipboardItemId, PasteError> {
        let connection = self.database.connect()?;
        let item_id = connection
            .query_row(
                "SELECT h.item_id
                 FROM clipboard_history AS h
                 JOIN clipboard_items AS i ON i.id = h.item_id
                 WHERE i.capture_state = 'ready'
                 ORDER BY h.is_pinned DESC,
                          h.sort_order DESC,
                          h.last_seen_at DESC,
                          h.id DESC
                 LIMIT 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .ok_or(PasteError::NoRestorableFormat)?;
        self.paste_item_plain_text(item_id, move_to_first)
            .map(|()| item_id)
    }

    /// 按多选顺序合并多个条目，供外部拖拽使用；返回的临时图片文件需在拖拽期间保持存活。
    pub(crate) fn load_merged_drag_payloads(
        &self,
        item_ids: &[i64],
    ) -> Result<(Vec<ClipboardPayload>, Vec<ImageTempFile>), PasteError> {
        let MergedPayload {
            payloads,
            image_files,
        } = self.load_merged_payloads(item_ids, false)?;
        Ok((payloads, image_files))
    }

    pub fn load_payloads_for_item(
        &self,
        item_id: i64,
    ) -> Result<Vec<ClipboardPayload>, PasteError> {
        let connection = self.database.connect()?;
        self.load_payloads(&connection, item_id)
    }

    /// 读取条目优先级最高的纯文本格式完整内容（未经列表预览截断），供编辑和链接解析使用。
    /// 没有可解码的纯文本格式时返回 `Ok(None)`。
    pub fn item_plain_text(&self, item_id: i64) -> Result<Option<String>, PasteError> {
        let payloads = self.load_payloads_for_item(item_id)?;
        Ok(payloads
            .iter()
            .filter(|payload| payload_role(payload) == FormatRole::PlainText)
            .min_by_key(|payload| plain_text_payload_priority(payload))
            .map(|payload| decode_text_payload(payload)))
    }

    pub fn copy_text(&self, text: &str) -> Result<(), PasteError> {
        let payload = text_payload(text);
        copy_to_clipboard(&[payload], &self.clipboard_write_suppressor)
            .map_err(PasteError::Clipboard)
    }

    pub fn paste_text(&self, text: &str) -> Result<(), PasteError> {
        let payload = text_payload(text);
        paste_into_active_window(&[payload], &self.clipboard_write_suppressor)
            .map_err(PasteError::Clipboard)
    }

    /// 将图库图片的原始路径和 Windows DIB 图片格式一起写入剪贴板并粘贴。
    pub fn paste_gallery_image(&self, path: &Path) -> Result<(), PasteError> {
        let bytes = std::fs::read(path)
            .map_err(|error| PasteError::GalleryImage(format!("无法读取图片：{error}")))?;
        let image = image::load_from_memory(&bytes)
            .map_err(|error| PasteError::GalleryImage(format!("无法解码图片：{error}")))?;
        let mut bmp = Cursor::new(Vec::new());
        image
            .write_to(&mut bmp, image::ImageFormat::Bmp)
            .map_err(|error| PasteError::GalleryImage(format!("无法生成剪贴板图片：{error}")))?;
        let bmp = bmp.into_inner();
        let dib = bmp
            .get(14..)
            .ok_or_else(|| PasteError::GalleryImage("生成的 BMP 数据无效".to_owned()))?;
        let payload = ClipboardPayload {
            format_kind: FormatKind::Image.as_str().to_owned(),
            format_namespace: "windows".to_owned(),
            format_key: "CF_DIB".to_owned(),
            format_name: "CF_DIB".to_owned(),
            native_format_id: Some(8),
            bytes: dib.to_vec(),
        };
        let file_payload = file_list_payload(&[path.to_path_buf()])?;
        let payloads = [file_payload, payload];
        paste_into_active_window(&payloads, &self.clipboard_write_suppressor)
            .map_err(PasteError::Clipboard)
    }

    /// 将图库图片写入系统剪贴板，但不向当前窗口执行粘贴。
    pub fn copy_gallery_image(&self, path: &Path) -> Result<(), PasteError> {
        let bytes = std::fs::read(path)
            .map_err(|error| PasteError::GalleryImage(format!("无法读取图片：{error}")))?;
        let image = image::load_from_memory(&bytes)
            .map_err(|error| PasteError::GalleryImage(format!("无法解码图片：{error}")))?;
        let mut bmp = Cursor::new(Vec::new());
        image
            .write_to(&mut bmp, image::ImageFormat::Bmp)
            .map_err(|error| PasteError::GalleryImage(format!("无法生成剪贴板图片：{error}")))?;
        let bmp = bmp.into_inner();
        let dib = bmp
            .get(14..)
            .ok_or_else(|| PasteError::GalleryImage("生成的 BMP 数据无效".to_owned()))?;
        let payload = ClipboardPayload {
            format_kind: FormatKind::Image.as_str().to_owned(),
            format_namespace: "windows".to_owned(),
            format_key: "CF_DIB".to_owned(),
            format_name: "CF_DIB".to_owned(),
            native_format_id: Some(8),
            bytes: dib.to_vec(),
        };
        let file_payload = file_list_payload(&[path.to_path_buf()])?;
        copy_to_clipboard(&[file_payload, payload], &self.clipboard_write_suppressor)
            .map_err(PasteError::Clipboard)
    }

    /// 使用系统默认程序打开条目：文件类型直接打开路径，图片类型生成临时文件后打开。
    pub fn open_item_with_default(&self, item_id: i64) -> Result<(), PasteError> {
        let connection = self.database.connect()?;
        let item_uuid: String = connection.query_row(
            "SELECT uuid FROM clipboard_items WHERE id = ?1",
            [item_id],
            |row| row.get(0),
        )?;
        let payloads = self.load_payloads(&connection, item_id)?;
        // 文件类型优先：取第一个文件路径直接用默认程序打开。
        if let Some(path) = payloads
            .iter()
            .find(|payload| matches!(payload_role(payload), FormatRole::FileList))
            .and_then(|payload| parse_file_list(&payload.bytes).into_iter().next())
        {
            return open_with_default_program(&path).map_err(PasteError::Clipboard);
        }
        // 图片缓存由条目 UUID 和图片哈希标识，重复打开时复用同一文件。
        if let Some(path) =
            create_openable_image_file(&payloads, &item_uuid).map_err(PasteError::Clipboard)?
        {
            return open_with_default_program(&path).map_err(PasteError::Clipboard);
        }
        Err(PasteError::NoRestorableFormat)
    }

    fn load_payloads(
        &self,
        connection: &Connection,
        item_id: i64,
    ) -> Result<Vec<ClipboardPayload>, PasteError> {
        let mut statement = connection.prepare(
            "SELECT
                f.format_namespace,
                f.format_kind,
                f.format_key,
                f.format_name,
                f.preview_content_kind,
                f.native_format_id,
                f.storage_kind,
                f.inline_data,
                b.sha256,
                b.byte_length,
                b.relative_path
             FROM clipboard_formats AS f
             LEFT JOIN binary_blobs AS b ON b.id = f.blob_id
             WHERE f.item_id = ?1
               AND f.capture_state IN ('captured', 'generated')
               AND f.restore_state = 'restorable'
             ORDER BY f.format_order",
        )?;
        let rows = statement.query_map([item_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<Vec<u8>>>(7)?,
                row.get::<_, Option<Vec<u8>>>(8)?,
                row.get::<_, Option<i64>>(9)?,
                row.get::<_, Option<String>>(10)?,
            ))
        })?;
        let mut loaded_payloads = Vec::new();
        for row in rows {
            let (
                format_namespace,
                format_kind,
                format_key,
                format_name,
                preview_content_kind,
                native_format_id,
                storage_kind,
                inline_data,
                sha256,
                byte_length,
                relative_path,
            ) = row?;
            if FormatKind::from_str(&format_kind).is_none()
                || !matches!(storage_kind.as_str(), "inline" | "external")
            {
                continue;
            }
            let bytes = self.load_bytes(
                &storage_kind,
                inline_data,
                sha256,
                byte_length,
                relative_path,
            )?;
            loaded_payloads.push(LoadedPayload {
                payload: ClipboardPayload {
                    format_kind,
                    format_namespace,
                    format_key,
                    format_name,
                    native_format_id: native_format_id
                        .map(|value| {
                            u32::try_from(value)
                                .map_err(|_| PasteError::InvalidNativeFormatId(value))
                        })
                        .transpose()?,
                    bytes,
                },
                preview_content_kind,
            });
        }
        loaded_payloads.retain(|loaded| !payload_role(&loaded.payload).is_internal());
        let native_image_available = loaded_payloads
            .iter()
            .any(|loaded| payload_role(&loaded.payload) == FormatRole::Image);
        loaded_payloads.retain(|loaded| {
            !(native_image_available
                && (is_image_only_html_payload(loaded) || is_empty_textual_payload(loaded)))
        });

        let mut payloads = loaded_payloads
            .into_iter()
            .map(|loaded| loaded.payload)
            .collect::<Vec<_>>();
        if payloads
            .iter()
            .any(|p| payload_role(p) == FormatRole::FileList)
        {
            let root = self
                .database
                .path()
                .parent()
                .ok_or_else(|| PasteError::Clipboard("数据目录无效".into()))?;
            if let Some(paths) = crate::services::sync::managed_file_list(
                connection,
                &self.blob_store,
                root,
                item_id,
            )
            .map_err(|e| PasteError::Clipboard(e.to_string()))?
            {
                for payload in payloads
                    .iter_mut()
                    .filter(|p| payload_role(p) == FormatRole::FileList)
                {
                    payload.bytes = crate::platform::image_temp_file::hdrop_bytes_for_paths(&paths);
                }
            }
        }
        if payloads
            .iter()
            .any(|payload| payload_role(payload) == FormatRole::VirtualFile)
            && !virtual_file_payloads_complete(&payloads)
        {
            payloads.retain(|payload| payload_role(payload) != FormatRole::VirtualFile);
        }
        if payloads.is_empty() {
            return Err(PasteError::NoRestorableFormat);
        }
        Ok(payloads)
    }

    fn load_default_payloads(
        &self,
        connection: &Connection,
        item_id: i64,
    ) -> Result<Vec<ClipboardPayload>, PasteError> {
        let payloads = self.load_payloads(connection, item_id)?;
        if let Some(custom_text) = payloads.iter().find(|payload| {
            crate::domain::capture::is_custom_text_format(
                &payload.format_namespace,
                &payload.format_key,
            )
        }) {
            return Ok(vec![custom_text.clone()]);
        }
        Ok(file_list_paste_payloads(&payloads, false).unwrap_or(payloads))
    }

    fn load_merged_payloads(
        &self,
        item_ids: &[i64],
        file_paths_as_text: bool,
    ) -> Result<MergedPayload, PasteError> {
        if item_ids.is_empty() {
            return Err(PasteError::Merge("至少需要选择一项内容".to_owned()));
        }

        let connection = self.database.connect()?;
        if item_ids.len() == 1 {
            return Ok(MergedPayload {
                payloads: self.load_default_payloads(&connection, item_ids[0])?,
                image_files: Vec::new(),
            });
        }

        let payloads = item_ids
            .iter()
            .map(|item_id| self.load_default_payloads(&connection, *item_id))
            .collect::<Result<Vec<_>, _>>()?;
        // 文件和纯图片可组成文件列表；含有实际文本时才将文件转为可内嵌内容。
        let file_paths_as_text = file_paths_as_text
            && !payloads.iter().all(|payloads| {
                payloads
                    .iter()
                    .any(|payload| payload_role(payload) == FormatRole::FileList)
                    || payloads
                        .iter()
                        .any(|payload| payload_role(payload) == FormatRole::Image)
                        && !payloads.iter().any(|payload| match payload_role(payload) {
                            FormatRole::PlainText => {
                                !decode_text_payload(payload).trim().is_empty()
                            }
                            FormatRole::Html => {
                                !normalize_html_fragment(&String::from_utf8_lossy(&payload.bytes))
                                    .trim()
                                    .is_empty()
                            }
                            _ => false,
                        })
            });
        let sources = item_ids
            .iter()
            .zip(payloads)
            .map(|(item_id, payloads)| {
                normalize_merge_source(*item_id, payloads, file_paths_as_text)
            })
            .collect::<Result<Vec<_>, _>>()?;
        determine_merge_payload(sources)
    }

    fn load_bytes(
        &self,
        storage_kind: &str,
        inline_data: Option<Vec<u8>>,
        sha256: Option<Vec<u8>>,
        byte_length: Option<i64>,
        relative_path: Option<String>,
    ) -> Result<Vec<u8>, PasteError> {
        Ok(match storage_kind {
            "inline" => inline_data.ok_or(PasteError::NoRestorableFormat)?,
            "external" => {
                let hash = sha256.ok_or(PasteError::NoRestorableFormat)?;
                let sha256: [u8; 32] = hash
                    .try_into()
                    .map_err(|value: Vec<u8>| PasteError::InvalidHashLength(value.len()))?;
                let byte_length = byte_length
                    .ok_or(PasteError::NoRestorableFormat)?
                    .try_into()
                    .map_err(|_| PasteError::InvalidByteLength)?;
                let relative_path = relative_path.ok_or(PasteError::NoRestorableFormat)?;
                self.blob_store
                    .read(&RawBlob::from_record(sha256, byte_length, relative_path))?
            }
            _ => return Err(PasteError::NoRestorableFormat),
        })
    }

    /// 写入粘贴计数和排序后发布变更；只读数据库不写入也不发布。
    fn record_paste(
        &self,
        connection: &Connection,
        item_id: i64,
        move_to_first: Option<PasteOrderTarget>,
    ) -> Result<(), PasteError> {
        if let Some(event) = self.update_paste_counts(connection, item_id, move_to_first)? {
            publish_pasted_item(event);
        }
        Ok(())
    }

    fn update_paste_counts(
        &self,
        connection: &Connection,
        item_id: i64,
        move_to_first: Option<PasteOrderTarget>,
    ) -> Result<Option<PastedItem>, PasteError> {
        if connection.is_readonly(rusqlite::MAIN_DB)? {
            return Ok(None);
        }
        let timestamp = current_timestamp_millis();
        let read_count = |row: &rusqlite::Row<'_>| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?.max(0) as u64))
        };
        let history = connection
            .query_row(
                "UPDATE clipboard_history
                 SET paste_count = paste_count + 1,
                     last_pasted_at = ?1,
                     updated_at = ?1
                 WHERE item_id = ?2
                 RETURNING id, paste_count",
                rusqlite::params![timestamp, item_id],
                read_count,
            )
            .optional()?;
        let favorite = connection
            .query_row(
                "UPDATE favorites
                 SET paste_count = paste_count + 1,
                     last_pasted_at = ?1,
                     updated_at = ?1
                 WHERE item_id = ?2
                 RETURNING id, paste_count",
                rusqlite::params![timestamp, item_id],
                read_count,
            )
            .optional()?;
        let mut event = PastedItem {
            item_id,
            history,
            favorite,
            moved_to_first: None,
        };
        if let Some(target) = move_to_first
            && self.move_item_to_first(item_id, target)?
        {
            event.moved_to_first = Some(target);
        }
        Ok(Some(event))
    }

    /// 返回条目是否实际改变了位置。
    fn move_item_to_first(
        &self,
        item_id: i64,
        target: PasteOrderTarget,
    ) -> Result<bool, PasteError> {
        Ok(match target {
            PasteOrderTarget::Clipboard => {
                HistoryRepository::new(self.database.clone(), self.blob_store.clone())
                    .move_item_to_top(item_id)?
            }
            PasteOrderTarget::Favorites => {
                FavoriteRepository::new(self.database.clone()).move_item_to_top(item_id)?
            }
        })
    }
}

fn normalize_merge_source(
    item_id: i64,
    payloads: Vec<ClipboardPayload>,
    file_paths_as_text: bool,
) -> Result<MergeSource, PasteError> {
    if payloads
        .iter()
        .any(|payload| payload_role(payload) == FormatRole::VirtualFile)
    {
        return Err(PasteError::Merge(format!(
            "第 {item_id} 项是虚拟文件，暂不支持合并"
        )));
    }

    if let Some(file_payload) = payloads
        .iter()
        .find(|payload| payload_role(payload) == FormatRole::FileList)
    {
        let paths = parse_file_list(&file_payload.bytes);
        if paths.is_empty() {
            return Err(PasteError::Merge(format!("第 {item_id} 项没有可用的文件")));
        }
        // 混合复制、粘贴使用可内嵌内容；纯文件合并和拖拽继续传输真实文件。
        if file_paths_as_text {
            let text = paths
                .iter()
                .map(|path| path.to_string_lossy())
                .collect::<Vec<_>>()
                .join("\r\n");
            let html = paths
                .iter()
                .map(|path| {
                    file_image_to_html(path)
                        .unwrap_or_else(|| plain_text_to_html(&path.to_string_lossy()))
                })
                .collect::<Vec<_>>()
                .join("<br />");
            return Ok(MergeSource::Text {
                text,
                html: Some(html),
            });
        }
        if paths.iter().any(|path| !path.exists()) {
            return Err(PasteError::Merge(format!(
                "第 {item_id} 项包含已不存在或无法访问的文件"
            )));
        }
        return Ok(MergeSource::Files(paths));
    }

    let image_payload = payloads
        .iter()
        .find(|payload| payload_role(payload) == FormatRole::Image);
    let text = payloads
        .iter()
        .find(|payload| payload_role(payload) == FormatRole::PlainText)
        .map(decode_text_payload)
        .filter(|text| !text.trim().is_empty());
    let html = payloads
        .iter()
        .find(|payload| payload_role(payload) == FormatRole::Html)
        .map(|payload| normalize_html_fragment(&String::from_utf8_lossy(&payload.bytes)))
        .filter(|html| !html.trim().is_empty());

    if text.is_none() && html.is_none() {
        if let Some(image_payload) = image_payload {
            let image_file =
                ImageTempFile::from_bytes(&image_payload.bytes, &image_payload.format_key)
                    .map_err(PasteError::Merge)?;
            let html = image_file_to_html(&image_file)?;
            return Ok(MergeSource::Image { image_file, html });
        }
        return Err(PasteError::Merge(format!(
            "第 {item_id} 项不是可合并的文本、图片或文件"
        )));
    }

    let image_html = image_payload.map(image_to_html).transpose()?;
    let has_plain_text = text.is_some();
    let text = text
        .clone()
        .or_else(|| {
            html.as_deref()
                .map(strip_html_tags)
                .filter(|text| !text.is_empty())
        })
        .or_else(|| image_html.as_ref().map(|_| "[图片]".to_owned()))
        .unwrap_or_default();
    let html = match (html, image_html) {
        (Some(html), _) => Some(html),
        (None, Some(image_html)) => {
            let mut html = if has_plain_text {
                plain_text_to_html(&text)
            } else {
                String::new()
            };
            html.push_str(&image_html);
            Some(html)
        }
        (None, None) => None,
    };
    Ok(MergeSource::Text { text, html })
}

fn determine_merge_payload(sources: Vec<MergeSource>) -> Result<MergedPayload, PasteError> {
    let has_files = sources
        .iter()
        .any(|source| matches!(source, MergeSource::Files(_)));
    let has_text = sources
        .iter()
        .any(|source| matches!(source, MergeSource::Text { .. }));
    let has_images = sources
        .iter()
        .any(|source| matches!(source, MergeSource::Image { .. }));
    if has_files && has_text {
        return Err(PasteError::Merge("文件不能与文本混合合并".to_owned()));
    }

    if has_files || (has_images && !has_text) {
        let mut paths = Vec::new();
        let mut image_files = Vec::new();
        let mut image_html = Vec::new();
        for source in sources {
            match source {
                MergeSource::Files(file_paths) => paths.extend(file_paths),
                MergeSource::Image { image_file, html } => {
                    paths.push(image_file.path().to_path_buf());
                    image_files.push(image_file);
                    image_html.push(wrap_merge_html_block(&html));
                }
                MergeSource::Text { .. } => {
                    return Err(PasteError::Merge("文件不能与文本混合合并".to_owned()));
                }
            }
        }

        let mut payloads = vec![file_list_payload(&paths)?];
        if has_files {
            payloads = file_list_paste_payloads(&payloads, false).unwrap_or(payloads);
        } else if !image_html.is_empty() {
            let text = vec!["[图片]"; image_html.len()].join("\n\n");
            payloads.push(text_payload(&text));
            payloads.push(html_payload(&image_html.join("")));
        }
        return Ok(MergedPayload {
            payloads,
            image_files,
        });
    }

    let requires_rich_text = has_images
        || sources
            .iter()
            .any(|source| matches!(source, MergeSource::Text { html: Some(_), .. }));
    if !requires_rich_text {
        let text = sources
            .into_iter()
            .map(|source| match source {
                MergeSource::Text { text, .. } => text,
                _ => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        return Ok(MergedPayload {
            payloads: vec![text_payload(&text)],
            image_files: Vec::new(),
        });
    }

    let mut plain_segments = Vec::new();
    let mut html_segments = Vec::new();
    for source in sources {
        match source {
            MergeSource::Text { text, html } => {
                plain_segments.push(text.clone());
                let html = html.unwrap_or_else(|| plain_text_to_html(&text));
                html_segments.push(wrap_merge_html_block(&html));
            }
            MergeSource::Image {
                image_file: _,
                html,
            } => {
                plain_segments.push("[图片]".to_owned());
                html_segments.push(wrap_merge_html_block(&html));
            }
            MergeSource::Files(_) => {
                return Err(PasteError::Merge("文件不能与文本混合合并".to_owned()));
            }
        }
    }
    Ok(MergedPayload {
        payloads: vec![
            text_payload(&plain_segments.join("\n\n")),
            html_payload(&html_segments.join("")),
        ],
        image_files: Vec::new(),
    })
}

fn file_list_payload(paths: &[PathBuf]) -> Result<ClipboardPayload, PasteError> {
    if paths.is_empty() {
        return Err(PasteError::Merge("没有可用于合并的文件".to_owned()));
    }
    Ok(ClipboardPayload {
        format_kind: FormatKind::FileList.as_str().to_owned(),
        format_namespace: "windows".to_owned(),
        format_key: "CF_HDROP".to_owned(),
        format_name: "CF_HDROP".to_owned(),
        native_format_id: Some(15),
        bytes: hdrop_bytes_for_paths(paths),
    })
}

fn image_to_html(payload: &ClipboardPayload) -> Result<String, PasteError> {
    let image_file = ImageTempFile::from_bytes(&payload.bytes, &payload.format_key)
        .map_err(PasteError::Merge)?;
    image_file_to_html(&image_file)
}

fn image_file_to_html(image_file: &ImageTempFile) -> Result<String, PasteError> {
    let png = std::fs::read(image_file.path())
        .map_err(|error| PasteError::Merge(format!("读取图片失败：{error}")))?;
    Ok(png_to_html(&png))
}

/// 可解码的本地图片直接嵌入合并内容，其余文件和失效路径由调用方回退为路径文本。
fn file_image_to_html(path: &Path) -> Option<String> {
    let image = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    let mut png = Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).ok()?;
    Some(png_to_html(png.get_ref()))
}

fn png_to_html(png: &[u8]) -> String {
    format!(
        "<img src=\"data:image/png;base64,{}\" alt=\"图片\" style=\"max-width:100%;height:auto;display:block;\" />",
        BASE64.encode(png)
    )
}

fn html_payload(html: &str) -> ClipboardPayload {
    ClipboardPayload {
        format_kind: FormatKind::Html.as_str().to_owned(),
        format_namespace: "windows".to_owned(),
        format_key: "registered:HTML Format".to_owned(),
        format_name: "HTML Format".to_owned(),
        native_format_id: None,
        bytes: build_cf_html(html),
    }
}

fn build_cf_html(fragment: &str) -> Vec<u8> {
    let document = html_document_with_markers(fragment);
    let header = "Version:0.9\r\nStartHTML:0000000000\r\nEndHTML:0000000000\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\n";
    let start_html = header.len();
    let end_html = start_html + document.len();
    let start_fragment = start_html
        + document
            .find("<!--StartFragment-->")
            .map(|index| index + "<!--StartFragment-->".len())
            .unwrap_or(0);
    let end_fragment = start_html
        + document
            .find("<!--EndFragment-->")
            .unwrap_or(document.len());
    format!(
        "Version:0.9\r\nStartHTML:{start_html:010}\r\nEndHTML:{end_html:010}\r\nStartFragment:{start_fragment:010}\r\nEndFragment:{end_fragment:010}\r\n{document}"
    )
    .into_bytes()
}

fn html_document_with_markers(fragment: &str) -> String {
    if fragment.contains("<!--StartFragment-->") && fragment.contains("<!--EndFragment-->") {
        return fragment.to_owned();
    }
    let lower = fragment.to_ascii_lowercase();
    if lower.contains("<html") {
        if let Some(body_start) = lower.find("<body")
            && let Some(body_end) = fragment[body_start..].find('>')
            && let Some(close_start) = lower.rfind("</body>")
        {
            let body_end = body_start + body_end + 1;
            return format!(
                "{}<!--StartFragment-->{}<!--EndFragment-->{}",
                &fragment[..body_end],
                &fragment[body_end..close_start],
                &fragment[close_start..]
            );
        }
    }
    format!(
        "<!DOCTYPE html>\n<html>\n<head><meta charset=\"utf-8\"></head>\n<body><!--StartFragment-->{fragment}<!--EndFragment--></body>\n</html>"
    )
}

fn normalize_html_fragment(raw: &str) -> String {
    if let (Some(start), Some(end)) = (
        raw.find("<!--StartFragment-->")
            .map(|index| index + "<!--StartFragment-->".len()),
        raw.find("<!--EndFragment-->"),
    ) && start < end
    {
        return raw[start..end].to_owned();
    }
    let (start, end) = match (
        html_offset(raw, "StartFragment:"),
        html_offset(raw, "EndFragment:"),
    ) {
        (Some(start), Some(end)) => (start, end),
        _ => match (html_offset(raw, "StartHTML:"), html_offset(raw, "EndHTML:")) {
            (Some(start), Some(end)) => (start, end),
            _ => return raw.to_owned(),
        },
    };
    raw.as_bytes()
        .get(start..end)
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .unwrap_or_else(|| raw.to_owned())
}

fn html_offset(raw: &str, key: &str) -> Option<usize> {
    let start = raw.find(key)? + key.len();
    let digits = raw[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
}

fn strip_html_tags(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut tag = String::new();
    for character in html.chars() {
        match character {
            '<' => {
                in_tag = true;
                tag.clear();
            }
            '>' if in_tag => {
                let tag_name = tag
                    .trim()
                    .trim_start_matches('/')
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if tag_name == "br"
                    || (tag.trim_start().starts_with('/')
                        && matches!(
                            tag_name.as_str(),
                            "p" | "div"
                                | "li"
                                | "tr"
                                | "h1"
                                | "h2"
                                | "h3"
                                | "h4"
                                | "h5"
                                | "h6"
                                | "blockquote"
                        ))
                {
                    text.push('\n');
                }
                in_tag = false;
            }
            character if in_tag => tag.push(character),
            character => text.push(character),
        }
    }
    text.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim()
        .to_owned()
}

fn wrap_merge_html_block(fragment: &str) -> String {
    format!("<div style=\"margin:0 0 12px 0;\">{fragment}</div>")
}

fn plain_text_to_html(text: &str) -> String {
    html_escape(text)
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', "<br />")
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn text_payload(text: &str) -> ClipboardPayload {
    let bytes = text
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    ClipboardPayload {
        format_kind: FormatKind::Text.as_str().to_owned(),
        format_namespace: "windows".to_owned(),
        format_key: "CF_UNICODETEXT".to_owned(),
        format_name: "CF_UNICODETEXT".to_owned(),
        native_format_id: Some(13),
        bytes,
    }
}

/// 将纯文本载荷的字节解码为字符串；与列表预览的 decode_text 逻辑一致，
/// 但这里只能依据格式身份判断 UTF-16（载荷不携带 encoding 元数据）。
fn decode_text_payload(payload: &ClipboardPayload) -> String {
    let bytes = payload.bytes.as_slice();
    if payload.format_key.eq_ignore_ascii_case("CF_UNICODETEXT")
        || crate::domain::capture::is_custom_text_format(
            &payload.format_namespace,
            &payload.format_key,
        )
    {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .take_while(|unit| *unit != 0)
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes)
            .trim_end_matches('\0')
            .to_owned()
    }
}

/// 选取优先级最高的非空纯文本格式；条目没有可用纯文本时返回空。
fn preferred_plain_text_payload(payloads: &[ClipboardPayload]) -> Option<ClipboardPayload> {
    payloads
        .iter()
        .filter(|payload| {
            payload_role(payload) == FormatRole::PlainText
                && !decode_text_payload(payload).trim().is_empty()
        })
        .min_by_key(|payload| plain_text_payload_priority(payload))
        .cloned()
}

/// 把文件列表载荷转换为每行一项的文本；`names_only` 为 true 时只取文件名，否则取完整路径。
/// 条目没有可解析的文件列表时返回空。
fn file_list_text(payloads: &[ClipboardPayload], names_only: bool) -> Option<String> {
    let paths = payloads
        .iter()
        .find(|payload| payload_role(payload) == FormatRole::FileList)
        .map(|payload| parse_file_list(&payload.bytes))
        .filter(|paths| !paths.is_empty())?;
    let lines = paths
        .iter()
        .map(|path| {
            // 盘符根目录等没有文件名的路径回落为完整路径。
            let name = names_only.then(|| path.file_name()).flatten();
            name.unwrap_or(path.as_os_str())
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    Some(lines.join("\r\n"))
}

/// 文件同时提供路径文本和文件列表；正常模式保留其余非文本格式，纯文本模式让路径文本优先。
fn file_list_paste_payloads(
    payloads: &[ClipboardPayload],
    text_first: bool,
) -> Option<Vec<ClipboardPayload>> {
    let text = text_payload(&file_list_text(payloads, false)?);
    let mut files = payloads
        .iter()
        .filter(|payload| payload_role(payload) == FormatRole::FileList)
        .cloned()
        .collect::<Vec<_>>();
    if text_first {
        files.insert(0, text);
    } else {
        files.push(text);
        files.extend(
            payloads
                .iter()
                .filter(|payload| {
                    !matches!(
                        payload_role(payload),
                        FormatRole::FileList | FormatRole::PlainText
                    )
                })
                .cloned(),
        );
    }
    Some(files)
}

/// 关闭格式粘贴时的合并载荷：文件优先提供路径文本并保留文件列表；图片合并保持原样，避免只粘贴“[图片]”。
fn plain_text_merged_payloads(
    payloads: Vec<ClipboardPayload>,
    has_image_files: bool,
) -> Vec<ClipboardPayload> {
    if payloads
        .iter()
        .any(|payload| payload_role(payload) == FormatRole::FileList)
    {
        // 纯图片合并生成临时文件并携带 HTML；真实文件条目的文本载荷也可能是路径，不能据此区分。
        if has_image_files
            && payloads
                .iter()
                .any(|payload| payload_role(payload) == FormatRole::Html)
        {
            return payloads;
        }
        return file_list_paste_payloads(&payloads, true).unwrap_or(payloads);
    }
    match preferred_plain_text_payload(&payloads) {
        Some(payload) => vec![payload],
        None => payloads,
    }
}

fn payload_role(payload: &ClipboardPayload) -> FormatRole {
    let kind = FormatKind::from_str(&payload.format_kind).unwrap_or(FormatKind::Unknown);
    classify_format_role(kind, &payload.format_key, &payload.format_name)
}

/// 选择要展示或粘贴的纯文本格式，避免按系统枚举顺序误选 ANSI 文本。
fn plain_text_payload_priority(payload: &ClipboardPayload) -> u8 {
    if crate::domain::capture::is_custom_text_format(&payload.format_namespace, &payload.format_key)
    {
        return 0;
    }
    match payload.format_key.to_ascii_lowercase().as_str() {
        "cf_unicodetext" => 1,
        "cf_text" => 2,
        "cf_oemtext" => 3,
        _ => 4,
    }
}

fn is_image_only_html_payload(payload: &LoadedPayload) -> bool {
    if payload_role(&payload.payload) != FormatRole::Html {
        return false;
    }
    payload.preview_content_kind.as_deref() == Some("image")
}

fn is_empty_textual_payload(payload: &LoadedPayload) -> bool {
    matches!(
        payload_role(&payload.payload),
        FormatRole::PlainText | FormatRole::Html
    ) && payload.preview_content_kind.as_deref() == Some("unknown")
}

fn virtual_file_payloads_complete(payloads: &[ClipboardPayload]) -> bool {
    payloads.iter().any(|payload| {
        payload_role(payload) == FormatRole::VirtualFile
            && payload
                .format_key
                .to_ascii_lowercase()
                .contains("filegroupdescriptor")
    }) && payloads.iter().any(|payload| {
        payload_role(payload) == FormatRole::VirtualFile
            && payload
                .format_key
                .to_ascii_lowercase()
                .contains("filecontents")
    })
}

fn current_timestamp_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn plain_text_merged_payloads_keeps_only_text_for_rich_merge() {
        let payloads = super::plain_text_merged_payloads(
            vec![
                super::text_payload(
                    "a

b",
                ),
                super::html_payload("<b>a</b><i>b</i>"),
            ],
            false,
        );
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0].format_key, "CF_UNICODETEXT");
    }

    #[test]
    fn plain_text_merged_payloads_keeps_file_merge_unchanged() {
        let payloads = super::plain_text_merged_payloads(
            vec![
                super::file_list_payload(&[std::path::PathBuf::from(r"C:\a.png")]).unwrap(),
                super::text_payload("[图片]"),
                super::html_payload("<img>"),
            ],
            true,
        );
        assert_eq!(payloads.len(), 3);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn plain_text_merged_payloads_pastes_paths_for_pure_file_merge() {
        let files = super::file_list_payload(&[
            std::path::PathBuf::from(r"C:\docs\a.txt"),
            std::path::PathBuf::from(r"D:\b.png"),
        ])
        .unwrap();
        let payloads = super::plain_text_merged_payloads(vec![files.clone()], false);
        assert_eq!(payloads.len(), 2);
        assert_eq!(payloads[0].format_key, "CF_UNICODETEXT");
        assert_eq!(payloads[1], files);
        assert_eq!(
            super::decode_text_payload(&payloads[0]),
            "C:\\docs\\a.txt\r\nD:\\b.png"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn file_paste_payloads_keep_files_and_paths_in_mode_order() {
        let paths = vec![
            PathBuf::from(r"C:\docs\中文.txt"),
            PathBuf::from(r"D:\b.png"),
        ];
        let files = file_list_payload(&paths).unwrap();
        let text = text_payload("C:\\docs\\中文.txt\r\nD:\\b.png");
        let html = html_payload("<b>文件</b>");
        let original = vec![text_payload("旧路径"), html.clone(), files.clone()];
        assert_eq!(
            file_list_paste_payloads(&original, false).unwrap(),
            vec![files.clone(), text.clone(), html]
        );
        assert_eq!(
            file_list_paste_payloads(&original, true).unwrap(),
            vec![text.clone(), files.clone()]
        );

        let merged =
            determine_merge_payload(vec![MergeSource::Files(paths)]).expect("无法生成文件合并载荷");
        assert_eq!(merged.payloads, vec![files.clone(), text.clone()]);
        assert_eq!(
            plain_text_merged_payloads(merged.payloads, false),
            vec![text, files]
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn file_list_text_returns_names_or_paths() {
        let payloads = vec![
            super::file_list_payload(&[
                std::path::PathBuf::from(r"C:\docs\a.txt"),
                std::path::PathBuf::from(r"C:\"),
            ])
            .unwrap(),
        ];
        assert_eq!(
            super::file_list_text(&payloads, true).as_deref(),
            Some("a.txt\r\nC:\\")
        );
        assert_eq!(
            super::file_list_text(&payloads, false).as_deref(),
            Some("C:\\docs\\a.txt\r\nC:\\")
        );
        assert_eq!(
            super::file_list_text(&[super::text_payload("x")], false),
            None
        );
    }

    use crate::domain::capture::{CapturedClipboard, FormatKind, RawFormat, SourceApplication};
    use crate::services::{
        capture::{CaptureService, mock::MockClipboardSource},
        custom_text::CustomTextService,
        storage::RawBlobStore,
    };

    use super::*;

    #[test]
    fn merged_files_mix_paths_with_text_html_and_images() {
        let paths = vec![PathBuf::from(r"C:\资料\a&b.txt")];
        let files = file_list_payload(&paths).expect("无法生成文件载荷");
        let text = text_payload("说明");
        let rich = html_payload("<b>富文本</b>");
        let mut png = Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(1, 1)
            .write_to(&mut png, image::ImageFormat::Png)
            .expect("无法生成测试图片");
        let image = ClipboardPayload {
            format_kind: FormatKind::Image.as_str().to_owned(),
            format_namespace: "windows".to_owned(),
            format_key: "registered:PNG".to_owned(),
            format_name: "PNG".to_owned(),
            native_format_id: None,
            bytes: png.into_inner(),
        };
        let sources = [vec![files], vec![text], vec![rich], vec![image]]
            .into_iter()
            .map(|payloads| normalize_merge_source(1, payloads, true))
            .collect::<Result<Vec<_>, _>>()
            .expect("无法归一化合并内容");
        let merged = determine_merge_payload(sources).expect("文件应支持与其他类型混合合并");
        assert_eq!(merged.payloads.len(), 2);
        assert_eq!(
            decode_text_payload(&merged.payloads[0]),
            "C:\\资料\\a&b.txt\n\n说明\n\n富文本\n\n[图片]"
        );
        let html = normalize_html_fragment(&String::from_utf8_lossy(&merged.payloads[1].bytes));
        let positions = [
            "C:\\资料\\a&amp;b.txt",
            "说明",
            "<b>富文本</b>",
            "data:image/png;base64,",
        ]
        .map(|fragment| html.find(fragment).expect("合并内容缺失"));
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(
            plain_text_merged_payloads(merged.payloads.clone(), false),
            vec![merged.payloads[0].clone()]
        );
    }

    #[test]
    fn image_files_in_mixed_merge_embed_pixels_and_keep_file_drag_payloads() {
        let directory = tempfile::tempdir().expect("无法创建图片目录");
        let paths = vec![
            directory.path().join("图片.png"),
            directory.path().join("图片.jpg"),
        ];
        for path in &paths {
            image::DynamicImage::new_rgb8(2, 1)
                .save(path)
                .expect("无法保存测试图片");
        }
        let files = file_list_payload(&paths).expect("无法生成文件载荷");
        let source =
            normalize_merge_source(1, vec![files.clone()], true).expect("无法归一化图片文件");
        let text =
            normalize_merge_source(2, vec![text_payload("说明")], true).expect("无法归一化文本");
        let merged = determine_merge_payload(vec![source, text]).expect("无法合并图片文件");
        assert_eq!(merged.payloads.len(), 2);
        let html = normalize_html_fragment(&String::from_utf8_lossy(&merged.payloads[1].bytes));
        assert_eq!(html.matches("data:image/png;base64,").count(), 2);
        assert!(!html.contains("图片.png") && !html.contains("图片.jpg"));
        for encoded in html.split("data:image/png;base64,").skip(1) {
            let png = BASE64
                .decode(encoded.split('"').next().unwrap())
                .expect("图片编码无效");
            let image = image::load_from_memory(&png).expect("内嵌图片无法解码");
            assert_eq!((image.width(), image.height()), (2, 1));
        }

        let source = normalize_merge_source(1, vec![files], false).expect("无法生成文件拖拽内容");
        assert!(matches!(source, MergeSource::Files(actual) if actual == paths));
    }

    #[test]
    fn pure_file_merge_preparation_preserves_files_and_paths() {
        let directory = tempfile::tempdir().expect("无法创建测试目录");
        let paths = vec![
            directory.path().join("文件.txt"),
            directory.path().join("图片.png"),
        ];
        std::fs::write(&paths[0], "文件内容").expect("无法创建测试文件");
        image::DynamicImage::new_rgb8(1, 1)
            .save(&paths[1])
            .expect("无法创建测试图片");
        let database =
            Database::open(directory.path().join("test.sqlite3")).expect("无法创建数据库");
        let store = RawBlobStore::new(directory.path()).expect("无法创建文件存储");
        let capture = CaptureService::new(database.clone(), store.clone());
        let connection = database.connect().expect("无法连接数据库");
        let mut item_ids = Vec::new();
        for path in &paths {
            let history_id = capture
                .store_captured_clipboard(&CapturedClipboard {
                    formats: vec![RawFormat::captured(
                        "windows",
                        "CF_HDROP",
                        "CF_HDROP",
                        FormatKind::FileList,
                        hdrop_bytes_for_paths(std::slice::from_ref(path)),
                        0,
                        true,
                    )],
                    primary_format_identity: Some(("windows".to_owned(), "CF_HDROP".to_owned())),
                    preview_candidate: None,
                    source_application: SourceApplication::default(),
                })
                .expect("无法保存文件条目");
            item_ids.push(
                connection
                    .query_row(
                        "SELECT item_id FROM clipboard_history WHERE id = ?1",
                        [history_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .expect("无法读取文件条目 ID"),
            );
        }
        let service = PasteService::new(database, store);
        let prepared = service
            .prepare_merged_paste(&item_ids)
            .expect("无法准备纯文件合并粘贴");
        let files = file_list_payload(&paths).expect("无法生成预期文件列表");
        let text = text_payload(
            &paths
                .iter()
                .map(|path| path.to_string_lossy())
                .collect::<Vec<_>>()
                .join("\r\n"),
        );
        assert_eq!(prepared.payloads, vec![files.clone(), text.clone()]);
        assert_eq!(
            plain_text_merged_payloads(prepared.payloads, false),
            vec![text, files]
        );

        let mut captured = MockClipboardSource::text("").capture();
        captured.formats = vec![RawFormat::captured(
            "windows",
            "registered:PNG",
            "PNG",
            FormatKind::Image,
            std::fs::read(&paths[1]).expect("无法读取测试图片"),
            0,
            true,
        )];
        captured.primary_format_identity =
            Some(("windows".to_owned(), "registered:PNG".to_owned()));
        let history_id = capture
            .store_captured_clipboard(&captured)
            .expect("无法保存原生图片条目");
        let image_id = connection
            .query_row(
                "SELECT item_id FROM clipboard_history WHERE id = ?1",
                [history_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("无法读取原生图片条目 ID");
        item_ids.insert(0, image_id);
        let prepared = service
            .prepare_merged_paste(&item_ids)
            .expect("无法准备原生图片与文件合并粘贴");
        assert_eq!(prepared.image_files.len(), 1);
        let image_path = prepared.image_files[0].path().to_path_buf();
        assert!(image_path.is_file());
        let mut expected_paths = vec![image_path];
        expected_paths.extend(paths);
        let files = file_list_payload(&expected_paths).expect("无法生成预期文件列表");
        let text = text_payload(
            &expected_paths
                .iter()
                .map(|path| path.to_string_lossy())
                .collect::<Vec<_>>()
                .join("\r\n"),
        );
        assert_eq!(prepared.payloads, vec![files.clone(), text.clone()]);
        assert_eq!(
            plain_text_merged_payloads(prepared.payloads, true),
            vec![text, files]
        );

        captured.formats.push(RawFormat::captured(
            "mime",
            "text/plain",
            "纯文本",
            FormatKind::Text,
            "图文说明".as_bytes().to_vec(),
            1,
            true,
        ));
        let history_id = capture
            .store_captured_clipboard(&captured)
            .expect("无法保存图文条目");
        item_ids[0] = connection
            .query_row(
                "SELECT item_id FROM clipboard_history WHERE id = ?1",
                [history_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("无法读取图文条目 ID");
        let prepared = service
            .prepare_merged_paste(&item_ids)
            .expect("图文条目应继续支持混合合并");
        assert_eq!(
            prepared
                .payloads
                .iter()
                .map(payload_role)
                .collect::<Vec<_>>(),
            [FormatRole::PlainText, FormatRole::Html]
        );
        assert!(decode_text_payload(&prepared.payloads[0]).contains("图文说明"));
    }

    #[test]
    fn read_only_database_loads_payloads_without_updating_paste_counts() {
        let directory = tempfile::tempdir().expect("无法创建临时目录");
        let database =
            Database::open(directory.path().join("test.sqlite3")).expect("无法创建数据库");
        let store = RawBlobStore::new(directory.path()).expect("无法创建存储");
        let history_id = CaptureService::new(database.clone(), store.clone())
            .store_captured_clipboard(&MockClipboardSource::text("只读文本").capture())
            .expect("无法捕获文本");
        let connection = database.connect().expect("无法连接数据库");
        let item_id: i64 = connection
            .query_row(
                "SELECT item_id FROM clipboard_history WHERE id = ?1",
                [history_id],
                |row| row.get(0),
            )
            .expect("无法查询内容 ID");
        connection
            .execute_batch(
                &format!("UPDATE database_compatibility SET minimum_write_version = {}; PRAGMA user_version = {};",
                    crate::services::database::DATABASE_WRITE_VERSION + 1, crate::services::database::CURRENT_SCHEMA_VERSION + 1),
            )
            .expect("无法模拟只读兼容数据库");
        drop(connection);
        let service = PasteService::new(database.clone(), store);
        assert!(
            !service
                .load_payloads_for_item(item_id)
                .expect("无法读取只读载荷")
                .is_empty()
        );
        let connection = database.connect().expect("无法连接只读数据库");
        let event = service
            .update_paste_counts(&connection, item_id, Some(PasteOrderTarget::Clipboard))
            .expect("只读模式应跳过计数和排序写入");
        assert_eq!(event, None);
        let count: i64 = connection
            .query_row(
                "SELECT paste_count FROM clipboard_history WHERE id = ?1",
                [history_id],
                |row| row.get(0),
            )
            .expect("无法查询粘贴计数");
        assert_eq!(count, 0);
    }

    #[test]
    fn missing_history_content_returns_explicit_error() {
        let directory = tempfile::tempdir().expect("无法创建临时目录");
        let database =
            Database::open(directory.path().join("test.sqlite3")).expect("无法创建数据库");
        let store = RawBlobStore::new(directory.path()).expect("无法创建文件存储");
        let service = PasteService::new(database, store);

        assert!(matches!(
            service.paste_item(999, None),
            Err(PasteError::NoRestorableFormat)
        ));
    }

    #[test]
    fn inline_primary_format_is_loaded_as_paste_payload() {
        let directory = tempfile::tempdir().expect("无法创建临时目录");
        let database =
            Database::open(directory.path().join("test.sqlite3")).expect("无法创建数据库");
        let store = RawBlobStore::new(directory.path()).expect("无法创建文件存储");
        let capture = CaptureService::new(database.clone(), store.clone());
        let mut captured = MockClipboardSource::text("测试").capture();
        captured.formats[0].format_key = "CF_UNICODETEXT".to_owned();
        captured.formats[0].format_name = "CF_UNICODETEXT".to_owned();
        captured.primary_format_identity = Some(("mime".to_owned(), "CF_UNICODETEXT".to_owned()));
        let history_id = capture
            .store_captured_clipboard(&captured)
            .expect("无法保存捕获内容");
        let service = PasteService::new(database.clone(), store);
        let connection = database.connect().expect("无法连接数据库");
        let item_id: i64 = connection
            .query_row(
                "SELECT item_id FROM clipboard_history WHERE id = ?1",
                [history_id],
                |row| row.get(0),
            )
            .expect("无法读取内容 ID");
        let payloads = service
            .load_payloads(&connection, item_id)
            .expect("无法读取粘贴载荷");
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0].format_key, "CF_UNICODETEXT");
        assert_eq!(payloads[0].bytes, "测试".as_bytes());
    }

    #[test]
    fn paste_payload_loading_preserves_all_restorable_formats() {
        let directory = tempfile::tempdir().expect("无法创建测试临时目录");
        let database =
            Database::open(directory.path().join("test.sqlite3")).expect("无法创建数据库");
        let store = RawBlobStore::new(directory.path()).expect("无法创建文件存储");
        let capture = CaptureService::new(database.clone(), store.clone());
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
            "<b>测试</b>".as_bytes().to_vec(),
            1,
            true,
        );
        let history_id = capture
            .store_captured_clipboard(&CapturedClipboard {
                primary_format_identity: Some((
                    text.format_namespace.clone(),
                    text.format_key.clone(),
                )),
                formats: vec![text, html],
                preview_candidate: None,
                source_application: SourceApplication::default(),
            })
            .expect("无法保存捕获内容");
        let connection = database.connect().expect("无法连接数据库");
        let item_id: i64 = connection
            .query_row(
                "SELECT item_id FROM clipboard_history WHERE id = ?1",
                [history_id],
                |row| row.get(0),
            )
            .expect("无法读取内容 ID");

        let payloads = PasteService::new(database, store)
            .load_payloads(&connection, item_id)
            .expect("无法读取多格式粘贴载荷");
        assert_eq!(
            payloads
                .iter()
                .map(|payload| payload.format_key.as_str())
                .collect::<Vec<_>>(),
            ["CF_UNICODETEXT", "registered:HTML Format",]
        );
    }

    #[test]
    fn default_payloads_prefer_custom_text_but_explicit_loading_keeps_original_formats() {
        let directory = tempfile::tempdir().expect("无法创建测试临时目录");
        let database =
            Database::open(directory.path().join("test.sqlite3")).expect("无法创建数据库");
        let store = RawBlobStore::new(directory.path()).expect("无法创建文件存储");
        let capture = CaptureService::new(database.clone(), store.clone());
        let text = RawFormat::captured(
            "windows",
            "CF_UNICODETEXT",
            "CF_UNICODETEXT",
            FormatKind::Text,
            "原始文本"
                .encode_utf16()
                .chain([0])
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
            "<b>原始文本</b>".as_bytes().to_vec(),
            1,
            true,
        );
        let history_id = capture
            .store_captured_clipboard(&CapturedClipboard {
                primary_format_identity: Some((
                    text.format_namespace.clone(),
                    text.format_key.clone(),
                )),
                formats: vec![text, html],
                preview_candidate: None,
                source_application: SourceApplication::default(),
            })
            .expect("无法保存捕获内容");
        let item_id: i64 = database
            .connect()
            .expect("无法连接数据库")
            .query_row(
                "SELECT item_id FROM clipboard_history WHERE id = ?1",
                [history_id],
                |row| row.get(0),
            )
            .expect("无法读取内容 ID");
        CustomTextService::new(database.clone(), store.clone())
            .save_for_item(item_id, "修改后的文本")
            .expect("无法保存自定义文本");

        let service = PasteService::new(database.clone(), store);
        let connection = database.connect().expect("无法连接数据库");
        let default_payloads = service
            .load_default_payloads(&connection, item_id)
            .expect("无法读取默认粘贴载荷");
        assert_eq!(default_payloads.len(), 1);
        assert_eq!(default_payloads[0].format_namespace, "qcclip");
        assert_eq!(default_payloads[0].format_key, "custom-text");
        assert_eq!(
            service
                .item_plain_text(item_id)
                .expect("无法读取自定义纯文本")
                .as_deref(),
            Some("修改后的文本")
        );

        let all_payloads = service
            .load_payloads(&connection, item_id)
            .expect("无法读取全部格式");
        assert_eq!(
            all_payloads
                .iter()
                .map(|payload| payload.format_key.as_str())
                .collect::<Vec<_>>(),
            ["CF_UNICODETEXT", "registered:HTML Format", "custom-text"]
        );
    }

    #[test]
    fn restore_payload_filters_internal_formats_and_preserves_semantic_formats() {
        let directory = tempfile::tempdir().expect("无法创建测试临时目录");
        let database =
            Database::open(directory.path().join("test.sqlite3")).expect("无法创建测试数据库");
        let store = RawBlobStore::new(directory.path()).expect("无法创建文件存储");
        let capture = CaptureService::new(database.clone(), store.clone());
        let text = RawFormat::captured(
            "windows",
            "CF_UNICODETEXT",
            "CF_UNICODETEXT",
            FormatKind::Text,
            "测试".encode_utf16().flat_map(u16::to_le_bytes).collect(),
            0,
            true,
        );
        let locale = RawFormat::captured(
            "windows",
            "CF_LOCALE",
            "CF_LOCALE",
            FormatKind::Unknown,
            16_u32.to_le_bytes().to_vec(),
            1,
            true,
        );
        let chromium = RawFormat::captured(
            "windows",
            "registered:Chromium internal source URL",
            "Chromium internal source URL",
            FormatKind::Custom,
            b"https://example.com".to_vec(),
            2,
            true,
        );
        let history_id = capture
            .store_captured_clipboard(&CapturedClipboard {
                primary_format_identity: Some((
                    text.format_namespace.clone(),
                    text.format_key.clone(),
                )),
                formats: vec![text, locale, chromium],
                preview_candidate: None,
                source_application: SourceApplication::default(),
            })
            .expect("无法保存带内部格式的捕获内容");
        let connection = database.connect().expect("无法连接数据库");
        let item_id: i64 = connection
            .query_row(
                "SELECT item_id FROM clipboard_history WHERE id = ?1",
                [history_id],
                |row| row.get(0),
            )
            .expect("无法读取内容 ID");

        let payloads = PasteService::new(database, store)
            .load_payloads(&connection, item_id)
            .expect("无法读取过滤后的粘贴载荷");
        assert_eq!(
            payloads
                .iter()
                .map(|payload| payload.format_key.as_str())
                .collect::<Vec<_>>(),
            ["CF_UNICODETEXT"]
        );
    }

    #[test]
    fn default_paste_payload_omits_image_html_when_real_image_exists() {
        let directory = tempfile::tempdir().expect("无法创建测试临时目录");
        let database =
            Database::open(directory.path().join("test.sqlite3")).expect("无法创建测试数据库");
        let store = RawBlobStore::new(directory.path()).expect("无法创建文件存储");
        let capture = CaptureService::new(database.clone(), store.clone());
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
        let history_id = capture
            .store_captured_clipboard(&CapturedClipboard {
                primary_format_identity: Some((
                    html.format_namespace.clone(),
                    html.format_key.clone(),
                )),
                formats: vec![html, image, empty_text],
                preview_candidate: None,
                source_application: SourceApplication::default(),
            })
            .expect("无法保存图片型 HTML 捕获内容");
        let connection = database.connect().expect("无法连接数据库");
        let item_id: i64 = connection
            .query_row(
                "SELECT item_id FROM clipboard_history WHERE id = ?1",
                [history_id],
                |row| row.get(0),
            )
            .expect("无法读取内容 ID");

        let payloads = PasteService::new(database, store)
            .load_payloads(&connection, item_id)
            .expect("无法读取图片粘贴载荷");
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0].format_key, "registered:PNG");
    }

    #[test]
    fn paste_counts_are_synchronized_between_history_and_favorites() {
        let directory = tempfile::tempdir().expect("无法创建临时目录");
        let database =
            Database::open(directory.path().join("test.sqlite3")).expect("无法创建数据库");
        let store = RawBlobStore::new(directory.path()).expect("无法创建文件存储");
        let capture = CaptureService::new(database.clone(), store.clone());
        let history_id = capture
            .store_captured_clipboard(&MockClipboardSource::text("测试").capture())
            .expect("无法保存捕获内容");
        let connection = database.connect().expect("无法连接数据库");
        let item_id: i64 = connection
            .query_row(
                "SELECT item_id FROM clipboard_history WHERE id = ?1",
                [history_id],
                |row| row.get(0),
            )
            .expect("无法读取内容 ID");
        connection
            .execute(
                "INSERT INTO favorites (uuid, item_id, is_pinned, sort_order, added_at, updated_at)
                 VALUES ('favorite-test', ?1, 0, 0, 1, 1)",
                [item_id],
            )
            .expect("无法创建测试收藏");
        let service = PasteService::new(database, store);
        let event = service
            .update_paste_counts(&connection, item_id, None)
            .expect("无法更新粘贴计数")
            .expect("可写数据库应返回粘贴变更");
        let favorite_id: i64 = connection
            .query_row(
                "SELECT id FROM favorites WHERE item_id = ?1",
                [item_id],
                |row| row.get(0),
            )
            .expect("无法读取收藏 ID");
        assert_eq!(
            event,
            PastedItem {
                item_id,
                history: Some((history_id, 1)),
                favorite: Some((favorite_id, 1)),
                moved_to_first: None,
            }
        );
        let history_count: i64 = connection
            .query_row(
                "SELECT paste_count FROM clipboard_history WHERE item_id = ?1",
                [item_id],
                |row| row.get(0),
            )
            .expect("无法读取历史粘贴计数");
        let favorite_count: i64 = connection
            .query_row(
                "SELECT paste_count FROM favorites WHERE item_id = ?1",
                [item_id],
                |row| row.get(0),
            )
            .expect("无法读取收藏粘贴计数");
        assert_eq!(history_count, 1);
        assert_eq!(favorite_count, 1);
    }

    #[test]
    fn pasted_item_reports_only_actual_moves_to_first() {
        let directory = tempfile::tempdir().expect("无法创建临时目录");
        let database =
            Database::open(directory.path().join("test.sqlite3")).expect("无法创建数据库");
        let store = RawBlobStore::new(directory.path()).expect("无法创建文件存储");
        let capture = CaptureService::new(database.clone(), store.clone());
        let older_history_id = capture
            .store_captured_clipboard(&MockClipboardSource::text("较早").capture())
            .expect("无法保存较早内容");
        capture
            .store_captured_clipboard(&MockClipboardSource::text("较新").capture())
            .expect("无法保存较新内容");
        let connection = database.connect().expect("无法连接数据库");
        let older_item_id: i64 = connection
            .query_row(
                "SELECT item_id FROM clipboard_history WHERE id = ?1",
                [older_history_id],
                |row| row.get(0),
            )
            .expect("无法读取内容 ID");
        let service = PasteService::new(database, store);

        let moved = service
            .update_paste_counts(
                &connection,
                older_item_id,
                Some(PasteOrderTarget::Clipboard),
            )
            .expect("无法更新粘贴计数")
            .expect("可写数据库应返回粘贴变更");
        assert_eq!(moved.history, Some((older_history_id, 1)));
        assert_eq!(moved.favorite, None);
        assert_eq!(moved.moved_to_first, Some(PasteOrderTarget::Clipboard));

        let already_first = service
            .update_paste_counts(
                &connection,
                older_item_id,
                Some(PasteOrderTarget::Clipboard),
            )
            .expect("无法更新粘贴计数")
            .expect("可写数据库应返回粘贴变更");
        assert_eq!(already_first.history, Some((older_history_id, 2)));
        assert_eq!(already_first.moved_to_first, None);
    }
}
