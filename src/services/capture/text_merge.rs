use std::collections::HashSet;

use rusqlite::{OptionalExtension, Transaction, params};

use crate::{
    domain::capture::{
        CapturedClipboard, FormatCaptureState, FormatKind, FormatRole, RawFormat, RestoreState,
        classify_format_role, is_custom_text_format,
    },
    services::{
        database::Database,
        storage::{RawBlob, RawBlobStore},
    },
};

use super::{CaptureError, HashedClipboard, hash_clipboard};

pub(super) const CANDIDATE_QUERY: &str = "SELECT i.id FROM clipboard_items i
             LEFT JOIN clipboard_history h ON h.item_id = i.id
             WHERE i.id IN (
                 SELECT item_id FROM clipboard_formats
                 WHERE format_kind = 'text' AND substr(preview_text, 1, 128) = ?3 AND +preview_text = ?1
                 UNION
                 SELECT item_id FROM clipboard_formats
                 WHERE format_kind = 'text' AND substr(preview_text, 1, 128) IS NULL
             ) AND i.capture_state = 'ready' AND i.origin_kind IN ('captured', 'imported', 'synced')
               AND NOT EXISTS (SELECT 1 FROM clipboard_formats f WHERE f.item_id = i.id
                   AND f.format_namespace = 'qcclip' AND f.format_key = 'custom-text')
               AND NOT EXISTS (SELECT 1 FROM clipboard_items child WHERE child.parent_item_id = i.id)
             ORDER BY (i.id = ?2) DESC, h.is_pinned DESC, h.last_seen_at DESC, i.id DESC";

pub(super) struct TextMerge {
    pub item_id: i64,
    pub captured: Option<CapturedClipboard>,
    pub hashed: Option<HashedClipboard>,
    pub formats_changed: bool,
}

pub(super) fn find_text_merge(
    transaction: &Transaction<'_>,
    incoming: &CapturedClipboard,
    blob_store: &RawBlobStore,
    exact_id: Option<i64>,
    incoming_hash: &HashedClipboard,
) -> Result<Option<TextMerge>, CaptureError> {
    if !Database::maintenance_supported(transaction)? {
        return Ok(None);
    }
    if let Some(item_id) = exact_id
        && stored_formats_unchanged(transaction, incoming, incoming_hash, item_id, true)?
    {
        return Ok(Some(TextMerge {
            item_id,
            captured: None,
            hashed: None,
            formats_changed: false,
        }));
    }
    let Some(text) = full_text(incoming) else {
        return Ok(None);
    };
    let candidates = {
        // 预览仅缩小查询范围，候选必须再用完整原文校验，不能直接据此合并。
        let mut statement = transaction.prepare(CANDIDATE_QUERY)?;
        // 一元加号阻止 SQLite 将完整相等条件传播到 substr 表达式，保证使用前缀索引。
        let prefix: String = text.trim().chars().take(128).collect();
        statement
            .query_map(params![text.trim(), exact_id, prefix], |row| {
                row.get::<_, i64>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let mut visited = HashSet::new();
    for candidate_id in candidates {
        let mut item_id = candidate_id;
        while visited.insert(item_id) {
            let stored = match load_capture(transaction, item_id, blob_store, true) {
                Ok(Some(stored)) => stored,
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!(item_id, %error, "无法读取文本合并候选，保留原条目并跳过");
                    break;
                }
            };
            if full_text(&stored).as_deref() != Some(text.as_str()) {
                break;
            }
            if stored_formats_unchanged(transaction, incoming, incoming_hash, item_id, false)? {
                return Ok(Some(TextMerge {
                    item_id,
                    captured: None,
                    hashed: None,
                    formats_changed: false,
                }));
            }
            let stored = match load_capture(transaction, item_id, blob_store, false) {
                Ok(Some(stored)) => stored,
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!(item_id, %error, "无法读取待合并格式，保留原条目并跳过");
                    break;
                }
            };
            let merged = merge_formats(incoming, &stored);
            let hashed = hash_clipboard(&merged)?;
            let owner: Option<i64> = transaction
                .query_row(
                    "SELECT id FROM clipboard_items WHERE hash_version = ?1 AND content_hash = ?2",
                    params![hashed.hash_version, hashed.content_hash.as_slice()],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(owner) = owner.filter(|owner| *owner != item_id) {
                // 已发布数据库可能已有多个同文条目，复用哈希所属条目以遵守原有唯一约束。
                item_id = owner;
                continue;
            }
            let formats_changed = merged.formats != stored.formats
                || merged.primary_format_identity != stored.primary_format_identity;
            return Ok(Some(TextMerge {
                item_id,
                captured: Some(merged),
                hashed: Some(hashed),
                formats_changed,
            }));
        }
    }
    Ok(None)
}

fn stored_formats_unchanged(
    transaction: &Transaction<'_>,
    incoming: &CapturedClipboard,
    hashed: &HashedClipboard,
    item_id: i64,
    require_complete: bool,
) -> Result<bool, CaptureError> {
    let mut statement = transaction.prepare(
        "SELECT format_namespace, format_key, format_name, native_format_id, format_kind,
            capture_state, restore_state, encoding, mime_type, error_code, error_message,
            restore_error_code, restore_error_message, is_primary, data_hash
         FROM clipboard_formats WHERE item_id = ?1 ORDER BY format_order",
    )?;
    let mut rows = statement.query([item_id])?;
    let mut count = 0;
    while let Some(row) = rows.next()? {
        let namespace: String = row.get(0)?;
        let key: String = row.get(1)?;
        let Some((index, format)) =
            incoming.formats.iter().enumerate().find(|(_, format)| {
                format.format_namespace == namespace && format.format_key == key
            })
        else {
            if require_complete || row.get::<_, bool>(13)? {
                return Ok(false);
            }
            continue;
        };
        let primary = incoming
            .primary_format_identity
            .as_ref()
            .is_some_and(|identity| identity.0 == namespace && identity.1 == key);
        if row.get::<_, String>(2)? != format.format_name
            || row.get::<_, Option<u32>>(3)? != format.native_format_id
            || row.get::<_, String>(4)? != format.format_kind.as_str()
            || row.get::<_, String>(5)? != format.capture_state.as_str()
            || row.get::<_, String>(6)? != format.restore_state.as_str()
            || row.get::<_, Option<String>>(7)? != format.encoding
            || row.get::<_, Option<String>>(8)? != format.mime_type
            || row.get::<_, Option<String>>(9)? != format.error_code
            || row.get::<_, Option<String>>(10)? != format.error_message
            || row.get::<_, Option<String>>(11)? != format.restore_error_code
            || row.get::<_, Option<String>>(12)? != format.restore_error_message
            || row.get::<_, bool>(13)? != primary
            || row.get::<_, Option<Vec<u8>>>(14)?.as_deref()
                != hashed.format_hashes[index]
                    .as_ref()
                    .map(|hash| hash.as_slice())
        {
            return Ok(false);
        }
        count += 1;
    }
    Ok(count == incoming.formats.len())
}

fn full_text(captured: &CapturedClipboard) -> Option<String> {
    let single_spreadsheet_cell = is_single_spreadsheet_cell(captured);
    for format in &captured.formats {
        if is_custom_text_format(&format.format_namespace, &format.format_key) {
            return None;
        }
        match role(format) {
            FormatRole::Image => {
                if !single_spreadsheet_cell
                    || format.format_namespace != "windows"
                    || !matches!(
                        format.format_key.as_str(),
                        "CF_BITMAP" | "CF_DIB" | "CF_DIBV5"
                    )
                {
                    return None;
                }
            }
            FormatRole::FileList | FormatRole::VirtualFile => return None,
            FormatRole::Html => {
                let Some(bytes) = format.bytes.as_deref() else {
                    continue;
                };
                let html = std::str::from_utf8(bytes).ok()?.to_ascii_lowercase();
                if html.split('<').skip(1).any(|tag| {
                    let name = tag
                        .trim_start()
                        .split(|character: char| !character.is_ascii_alphanumeric())
                        .next()
                        .unwrap_or_default();
                    matches!(
                        name,
                        "img"
                            | "svg"
                            | "canvas"
                            | "object"
                            | "embed"
                            | "iframe"
                            | "video"
                            | "audio"
                            | "picture"
                    )
                }) || html.contains("url(")
                {
                    return None;
                }
            }
            FormatRole::Rtf => {
                let bytes = format.bytes.as_deref().unwrap_or_default();
                if [b"\\pict".as_slice(), b"\\object", b"\\objdata"]
                    .iter()
                    .any(|control| {
                        bytes
                            .windows(control.len())
                            .any(|part| part.eq_ignore_ascii_case(control))
                    })
                {
                    return None;
                }
            }
            _ => {}
        }
    }
    let format = captured
        .formats
        .iter()
        .filter(|format| {
            role(format) == FormatRole::PlainText
                && format.capture_state.has_data()
                && format.restore_state == RestoreState::Restorable
        })
        .min_by_key(|format| {
            if format.format_key.eq_ignore_ascii_case("CF_UNICODETEXT") {
                0
            } else if matches!(format.format_key.as_str(), "CF_TEXT" | "CF_OEMTEXT") {
                2
            } else {
                1
            }
        })?;
    let bytes = format.bytes.as_deref()?;
    let text = if format.format_key.eq_ignore_ascii_case("CF_UNICODETEXT")
        || format.encoding.as_deref() == Some("utf-16le")
    {
        if bytes.len() % 2 != 0 {
            return None;
        }
        let units = bytes
            .chunks_exact(2)
            .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
            .take_while(|unit| *unit != 0)
            .collect::<Vec<_>>();
        String::from_utf16(&units).ok()?
    } else {
        if format
            .encoding
            .as_deref()
            .is_some_and(|encoding| !matches!(encoding, "utf-8" | "utf8" | "us-ascii"))
        {
            return None;
        }
        if matches!(format.format_key.as_str(), "CF_TEXT" | "CF_OEMTEXT") && !bytes.is_ascii() {
            return None;
        }
        std::str::from_utf8(bytes)
            .ok()?
            .trim_end_matches('\0')
            .to_owned()
    };
    let text = if single_spreadsheet_cell {
        // Excel 为单元格文本附加一行结束符；只归一化已确认的单单元格导出。
        let value = text.strip_suffix("\r\n").unwrap_or(&text);
        if value.contains(['\r', '\n', '\t']) {
            return None;
        }
        value.to_owned()
    } else {
        text
    };
    (!text.trim().is_empty()).then_some(text)
}

fn is_single_spreadsheet_cell(captured: &CapturedClipboard) -> bool {
    const NAMESPACE: &str = "urn:schemas-microsoft-com:office:spreadsheet";
    let Some(xml) = captured
        .formats
        .iter()
        .find(|format| {
            format.format_namespace == "windows"
                && format.format_key == "registered:XML Spreadsheet"
                && format.capture_state.has_data()
                && format.restore_state == RestoreState::Restorable
        })
        .and_then(|format| format.bytes.as_deref())
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
    else {
        return false;
    };
    let Ok(document) = roxmltree::Document::parse(xml.trim_end_matches('\0')) else {
        return false;
    };
    if !document
        .root_element()
        .has_tag_name((NAMESPACE, "Workbook"))
    {
        return false;
    }
    let mut worksheets = document
        .descendants()
        .filter(|node| node.has_tag_name((NAMESPACE, "Worksheet")));
    let Some(worksheet) = worksheets.next() else {
        return false;
    };
    if worksheets.next().is_some() {
        return false;
    }
    let mut tables = worksheet
        .children()
        .filter(|node| node.has_tag_name((NAMESPACE, "Table")));
    let Some(table) = tables.next() else {
        return false;
    };
    if tables.next().is_some()
        || table.attribute((NAMESPACE, "ExpandedColumnCount")) != Some("1")
        || table.attribute((NAMESPACE, "ExpandedRowCount")) != Some("1")
    {
        return false;
    }
    let mut rows = table
        .children()
        .filter(|node| node.has_tag_name((NAMESPACE, "Row")));
    let Some(row) = rows.next() else {
        return false;
    };
    if rows.next().is_some() {
        return false;
    }
    let mut cells = row
        .children()
        .filter(|node| node.has_tag_name((NAMESPACE, "Cell")));
    let Some(cell) = cells.next() else {
        return false;
    };
    cells.next().is_none()
        && ["MergeAcross", "MergeDown"].iter().all(|attribute| {
            cell.attribute((NAMESPACE, *attribute))
                .is_none_or(|value| value == "0")
        })
        && cell
            .children()
            .any(|node| node.has_tag_name((NAMESPACE, "Data")))
}

fn role(format: &RawFormat) -> FormatRole {
    classify_format_role(format.format_kind, &format.format_key, &format.format_name)
}

fn merge_formats(incoming: &CapturedClipboard, stored: &CapturedClipboard) -> CapturedClipboard {
    let mut merged = stored.clone();
    merged.primary_format_identity = incoming.primary_format_identity.clone();
    merged.source_application = incoming.source_application.clone();
    merged.preview_candidate = incoming.preview_candidate.clone();
    for format in &incoming.formats {
        // 同文条目保留全部格式；同一格式采用新数据，捕获或恢复能力退化时保留旧数据。
        if let Some(current) = merged
            .formats
            .iter_mut()
            .find(|current| current.stable_identity() == format.stable_identity())
        {
            if !current.capture_state.has_data()
                || (format.capture_state.has_data()
                    && (current.restore_state != RestoreState::Restorable
                        || format.restore_state == RestoreState::Restorable))
            {
                *current = format.clone();
            }
        } else {
            merged.formats.push(format.clone());
        }
    }
    for (order, format) in merged.formats.iter_mut().enumerate() {
        format.format_order = order as u32;
    }
    merged
}

fn load_capture(
    transaction: &Transaction<'_>,
    item_id: i64,
    blob_store: &RawBlobStore,
    text_only: bool,
) -> Result<Option<CapturedClipboard>, CaptureError> {
    let eligible: bool = transaction.query_row(
        "SELECT capture_state = 'ready' AND origin_kind IN ('captured', 'imported', 'synced')
             AND NOT EXISTS (SELECT 1 FROM clipboard_items WHERE parent_item_id = ?1)
         FROM clipboard_items WHERE id = ?1",
        [item_id],
        |row| row.get(0),
    )?;
    if !eligible {
        return Ok(None);
    }
    let mut statement = transaction.prepare(
        "SELECT f.format_namespace, f.format_key, f.format_name, f.format_order,
                f.native_format_id, f.format_kind, f.capture_state, f.restore_state,
                f.inline_data, b.sha256, b.byte_length, b.relative_path,
                f.encoding, f.mime_type, f.error_code, f.error_message,
                f.restore_error_code, f.restore_error_message, f.is_primary, f.storage_kind
         FROM clipboard_formats f LEFT JOIN binary_blobs b ON b.id = f.blob_id
         WHERE f.item_id = ?1 ORDER BY f.format_order",
    )?;
    let mut rows = statement.query([item_id])?;
    let mut formats = Vec::new();
    let mut primary_format_identity = None;
    while let Some(row) = rows.next()? {
        let kind: String = row.get(5)?;
        let Some(format_kind) = FormatKind::from_str(&kind) else {
            return Ok(None);
        };
        let key: String = row.get(1)?;
        let name: String = row.get(2)?;
        let role = classify_format_role(format_kind, &key, &name);
        let needs_bytes = !text_only
            || matches!(
                role,
                FormatRole::PlainText | FormatRole::Html | FormatRole::Rtf
            )
            || key == "registered:XML Spreadsheet";
        let capture_state = match row.get::<_, String>(6)?.as_str() {
            "captured" => FormatCaptureState::Captured,
            "generated" => FormatCaptureState::Generated,
            "failed" => FormatCaptureState::Failed,
            "unsupported" => FormatCaptureState::Unsupported,
            _ => return Ok(None),
        };
        let restore_state = match row.get::<_, String>(7)?.as_str() {
            "restorable" => RestoreState::Restorable,
            "not_restorable" => RestoreState::NotRestorable,
            "pending" => RestoreState::Pending,
            "not_applicable" => RestoreState::NotApplicable,
            _ => return Ok(None),
        };
        // 比较完整文本时不读取位图和私有格式的大块数据，确认需要更新后再加载。
        let bytes = if !needs_bytes {
            None
        } else {
            match row.get::<_, String>(19)?.as_str() {
                "inline" => row.get(8)?,
                "external" => {
                    let hash: Vec<u8> = row.get(9)?;
                    let length: i64 = row.get(10)?;
                    let path: String = row.get(11)?;
                    let hash = hash.try_into().map_err(|_| {
                        CaptureError::InvalidFormat("外部格式哈希长度无效".to_owned())
                    })?;
                    let length = u64::try_from(length)
                        .map_err(|_| CaptureError::InvalidFormat("外部格式长度无效".to_owned()))?;
                    Some(blob_store.read(&RawBlob::from_record(hash, length, path))?)
                }
                "none" => None,
                _ => return Ok(None),
            }
        };
        let format = RawFormat {
            format_namespace: row.get(0)?,
            format_key: key,
            format_name: name,
            format_order: row.get(3)?,
            native_format_id: row.get(4)?,
            format_kind,
            bytes,
            capture_state,
            restore_state,
            encoding: row.get(12)?,
            mime_type: row.get(13)?,
            error_code: row.get(14)?,
            error_message: row.get(15)?,
            restore_error_code: row.get(16)?,
            restore_error_message: row.get(17)?,
        };
        if row.get::<_, bool>(18)? {
            primary_format_identity =
                Some((format.format_namespace.clone(), format.format_key.clone()));
        }
        formats.push(format);
    }
    Ok(Some(CapturedClipboard {
        formats,
        primary_format_identity,
        preview_candidate: None,
        source_application: Default::default(),
    }))
}
