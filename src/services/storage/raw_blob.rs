use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::services::data_runtime;

const DATA_DIRECTORY: &str = "data";
const PENDING_DIRECTORY: &str = ".pending";
/// 原样保存的图片单独存放，避免在满是哈希文件的数据目录里不好找。
const IMAGES_DIRECTORY: &str = "images";
const ZSTD_COMPRESSION_LEVEL: i32 = 3;

/// 是否把图片以可直接打开的文件形式落盘。
///
/// 由设置页控制，修改后立即对后续捕获生效，已有文件保持原有存储方式。
/// 开启时不压缩图片字节，文件名使用内容哈希加图片扩展名，例如 `a1b2....png`。
static SAVE_IMAGES_AS_FILES: AtomicBool = AtomicBool::new(false);

pub fn save_images_as_files() -> bool {
    SAVE_IMAGES_AS_FILES.load(Ordering::Relaxed)
}

pub fn set_save_images_as_files(enabled: bool) {
    SAVE_IMAGES_AS_FILES.store(enabled, Ordering::Relaxed);
}

/// 按文件头判断常见图片容器格式，命中时返回落盘使用的扩展名。
///
/// 剪贴板里的图片多数直接携带 PNG 等容器字节，少数是 CF_DIB 裸位图；
/// 裸位图没有可识别的容器，返回 `None` 时继续按原有方式存储。
pub fn image_extension_from_header(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else {
        None
    }
}

/// 判断扩展名是否属于原样保存的图片，用于决定落盘目录。
fn is_image_extension(extension: &str) -> bool {
    matches!(extension, "png" | "jpg" | "jpeg" | "gif" | "webp")
}

/// 同一张图的多个格式里，数字越小越优先落成明文文件。
///
/// PNG 无损且通用，优先；WebP 次之；JPEG 有损但兼容性好；GIF 只用于动图。
pub fn image_extension_rank(extension: &str) -> u8 {
    match extension {
        "png" => 0,
        "webp" => 1,
        "jpg" | "jpeg" => 2,
        _ => 3,
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum BlobCompression {
    None,
    Zstd,
}

impl BlobCompression {
    fn from_path(path: &Path) -> Self {
        if path.to_string_lossy().ends_with(".bin.zst") {
            Self::Zstd
        } else {
            Self::None
        }
    }

    /// 生成规范文件名。未压缩时使用调用方给出的扩展名，缺省仍是 `bin`。
    fn file_name(self, sha256: &[u8; 32], raw_extension: Option<&str>) -> String {
        match self {
            Self::None => format!("{}.{}", encode_hex(sha256), raw_extension.unwrap_or("bin")),
            Self::Zstd => format!("{}.bin.zst", encode_hex(sha256)),
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RawBlob {
    sha256: [u8; 32],
    byte_length: u64,
    relative_path: PathBuf,
}

impl RawBlob {
    pub fn from_record(
        sha256: [u8; 32],
        byte_length: u64,
        relative_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            sha256,
            byte_length,
            relative_path: relative_path.into(),
        }
    }

    pub fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }

    pub fn byte_length(&self) -> u64 {
        self.byte_length
    }

    pub fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    fn compression(&self) -> BlobCompression {
        BlobCompression::from_path(&self.relative_path)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingBlob {
    sha256: [u8; 32],
    byte_length: u64,
    relative_path: PathBuf,
    compression: BlobCompression,
    /// 未压缩落盘时使用的扩展名，`None` 表示沿用 `bin`。
    raw_extension: Option<String>,
}

impl PendingBlob {
    pub fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }

    pub fn byte_length(&self) -> u64 {
        self.byte_length
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReconcileReport {
    pub pending_files_removed: usize,
    pub verified_blobs: Vec<RawBlob>,
    pub recovered_blobs: Vec<RawBlob>,
    pub missing_blobs: Vec<RawBlob>,
    pub corrupt_blobs: Vec<RawBlob>,
    pub orphan_files: Vec<PathBuf>,
}

#[derive(Debug, Error)]
pub enum RawBlobError {
    #[error("无法创建原始数据目录“{path}”：{source}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("无法规范化原始数据目录“{path}”：{source}")]
    CanonicalizeDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("原始数据路径超出应用数据目录：“{path}”")]
    InvalidRelativePath { path: PathBuf },
    #[error("无法创建临时原始数据文件“{path}”：{source}")]
    CreatePending {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("无法写入临时原始数据文件“{path}”：{source}")]
    WritePending {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("无法压缩原始数据：{0}")]
    Compress(#[source] std::io::Error),
    #[error("无法提交原始数据文件“{from}”到“{to}”：{source}")]
    Commit {
        from: PathBuf,
        to: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("原始数据文件不存在：“{path}”")]
    Missing { path: PathBuf },
    #[error("原始数据文件已损坏：“{path}”（{reason}）")]
    Corrupt { path: PathBuf, reason: String },
    #[error("无法读取原始数据文件“{path}”：{source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("无法删除原始数据文件“{path}”：{source}")]
    Delete {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("无法扫描原始数据目录“{path}”：{source}")]
    Scan {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("数据正在导入，请稍后重试")]
    DataSwitching,
}

#[derive(Clone, Debug)]
pub struct RawBlobStore {
    root: PathBuf,
    data_directory: PathBuf,
    pending_directory: PathBuf,
}

impl RawBlobStore {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, RawBlobError> {
        let requested_root = root.as_ref().to_path_buf();
        let requested_data = requested_root.join(DATA_DIRECTORY);
        let requested_pending = requested_data.join(PENDING_DIRECTORY);
        fs::create_dir_all(&requested_pending).map_err(|source| RawBlobError::CreateDirectory {
            path: requested_pending.clone(),
            source,
        })?;

        let root = dunce::canonicalize(&requested_root).map_err(|source| {
            RawBlobError::CanonicalizeDirectory {
                path: requested_root,
                source,
            }
        })?;
        let data_directory = dunce::canonicalize(&requested_data).map_err(|source| {
            RawBlobError::CanonicalizeDirectory {
                path: requested_data,
                source,
            }
        })?;
        let pending_directory = dunce::canonicalize(&requested_pending).map_err(|source| {
            RawBlobError::CanonicalizeDirectory {
                path: requested_pending,
                source,
            }
        })?;
        if !data_directory.starts_with(&root) || !pending_directory.starts_with(&data_directory) {
            return Err(RawBlobError::InvalidRelativePath {
                path: data_directory,
            });
        }

        Ok(Self {
            root,
            data_directory,
            pending_directory,
        })
    }

    pub fn write_pending(&self, bytes: &[u8]) -> Result<PendingBlob, RawBlobError> {
        self.write_pending_inner(bytes, None)
    }

    /// 以原始字节和指定扩展名落盘，不做压缩。
    ///
    /// 用于需要保留可直接打开形式的图片；写入内容与数据库记录的哈希、长度保持一致，
    /// 因此去重、校验和还原链路都不受影响。
    pub fn write_pending_raw(
        &self,
        bytes: &[u8],
        extension: &str,
    ) -> Result<PendingBlob, RawBlobError> {
        self.write_pending_inner(bytes, Some(extension))
    }

    fn write_pending_inner(
        &self,
        bytes: &[u8],
        raw_extension: Option<&str>,
    ) -> Result<PendingBlob, RawBlobError> {
        let _operation =
            data_runtime::begin_operation().map_err(|_| RawBlobError::DataSwitching)?;
        let sha256 = hash_bytes(bytes);
        let compressed = match raw_extension {
            // 明确要求原样落盘时跳过压缩，避免图片变成无法直接识别的压缩块
            Some(_) => None,
            None => Some(
                zstd::bulk::compress(bytes, ZSTD_COMPRESSION_LEVEL)
                    .map_err(RawBlobError::Compress)?,
            ),
        };
        let (stored_bytes, compression) = match compressed.as_deref() {
            Some(data) if data.len() < bytes.len() => (data, BlobCompression::Zstd),
            _ => (bytes, BlobCompression::None),
        };
        let relative_path = PathBuf::from(DATA_DIRECTORY)
            .join(PENDING_DIRECTORY)
            .join(format!("{}.tmp", Uuid::new_v4()));
        let path = self.resolve_relative_path(&relative_path)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|source| RawBlobError::CreatePending {
                path: path.clone(),
                source,
            })?;
        file.write_all(stored_bytes)
            .and_then(|_| file.sync_all())
            .map_err(|source| RawBlobError::WritePending {
                path: path.clone(),
                source,
            })?;

        let pending = PendingBlob {
            sha256,
            byte_length: bytes.len() as u64,
            relative_path,
            compression,
            raw_extension: raw_extension.map(str::to_owned),
        };
        self.verify_file(
            &path,
            pending.byte_length,
            &pending.sha256,
            pending.compression,
        )?;
        Ok(pending)
    }

    pub fn commit_pending(&self, pending: &PendingBlob) -> Result<RawBlob, RawBlobError> {
        let _operation =
            data_runtime::begin_operation().map_err(|_| RawBlobError::DataSwitching)?;
        let pending_path = self.resolve_relative_path(&pending.relative_path)?;
        self.verify_file(
            &pending_path,
            pending.byte_length,
            &pending.sha256,
            pending.compression,
        )?;

        let blob = self.canonical_blob(
            pending.sha256,
            pending.byte_length,
            pending.compression,
            pending.raw_extension.as_deref(),
        );
        // 路径校验会规范化父目录，所以必须先建目录再解析路径
        self.ensure_parent_directory(&blob.relative_path)?;
        let target_path = self.resolve_relative_path(&blob.relative_path)?;
        if target_path.exists() {
            self.verify_file(
                &target_path,
                blob.byte_length,
                &blob.sha256,
                pending.compression,
            )?;
            self.remove_file(&pending_path)?;
            return Ok(blob);
        }

        if let Err(source) = fs::rename(&pending_path, &target_path) {
            if target_path.exists()
                && self
                    .verify_file(
                        &target_path,
                        blob.byte_length,
                        &blob.sha256,
                        pending.compression,
                    )
                    .is_ok()
            {
                self.remove_file(&pending_path)?;
                return Ok(blob);
            }
            return Err(RawBlobError::Commit {
                from: pending_path,
                to: target_path,
                source,
            });
        }
        self.verify_file(
            &target_path,
            blob.byte_length,
            &blob.sha256,
            pending.compression,
        )?;
        Ok(blob)
    }

    pub fn read(&self, blob: &RawBlob) -> Result<Vec<u8>, RawBlobError> {
        let _operation =
            data_runtime::begin_operation().map_err(|_| RawBlobError::DataSwitching)?;
        let path = self.resolve_relative_path(&blob.relative_path)?;
        self.read_verified_file(&path, blob.byte_length, &blob.sha256, blob.compression())
    }

    /// 读取预览资源时跳过重复的 SHA-256 校验，仍验证文件长度和解压结果。
    /// 原始数据已在启动恢复阶段校验，交互式预览不应为同一图片再次完整扫描数据。
    pub fn read_for_preview(&self, blob: &RawBlob) -> Result<Vec<u8>, RawBlobError> {
        let _operation =
            data_runtime::begin_operation().map_err(|_| RawBlobError::DataSwitching)?;
        let path = self.resolve_relative_path(&blob.relative_path)?;
        let stored_bytes = fs::read(&path).map_err(|source| RawBlobError::Read {
            path: path.clone(),
            source,
        })?;
        let bytes = match blob.compression() {
            BlobCompression::None => stored_bytes,
            BlobCompression::Zstd => {
                let capacity =
                    usize::try_from(blob.byte_length).map_err(|_| RawBlobError::Corrupt {
                        path: path.clone(),
                        reason: "原始数据长度超出当前平台可寻址范围".to_owned(),
                    })?;
                zstd::bulk::decompress(&stored_bytes, capacity).map_err(|error| {
                    RawBlobError::Corrupt {
                        path: path.clone(),
                        reason: format!("Zstd 解压失败：{error}"),
                    }
                })?
            }
        };
        if bytes.len() as u64 != blob.byte_length {
            return Err(RawBlobError::Corrupt {
                path,
                reason: format!(
                    "解压后期望长度 {} 字节，实际为 {} 字节",
                    blob.byte_length,
                    bytes.len()
                ),
            });
        }
        Ok(bytes)
    }

    pub fn verify(&self, blob: &RawBlob) -> Result<(), RawBlobError> {
        let _operation =
            data_runtime::begin_operation().map_err(|_| RawBlobError::DataSwitching)?;
        let path = self.resolve_relative_path(&blob.relative_path)?;
        self.verify_file(&path, blob.byte_length, &blob.sha256, blob.compression())
    }

    pub fn reconcile(&self, referenced_blobs: &[RawBlob]) -> Result<ReconcileReport, RawBlobError> {
        let _operation =
            data_runtime::begin_operation().map_err(|_| RawBlobError::DataSwitching)?;
        let mut report = ReconcileReport {
            pending_files_removed: self.cleanup_pending_files()?,
            ..ReconcileReport::default()
        };
        let mut referenced_paths = HashSet::new();

        for blob in referenced_blobs {
            match self.verify(blob) {
                Ok(()) => {
                    referenced_paths.insert(blob.relative_path.clone());
                    report.verified_blobs.push(blob.clone());
                }
                Err(error @ (RawBlobError::Missing { .. } | RawBlobError::Corrupt { .. })) => {
                    // 数据库里的路径可能沿用旧命名，按原扩展名重建规范文件名后再找一次
                    let raw_extension = blob
                        .relative_path
                        .extension()
                        .and_then(|value| value.to_str());
                    let canonical_blob = self.canonical_blob(
                        blob.sha256,
                        blob.byte_length,
                        blob.compression(),
                        raw_extension,
                    );
                    if canonical_blob.relative_path != blob.relative_path
                        && self.verify(&canonical_blob).is_ok()
                    {
                        referenced_paths.insert(canonical_blob.relative_path.clone());
                        report.recovered_blobs.push(canonical_blob);
                    } else {
                        match error {
                            RawBlobError::Missing { .. } => report.missing_blobs.push(blob.clone()),
                            RawBlobError::Corrupt { .. } => report.corrupt_blobs.push(blob.clone()),
                            _ => unreachable!("已筛选原始数据校验错误类型"),
                        }
                    }
                }
                Err(error) => return Err(error),
            }
        }

        let mut managed = Vec::new();
        collect_managed_files(
            &self.data_directory,
            Path::new(DATA_DIRECTORY),
            &mut managed,
        )?;
        for relative_path in managed {
            if !referenced_paths.contains(&relative_path) {
                report.orphan_files.push(relative_path);
            }
        }
        Ok(report)
    }

    pub fn delete_if_unreferenced(
        &self,
        blob: &RawBlob,
        reference_count: usize,
    ) -> Result<bool, RawBlobError> {
        let _operation =
            data_runtime::begin_operation().map_err(|_| RawBlobError::DataSwitching)?;
        if reference_count > 0 {
            return Ok(false);
        }
        let path = self.resolve_relative_path(&blob.relative_path)?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(RawBlobError::Delete { path, source }),
        }
    }

    fn canonical_blob(
        &self,
        sha256: [u8; 32],
        byte_length: u64,
        compression: BlobCompression,
        raw_extension: Option<&str>,
    ) -> RawBlob {
        // 原样保存的图片放进 images 子目录，压缩块保持数据库根目录不变
        let mut relative_path = PathBuf::from(DATA_DIRECTORY);
        if let Some(extension) = raw_extension {
            if is_image_extension(extension) && compression == BlobCompression::None {
                relative_path.push(IMAGES_DIRECTORY);
            }
        }
        relative_path.push(compression.file_name(&sha256, raw_extension));
        RawBlob {
            sha256,
            byte_length,
            relative_path,
        }
    }

    /// 创建数据文件所需的父目录，图片子目录在第一次写入前还不存在。
    fn ensure_parent_directory(&self, relative_path: &Path) -> Result<(), RawBlobError> {
        let Some(parent) = relative_path.parent() else {
            return Ok(());
        };
        if parent == Path::new(DATA_DIRECTORY) {
            return Ok(());
        }
        let path = self.root.join(parent);
        fs::create_dir_all(&path).map_err(|source| RawBlobError::CreateDirectory { path, source })
    }

    fn resolve_relative_path(&self, relative_path: &Path) -> Result<PathBuf, RawBlobError> {
        if relative_path.is_absolute()
            || relative_path
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
            || relative_path.components().next() != Some(Component::Normal(DATA_DIRECTORY.as_ref()))
        {
            return Err(RawBlobError::InvalidRelativePath {
                path: relative_path.to_path_buf(),
            });
        }
        let path = self.root.join(relative_path);
        let boundary_path = if path.exists() {
            dunce::canonicalize(&path).map_err(|source| RawBlobError::Read {
                path: path.clone(),
                source,
            })?
        } else {
            let parent = path
                .parent()
                .ok_or_else(|| RawBlobError::InvalidRelativePath {
                    path: relative_path.to_path_buf(),
                })?;
            // 父目录可能尚未创建（例如首次写入前的 images 子目录），
            // 此时路径上不可能存在指向外部的符号链接，直接用字面路径做边界判断；
            // 若直接对不存在的父目录做规范化，整个扫描会因为读错误中断。
            match dunce::canonicalize(parent) {
                Ok(canonical_parent) => canonical_parent.join(path.file_name().unwrap_or_default()),
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => path.clone(),
                Err(source) => {
                    return Err(RawBlobError::Read {
                        path: parent.to_path_buf(),
                        source,
                    });
                }
            }
        };
        if !boundary_path.starts_with(&self.data_directory) {
            return Err(RawBlobError::InvalidRelativePath {
                path: relative_path.to_path_buf(),
            });
        }
        Ok(path)
    }

    fn verify_file(
        &self,
        path: &Path,
        expected_length: u64,
        expected_hash: &[u8; 32],
        compression: BlobCompression,
    ) -> Result<(), RawBlobError> {
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Err(RawBlobError::Missing {
                    path: path.to_path_buf(),
                });
            }
            Err(source) => {
                return Err(RawBlobError::Read {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        if !metadata.is_file() {
            return Err(RawBlobError::Corrupt {
                path: path.to_path_buf(),
                reason: "目标不是普通文件".to_owned(),
            });
        }
        if compression == BlobCompression::None && metadata.len() != expected_length {
            return Err(RawBlobError::Corrupt {
                path: path.to_path_buf(),
                reason: format!(
                    "期望长度 {expected_length} 字节，实际为 {} 字节",
                    metadata.len()
                ),
            });
        }
        let (actual_length, actual_hash) = match compression {
            BlobCompression::None => hash_file(path)?,
            BlobCompression::Zstd => hash_decompressed_file(path, expected_length)?,
        };
        if actual_length != expected_length {
            return Err(RawBlobError::Corrupt {
                path: path.to_path_buf(),
                reason: format!(
                    "解压后期望长度 {expected_length} 字节，实际为 {actual_length} 字节"
                ),
            });
        }
        if &actual_hash != expected_hash {
            return Err(RawBlobError::Corrupt {
                path: path.to_path_buf(),
                reason: "SHA-256 校验不一致".to_owned(),
            });
        }
        Ok(())
    }

    fn read_verified_file(
        &self,
        path: &Path,
        expected_length: u64,
        expected_hash: &[u8; 32],
        compression: BlobCompression,
    ) -> Result<Vec<u8>, RawBlobError> {
        let stored_bytes = fs::read(path).map_err(|source| RawBlobError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let bytes = match compression {
            BlobCompression::None => stored_bytes,
            BlobCompression::Zstd => {
                let capacity =
                    usize::try_from(expected_length).map_err(|_| RawBlobError::Corrupt {
                        path: path.to_path_buf(),
                        reason: "原始数据长度超出当前平台可寻址范围".to_owned(),
                    })?;
                zstd::bulk::decompress(&stored_bytes, capacity).map_err(|error| {
                    RawBlobError::Corrupt {
                        path: path.to_path_buf(),
                        reason: format!("Zstd 解压失败：{error}"),
                    }
                })?
            }
        };
        if bytes.len() as u64 != expected_length {
            return Err(RawBlobError::Corrupt {
                path: path.to_path_buf(),
                reason: format!(
                    "解压后期望长度 {expected_length} 字节，实际为 {} 字节",
                    bytes.len()
                ),
            });
        }
        let actual_hash = hash_bytes(&bytes);
        if &actual_hash != expected_hash {
            return Err(RawBlobError::Corrupt {
                path: path.to_path_buf(),
                reason: "SHA-256 校验不一致".to_owned(),
            });
        }
        Ok(bytes)
    }

    pub(crate) fn cleanup_pending_files(&self) -> Result<usize, RawBlobError> {
        let entries =
            fs::read_dir(&self.pending_directory).map_err(|source| RawBlobError::Scan {
                path: self.pending_directory.clone(),
                source,
            })?;
        let mut removed = 0;
        for entry in entries {
            let entry = entry.map_err(|source| RawBlobError::Scan {
                path: self.pending_directory.clone(),
                source,
            })?;
            let file_type = entry.file_type().map_err(|source| RawBlobError::Scan {
                path: entry.path(),
                source,
            })?;
            if file_type.is_file() || file_type.is_symlink() {
                self.remove_file(&entry.path())?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    fn remove_file(&self, path: &Path) -> Result<(), RawBlobError> {
        fs::remove_file(path).map_err(|source| RawBlobError::Delete {
            path: path.to_path_buf(),
            source,
        })
    }
}

fn hash_bytes(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn hash_file(path: &Path) -> Result<(u64, [u8; 32]), RawBlobError> {
    let mut file = File::open(path).map_err(|source| RawBlobError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let mut hasher = Sha256::new();
    let mut byte_length = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| RawBlobError::Read {
                path: path.to_path_buf(),
                source,
            })?;
        if read == 0 {
            break;
        }
        byte_length = byte_length.saturating_add(read as u64);
        hasher.update(&buffer[..read]);
    }
    Ok((byte_length, hasher.finalize().into()))
}

fn hash_decompressed_file(
    path: &Path,
    expected_length: u64,
) -> Result<(u64, [u8; 32]), RawBlobError> {
    let file = File::open(path).map_err(|source| RawBlobError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let mut decoder =
        zstd::stream::read::Decoder::new(file).map_err(|error| RawBlobError::Corrupt {
            path: path.to_path_buf(),
            reason: format!("Zstd 解压初始化失败：{error}"),
        })?;
    let mut hasher = Sha256::new();
    let mut byte_length = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = decoder
            .read(&mut buffer)
            .map_err(|error| RawBlobError::Corrupt {
                path: path.to_path_buf(),
                reason: format!("Zstd 解压失败：{error}"),
            })?;
        if read == 0 {
            break;
        }
        byte_length = byte_length.saturating_add(read as u64);
        if byte_length > expected_length {
            return Err(RawBlobError::Corrupt {
                path: path.to_path_buf(),
                reason: "解压后数据超过数据库记录的原始长度".to_owned(),
            });
        }
        hasher.update(&buffer[..read]);
    }
    Ok((byte_length, hasher.finalize().into()))
}

/// 判断数据目录中的文件名是否由本模块管理。
///
/// 除原有的 `.bin` 与 `.bin.zst` 外，还要覆盖图片原样落盘产生的扩展名，
/// 否则这些文件不会进入孤儿清理，日后调整扫描规则时也容易漏判。
fn is_managed_data_file(file_name: &str) -> bool {
    if file_name.ends_with(".bin") || file_name.ends_with(".bin.zst") {
        return true;
    }
    Path::new(file_name)
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(is_image_extension)
}

/// 递归收集数据目录中由本模块管理的文件，图片子目录下探一层。
///
/// 图片改为存放在 `data/images` 之后，只扫根目录会漏掉它们，
/// 孤儿文件就检测不出来，所以这里要跟着下探。
fn collect_managed_files(
    directory: &Path,
    relative_prefix: &Path,
    output: &mut Vec<PathBuf>,
) -> Result<(), RawBlobError> {
    let entries = fs::read_dir(directory).map_err(|source| RawBlobError::Scan {
        path: directory.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| RawBlobError::Scan {
            path: directory.to_path_buf(),
            source,
        })?;
        let file_type = entry.file_type().map_err(|source| RawBlobError::Scan {
            path: entry.path(),
            source,
        })?;
        let file_name = entry.file_name();
        let relative_path = relative_prefix.join(&file_name);
        if file_type.is_dir() {
            // 只下探图片子目录，其余目录不属于本模块管理的存储范围
            if file_name.to_string_lossy().as_ref() == IMAGES_DIRECTORY {
                collect_managed_files(&entry.path(), &relative_path, output)?;
            }
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        if is_managed_data_file(file_name.to_string_lossy().as_ref()) {
            output.push(relative_path);
        }
    }
    Ok(())
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn store() -> (tempfile::TempDir, RawBlobStore) {
        let directory = tempfile::tempdir().expect("无法创建测试临时目录");
        let store = RawBlobStore::new(directory.path()).expect("无法创建原始数据存储");
        (directory, store)
    }

    #[test]
    fn write_read_and_reuse_same_hash_file() {
        let (_directory, store) = store();
        let first_pending = store.write_pending(b"hello").expect("无法写入临时文件");
        let first = store.commit_pending(&first_pending).expect("无法提交文件");
        let second_pending = store.write_pending(b"hello").expect("无法再次写入临时文件");
        let second = store.commit_pending(&second_pending).expect("无法复用文件");

        assert_eq!(first, second);
        assert_eq!(store.read(&first).expect("无法读取文件"), b"hello");
        let bin_count = fs::read_dir(&store.data_directory)
            .expect("无法扫描数据目录")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.path().extension().and_then(|value| value.to_str()) == Some("bin")
            })
            .count();
        assert_eq!(bin_count, 1);
    }

    #[test]
    fn zstd_compression_round_trips_raw_data() {
        let (_directory, store) = store();
        let bytes = vec![0x5a; 256 * 1024];
        let pending = store.write_pending(&bytes).expect("无法写入压缩临时文件");
        assert_eq!(pending.compression, BlobCompression::Zstd);

        let blob = store.commit_pending(&pending).expect("无法提交压缩文件");
        assert!(blob.relative_path().to_string_lossy().ends_with(".bin.zst"));
        let path = store
            .resolve_relative_path(blob.relative_path())
            .expect("无法解析压缩文件路径");
        assert!(fs::metadata(path).expect("无法读取压缩文件信息").len() < bytes.len() as u64);
        assert_eq!(store.read(&blob).expect("无法读取压缩文件"), bytes);
    }

    #[test]
    fn corrupt_compressed_file_is_detected() {
        let (_directory, store) = store();
        let bytes = vec![0x5a; 256 * 1024];
        let pending = store.write_pending(&bytes).expect("无法写入压缩临时文件");
        let blob = store.commit_pending(&pending).expect("无法提交压缩文件");
        let path = store
            .resolve_relative_path(blob.relative_path())
            .expect("无法解析压缩文件路径");
        fs::write(&path, b"broken zstd data").expect("无法损坏压缩文件");

        assert!(matches!(
            store.verify(&blob),
            Err(RawBlobError::Corrupt { .. })
        ));
    }

    #[test]
    fn missing_and_corrupt_files_are_detected() {
        let (_directory, store) = store();
        let pending = store.write_pending(b"hello").expect("无法写入临时文件");
        let blob = store.commit_pending(&pending).expect("无法提交文件");
        let path = store
            .resolve_relative_path(blob.relative_path())
            .expect("无法解析路径");

        fs::write(&path, b"broken").expect("无法损坏测试文件");
        assert!(matches!(
            store.verify(&blob),
            Err(RawBlobError::Corrupt { .. })
        ));
        fs::remove_file(&path).expect("无法删除测试文件");
        assert!(matches!(
            store.verify(&blob),
            Err(RawBlobError::Missing { .. })
        ));
    }

    #[test]
    fn existing_target_file_must_pass_content_validation() {
        let (_directory, store) = store();
        let first_pending = store.write_pending(b"hello").expect("无法写入临时文件");
        let blob = store.commit_pending(&first_pending).expect("无法提交文件");
        let path = store
            .resolve_relative_path(blob.relative_path())
            .expect("无法解析路径");
        fs::write(&path, b"xxxxx").expect("无法损坏测试文件");
        let second_pending = store.write_pending(b"hello").expect("无法再次写入临时文件");

        assert!(matches!(
            store.commit_pending(&second_pending),
            Err(RawBlobError::Corrupt { .. })
        ));
    }

    #[test]
    fn reconcile_removes_stale_files_and_repairs_noncanonical_references() {
        let (_directory, store) = store();
        let stale_pending = store.write_pending(b"stale").expect("无法写入临时文件");
        assert_eq!(stale_pending.byte_length(), 5);
        let pending = store.write_pending(b"hello").expect("无法写入正式数据");
        let blob = store.commit_pending(&pending).expect("无法提交正式数据");
        let mismatched = RawBlob::from_record(
            *blob.sha256(),
            blob.byte_length(),
            PathBuf::from("data").join("noncanonical.bin"),
        );

        let report = store.reconcile(&[mismatched]).expect("恢复扫描失败");

        assert_eq!(report.pending_files_removed, 1);
        assert_eq!(report.recovered_blobs, vec![blob]);
        assert!(report.missing_blobs.is_empty());
        assert!(report.corrupt_blobs.is_empty());
        assert!(report.orphan_files.is_empty());
    }

    #[test]
    fn path_traversal_is_rejected() {
        let (_directory, store) = store();
        let blob = RawBlob::from_record([0; 32], 0, "../outside.bin");

        assert!(matches!(
            store.read(&blob),
            Err(RawBlobError::InvalidRelativePath { .. })
        ));
    }

    #[test]
    fn raw_image_extension_is_kept_without_compression() {
        let (_directory, store) = store();
        // 重复字节压缩率极高，若走压缩路径一定会变成 .bin.zst
        let bytes = vec![0x5a; 256 * 1024];
        let pending = store
            .write_pending_raw(&bytes, "png")
            .expect("无法写入原样图片");
        assert_eq!(pending.compression, BlobCompression::None);

        let blob = store.commit_pending(&pending).expect("无法提交原样图片");
        // 图片落到独立的 images 子目录，避免和哈希命名的压缩块混在一起
        assert_eq!(
            blob.relative_path(),
            Path::new("data/images").join(format!("{}.png", encode_hex(blob.sha256())))
        );
        assert_eq!(store.read(&blob).expect("无法读取原样图片"), bytes);
    }

    #[test]
    fn reconcile_scans_image_directory() {
        let (_directory, store) = store();
        let bytes = vec![0x5a; 256 * 1024];
        let pending = store
            .write_pending_raw(&bytes, "png")
            .expect("无法写入原样图片");
        let blob = store.commit_pending(&pending).expect("无法提交原样图片");

        // 被引用的图片文件不算孤儿
        let report = store
            .reconcile(std::slice::from_ref(&blob))
            .expect("扫描失败");
        assert!(report.verified_blobs.contains(&blob));
        assert!(report.orphan_files.is_empty());

        // 子目录里未被引用的图片仍能被识别为孤儿
        let orphan = blob.relative_path().with_file_name("ffffffff.png");
        let orphan_path = store.root.join(&orphan);
        fs::copy(store.root.join(blob.relative_path()), &orphan_path).expect("无法复制图片");
        let report = store
            .reconcile(std::slice::from_ref(&blob))
            .expect("扫描失败");
        assert_eq!(report.orphan_files, vec![orphan]);
    }

    #[test]
    fn image_extension_is_detected_from_header() {
        assert_eq!(
            image_extension_from_header(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00]),
            Some("png")
        );
        assert_eq!(
            image_extension_from_header(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some("jpg")
        );
        assert_eq!(image_extension_from_header(b"GIF89a......"), Some("gif"));
        assert_eq!(
            image_extension_from_header(b"RIFF\x00\x00\x00\x00WEBP"),
            Some("webp")
        );
        assert_eq!(image_extension_from_header(b"plain text"), None);
    }

    #[test]
    fn image_extension_rank_prefers_lossless_then_webp() {
        assert!(image_extension_rank("png") < image_extension_rank("webp"));
        assert!(image_extension_rank("webp") < image_extension_rank("jpg"));
        assert_eq!(image_extension_rank("jpg"), image_extension_rank("jpeg"));
    }

    #[test]
    fn only_unreferenced_files_are_deleted() {
        let (_directory, store) = store();
        let pending = store.write_pending(b"hello").expect("无法写入临时文件");
        let blob = store.commit_pending(&pending).expect("无法提交文件");

        assert!(
            !store
                .delete_if_unreferenced(&blob, 1)
                .expect("检查引用失败")
        );
        assert!(store.verify(&blob).is_ok());
        assert!(
            store
                .delete_if_unreferenced(&blob, 0)
                .expect("删除文件失败")
        );
        assert!(matches!(
            store.verify(&blob),
            Err(RawBlobError::Missing { .. })
        ));
    }
}
