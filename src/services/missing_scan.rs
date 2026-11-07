use std::path::Path;

use crate::{
    domain::{
        capture::{FormatKind, FormatRole, classify_format_role, is_sync_managed_files_format},
        clipboard::ClipboardItemId,
    },
    services::{
        database::{
            Database, DatabaseError,
            missing_files::{ScanFormat, load_scan_candidates},
        },
        image_preview::parse_file_list,
        storage::{
            RawBlob, RawBlobError, RawBlobStore, path_is_certainly_missing,
            path_is_certainly_present,
        },
    },
};

#[derive(Debug)]
struct ScanAbort;

pub(crate) struct MissingScanOutput {
    pub hidden: Vec<ClipboardItemId>,
    pub confirmed_present: Vec<ClipboardItemId>,
}

/// Probe the filesystem after loading a database snapshot. An unavailable source is
/// unknown, and only a fully confirmed missing image or file list can be hidden.
/// A data switch aborts the whole scan so a partial snapshot is never applied.
pub(crate) fn scan(
    database: &Database,
    blobs: &RawBlobStore,
) -> Result<MissingScanOutput, DatabaseError> {
    let candidates = load_scan_candidates(database)?;
    let mut hidden = Vec::new();
    let mut confirmed_present = Vec::new();
    for candidate in candidates {
        let has_file_list = candidate.formats.iter().any(is_file_list_format);
        let has_target = candidate.formats.iter().any(is_target_format);
        if !has_target {
            continue;
        }
        let mut missing = true;
        for format in &candidate.formats {
            match format_is_certainly_missing(format, blobs, has_file_list) {
                Ok(value) => missing &= value,
                Err(ScanAbort) => return Err(DatabaseError::DataSwitching),
            }
        }
        if missing {
            hidden.push(candidate.item_id);
        }
        let mut present = true;
        for format in candidate
            .formats
            .iter()
            .filter(|format| is_target_format(format))
        {
            match format_is_certainly_present(format, blobs) {
                Ok(value) => present &= value,
                Err(ScanAbort) => return Err(DatabaseError::DataSwitching),
            }
        }
        if present {
            confirmed_present.push(candidate.item_id);
        }
    }
    Ok(MissingScanOutput {
        hidden,
        confirmed_present,
    })
}

fn is_file_list_format(format: &ScanFormat) -> bool {
    FormatKind::from_str(&format.kind).is_some_and(|kind| {
        classify_format_role(kind, &format.key, &format.name) == FormatRole::FileList
    })
}

fn is_target_format(format: &ScanFormat) -> bool {
    FormatKind::from_str(&format.kind).is_some_and(|kind| {
        matches!(
            classify_format_role(kind, &format.key, &format.name),
            FormatRole::Image | FormatRole::FileList
        )
    })
}

fn format_is_certainly_missing(
    format: &ScanFormat,
    blobs: &RawBlobStore,
    has_file_list: bool,
) -> Result<bool, ScanAbort> {
    let Some(kind) = FormatKind::from_str(&format.kind) else {
        return Ok(false);
    };
    // App-owned sync metadata describes managed copies; it must not keep a
    // missing entry visible.
    if is_sync_managed_files_format(&format.namespace, &format.key) {
        return Ok(true);
    }
    match classify_format_role(kind, &format.key, &format.name) {
        FormatRole::Internal => Ok(true),
        FormatRole::VirtualFile
            if has_file_list
                && matches!(
                    format.key.to_ascii_lowercase().as_str(),
                    "filename" | "filenamew" | "registered:filename" | "registered:filenamew"
                ) =>
        {
            Ok(true)
        }
        FormatRole::Image => Ok(format.storage == "external"
            && format
                .path
                .as_deref()
                .is_some_and(|path| blobs.original_file_is_missing(Path::new(path)))),
        FormatRole::FileList => {
            let Some(bytes) = format_bytes(format, blobs)? else {
                return Ok(false);
            };
            // Invalid/empty payloads cannot prove a file list has disappeared.
            if !valid_file_list_payload(&bytes) {
                return Ok(false);
            }
            let paths = parse_file_list(&bytes);
            Ok(!paths.is_empty() && paths.iter().all(|path| path_is_certainly_missing(path)))
        }
        _ => Ok(false),
    }
}

fn format_is_certainly_present(
    format: &ScanFormat,
    blobs: &RawBlobStore,
) -> Result<bool, ScanAbort> {
    let Some(kind) = FormatKind::from_str(&format.kind) else {
        return Ok(false);
    };
    // App-owned sync metadata lists managed copies; it never proves the
    // original files still exist.
    if is_sync_managed_files_format(&format.namespace, &format.key) {
        return Ok(false);
    }
    match classify_format_role(kind, &format.key, &format.name) {
        FormatRole::Image => Ok(format.storage == "external"
            && format
                .path
                .as_deref()
                .is_some_and(|path| blobs.original_file_is_present(Path::new(path)))),
        FormatRole::FileList => {
            let Some(bytes) = format_bytes(format, blobs)? else {
                return Ok(false);
            };
            if !valid_file_list_payload(&bytes) {
                return Ok(false);
            }
            let paths = parse_file_list(&bytes);
            Ok(!paths.is_empty() && paths.iter().all(|path| path_is_certainly_present(path)))
        }
        _ => Ok(false),
    }
}

fn format_bytes(format: &ScanFormat, blobs: &RawBlobStore) -> Result<Option<Vec<u8>>, ScanAbort> {
    match format.storage.as_str() {
        "inline" => Ok(format.inline_data.clone()),
        "external" => read_external(format, blobs),
        _ => Ok(None),
    }
}

fn valid_file_list_payload(bytes: &[u8]) -> bool {
    #[cfg(target_os = "windows")]
    if bytes.len() >= 20 {
        let offset = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
        // DROPFILES starts with a 20-byte header. Reject malformed headers instead of
        // allowing the preview parser to silently discard invalid entries.
        if bytes[1..4] == [0, 0, 0] && offset >= 20 {
            if offset >= bytes.len() || bytes[12..16] != [0, 0, 0, 0] {
                return false;
            }
            let wide = u32::from_le_bytes(bytes[16..20].try_into().unwrap());
            let data = &bytes[offset..];
            return match wide {
                0 => terminated_file_list_prefix(data).is_some_and(valid_utf8_paths),
                1 => {
                    let mut chunks = data.chunks_exact(2);
                    let units: Vec<u16> = chunks
                        .by_ref()
                        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                        .collect();
                    chunks.remainder().iter().all(|byte| *byte == 0)
                        && terminated_file_list_prefix(&units).is_some_and(valid_utf16_paths)
                }
                _ => false,
            };
        }
    }
    valid_utf8_paths(bytes)
}

/// A DROPFILES allocation can extend beyond the double-NUL path-list terminator.
/// Only zero padding is safe to accept here; unexpected trailing data remains unknown.
#[cfg(target_os = "windows")]
fn terminated_file_list_prefix<T: Copy + Default + Eq>(units: &[T]) -> Option<&[T]> {
    let end = units
        .windows(2)
        .position(|pair| pair[0] == T::default() && pair[1] == T::default())?
        + 2;
    units[end..]
        .iter()
        .all(|unit| *unit == T::default())
        .then_some(&units[..end])
}

fn valid_utf8_paths(bytes: &[u8]) -> bool {
    if bytes.len() < 3 || !bytes.ends_with(&[0, 0]) {
        return false;
    }
    let entries = &bytes[..bytes.len() - 2];
    entries
        .split(|byte| *byte == 0)
        .all(|entry| !entry.is_empty() && std::str::from_utf8(entry).is_ok())
}

#[cfg(target_os = "windows")]
fn valid_utf16_paths(units: &[u16]) -> bool {
    if units.len() < 3 || !units.ends_with(&[0, 0]) {
        return false;
    }
    units[..units.len() - 2]
        .split(|unit| *unit == 0)
        .all(|entry| !entry.is_empty() && String::from_utf16(entry).is_ok())
}

fn read_external(format: &ScanFormat, blobs: &RawBlobStore) -> Result<Option<Vec<u8>>, ScanAbort> {
    let Some(hash) = format
        .hash
        .as_ref()
        .and_then(|hash| <[u8; 32]>::try_from(hash.as_slice()).ok())
    else {
        return Ok(None);
    };
    let Some(length) = u64::try_from(format.length).ok() else {
        return Ok(None);
    };
    let Some(path) = format.path.as_ref() else {
        return Ok(None);
    };
    let blob = RawBlob::from_record(hash, length, path);
    match blobs.read(&blob) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(RawBlobError::DataSwitching) => Err(ScanAbort),
        Err(_) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_file_lists_are_unknown() {
        assert!(!valid_file_list_payload(b"/missing\0"));
        assert!(!valid_file_list_payload(b"/missing\0\0\0"));
        assert!(!valid_file_list_payload(b"/missing\0\xff\0\0"));
        assert!(!valid_file_list_payload(b"/missing\0\0/other\0\0"));
        assert!(valid_file_list_payload(b"/missing\0/other\0\0"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn malformed_dropfiles_is_unknown() {
        let mut payload = vec![0; 20];
        payload[..4].copy_from_slice(&20_u32.to_le_bytes());
        payload[16..20].copy_from_slice(&1_u32.to_le_bytes());
        payload.extend_from_slice(&[0x00, 0xd8, 0, 0, 0, 0]); // lone surrogate
        assert!(!valid_file_list_payload(&payload));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn dropfiles_accepts_only_zero_allocation_padding() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("image.png");
        let payload = crate::platform::image_temp_file::hdrop_bytes(&path);
        for padding in [0, 1, 7, 8, 416] {
            let mut padded = payload.clone();
            padded.extend(vec![0; padding]);
            assert!(valid_file_list_payload(&padded), "zero padding: {padding}");
        }

        let mut nonzero_last_byte = payload.clone();
        nonzero_last_byte.extend_from_slice(&[0; 7]);
        *nonzero_last_byte.last_mut().unwrap() = 1;
        assert!(!valid_file_list_payload(&nonzero_last_byte));

        let mut nonzero_wide_tail = payload.clone();
        nonzero_wide_tail.extend_from_slice(&[1, 0]);
        assert!(!valid_file_list_payload(&nonzero_wide_tail));

        let mut another_path = payload.clone();
        another_path.extend_from_slice(&[b'x', 0, 0, 0, 0, 0]);
        assert!(!valid_file_list_payload(&another_path));

        for missing_bytes in [1, 2] {
            assert!(!valid_file_list_payload(
                &payload[..payload.len() - missing_bytes]
            ));
        }
    }
}
