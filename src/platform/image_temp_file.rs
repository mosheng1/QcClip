use std::{
    io::Write,
    path::Path,
    sync::Mutex,
    time::{Duration, Instant},
};

use image::{DynamicImage, ImageFormat};
use sha2::{Digest, Sha256};
use tempfile::{Builder, TempPath};

use crate::domain::capture::{FormatKind, FormatRole, classify_format_role};
use crate::platform::clipboard::ClipboardPayload;

/// 粘贴到外部程序时临时图片文件需要多存活一段时间，避免目标程序惰性读取时文件已被删除。
const IMAGE_TEMP_FILE_RETENTION: Duration = Duration::from_secs(30);
static RETAINED_IMAGE_FILES: Mutex<Vec<RetainedImageFile>> = Mutex::new(Vec::new());
static OPENED_IMAGE_FILE_LOCK: Mutex<()> = Mutex::new(());

struct RetainedImageFile {
    expires_at: Instant,
    _file: ImageTempFile,
}

pub(crate) struct ImageTempFile {
    path: TempPath,
}

impl ImageTempFile {
    pub(crate) fn from_bytes(bytes: &[u8], format_key: &str) -> Result<Self, String> {
        let image = decode_image(bytes, format_key)?;
        let mut file = Builder::new()
            .prefix("qcclip-image-")
            .suffix(".png")
            .tempfile()
            .map_err(|error| format!("创建图片临时文件失败：{error}"))?;
        image
            .write_to(file.as_file_mut(), ImageFormat::Png)
            .map_err(|error| format!("写入图片临时文件失败：{error}"))?;
        file.as_file_mut()
            .flush()
            .map_err(|error| format!("刷新图片临时文件失败：{error}"))?;
        Ok(Self {
            path: file.into_temp_path(),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

/// 将临时图片文件交给进程级保留池，到期后自动删除。
pub(crate) fn retain_image_temp_file(file: ImageTempFile) {
    let now = Instant::now();
    let mut retained = RETAINED_IMAGE_FILES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    retained.retain(|entry| entry.expires_at > now);
    retained.push(RetainedImageFile {
        expires_at: now + IMAGE_TEMP_FILE_RETENTION,
        _file: file,
    });
}

/// 非文件类型的图片载荷生成临时文件，供拖拽/粘贴时以 CF_HDROP 形式暴露给外部程序。
pub(crate) fn create_image_temp_file(
    payloads: &[ClipboardPayload],
) -> Result<Option<ImageTempFile>, String> {
    let has_file_payload = payloads.iter().any(|payload| {
        let kind = FormatKind::from_str(&payload.format_kind).unwrap_or(FormatKind::Unknown);
        matches!(
            classify_format_role(kind, &payload.format_key, &payload.format_name),
            FormatRole::FileList | FormatRole::VirtualFile
        )
    });
    if has_file_payload {
        return Ok(None);
    }

    payloads
        .iter()
        .find_map(|payload| {
            let kind = FormatKind::from_str(&payload.format_kind).unwrap_or(FormatKind::Unknown);
            (classify_format_role(kind, &payload.format_key, &payload.format_name)
                == FormatRole::Image)
                .then(|| ImageTempFile::from_bytes(&payload.bytes, &payload.format_key))
        })
        .transpose()
}

/// 生成 CF_HDROP 格式的载荷字节（DROPFILES 结构 + UTF-16 双 NUL 结尾路径）。
pub(crate) fn hdrop_bytes(path: &Path) -> Vec<u8> {
    let paths = [path.to_path_buf()];
    hdrop_bytes_for_paths(&paths)
}

/// 生成包含多个路径的 CF_HDROP 格式载荷。
pub(crate) fn hdrop_bytes_for_paths(paths: &[std::path::PathBuf]) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;

    let mut bytes = Vec::with_capacity(20 + paths.len() * 128);
    bytes.extend_from_slice(&20_u32.to_le_bytes());
    bytes.extend_from_slice(&[0; 8]);
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&1_u32.to_le_bytes());
    for path in paths {
        bytes.extend(
            path.as_os_str()
                .encode_wide()
                .chain([0])
                .flat_map(u16::to_le_bytes),
        );
    }
    bytes.extend_from_slice(&[0, 0]);
    bytes
}

/// “使用默认程序打开”使用的专属临时目录，应用启动时整体清理。
pub(crate) fn opened_files_directory() -> std::path::PathBuf {
    std::env::temp_dir().join("qcclip-opened")
}

/// 清空“使用默认程序打开”遗留的临时文件目录，应用启动时调用。
pub(crate) fn cleanup_opened_files_directory() {
    let _guard = OPENED_IMAGE_FILE_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let directory = opened_files_directory();
    if directory.is_dir()
        && let Err(error) = std::fs::remove_dir_all(&directory)
    {
        tracing::error!("清理打开的临时文件目录失败：{error}");
    }
}

/// 为“使用默认程序打开”生成可复用的临时图片文件。
pub(crate) fn create_openable_image_file(
    payloads: &[ClipboardPayload],
    item_uuid: &str,
) -> Result<Option<std::path::PathBuf>, String> {
    create_openable_image_file_in(payloads, item_uuid, &opened_files_directory())
}

fn create_openable_image_file_in(
    payloads: &[ClipboardPayload],
    item_uuid: &str,
    directory: &Path,
) -> Result<Option<std::path::PathBuf>, String> {
    let has_file_payload = payloads.iter().any(|payload| {
        let kind = FormatKind::from_str(&payload.format_kind).unwrap_or(FormatKind::Unknown);
        matches!(
            classify_format_role(kind, &payload.format_key, &payload.format_name),
            FormatRole::FileList | FormatRole::VirtualFile
        )
    });
    if has_file_payload {
        return Ok(None);
    }
    let Some(payload) = payloads.iter().find(|payload| {
        let kind = FormatKind::from_str(&payload.format_kind).unwrap_or(FormatKind::Unknown);
        classify_format_role(kind, &payload.format_key, &payload.format_name) == FormatRole::Image
    }) else {
        return Ok(None);
    };
    let item_uuid = uuid::Uuid::parse_str(item_uuid)
        .map_err(|_| "打开图片失败：剪贴板条目标识无效".to_owned())?
        .simple()
        .to_string();
    let image = decode_image(&payload.bytes, &payload.format_key)?;
    let _guard = OPENED_IMAGE_FILE_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    std::fs::create_dir_all(directory)
        .map_err(|error| format!("创建打开的临时文件目录失败：{error}"))?;
    let content_hash = Sha256::digest(&payload.bytes);
    let path = directory.join(format!("{item_uuid}-{}.png", hex_digest(&content_hash)));
    if path.is_file() {
        return Ok(Some(path));
    }
    let mut file =
        std::fs::File::create(&path).map_err(|error| format!("创建打开的临时图片失败：{error}"))?;
    let result = image
        .write_to(&mut file, ImageFormat::Png)
        .map_err(|error| format!("写入打开的临时图片失败：{error}"))
        .and_then(|_| {
            file.flush()
                .map_err(|error| format!("刷新打开的临时图片失败：{error}"))
        });
    drop(file);
    if let Err(error) = result {
        let _ = std::fs::remove_file(&path);
        return Err(error);
    }
    Ok(Some(path))
}

fn hex_digest(digest: &[u8]) -> String {
    let mut value = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(value, "{byte:02x}");
    }
    value
}

/// 删除条目关联的打开图片缓存；被外部程序占用时留待启动清理兜底。
pub(crate) fn remove_openable_image_files(item_uuid: &str) {
    let Ok(item_uuid) = uuid::Uuid::parse_str(item_uuid) else {
        tracing::warn!("清理打开图片失败：剪贴板条目标识无效");
        return;
    };
    let _guard = OPENED_IMAGE_FILE_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    remove_openable_image_files_in(&item_uuid, &opened_files_directory());
}

fn remove_openable_image_files_in(item_uuid: &uuid::Uuid, directory: &Path) {
    let prefix = format!("{}-", item_uuid.simple());
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            tracing::warn!(%error, "读取打开图片缓存目录失败");
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let matches_item = path.is_file()
            && path.extension().is_some_and(|extension| extension == "png")
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&prefix));
        if matches_item && let Err(error) = std::fs::remove_file(path) {
            tracing::debug!(%error, "打开图片仍被占用，将在下次启动时重试清理");
        }
    }
}

fn decode_image(bytes: &[u8], format_key: &str) -> Result<DynamicImage, String> {
    if format_key.eq_ignore_ascii_case("CF_DIB") || format_key.eq_ignore_ascii_case("CF_DIBV5") {
        let normalized;
        let dib = if format_key.eq_ignore_ascii_case("CF_DIBV5") {
            normalized =
                normalize_dib_v5(bytes).ok_or_else(|| "无效的 CF_DIBV5 数据".to_owned())?;
            normalized.as_slice()
        } else {
            bytes
        };
        let bmp = bmp_file_bytes_from_dib(dib).ok_or_else(|| "无效的 DIB 数据".to_owned())?;
        image::load_from_memory(&bmp).map_err(|error| format!("解析 DIB 图片失败：{error}"))
    } else {
        image::load_from_memory(bytes).map_err(|error| format!("解析图片失败：{error}"))
    }
}

fn bmp_file_bytes_from_dib(bytes: &[u8]) -> Option<Vec<u8>> {
    let header_size =
        usize::try_from(u32::from_le_bytes(bytes.get(0..4)?.try_into().ok()?)).ok()?;
    if header_size < 12 || header_size > bytes.len() {
        return None;
    }
    let pixel_offset = if header_size == 12 {
        let bits_per_pixel = u16::from_le_bytes(bytes.get(10..12)?.try_into().ok()?);
        let palette_entries = if bits_per_pixel <= 8 {
            1_usize.checked_shl(u32::from(bits_per_pixel))?
        } else {
            0
        };
        header_size.checked_add(palette_entries.checked_mul(3)?)?
    } else {
        let bits_per_pixel = u16::from_le_bytes(bytes.get(14..16)?.try_into().ok()?);
        let compression = u32::from_le_bytes(bytes.get(16..20)?.try_into().ok()?);
        let colors_used = u32::from_le_bytes(bytes.get(32..36)?.try_into().ok()?);
        let palette_entries = if bits_per_pixel <= 8 {
            usize::try_from(if colors_used == 0 {
                1_u32.checked_shl(u32::from(bits_per_pixel))?
            } else {
                colors_used
            })
            .ok()?
        } else {
            0
        };
        let bitfield_bytes = if header_size == 40 && matches!(compression, 3 | 6) {
            if compression == 3 { 12 } else { 16 }
        } else {
            0
        };
        header_size
            .checked_add(bitfield_bytes)?
            .checked_add(palette_entries.checked_mul(4)?)?
    };
    if pixel_offset > bytes.len() {
        return None;
    }
    let file_size = u32::try_from(14_usize.checked_add(bytes.len())?).ok()?;
    let file_pixel_offset = u32::try_from(14_usize.checked_add(pixel_offset)?).ok()?;
    let mut result = Vec::with_capacity(file_size as usize);
    result.extend_from_slice(b"BM");
    result.extend_from_slice(&file_size.to_le_bytes());
    result.extend_from_slice(&[0; 4]);
    result.extend_from_slice(&file_pixel_offset.to_le_bytes());
    result.extend_from_slice(bytes);
    Some(result)
}

fn normalize_dib_v5(bytes: &[u8]) -> Option<Vec<u8>> {
    const INFO_HEADER_SIZE: usize = 40;
    const V5_HEADER_SIZE: usize = 124;
    if bytes.len() < V5_HEADER_SIZE
        || u32::from_le_bytes(bytes.get(0..4)?.try_into().ok()?) != V5_HEADER_SIZE as u32
    {
        return None;
    }
    let compression = u32::from_le_bytes(bytes.get(16..20)?.try_into().ok()?);
    let mask_size = if matches!(compression, 3 | 6) { 12 } else { 0 };
    let mut normalized = Vec::with_capacity(
        INFO_HEADER_SIZE + mask_size + bytes.len().saturating_sub(V5_HEADER_SIZE),
    );
    normalized.extend_from_slice(&bytes[..INFO_HEADER_SIZE]);
    normalized[..4].copy_from_slice(&(INFO_HEADER_SIZE as u32).to_le_bytes());
    if mask_size != 0 {
        normalized[16..20].copy_from_slice(&3_u32.to_le_bytes());
        normalized.extend_from_slice(&bytes[40..52]);
    }
    normalized.extend_from_slice(&bytes[V5_HEADER_SIZE..]);
    Some(normalized)
}

#[cfg(test)]
mod tests {
    use std::{fs::OpenOptions, io::Cursor, os::windows::fs::OpenOptionsExt};

    use image::{DynamicImage, ImageFormat};

    use crate::platform::clipboard::ClipboardPayload;

    use super::{ImageTempFile, create_openable_image_file_in, remove_openable_image_files_in};

    #[test]
    fn image_temp_file_allows_exclusive_read_and_removes_on_drop() {
        let mut bytes = Cursor::new(Vec::new());
        DynamicImage::new_rgba8(1, 1)
            .write_to(&mut bytes, ImageFormat::Png)
            .expect("生成测试图片失败");
        let file = ImageTempFile::from_bytes(bytes.get_ref(), "png").expect("创建图片临时文件失败");
        let path = file.path().to_path_buf();

        let reader = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .expect("临时图片应允许目标程序独占打开");
        assert!(reader.metadata().expect("读取图片属性失败").len() > 0);
        drop(reader);
        drop(file);
        assert!(!path.exists());
    }

    #[test]
    fn opened_image_cache_reuses_identical_images_and_cleans_item_versions() {
        let directory = tempfile::tempdir().expect("创建测试目录失败");
        let item_uuid = uuid::Uuid::new_v4();
        let first_payload = image_payload([1, 2, 3, 255]);
        let first = create_openable_image_file_in(
            std::slice::from_ref(&first_payload),
            &item_uuid.to_string(),
            directory.path(),
        )
        .expect("创建图片缓存失败")
        .expect("应返回图片路径");
        let repeated = create_openable_image_file_in(
            std::slice::from_ref(&first_payload),
            &item_uuid.to_string(),
            directory.path(),
        )
        .expect("重复创建图片缓存失败")
        .expect("应返回已存在的图片路径");
        let changed = create_openable_image_file_in(
            &[image_payload([4, 5, 6, 255])],
            &item_uuid.to_string(),
            directory.path(),
        )
        .expect("创建新版本图片缓存失败")
        .expect("应返回新图片路径");

        assert_eq!(first, repeated);
        assert_ne!(first, changed);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);

        remove_openable_image_files_in(&item_uuid, directory.path());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    fn image_payload(pixel: [u8; 4]) -> ClipboardPayload {
        let mut bytes = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(1, 1, pixel.into()))
            .write_to(&mut bytes, ImageFormat::Png)
            .expect("生成测试图片失败");
        ClipboardPayload {
            format_kind: "image".to_owned(),
            format_namespace: "mime".to_owned(),
            format_key: "image/png".to_owned(),
            format_name: "image/png".to_owned(),
            native_format_id: None,
            bytes: bytes.into_inner(),
        }
    }
}
