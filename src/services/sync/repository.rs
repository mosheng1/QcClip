use std::{collections::HashMap, fs, io::Read};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use uuid::Uuid;

use super::{
    Result,
    config::{CHUNK_BYTES, MAX_EVENT_BYTES, Policy},
    fail,
    protocol::{self, Event, Mode, Resource, WireFile, WireFormat, WireTag},
};
use crate::{
    domain::capture::{
        CapturedClipboard, FormatKind, FormatRole, RawFormat, SourceApplication,
        classify_format_role,
    },
    services::{
        capture::{CaptureService, IngestContext, hash_clipboard, touch_remote_history},
        database::{Database, LIST_ORDER_GAP},
        storage::{RawBlob, RawBlobStore},
    },
};

pub struct Packet {
    pub event: Event,
    pub resources: HashMap<String, Vec<u8>>,
    pub skipped: Vec<String>,
}

const MANAGED_FILES_KEY: &str = "sync-managed-files";

#[derive(serde::Serialize, serde::Deserialize)]
struct ManagedFile {
    relative: String,
    hash: [u8; 32],
    length: usize,
}

pub(crate) fn managed_file_list(
    connection: &Connection,
    blobs: &RawBlobStore,
    root: &std::path::Path,
    item_id: i64,
) -> Result<Option<Vec<std::path::PathBuf>>> {
    let metadata = connection.query_row("SELECT f.inline_data, b.sha256, b.byte_length, b.relative_path FROM clipboard_formats f
        LEFT JOIN binary_blobs b ON b.id = f.blob_id WHERE f.item_id = ?1 AND f.format_namespace = 'qcclip' AND f.format_key = ?2",
        params![item_id, MANAGED_FILES_KEY], |r| Ok((r.get::<_, Option<Vec<u8>>>(0)?, r.get::<_, Option<Vec<u8>>>(1)?,
            r.get::<_, Option<i64>>(2)?, r.get::<_, Option<String>>(3)?))).optional()?;
    let Some((inline, hash, length, path)) = metadata else {
        return Ok(None);
    };
    let bytes = if let Some(bytes) = inline {
        bytes
    } else {
        let hash = hash
            .and_then(|h| h.try_into().ok())
            .ok_or_else(|| super::Error::Message("受管理文件清单哈希无效".into()))?;
        let length = length
            .and_then(|n| u64::try_from(n).ok())
            .filter(|n| *n <= 256 * 1024)
            .ok_or_else(|| super::Error::Message("受管理文件清单长度无效".into()))?;
        blobs.read(&RawBlob::from_record(
            hash,
            length,
            path.ok_or_else(|| super::Error::Message("受管理文件清单缺失".into()))?,
        ))?
    };
    let files: Vec<ManagedFile> = serde_json::from_slice(&bytes)?;
    if files.is_empty() || files.len() > 256 {
        return fail("受管理文件数量无效");
    }
    let boundary = dunce::canonicalize(root.join("data/sync-files"))?;
    let mut paths = Vec::new();
    for file in files {
        let relative = std::path::Path::new(&file.relative);
        if !file.relative.starts_with("data/sync-files/")
            || relative
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return fail("受管理文件路径越界");
        }
        let path = dunce::canonicalize(root.join(relative))?;
        if !path.starts_with(&boundary)
            || !path.is_file()
            || fs::metadata(&path)?.len() != file.length as u64
        {
            return fail("受管理文件缺失、大小改变或路径越界");
        }
        let mut input = fs::File::open(&path)?;
        let mut bytes = Vec::new();
        (&mut input)
            .take(file.length as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() != file.length || protocol::hash(&bytes) != file.hash {
            return fail("受管理文件内容已改变");
        }
        paths.push(path);
    }
    Ok(Some(paths))
}

pub fn load_formats(
    connection: &Connection,
    blobs: &RawBlobStore,
    item_id: i64,
) -> Result<CapturedClipboard> {
    let raw_size: i64 = connection.query_row(
        "SELECT COALESCE(SUM(COALESCE(length(f.inline_data), b.byte_length, 0)), 0)
        FROM clipboard_formats f LEFT JOIN binary_blobs b ON b.id = f.blob_id
        WHERE f.item_id = ?1 AND f.capture_state IN ('captured', 'generated')",
        [item_id],
        |r| r.get(0),
    )?;
    if raw_size < 0 || raw_size as u64 > MAX_EVENT_BYTES as u64 {
        return fail("原始格式快照超过事件上限，已停止读取");
    }
    let mut statement = connection.prepare("SELECT f.format_namespace, f.format_key, f.format_name, f.format_kind,
        f.mime_type, f.encoding, f.is_primary, f.inline_data, b.sha256, b.byte_length, b.relative_path
        FROM clipboard_formats f LEFT JOIN binary_blobs b ON b.id = f.blob_id
        WHERE f.item_id = ?1 AND f.capture_state IN ('captured', 'generated') ORDER BY f.format_order")?;
    let rows = statement.query_map([item_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, Option<String>>(5)?,
            r.get::<_, bool>(6)?,
            r.get::<_, Option<Vec<u8>>>(7)?,
            r.get::<_, Option<Vec<u8>>>(8)?,
            r.get::<_, Option<i64>>(9)?,
            r.get::<_, Option<String>>(10)?,
        ))
    })?;
    let mut captured = CapturedClipboard {
        formats: Vec::new(),
        primary_format_identity: None,
        preview_candidate: None,
        source_application: SourceApplication::default(),
    };
    for row in rows {
        let (namespace, key, name, kind, mime, encoding, primary, inline, hash, length, path) =
            row?;
        let bytes = if let Some(bytes) = inline {
            bytes
        } else {
            let hash: [u8; 32] = hash
                .and_then(|h| h.try_into().ok())
                .ok_or_else(|| super::Error::Message("原始资源哈希无效".into()))?;
            let length = length
                .and_then(|l| u64::try_from(l).ok())
                .ok_or_else(|| super::Error::Message("原始资源长度无效".into()))?;
            blobs.read(&RawBlob::from_record(
                hash,
                length,
                path.ok_or_else(|| super::Error::Message("原始资源缺失".into()))?,
            ))?
        };
        let mut format = RawFormat::captured(
            namespace,
            key,
            name,
            FormatKind::from_str(&kind).unwrap_or(FormatKind::Unknown),
            bytes,
            captured.formats.len() as u32,
            false,
        );
        format.mime_type = mime;
        format.encoding = encoding;
        crate::platform::clipboard::assess_remote_format(&mut format);
        if primary {
            captured.primary_format_identity =
                Some((format.format_namespace.clone(), format.format_key.clone()));
        }
        captured.formats.push(format);
    }
    Ok(captured)
}

pub fn allowed(format: &RawFormat, policy: &Policy) -> bool {
    if format.format_namespace == "qcclip" && format.format_key == MANAGED_FILES_KEY {
        return false;
    }
    if !format.capture_state.has_data()
        || format.bytes.is_none()
        || policy
            .excluded_formats
            .iter()
            .any(|s| s == &format!("{}/{}", format.format_namespace, format.format_key))
    {
        return false;
    }
    let role = classify_format_role(format.format_kind, &format.format_key, &format.format_name);
    match role {
        FormatRole::PlainText => policy.text,
        FormatRole::Html | FormatRole::Rtf => policy.rich_text,
        FormatRole::Image => policy.images,
        FormatRole::FileList => policy.file_paths || policy.file_contents,
        FormatRole::Custom => policy.custom && !policy.common_only,
        FormatRole::VirtualFile | FormatRole::Internal => false,
    }
}

fn add_resource(resources: &mut HashMap<String, Vec<u8>>, bytes: Vec<u8>) -> Resource {
    let resource = Resource {
        id: Uuid::new_v4().to_string(),
        length: bytes.len(),
        hash: protocol::hash(&bytes),
        chunks: bytes.len().div_ceil(CHUNK_BYTES).max(1),
    };
    resources.insert(resource.id.clone(), bytes);
    resource
}

pub fn build(
    database: &Database,
    blobs: &RawBlobStore,
    item_id: i64,
    mut event: Event,
    policy: &Policy,
) -> Result<Packet> {
    let connection = database.connect()?;
    let (uuid, version, hash): (String, i64, Vec<u8>) = connection.query_row(
        "SELECT uuid, hash_version, content_hash FROM clipboard_items WHERE id = ?1 AND capture_state = 'ready'", [item_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    event.item_uuid = uuid;
    event.hash_version = version;
    event.source_hash = hash
        .try_into()
        .map_err(|_| super::Error::Message("内容哈希无效".into()))?;
    event.source_app_name = connection
        .query_row(
            "SELECT CASE WHEN COALESCE(h.capture_count, 0) > 0 THEN h.last_source_app_name
                     ELSE COALESCE(s.app_name, h.last_source_app_name) END
         FROM clipboard_items AS i
         LEFT JOIN clipboard_history AS h ON h.item_id = i.id
         LEFT JOIN sync_sources AS s ON s.item_id = i.id WHERE i.id = ?1",
            [item_id],
            |row| row.get::<_, Option<String>>(0),
        )?
        .map(|name| name.chars().filter(|c| !c.is_control()).collect::<String>())
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty() && name.len() <= 512);
    let captured = if event.mode == Mode::Touch {
        CapturedClipboard {
            formats: Vec::new(),
            primary_format_identity: None,
            preview_candidate: None,
            source_application: SourceApplication::default(),
        }
    } else {
        load_formats(&connection, blobs, item_id)?
    };
    event.primary = captured.primary_format_identity.clone();
    let mut resources = HashMap::new();
    let mut skipped = Vec::new();
    let mut total = 0usize;
    if event.mode != Mode::Touch {
        for format in &captured.formats {
            let role =
                classify_format_role(format.format_kind, &format.format_key, &format.format_name);
            if !allowed(format, policy) {
                if !role.is_internal() {
                    skipped.push(format!("{}：未启用或暂不支持", format.format_name));
                }
                continue;
            }
            let bytes = format
                .bytes
                .as_ref()
                .ok_or_else(|| super::Error::Message("原始格式数据缺失".into()))?;
            if role == FormatRole::FileList && policy.file_contents {
                let root = database
                    .path()
                    .parent()
                    .ok_or_else(|| super::Error::Message("数据目录无效".into()))?;
                let paths = managed_file_list(&connection, blobs, root, item_id)?
                    .unwrap_or_else(|| crate::services::image_preview::parse_file_list(bytes));
                if paths.is_empty() {
                    return fail("文件列表为空，无法发送文件内容");
                }
                let mut file_total = 0usize;
                for path in paths {
                    let metadata = fs::symlink_metadata(&path)?;
                    if !metadata.is_file() || metadata.file_type().is_symlink() {
                        return fail("文件列表包含目录或链接，当前只能发送普通文件");
                    }
                    if metadata.len() > policy.file_limit_mb as u64 * 1024 * 1024 {
                        return fail("单个文件超过目标设备限制");
                    }
                    let mut file = fs::File::open(&path)?;
                    let before = file.metadata()?;
                    let mut data = Vec::new();
                    (&mut file)
                        .take(policy.file_limit_mb as u64 * 1024 * 1024 + 1)
                        .read_to_end(&mut data)?;
                    let after = file.metadata()?;
                    if data.len() as u64 != before.len()
                        || before.len() != after.len()
                        || before.modified()? != after.modified()?
                    {
                        return fail("读取期间文件发生变化，请重新发送");
                    }
                    file_total += data.len();
                    total += data.len();
                    if file_total > policy.files_limit_mb as usize * 1024 * 1024
                        || total > MAX_EVENT_BYTES
                    {
                        return fail("条目文件总量超过限制");
                    }
                    let name = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .ok_or_else(|| super::Error::Message("文件名称无效".into()))?
                        .to_owned();
                    event.files.push(WireFile {
                        name,
                        resource: add_resource(&mut resources, data),
                    });
                }
                continue;
            }
            if role == FormatRole::Image
                && bytes.len() > policy.image_limit_mb as usize * 1024 * 1024
            {
                return fail("图片超过目标设备限制，条目未发送");
            }
            total += bytes.len();
            if total > MAX_EVENT_BYTES {
                return fail("同步条目超过事件上限");
            }
            if role == FormatRole::FileList {
                skipped.push("仅发送来源设备路径引用，不包含文件内容".into());
            }
            event.formats.push(WireFormat {
                namespace: format.format_namespace.clone(),
                key: format.format_key.clone(),
                name: format.format_name.clone(),
                kind: format.format_kind.as_str().into(),
                mime: format.mime_type.clone(),
                encoding: format.encoding.clone(),
                resource: add_resource(&mut resources, bytes.clone()),
            });
        }
        if event.formats.is_empty() && event.files.is_empty() {
            return fail("没有符合目标格式策略的可发送内容");
        }
        event.complete = event.files.is_empty() && event.formats.len() == captured.formats.len();
    }
    if event.mode != Mode::Touch {
        let mut statement = connection.prepare("SELECT t.uuid, t.name, t.color FROM tags t JOIN clipboard_item_tags r ON r.tag_id = t.id WHERE r.item_id = ?1")?;
        event.tags = statement
            .query_map([item_id], |r| {
                Ok(WireTag {
                    uuid: r.get(0)?,
                    name: r.get(1)?,
                    color: r.get(2)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        event.note = connection
            .query_row(
                "SELECT note_text FROM clipboard_item_notes WHERE item_id = ?1",
                [item_id],
                |r| r.get(0),
            )
            .optional()?;
    }
    Ok(Packet {
        event,
        resources,
        skipped,
    })
}

pub(super) fn received(database: &Database, event: &Event) -> Result<bool> {
    let connection = database.connect()?;
    Ok(connection.query_row("SELECT EXISTS(SELECT 1 FROM sync_watermarks WHERE device_id = ?1 AND item_uuid = ?2 AND stream = ?3 AND sequence >= ?4)",
        params![event.sender, event.item_uuid, event.mode.stream(), event.sequence as i64], |r| r.get(0))?)
}

#[derive(Default)]
pub struct ApplyOutcome {
    pub applied: bool,
    pub note_conflict: bool,
    pub change: Option<ReceivedChange>,
    pub clipboard_item: Option<i64>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ReceivedChange {
    pub item_id: i64,
    pub history_id: Option<i64>,
    pub favorite_id: Option<i64>,
    pub tags_changed: bool,
}

impl ReceivedChange {
    pub fn merge(&mut self, newer: Self) {
        self.history_id = newer.history_id;
        self.favorite_id = newer.favorite_id;
        self.tags_changed |= newer.tags_changed;
    }
}

#[cfg(test)]
pub fn apply(
    database: &Database,
    blobs: &RawBlobStore,
    event: &Event,
    resources: &HashMap<String, Vec<u8>>,
) -> Result<bool> {
    Ok(apply_with_feedback(database, blobs, event, resources, "测试设备")?.applied)
}

pub fn apply_with_feedback(
    database: &Database,
    blobs: &RawBlobStore,
    event: &Event,
    resources: &HashMap<String, Vec<u8>>,
    sender_name: &str,
) -> Result<ApplyOutcome> {
    let mut connection = database.connect_rewrite()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let timestamp = protocol::now() as i64 * 1000;
    let duplicate: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM sync_receipts WHERE device_id = ?1 AND event_id = ?2)",
        params![event.sender, event.id],
        |r| r.get(0),
    )?;
    let watermark: i64 = transaction.query_row("SELECT sequence FROM sync_watermarks WHERE device_id = ?1 AND item_uuid = ?2 AND stream = ?3",
        params![event.sender, event.item_uuid, event.mode.stream()], |r| r.get(0)).optional()?.unwrap_or(0);
    if duplicate || event.sequence <= watermark as u64 {
        return Ok(ApplyOutcome::default());
    }
    let mapping: Option<(Option<i64>, Vec<u8>, Vec<u8>)> = transaction.query_row(
        "SELECT item_id, source_hash, local_hash FROM sync_items WHERE device_id = ?1 AND item_uuid = ?2", params![event.sender, event.item_uuid],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?;
    let mut target = mapping.as_ref().and_then(|m| m.0);
    if target.is_none() && (event.complete || event.mode == Mode::Touch) {
        target = transaction.query_row("SELECT i.id FROM clipboard_items i WHERE hash_version = ?1 AND content_hash = ?2 AND capture_state = 'ready'",
            params![event.hash_version, event.source_hash.as_slice()], |r| r.get(0)).optional()?;
    }
    let newest_content: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(sequence), 0) FROM sync_watermarks
         WHERE device_id = ?1 AND item_uuid = ?2 AND stream IN ('content', 'favorite')",
        params![event.sender, event.item_uuid],
        |row| row.get(0),
    )?;
    // 两个列表的关系分别接收，迟到的收藏或历史不能还原较新的内容。
    let update_content = event.mode.manual() && event.sequence >= newest_content as u64;
    let mut staged_files = None;
    let item_id = if event.mode == Mode::Touch {
        if let Some(item_id) = target {
            touch_remote_history(&transaction, item_id, timestamp, false)?;
        }
        target
    } else if let Some(item_id) = target.filter(|_| !update_content) {
        if event.mode.history() {
            touch_remote_history(&transaction, item_id, timestamp, true)?;
        }
        Some(item_id)
    } else {
        let mut captured = CapturedClipboard {
            formats: Vec::new(),
            primary_format_identity: event.primary.clone(),
            preview_candidate: None,
            source_application: SourceApplication::default(),
        };
        for f in &event.formats {
            if f.namespace == "qcclip" && f.key == MANAGED_FILES_KEY {
                return fail("远程消息不能指定本机受管理文件清单");
            }
            let data = verified_resource(&f.resource, resources)?;
            let mut format = RawFormat::captured(
                &f.namespace,
                &f.key,
                &f.name,
                FormatKind::from_str(&f.kind).unwrap_or(FormatKind::Unknown),
                data,
                captured.formats.len() as u32,
                false,
            );
            format.mime_type = f.mime.clone();
            format.encoding = f.encoding.clone();
            crate::platform::clipboard::assess_remote_format(&mut format);
            let role =
                classify_format_role(format.format_kind, &format.format_key, &format.format_name);
            if matches!(role, FormatRole::Internal | FormatRole::VirtualFile) {
                return fail("远程消息包含内部格式或不完整虚拟文件");
            }
            if role == FormatRole::Image {
                crate::services::image_preview::validate_remote_image(
                    format.bytes.as_deref().unwrap_or_default(),
                    &format.format_key,
                )
                .map_err(|_| super::Error::Message("远程图片损坏或解码内存超过限制".into()))?;
            }
            captured.formats.push(format);
        }
        if !event.files.is_empty() {
            let root = database
                .path()
                .parent()
                .ok_or_else(|| super::Error::Message("数据目录无效".into()))?;
            let parent = root.join("data/sync-files");
            fs::create_dir_all(&parent)?;
            let dir = tempfile::Builder::new()
                .prefix("received-")
                .tempdir_in(parent)?;
            let mut paths = Vec::new();
            let mut managed = Vec::new();
            for (index, f) in event.files.iter().enumerate() {
                let name = safe_file_name(&f.name)?;
                let subdir = dir.path().join(index.to_string());
                fs::create_dir(&subdir)?;
                let path = subdir.join(name);
                fs::write(&path, verified_resource(&f.resource, resources)?)?;
                managed.push(ManagedFile {
                    relative: path
                        .strip_prefix(root)
                        .map_err(|_| super::Error::Message("受管理文件路径越界".into()))?
                        .to_string_lossy()
                        .replace('\\', "/"),
                    hash: f.resource.hash,
                    length: f.resource.length,
                });
                paths.push(path);
            }
            let mut format = RawFormat::captured(
                "windows",
                "CF_HDROP",
                "CF_HDROP",
                FormatKind::FileList,
                crate::platform::image_temp_file::hdrop_bytes_for_paths(&paths),
                captured.formats.len() as u32,
                false,
            );
            crate::platform::clipboard::assess_remote_format(&mut format);
            captured
                .formats
                .retain(|f| !(f.format_namespace == "windows" && f.format_key == "CF_HDROP"));
            captured.formats.push(format);
            captured.formats.push(RawFormat::captured(
                "qcclip",
                MANAGED_FILES_KEY,
                "同步文件清单",
                FormatKind::Custom,
                serde_json::to_vec(&managed)?,
                captured.formats.len() as u32,
                false,
            ));
            staged_files = Some(dir);
        }
        let incoming_hash = hash_clipboard(&captured)?;
        if event.complete && incoming_hash.content_hash != event.source_hash {
            return fail("完整内容哈希校验失败");
        }
        if let Some(item_id) = target {
            let mut stored = load_formats(&transaction, blobs, item_id)?;
            let local_hash = hash_clipboard(&stored)?.content_hash;
            let recorded_hash: Vec<u8> = transaction.query_row(
                "SELECT content_hash FROM clipboard_items WHERE id = ?1",
                [item_id],
                |r| r.get(0),
            )?;
            if recorded_hash.as_slice() != local_hash
                || mapping
                    .as_ref()
                    .is_some_and(|m| m.2.as_slice() != local_hash)
            {
                return fail("目标条目存在本地修改，请在接收设备处理冲突后重试");
            }
            stored.formats.retain(|old| {
                !captured
                    .formats
                    .iter()
                    .any(|f| f.stable_identity() == old.stable_identity())
            });
            captured.formats.extend(stored.formats);
        }
        for (index, f) in captured.formats.iter_mut().enumerate() {
            f.format_order = index as u32;
        }
        let result = CaptureService::new(database.clone(), blobs.clone()).store_in_transaction(
            &transaction,
            &captured,
            IngestContext::Remote {
                replace_item: target,
                history: event.mode.history(),
            },
        )?;
        Some(result.item_id)
    };
    let mut note_conflict = false;
    if let Some(item_id) = item_id {
        if event.mode.favorite() {
            apply_favorite(&transaction, item_id, timestamp)?;
        }
        if event.mode != Mode::Touch {
            note_conflict = apply_metadata(&transaction, item_id, event, timestamp)?;
            if event.sequence >= newest_content as u64 {
                // 设备名称取自已认证的配对资料，来源独立于历史和收藏关系。
                transaction.execute(
                    "INSERT INTO sync_sources (item_id, device_id, device_name, app_name)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(item_id) DO UPDATE SET device_id = excluded.device_id,
                         device_name = excluded.device_name, app_name = excluded.app_name",
                    params![item_id, event.sender, sender_name, event.source_app_name],
                )?;
            }
        }
        let local_hash: Vec<u8> = transaction.query_row(
            "SELECT content_hash FROM clipboard_items WHERE id = ?1",
            [item_id],
            |r| r.get(0),
        )?;
        transaction.execute("INSERT INTO sync_items (device_id, item_uuid, item_id, source_hash, local_hash) VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(device_id, item_uuid) DO UPDATE SET item_id = excluded.item_id,
            source_hash = CASE WHEN ?6 OR sync_items.item_id IS NULL THEN excluded.source_hash ELSE sync_items.source_hash END,
            local_hash = CASE WHEN ?6 OR sync_items.item_id IS NULL THEN excluded.local_hash ELSE sync_items.local_hash END",
            params![event.sender, event.item_uuid, item_id, event.source_hash.as_slice(), local_hash,
                update_content])?;
    }
    transaction.execute("INSERT INTO sync_watermarks VALUES (?1, ?2, ?3, ?4) ON CONFLICT(device_id, item_uuid, stream) DO UPDATE SET sequence = excluded.sequence",
        params![event.sender, event.item_uuid, event.mode.stream(), event.sequence as i64])?;
    transaction.execute(
        "DELETE FROM sync_receipts WHERE expires < ?1",
        [protocol::now() as i64],
    )?;
    transaction.execute(
        "INSERT INTO sync_receipts VALUES (?1, ?2, ?3)",
        params![
            event.sender,
            event.id,
            event
                .expires
                .max(protocol::now() + super::config::EVENT_TTL) as i64
        ],
    )?;
    let change = item_id
        .map(|item_id| -> Result<ReceivedChange> {
            Ok(ReceivedChange {
                item_id,
                history_id: transaction
                    .query_row(
                        "SELECT id FROM clipboard_history WHERE item_id = ?1",
                        [item_id],
                        |row| row.get(0),
                    )
                    .optional()?,
                favorite_id: transaction
                    .query_row(
                        "SELECT id FROM favorites WHERE item_id = ?1",
                        [item_id],
                        |row| row.get(0),
                    )
                    .optional()?,
                tags_changed: !event.tags.is_empty(),
            })
        })
        .transpose()?;
    transaction.commit()?;
    if let Some(dir) = staged_files {
        let _ = dir.keep();
    }
    Ok(ApplyOutcome {
        applied: item_id.is_some(),
        note_conflict,
        change,
        clipboard_item: item_id.filter(|_| event.sequence >= newest_content as u64),
    })
}

fn verified_resource(resource: &Resource, resources: &HashMap<String, Vec<u8>>) -> Result<Vec<u8>> {
    let bytes = resources
        .get(&resource.id)
        .ok_or_else(|| super::Error::Message("同步资源尚未完整到达".into()))?;
    if bytes.len() != resource.length || protocol::hash(bytes) != resource.hash {
        return fail("资源长度或哈希校验失败");
    }
    Ok(bytes.clone())
}

fn safe_file_name(name: &str) -> Result<&str> {
    if name.is_empty()
        || name.len() > 240
        || name.contains(['/', '\\', ':', '\0'])
        || name.chars().any(char::is_control)
        || name.ends_with(['.', ' '])
        || matches!(name, "." | "..")
    {
        return fail("远程文件名称不安全");
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit())
    {
        return fail("远程文件名称是系统保留名称");
    }
    Ok(name)
}

fn apply_favorite(tx: &Transaction<'_>, item_id: i64, timestamp: i64) -> Result<()> {
    tx.execute("INSERT INTO favorites (uuid, item_id, is_pinned, sort_order, added_at, updated_at)
        SELECT ?1, ?2, 0, COALESCE(MAX(sort_order), -1) + ?3, ?4, ?4 FROM favorites WHERE is_pinned = 0
        ON CONFLICT(item_id) DO NOTHING", params![Uuid::new_v4().to_string(), item_id, LIST_ORDER_GAP, timestamp])?;
    Ok(())
}

fn apply_metadata(
    tx: &Transaction<'_>,
    item_id: i64,
    event: &Event,
    timestamp: i64,
) -> Result<bool> {
    for tag in &event.tags {
        let normalized = tag.name.trim().to_lowercase();
        if normalized.is_empty()
            || normalized.chars().count() > 100
            || Uuid::parse_str(&tag.uuid).is_err()
            || tag.color.len() != 7
            || !tag.color.starts_with('#')
            || !tag.color[1..].bytes().all(|b| b.is_ascii_hexdigit())
        {
            return fail("同步标签数据无效");
        }
        let existing: Option<(i64, String)> = tx.query_row("SELECT id, uuid FROM tags WHERE uuid = ?1 OR normalized_name = ?2 ORDER BY (uuid = ?1) DESC LIMIT 1",
            params![tag.uuid, normalized], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
        let id = if let Some((id, _)) = existing {
            id
        } else {
            tx.execute("INSERT INTO tags (uuid, name, normalized_name, color, sort_order, created_at, updated_at)
                SELECT ?1, ?2, ?3, ?4, COALESCE(MAX(sort_order), -1) + 1, ?5, ?5 FROM tags",
                params![tag.uuid, tag.name.trim(), normalized, tag.color, timestamp])?;
            tx.last_insert_rowid()
        };
        tx.execute("INSERT INTO clipboard_item_tags (uuid, item_id, tag_id, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)
            ON CONFLICT(item_id, tag_id) DO NOTHING", params![Uuid::new_v4().to_string(), item_id, id, timestamp])?;
    }
    if let Some(note) = event
        .note
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        let existing: Option<String> = tx
            .query_row(
                "SELECT note_text FROM clipboard_item_notes WHERE item_id = ?1",
                [item_id],
                |row| row.get(0),
            )
            .optional()?;
        if existing
            .as_deref()
            .is_some_and(|text| !text.trim().is_empty() && text.trim() != note)
        {
            return Ok(true);
        }
        tx.execute("INSERT INTO clipboard_item_notes (uuid, item_id, note_text, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)
            ON CONFLICT(item_id) DO UPDATE SET note_text = excluded.note_text, updated_at = ?4
            WHERE trim(clipboard_item_notes.note_text) = ''",
            params![Uuid::new_v4().to_string(), item_id, note, timestamp])?;
    }
    Ok(false)
}

pub(super) fn cleanup_managed_files(database: &Database, blobs: &RawBlobStore) -> Result<()> {
    cleanup_managed_files_after(database, blobs, std::time::Duration::from_secs(600))
}

pub(super) fn cleanup_managed_files_after(
    database: &Database,
    blobs: &RawBlobStore,
    minimum_age: std::time::Duration,
) -> Result<()> {
    let root = database
        .path()
        .parent()
        .ok_or_else(|| super::Error::Message("数据目录无效".into()))?
        .join("data/sync-files");
    if !root.exists() {
        return Ok(());
    }
    let boundary = dunce::canonicalize(&root)?;
    // 与接收事务互斥，避免删除即将被数据库引用的文件目录。
    let mut connection = database.connect_rewrite()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut referenced = std::collections::HashSet::new();
    {
        let mut statement = transaction.prepare(
            "SELECT f.inline_data, b.sha256, b.byte_length, b.relative_path
            FROM clipboard_formats f LEFT JOIN binary_blobs b ON b.id = f.blob_id
            WHERE f.format_namespace = 'qcclip' AND f.format_key = ?1",
        )?;
        let rows = statement.query_map([MANAGED_FILES_KEY], |r| {
            Ok((
                r.get::<_, Option<Vec<u8>>>(0)?,
                r.get::<_, Option<Vec<u8>>>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?;
        for row in rows {
            let (inline, hash, length, path) = row?;
            let bytes = if let Some(bytes) = inline {
                bytes
            } else {
                let hash = hash
                    .and_then(|h| h.try_into().ok())
                    .ok_or_else(|| super::Error::Message("托管清单哈希无效".into()))?;
                let length = length
                    .and_then(|l| u64::try_from(l).ok())
                    .filter(|l| *l <= 256 * 1024)
                    .ok_or_else(|| super::Error::Message("托管清单长度无效".into()))?;
                blobs.read(&RawBlob::from_record(
                    hash,
                    length,
                    path.ok_or_else(|| super::Error::Message("托管清单缺失".into()))?,
                ))?
            };
            for file in serde_json::from_slice::<Vec<ManagedFile>>(&bytes)? {
                let parts: Vec<_> = file.relative.split('/').collect();
                if parts.len() < 4
                    || parts[0] != "data"
                    || parts[1] != "sync-files"
                    || !parts[2].starts_with("received-")
                    || parts
                        .iter()
                        .any(|p| p.is_empty() || *p == "." || *p == ".." || p.contains(['\\', ':']))
                {
                    return fail("托管清单路径无效，已取消清理");
                }
                referenced.insert(parts[2].to_owned());
            }
        }
    }
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = entry.file_type()?;
        if !name.starts_with("received-")
            || referenced.contains(&name)
            || !kind.is_dir()
            || kind.is_symlink()
        {
            continue;
        }
        let old = entry
            .metadata()?
            .modified()?
            .elapsed()
            .is_ok_and(|age| age >= minimum_age);
        let resolved = dunce::canonicalize(entry.path())?;
        if old && resolved.parent() == Some(boundary.as_path()) {
            fs::remove_dir_all(resolved)?;
        }
    }
    transaction.commit()?;
    Ok(())
}
