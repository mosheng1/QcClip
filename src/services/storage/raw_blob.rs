use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use super::path::{path_is_certainly_missing, path_is_certainly_present};
use crate::services::data_runtime;

const DATA_DIRECTORY: &str = "data";
const PENDING_DIRECTORY: &str = ".pending";
const ZSTD_COMPRESSION_LEVEL: i32 = 3;

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

    fn file_name(self, sha256: &[u8; 32]) -> String {
        match self {
            Self::None => format!("{}.bin", encode_hex(sha256)),
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
    /// 仅探测原始文件；无法确认路径边界或所在卷不可用时不能断定文件已丢失。
    pub(crate) fn original_file_is_missing(&self, relative_path: &Path) -> bool {
        self.resolve_relative_path(relative_path)
            .ok()
            .is_some_and(|path| path_is_certainly_missing(&path))
    }

    pub(crate) fn original_file_is_present(&self, relative_path: &Path) -> bool {
        self.resolve_relative_path(relative_path)
            .ok()
            .is_some_and(|path| path_is_certainly_present(&path))
    }

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
        let _operation =
            data_runtime::begin_operation().map_err(|_| RawBlobError::DataSwitching)?;
        let sha256 = hash_bytes(bytes);
        let compressed =
            zstd::bulk::compress(bytes, ZSTD_COMPRESSION_LEVEL).map_err(RawBlobError::Compress)?;
        let (stored_bytes, compression) = if compressed.len() < bytes.len() {
            (compressed.as_slice(), BlobCompression::Zstd)
        } else {
            (bytes, BlobCompression::None)
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

        let blob = self.canonical_blob(pending.sha256, pending.byte_length, pending.compression);
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
                    let canonical_blob =
                        self.canonical_blob(blob.sha256, blob.byte_length, blob.compression());
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

        let entries = fs::read_dir(&self.data_directory).map_err(|source| RawBlobError::Scan {
            path: self.data_directory.clone(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| RawBlobError::Scan {
                path: self.data_directory.clone(),
                source,
            })?;
            let file_type = entry.file_type().map_err(|source| RawBlobError::Scan {
                path: entry.path(),
                source,
            })?;
            let file_name = entry.file_name();
            let file_name = file_name.to_string_lossy();
            if !file_type.is_file()
                || !(file_name.ends_with(".bin") || file_name.ends_with(".bin.zst"))
            {
                continue;
            }
            let relative_path = PathBuf::from(DATA_DIRECTORY).join(entry.file_name());
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
    ) -> RawBlob {
        RawBlob {
            sha256,
            byte_length,
            relative_path: PathBuf::from(DATA_DIRECTORY).join(compression.file_name(&sha256)),
        }
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
            let canonical_parent =
                dunce::canonicalize(parent).map_err(|source| RawBlobError::Read {
                    path: parent.to_path_buf(),
                    source,
                })?;
            canonical_parent.join(path.file_name().unwrap_or_default())
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

    #[cfg(unix)]
    #[test]
    fn missing_probe_rejects_symlinked_parent_outside_data_directory() {
        use std::os::unix::fs::symlink;

        let (directory, store) = store();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), store.data_directory.join("escape")).unwrap();
        assert!(!store.original_file_is_missing(Path::new("data/escape/missing.bin")));
        assert!(store.original_file_is_missing(Path::new("data/missing.bin")));
        assert!(directory.path().join("data").exists());
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
