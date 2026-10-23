use std::{
    collections::HashSet,
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use atomic_write_file::AtomicWriteFile;
use gpui::{App, Global};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    app_paths::write_location,
    config::HistoryConfig,
    services::{
        database::{Database, HistoryCleanupReport, HistoryRepository},
        storage::{RawBlobError, RawBlobStore},
    },
};

const MAGIC: &[u8] = b"QCCLIPDATA1\n";
const IMPORT_DIRECTORY: &str = ".import-pending";
const IMPORT_MARKER: &str = "import.marker";
const MAX_PATH_LENGTH: u32 = 32 * 1024;
const MAX_ENTRIES: u32 = 1_000_000;
const MAX_ENTRY_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const FILE_OPERATION_RETRIES: usize = 30;
const FILE_OPERATION_RETRY_MILLIS: u64 = 100;
const MANAGED_ROOT_FILES: &[&str] = &[
    "qcclip.sqlite3",
    "qcclip.sqlite3-wal",
    "qcclip.sqlite3-shm",
    "config.json",
    "state.json",
];
const MANAGED_ROOT_DIRECTORIES: &[&str] = &["data", "image_library", "sound"];
const DATA_LOCATION_DIRECTORIES: &[&str] = &["data", "image_library", "sound", "log"];

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DataManagementStats {
    pub history_count: u64,
    pub pinned_history_count: u64,
    pub in_scope_history_count: u64,
    pub retention_limit_used_count: u64,
    pub over_limit_history_count: u64,
    pub pending_cleanup_count: u64,
    pub favorite_count: u64,
    pub tag_count: u64,
    pub content_type_counts: [u64; 5],
    pub favorite_content_type_counts: [u64; 5],
    pub source_app_counts: Vec<SourceAppStats>,
    pub total_bytes: u64,
    pub database_bytes: u64,
    pub raw_data_bytes: u64,
    pub image_library_bytes: u64,
    pub sound_bytes: u64,
    pub other_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceAppStats {
    pub name: String,
    pub count: u64,
    pub icon_path: Option<PathBuf>,
}

#[derive(Debug, Error)]
pub enum DataManagementError {
    #[error("数据管理数据库操作失败：{0}")]
    Database(#[from] crate::services::database::DatabaseError),
    #[error("数据管理 SQL 操作失败：{0}")]
    Sql(#[from] rusqlite::Error),
    #[error("数据管理文件操作失败：{0}")]
    Io(#[from] io::Error),
    #[error("数据管理原始存储操作失败：{0}")]
    Storage(#[from] RawBlobError),
    #[error("历史清理失败：{0}")]
    Cleanup(#[from] crate::services::database::HistoryCleanupError),
    #[error("导入数据文件格式无效：{0}")]
    InvalidDataFile(String),
    #[error("导出路径无效：{0}")]
    InvalidPath(String),
    #[error("导入失败（{operation}），且恢复原数据失败（{rollback}）")]
    ImportRollback {
        operation: io::Error,
        rollback: io::Error,
    },
    #[error("重置失败（{operation}），且恢复原数据失败（{rollback}）")]
    ResetRollback {
        operation: String,
        rollback: io::Error,
    },
    #[error("数据正在导入，请稍后重试")]
    DataSwitching,
    #[error("当前程序无法完整处理较新数据库的资源，切换数据目录和导出请使用新版程序")]
    UnsupportedResources,
    #[error("便携版的数据目录固定在程序旁，不允许更改数据位置")]
    PortableDataLocation,
}

#[derive(Clone)]
pub struct DataManagementService {
    database: Database,
    blob_store: RawBlobStore,
    data_root: PathBuf,
    default_data_root: PathBuf,
    location_path: PathBuf,
    portable: bool,
}

pub(crate) struct PreparedDataLocation {
    pub(crate) database: Database,
    pub(crate) blob_store: RawBlobStore,
    pub(crate) data_root: PathBuf,
    pub(crate) cleanup_error: Option<String>,
    pub(crate) switch_guard: super::data_runtime::DataSwitchGuard,
}

impl Global for DataManagementService {}

impl DataManagementService {
    #[cfg(test)]
    pub(crate) fn legacy_test_service(root: &Path) -> Self {
        Self {
            database: Database::open(root.join("qcclip.sqlite3")).expect("创建测试数据库失败"),
            blob_store: RawBlobStore::new(root).expect("创建测试二进制存储失败"),
            data_root: root.to_path_buf(),
            default_data_root: root.to_path_buf(),
            location_path: root.join("location.json"),
            portable: false,
        }
    }

    pub fn install(
        database: Database,
        blob_store: RawBlobStore,
        data_root: impl Into<PathBuf>,
        default_data_root: impl Into<PathBuf>,
        location_path: impl Into<PathBuf>,
        portable: bool,
        cx: &mut App,
    ) {
        super::data_runtime::initialize();
        cx.set_global(Self {
            database,
            blob_store,
            data_root: data_root.into(),
            default_data_root: default_data_root.into(),
            location_path: location_path.into(),
            portable,
        });
    }

    pub fn stats(
        &self,
        policy: &HistoryConfig,
    ) -> Result<DataManagementStats, DataManagementError> {
        let _operation = super::data_runtime::begin_operation()
            .map_err(|_| DataManagementError::DataSwitching)?;
        let repository = HistoryRepository::new(self.database.clone(), self.blob_store.clone());
        let retention_counts: crate::services::database::HistoryRetentionCounts =
            repository.retention_counts(policy)?;
        let mut stats = DataManagementStats {
            history_count: u64::try_from(
                retention_counts
                    .in_scope_items
                    .saturating_add(retention_counts.pending_cleanup_items),
            )
            .unwrap_or(0),
            pinned_history_count: u64::try_from(
                retention_counts
                    .in_scope_items
                    .saturating_sub(retention_counts.in_scope_unpinned_items),
            )
            .unwrap_or(0),
            in_scope_history_count: u64::try_from(retention_counts.in_scope_items).unwrap_or(0),
            retention_limit_used_count: u64::try_from(retention_counts.in_scope_unpinned_items)
                .unwrap_or(0),
            over_limit_history_count: u64::try_from(retention_counts.over_limit_items).unwrap_or(0),
            pending_cleanup_count: u64::try_from(retention_counts.pending_cleanup_items)
                .unwrap_or(0),
            favorite_count: {
                let connection = self.database.connect()?;
                let count: i64 = connection.query_row(
                    "SELECT COUNT(*)
                     FROM favorites AS f
                     JOIN clipboard_items AS i ON i.id = f.item_id
                     WHERE i.capture_state = 'ready'",
                    [],
                    |row| row.get(0),
                )?;
                u64::try_from(count).unwrap_or(0)
            },
            tag_count: {
                let connection = self.database.connect()?;
                let count: i64 =
                    connection.query_row("SELECT COUNT(*) FROM tags", [], |row| row.get(0))?;
                u64::try_from(count).unwrap_or(0)
            },
            content_type_counts: {
                let connection = self.database.connect()?;
                content_type_counts(&connection, false)?
            },
            favorite_content_type_counts: {
                let connection = self.database.connect()?;
                content_type_counts(&connection, true)?
            },
            source_app_counts: {
                let connection = self.database.connect()?;
                let mut statement = connection.prepare(
                    "SELECT COALESCE(NULLIF(TRIM(h.last_source_app_name), ''), '') AS source_name,
                            COUNT(*), MAX(a.relative_path)
                     FROM clipboard_history AS h
                     JOIN clipboard_items AS i ON i.id = h.item_id
                     LEFT JOIN application_icon_assets AS a ON a.id = h.last_source_app_icon_id
                     WHERE i.capture_state = 'ready'
                     GROUP BY source_name
                     ORDER BY COUNT(*) DESC, source_name COLLATE NOCASE ASC",
                )?;
                let rows = statement.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })?;
                rows.map(|row| {
                    let (name, count, icon_path) = row?;
                    Ok(SourceAppStats {
                        name,
                        count: u64::try_from(count).unwrap_or(0),
                        icon_path: icon_path.and_then(|path| {
                            let path = self.data_root.join(path);
                            path.is_file().then_some(path)
                        }),
                    })
                })
                .collect::<Result<Vec<_>, rusqlite::Error>>()?
            },
            ..Default::default()
        };
        for path in managed_files(&self.data_root)? {
            let size = match fs::metadata(&path) {
                Ok(metadata) => metadata.len(),
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            stats.total_bytes = stats.total_bytes.saturating_add(size);
            let relative = path.strip_prefix(&self.data_root).unwrap_or(&path);
            match relative.components().next() {
                Some(Component::Normal(name))
                    if name == "qcclip.sqlite3"
                        || name == "qcclip.sqlite3-wal"
                        || name == "qcclip.sqlite3-shm" =>
                {
                    stats.database_bytes = stats.database_bytes.saturating_add(size)
                }
                Some(Component::Normal(name)) if name == "data" => {
                    stats.raw_data_bytes = stats.raw_data_bytes.saturating_add(size)
                }
                Some(Component::Normal(name)) if name == "image_library" => {
                    stats.image_library_bytes = stats.image_library_bytes.saturating_add(size)
                }
                Some(Component::Normal(name)) if name == "sound" => {
                    stats.sound_bytes = stats.sound_bytes.saturating_add(size)
                }
                _ => stats.other_bytes = stats.other_bytes.saturating_add(size),
            }
        }
        Ok(stats)
    }

    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub(crate) fn can_change_data_location(&self) -> bool {
        !self.portable
    }

    pub fn is_default_data_root(&self) -> bool {
        paths_equal(&self.data_root, &self.default_data_root)
    }

    pub fn default_data_root(&self) -> &Path {
        &self.default_data_root
    }

    pub(crate) fn replace_runtime_data(
        &mut self,
        database: Database,
        blob_store: RawBlobStore,
        data_root: PathBuf,
    ) {
        self.database = database;
        self.blob_store = blob_store;
        self.data_root = data_root;
    }

    pub(crate) fn prepare_data_location_switch(
        &self,
        target: &Path,
    ) -> Result<PreparedDataLocation, DataManagementError> {
        if !self.can_change_data_location() {
            return Err(DataManagementError::PortableDataLocation);
        }
        let connection = self.database.connect()?;
        if !Database::maintenance_supported(&connection)? {
            return Err(DataManagementError::UnsupportedResources);
        }
        connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        drop(connection);

        let switch_guard = super::data_runtime::begin_switch();
        if let Err(error) = migrate_data_root(&self.data_root, target) {
            drop(switch_guard);
            return Err(error);
        }
        let target = target.to_path_buf();
        let database = match Database::open(target.join("qcclip.sqlite3")) {
            Ok(database) => database,
            Err(error) => {
                drop(switch_guard);
                return Err(error.into());
            }
        };
        let blob_store = match RawBlobStore::new(&target) {
            Ok(blob_store) => blob_store,
            Err(error) => {
                drop(switch_guard);
                return Err(error.into());
            }
        };
        if let Err(error) = crate::logging::switch_directory(&target.join("log")) {
            drop(switch_guard);
            return Err(error.into());
        }
        if let Err(error) = write_location(&self.location_path, &target) {
            if let Err(rollback_error) =
                crate::logging::switch_directory(&self.data_root.join("log"))
            {
                tracing::error!(%rollback_error, "恢复原日志目录失败");
            }
            drop(switch_guard);
            return Err(error.into());
        }
        let old_data_root = self.data_root.clone();
        let cleanup_error = cleanup_old_data_root(&old_data_root)
            .err()
            .map(|error| error.to_string());
        if let Some(error) = &cleanup_error {
            tracing::warn!(%error, "清理旧数据目录失败，已保留旧目录");
        }
        Ok(PreparedDataLocation {
            database,
            blob_store,
            data_root: target,
            cleanup_error,
            switch_guard,
        })
    }

    pub fn cleanup_history(
        &self,
        policy: &HistoryConfig,
    ) -> Result<HistoryCleanupReport, DataManagementError> {
        super::data_runtime::with_exclusive_operation(|| {
            let repository = HistoryRepository::new(self.database.clone(), self.blob_store.clone());
            let report = repository.cleanup_out_of_scope(policy)?;
            if let Err(error) = self.reclaim_database_space() {
                tracing::warn!("历史清理已完成，但数据库空间回收失败：{error}");
            }
            Ok(report)
        })
    }

    pub fn cleanup_history_on_startup(
        &self,
        policy: &HistoryConfig,
    ) -> Result<HistoryCleanupReport, DataManagementError> {
        if !policy.cleanup_on_startup {
            return Ok(HistoryCleanupReport::default());
        }
        self.cleanup_history(policy)
    }

    pub fn clear_all_history(&self) -> Result<HistoryCleanupReport, DataManagementError> {
        self.clear_history(false)
    }

    pub fn clear_unpinned_history(&self) -> Result<HistoryCleanupReport, DataManagementError> {
        self.clear_history(true)
    }

    fn clear_history(
        &self,
        keep_pinned: bool,
    ) -> Result<HistoryCleanupReport, DataManagementError> {
        super::data_runtime::with_exclusive_operation(|| {
            let repository = HistoryRepository::new(self.database.clone(), self.blob_store.clone());
            let report = if keep_pinned {
                repository.clear_unpinned()?
            } else {
                repository.clear_all()?
            };
            if let Err(error) = self.reclaim_database_space() {
                tracing::warn!("历史清理已完成，但数据库空间回收失败：{error}");
            }
            Ok(report)
        })
    }

    fn reclaim_database_space(&self) -> Result<(), DataManagementError> {
        let connection = self.database.connect()?;
        if !Database::maintenance_supported(&connection)? {
            return Ok(());
        }
        let free_pages: i64 =
            connection.query_row("PRAGMA freelist_count", [], |row| row.get(0))?;
        if free_pages > 0 {
            // 删除记录只释放数据库内部页，需要在删除事务提交后重建文件以归还磁盘空间。
            connection.execute_batch("VACUUM;")?;
        }
        let (busy, _, _): (i64, i64, i64) =
            connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?;
        if busy != 0 {
            tracing::warn!("历史清理已完成，但数据库仍被占用，日志空间暂未完全回收");
        }
        Ok(())
    }

    pub(crate) fn prepare_application_reset(
        &self,
    ) -> Result<PreparedDataLocation, DataManagementError> {
        let target = self.default_data_root.clone();
        fs::create_dir_all(&target)?;
        let staging = tempfile::Builder::new()
            .prefix(".reset-staging-")
            .tempdir_in(&target)?;
        Database::open(staging.path().join("qcclip.sqlite3"))?;
        let switch_guard = super::data_runtime::begin_switch();
        let roots = if paths_equal(&self.data_root, &target) {
            vec![target.clone()]
        } else {
            vec![self.data_root.clone(), target.clone()]
        };
        if roots.len() == 2 {
            let current = dunce::canonicalize(&self.data_root)?;
            let default = dunce::canonicalize(&target)?;
            if current.starts_with(&default) || default.starts_with(&current) {
                return Err(DataManagementError::InvalidPath(
                    "当前数据目录与默认数据目录相互嵌套，无法安全重置".to_owned(),
                ));
            }
        }
        let mut backups = Vec::new();
        let mut moved_current = Vec::new();
        let new_paths = MANAGED_ROOT_FILES
            .iter()
            .copied()
            .chain(DATA_LOCATION_DIRECTORIES.iter().copied())
            .map(|name| target.join(name))
            .collect::<Vec<_>>();
        let mut installed = false;
        let (database, blob_store) =
            crate::logging::with_directory_switch(&target.join("log"), || {
                let result = (|| -> Result<(Database, RawBlobStore), DataManagementError> {
                    // 先将旧数据移到同磁盘暂存目录，任一步失败时可恢复原文件。
                    for root in &roots {
                        let entries = fs::read_dir(root)?.collect::<Result<Vec<_>, _>>()?;
                        let backup = tempfile::Builder::new()
                            .prefix(".reset-rollback-")
                            .tempdir_in(root)?;
                        let backup_path = backup.path().to_path_buf();
                        backups.push(backup);
                        for entry in entries {
                            if entry.path() == staging.path() {
                                continue;
                            }
                            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                            if is_location_managed_root_entry(&name)
                                || is_sqlite_shared_memory_artifact(&entry.file_name())
                            {
                                let old = backup_path.join(entry.file_name());
                                rename_with_retry(&entry.path(), &old)?;
                                moved_current.push((entry.path(), old));
                            }
                        }
                    }
                    installed = true;
                    rename_with_retry(
                        &staging.path().join("qcclip.sqlite3"),
                        &target.join("qcclip.sqlite3"),
                    )?;
                    let database = Database::open(target.join("qcclip.sqlite3"))?;
                    let blob_store = RawBlobStore::new(&target)?;
                    fs::create_dir_all(target.join("log"))?;
                    if !self.portable {
                        match fs::remove_file(&self.location_path) {
                            Ok(()) => {}
                            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                            Err(error) => return Err(error.into()),
                        }
                    }
                    Ok((database, blob_store))
                })();
                match result {
                    Ok(sources) => Ok(sources),
                    Err(error) => {
                        let rollback = rollback_import(
                            if installed { &new_paths } else { &[] },
                            &moved_current,
                        );
                        if let Err(rollback) = rollback {
                            for backup in backups.drain(..) {
                                let _ = backup.keep();
                            }
                            return Err(DataManagementError::ResetRollback {
                                operation: error.to_string(),
                                rollback,
                            });
                        }
                        Err(error)
                    }
                }
            })?;
        let mut cleanup_error = None;
        for backup in backups {
            if let Err(error) = backup.close() {
                tracing::error!(%error, "重置后清除旧数据失败");
                cleanup_error.get_or_insert(error.to_string());
            }
        }
        Ok(PreparedDataLocation {
            database,
            blob_store,
            data_root: target,
            cleanup_error,
            switch_guard,
        })
    }

    pub fn export_data(&self, destination: &Path) -> Result<(), DataManagementError> {
        let _operation = super::data_runtime::begin_operation()
            .map_err(|_| DataManagementError::DataSwitching)?;
        let destination = destination.to_path_buf();
        if let Some(parent) = destination
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        if path_is_within_root(&destination, &self.data_root)? {
            return Err(DataManagementError::InvalidPath(
                "导出文件不能保存到应用数据目录内".to_owned(),
            ));
        }
        // 数据库使用 WAL。导出前先做检查点，避免把正在变化的 WAL 文件和主库分别复制。
        let connection = self.database.connect()?;
        if !Database::maintenance_supported(&connection)? {
            return Err(DataManagementError::UnsupportedResources);
        }
        connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        drop(connection);
        let mut file = AtomicWriteFile::open(destination)?;
        file.write_all(MAGIC)?;
        let entries = managed_files(&self.data_root)?
            .into_iter()
            .filter(|path| {
                path.file_name().and_then(|name| name.to_str()) != Some("qcclip.sqlite3-shm")
            })
            .collect::<Vec<_>>();
        if entries.len() > MAX_ENTRIES as usize {
            return Err(DataManagementError::InvalidPath(
                "受管理文件数量超过导出格式限制".to_owned(),
            ));
        }
        file.write_all(&(entries.len() as u32).to_le_bytes())?;
        for path in entries {
            let relative = path
                .strip_prefix(&self.data_root)
                .map_err(|_| DataManagementError::InvalidPath(path.display().to_string()))?;
            let relative = relative.to_string_lossy().replace('\\', "/");
            let path_bytes = relative.as_bytes();
            if path_bytes.len() > MAX_PATH_LENGTH as usize {
                return Err(DataManagementError::InvalidPath(relative));
            }
            let byte_length = fs::metadata(&path)?.len();
            if byte_length > MAX_ENTRY_BYTES {
                return Err(DataManagementError::InvalidPath(relative));
            }
            let digest = hash_file(&path)?;
            file.write_all(&(path_bytes.len() as u32).to_le_bytes())?;
            file.write_all(&byte_length.to_le_bytes())?;
            file.write_all(&digest)?;
            file.write_all(path_bytes)?;
            let mut input = File::open(&path)?;
            let actual_digest = copy_file_contents(&mut input, file.as_file_mut(), byte_length)?;
            if actual_digest != digest {
                return Err(DataManagementError::InvalidDataFile(format!(
                    "导出期间文件发生变化：{relative}"
                )));
            }
        }
        file.as_file().sync_all()?;
        file.commit()?;
        Ok(())
    }

    pub fn stage_import(&self, archive: &Path) -> Result<(), DataManagementError> {
        let pending_root = self.data_root.join(IMPORT_DIRECTORY);
        fs::create_dir_all(&pending_root)?;
        let marker = pending_root.join(IMPORT_MARKER);
        if fs::symlink_metadata(&marker).is_ok() {
            return Err(DataManagementError::InvalidDataFile(
                "已有导入等待应用，请重启应用后再导入".to_owned(),
            ));
        }
        let staging = pending_root.join(Uuid::new_v4().to_string());
        fs::create_dir_all(&staging)?;
        let marker_temp = pending_root.join(format!("{}.tmp", Uuid::new_v4()));
        let result = (|| {
            extract_archive(archive, &staging)?;
            let _ = Database::open(staging.join("qcclip.sqlite3"))?;
            remove_sqlite_shared_memory_artifacts(&staging)?;
            fs::write(
                &marker_temp,
                staging
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .as_bytes(),
            )?;
            fs::rename(&marker_temp, &marker)?;
            Ok::<_, DataManagementError>(())
        })();
        if let Err(error) = result {
            let _ = fs::remove_file(&marker_temp);
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
        Ok(())
    }

    pub fn suggested_export_file_name(&self) -> String {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_millis())
            .unwrap_or_default();
        format!("qcclip-{timestamp}.qcclip-data")
    }
}

fn content_type_counts(
    connection: &Connection,
    favorites_only: bool,
) -> Result<[u64; 5], rusqlite::Error> {
    // 收藏关系可以在历史关系被清理后继续存在，因此收藏统计不能从 clipboard_history 起算。
    let relation_sql = if favorites_only {
        "SELECT fav.id, fav.item_id
         FROM favorites AS fav
         JOIN clipboard_items AS relation_item ON relation_item.id = fav.item_id
         WHERE relation_item.capture_state = 'ready'"
    } else {
        "SELECT h.id, h.item_id
         FROM clipboard_history AS h
         JOIN clipboard_items AS relation_item ON relation_item.id = h.item_id
         WHERE relation_item.capture_state = 'ready'"
    };
    let sql = format!(
        "WITH relation_types AS (
             SELECT relation.id,
                    MAX(CASE WHEN i.content_kind = 'text'
                                  OR f.preview_content_kind = 'text'
                                  OR (f.format_kind IN ('text', 'html', 'rtf')
                                      AND f.preview_content_kind IS NULL)
                                  OR (f.format_kind = 'html'
                                      AND f.preview_content_kind = 'mixed')
                             THEN 1 ELSE 0 END) AS has_text,
                    MAX(CASE WHEN i.content_kind = 'image'
                                  OR f.format_kind = 'image'
                                  OR f.preview_content_kind IN ('image', 'mixed')
                             THEN 1 ELSE 0 END) AS has_image,
                    MAX(CASE WHEN i.content_kind = 'file_list'
                                  OR f.format_kind = 'file_list'
                                  OR f.preview_content_kind = 'file_list'
                             THEN 1 ELSE 0 END) AS has_file,
                    MAX(CASE WHEN i.content_kind = 'link'
                                  OR f.preview_content_kind = 'link'
                             THEN 1 ELSE 0 END) AS has_link
             FROM ({relation_sql}) AS relation
             JOIN clipboard_items AS i ON i.id = relation.item_id
             LEFT JOIN clipboard_formats AS f ON f.item_id = relation.item_id
             GROUP BY relation.id
         )
         SELECT
             COALESCE(SUM(has_text), 0),
             COALESCE(SUM(has_image), 0),
             COALESCE(SUM(has_file), 0),
             COALESCE(SUM(has_link), 0),
             COALESCE(SUM(
                 CASE WHEN has_text = 0
                           AND has_image = 0
                           AND has_file = 0
                           AND has_link = 0
                      THEN 1 ELSE 0 END
             ), 0)
         FROM relation_types"
    );
    let counts: [i64; 5] = connection.query_row(&sql, [], |row| {
        Ok([
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
        ])
    })?;
    Ok(counts.map(|count| u64::try_from(count).unwrap_or(0)))
}

/// 在数据库打开前应用上一次由设置页暂存的导入内容。
pub fn apply_pending_import(data_root: &Path) -> Result<bool, DataManagementError> {
    let _switch = super::data_runtime::begin_switch();
    apply_pending_import_while_paused(data_root)
}

pub(crate) fn apply_pending_import_for_reload(
    data_root: &Path,
) -> Result<(bool, super::data_runtime::DataSwitchGuard), DataManagementError> {
    let switch = super::data_runtime::begin_switch();
    let applied = apply_pending_import_while_paused(data_root)?;
    Ok((applied, switch))
}

fn apply_pending_import_while_paused(data_root: &Path) -> Result<bool, DataManagementError> {
    let pending_root = data_root.join(IMPORT_DIRECTORY);
    let marker = pending_root.join(IMPORT_MARKER);
    match fs::symlink_metadata(&marker) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => {
            return Err(DataManagementError::InvalidDataFile(
                "导入暂存标记不是普通文件".to_owned(),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            cleanup_unmarked_import_data(&pending_root);
            return Ok(false);
        }
        Err(error) => return Err(error.into()),
    }
    let name = fs::read_to_string(&marker)?.trim().to_owned();
    let staging_id = Uuid::parse_str(&name)
        .map_err(|_| DataManagementError::InvalidDataFile("导入暂存标记无效".to_owned()))?;
    let staging = pending_root.join(staging_id.to_string());
    if !staging.is_dir() {
        return Err(DataManagementError::InvalidDataFile(
            "导入暂存目录不存在".to_owned(),
        ));
    }
    let mut staged_entries = Vec::new();
    for entry in fs::read_dir(&staging)? {
        let entry = entry?;
        let name = entry.file_name();
        if is_importable_root_entry(&name) {
            staged_entries.push((entry.path(), name));
        }
    }
    let has_staged_entry = |name: &str| {
        staged_entries
            .iter()
            .any(|(_, staged_name)| staged_name.to_string_lossy().eq_ignore_ascii_case(name))
    };
    let rollback = pending_root.join(format!("rollback-{}", Uuid::new_v4()));
    fs::create_dir_all(&rollback).map_err(|error| io_with_path(error, &rollback))?;
    let mut moved_current = Vec::new();
    for name in MANAGED_ROOT_FILES
        .iter()
        .copied()
        .chain(MANAGED_ROOT_DIRECTORIES.iter().copied())
    {
        // 数据库主文件是导入必需项；即使归档没有 WAL/SHM，也要清理当前侧文件，
        // 避免它们与新的数据库主文件混用。其他文件缺失时保留当前内容。
        let database_sidecar = matches!(
            name,
            "qcclip.sqlite3" | "qcclip.sqlite3-wal" | "qcclip.sqlite3-shm"
        );
        if !database_sidecar && !has_staged_entry(name) {
            continue;
        }
        let current = data_root.join(name);
        let current_exists = match fs::symlink_metadata(&current) {
            Ok(_) => true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => {
                let operation = io_with_path(error, &current);
                return Err(match restore_rollback(&moved_current) {
                    Ok(()) => operation.into(),
                    Err(rollback) => DataManagementError::ImportRollback {
                        operation,
                        rollback,
                    },
                });
            }
        };
        if current_exists {
            let old = rollback.join(name);
            if let Err(error) = rename_with_retry(&current, &old) {
                let error = io_with_path(error, &current);
                return Err(match restore_rollback(&moved_current) {
                    Ok(()) => error.into(),
                    Err(rollback) => DataManagementError::ImportRollback {
                        operation: error,
                        rollback,
                    },
                });
            }
            moved_current.push((current, old));
        }
    }
    let mut copied_staged = Vec::new();
    for (source, name) in staged_entries {
        let target = data_root.join(name);
        copied_staged.push(target.clone());
        if let Err(error) = copy_import_entry(&source, &target) {
            return Err(match rollback_import(&copied_staged, &moved_current) {
                Ok(()) => error.into(),
                Err(rollback) => DataManagementError::ImportRollback {
                    operation: error,
                    rollback,
                },
            });
        }
    }
    // 归档不包含 blob 暂存目录；提交导入前重建它，让原有服务句柄继续可写。
    // 首次启动导入纯内联数据时也可能新建 data，失败回滚须清理这个新目录。
    let blob_data = data_root.join("data");
    if matches!(fs::symlink_metadata(&blob_data), Err(error) if error.kind() == io::ErrorKind::NotFound)
    {
        copied_staged.push(blob_data);
    }
    if let Err(error) = RawBlobStore::new(data_root) {
        return Err(match rollback_import(&copied_staged, &moved_current) {
            Ok(()) => error.into(),
            Err(rollback) => DataManagementError::ImportRollback {
                operation: io::Error::other(error),
                rollback,
            },
        });
    }
    if let Err(error) = fs::remove_file(&marker) {
        let error = io_with_path(error, &marker);
        return Err(match rollback_import(&copied_staged, &moved_current) {
            Ok(()) => error.into(),
            Err(rollback) => DataManagementError::ImportRollback {
                operation: error,
                rollback,
            },
        });
    }
    if let Err(error) = fs::remove_dir_all(&staging) {
        tracing::warn!(%error, "清理已完成的导入暂存目录失败");
    }
    if let Err(error) = fs::remove_dir_all(&rollback) {
        tracing::warn!(%error, "清理已完成导入的旧数据目录失败");
    }
    cleanup_unmarked_import_data(&pending_root);
    Ok(true)
}

pub(crate) fn reload_runtime_after_import(
    data_root: &Path,
    cx: &mut App,
) -> crate::config_runtime::ConfigReload {
    crate::state_runtime::StateRuntime::reload(data_root.join("state.json"), cx);
    crate::config_runtime::ConfigRuntime::reload(data_root.join("config.json"), cx)
}

pub(crate) fn refresh_after_import(reload: crate::config_runtime::ConfigReload, cx: &mut App) {
    crate::windows::menu::MenuService::close_all(cx);
    crate::config_runtime::ConfigRuntime::apply_imported_config(reload, cx);
    crate::windows::preview::PreviewWindowService::destroy(cx);
    crate::windows::notification::NotificationService::dismiss_all(cx);
    crate::windows::main::MainWindowService::reload_after_data_import(cx);
}

fn copy_import_entry(source: &Path, target: &Path) -> Result<(), io::Error> {
    let file_type = fs::symlink_metadata(source)
        .map_err(|error| io_with_path(error, source))?
        .file_type();
    if file_type.is_dir() {
        fs::create_dir(target).map_err(|error| io_with_path(error, target))?;
        for entry in fs::read_dir(source).map_err(|error| io_with_path(error, source))? {
            let entry = entry.map_err(|error| io_with_path(error, source))?;
            copy_import_entry(&entry.path(), &target.join(entry.file_name()))?;
        }
    } else if file_type.is_file() {
        retry_permission_denied(|| fs::copy(source, target))
            .map_err(|error| io_with_path(error, target))?;
        // Windows 的 FlushFileBuffers 需要可写句柄，不能用只读 File::open。
        retry_permission_denied(|| {
            OpenOptions::new()
                .write(true)
                .open(target)
                .and_then(|file| file.sync_all())
        })
        .map_err(|error| io_with_path(error, target))?;
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "导入暂存目录包含不支持的文件类型",
        ));
    }
    Ok(())
}

fn remove_sqlite_shared_memory_artifacts(staging: &Path) -> Result<(), io::Error> {
    for entry in fs::read_dir(staging)? {
        let entry = entry?;
        if is_sqlite_shared_memory_artifact(&entry.file_name()) {
            match retry_permission_denied(|| fs::remove_file(entry.path())) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}

fn is_importable_root_entry(name: &OsStr) -> bool {
    if is_sqlite_shared_memory_artifact(name) {
        return false;
    }
    let name = name.to_string_lossy();
    MANAGED_ROOT_FILES
        .iter()
        .chain(MANAGED_ROOT_DIRECTORIES)
        .any(|managed_name| name.eq_ignore_ascii_case(managed_name))
}

fn is_sqlite_shared_memory_artifact(name: &OsStr) -> bool {
    let name = name.to_string_lossy().to_ascii_lowercase();
    name == "qcclip.sqlite3-shm" || name.starts_with("qcclip.sqlite3-shm.")
}

fn rollback_import(
    copied_staged: &[PathBuf],
    moved_current: &[(PathBuf, PathBuf)],
) -> Result<(), io::Error> {
    let mut first_error = None;
    for target in copied_staged.iter().rev() {
        if let Err(error) = remove_imported_entry(target) {
            first_error.get_or_insert(error);
        }
    }
    if let Err(error) = restore_rollback(moved_current) {
        first_error.get_or_insert(error);
    }
    first_error.map_or(Ok(()), Err)
}

fn remove_imported_entry(path: &Path) -> Result<(), io::Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn cleanup_unmarked_import_data(pending_root: &Path) {
    let entries = match fs::read_dir(pending_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return,
        Err(error) => {
            tracing::warn!(%error, "读取导入暂存目录失败");
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(%error, "读取导入暂存目录条目失败");
                continue;
            }
        };
        let path = entry.path();
        let result = if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        };
        if let Err(error) = result {
            tracing::warn!(%error, "清理未提交的导入暂存文件失败");
        }
    }
}

fn restore_rollback(moved_current: &[(PathBuf, PathBuf)]) -> Result<(), io::Error> {
    let mut first_error = None;
    for (current, old) in moved_current.iter().rev() {
        if let Err(error) = rename_with_retry(old, current) {
            first_error.get_or_insert(io_with_path(error, current));
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn rename_with_retry(from: &Path, to: &Path) -> Result<(), io::Error> {
    retry_permission_denied(|| fs::rename(from, to))
}

fn retry_permission_denied<T, F>(mut operation: F) -> Result<T, io::Error>
where
    F: FnMut() -> Result<T, io::Error>,
{
    let mut last_error = None;
    for attempt in 0..FILE_OPERATION_RETRIES {
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                last_error = Some(error);
                if attempt + 1 < FILE_OPERATION_RETRIES {
                    thread::sleep(std::time::Duration::from_millis(
                        FILE_OPERATION_RETRY_MILLIS,
                    ));
                }
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error.expect("权限错误重试至少应执行一次"))
}

fn io_with_path(error: io::Error, path: &Path) -> io::Error {
    io::Error::new(error.kind(), format!("{}：{error}", path.display()))
}

fn managed_files(root: &Path) -> Result<Vec<PathBuf>, io::Error> {
    let mut files = Vec::new();
    for name in MANAGED_ROOT_FILES
        .iter()
        .copied()
        .chain(MANAGED_ROOT_DIRECTORIES.iter().copied())
    {
        let path = root.join(name);
        let file_type = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata.file_type(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if file_type.is_dir() {
            collect_files(root, &path, &mut files)?;
        } else if file_type.is_file() {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn migrate_data_root(source: &Path, target: &Path) -> Result<(), DataManagementError> {
    let source = dunce::canonicalize(source)?;
    let target = canonical_path_for_compare(target)?;
    if source == target {
        return Err(DataManagementError::InvalidPath(
            "目标数据目录与当前目录相同".to_owned(),
        ));
    }
    if target.starts_with(&source) || source.starts_with(&target) {
        return Err(DataManagementError::InvalidPath(
            "目标数据目录不能位于当前数据目录内或包含当前数据目录".to_owned(),
        ));
    }

    fs::create_dir_all(&target)?;
    validate_data_root_directory(&target)?;
    let staging = target.join(format!(".relocate-pending-{}", Uuid::new_v4()));
    fs::create_dir(&staging)?;
    let result = (|| {
        for name in MANAGED_ROOT_FILES
            .iter()
            .copied()
            .chain(DATA_LOCATION_DIRECTORIES.iter().copied())
        {
            if name == "qcclip.sqlite3-shm" {
                continue;
            }
            let source_entry = source.join(name);
            if !source_entry.exists() {
                continue;
            }
            copy_data_entry(&source_entry, &staging.join(name))?;
        }
        for name in MANAGED_ROOT_FILES
            .iter()
            .copied()
            .chain(DATA_LOCATION_DIRECTORIES.iter().copied())
        {
            if !staging.join(name).exists() {
                remove_imported_entry(&target.join(name))?;
            }
        }
        for entry in fs::read_dir(&staging)? {
            let entry = entry?;
            let name = entry.file_name();
            let destination = target.join(&name);
            remove_imported_entry(&destination)?;
            rename_with_retry(&entry.path(), &destination)?;
        }
        Ok::<(), io::Error>(())
    })();
    let cleanup_result = fs::remove_dir_all(&staging);
    match result {
        Ok(()) => {
            if let Err(error) = cleanup_result {
                if error.kind() != io::ErrorKind::NotFound {
                    return Err(error.into());
                }
            }
            Ok(())
        }
        Err(error) => {
            let _ = cleanup_result;
            Err(error.into())
        }
    }
}

fn canonical_path_for_compare(path: &Path) -> Result<PathBuf, io::Error> {
    if path.exists() {
        let metadata = fs::metadata(path)?;
        if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "目标数据路径不是目录",
            ));
        }
        return dunce::canonicalize(path);
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "目标数据路径无效"))?;
    Ok(dunce::canonicalize(parent)?.join(file_name))
}

fn validate_data_root_directory(root: &Path) -> Result<(), DataManagementError> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        let managed = MANAGED_ROOT_FILES
            .iter()
            .chain(MANAGED_ROOT_DIRECTORIES.iter())
            .any(|managed| name == managed.to_ascii_lowercase());
        if !managed && name != "log" && name != IMPORT_DIRECTORY {
            return Err(DataManagementError::InvalidPath(format!(
                "目标目录包含不受管理的文件或目录：{}",
                entry.path().display()
            )));
        }
    }
    Ok(())
}

fn copy_data_entry(source: &Path, target: &Path) -> Result<(), io::Error> {
    let file_type = fs::symlink_metadata(source)?.file_type();
    if file_type.is_dir() {
        fs::create_dir_all(target)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            let name = entry.file_name();
            if name == ".pending" || name == IMPORT_DIRECTORY {
                continue;
            }
            copy_data_entry(&entry.path(), &target.join(name))?;
        }
    } else if file_type.is_file() {
        if source
            .file_name()
            .is_some_and(|name| is_sqlite_shared_memory_artifact(name))
        {
            return Ok(());
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let expected_length = fs::metadata(source)?.len();
        let expected_hash = hash_file(source)?;
        fs::copy(source, target)?;
        let actual_length = fs::metadata(target)?.len();
        let actual_hash = hash_file(target)?;
        if actual_length != expected_length || actual_hash != expected_hash {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("复制数据文件后校验失败：{}", source.display()),
            ));
        }
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("数据目录包含不支持的文件类型：{}", source.display()),
        ));
    }
    Ok(())
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    match (dunce::canonicalize(left), dunce::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn cleanup_old_data_root(root: &Path) -> Result<(), io::Error> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries.collect::<Result<Vec<_>, _>>()?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut has_user_entries = false;
    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if is_location_managed_root_entry(&name) {
            remove_imported_entry(&entry.path())?;
        } else {
            has_user_entries = true;
        }
    }
    if !has_user_entries {
        match fs::remove_dir(root) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn is_location_managed_root_entry(name: &str) -> bool {
    MANAGED_ROOT_FILES
        .iter()
        .any(|managed| name == managed.to_ascii_lowercase())
        || DATA_LOCATION_DIRECTORIES
            .iter()
            .any(|managed| name == managed.to_ascii_lowercase())
        || name == IMPORT_DIRECTORY
        || name.starts_with(".relocate-pending-")
        || name.starts_with(".reset-staging-")
        || name.starts_with(".reset-rollback-")
}

fn path_is_within_root(path: &Path, root: &Path) -> Result<bool, io::Error> {
    let canonical_root = dunce::canonicalize(root)?;
    let canonical_path = if path.exists() {
        dunce::canonicalize(path)?
    } else {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let file_name = path
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "导出路径缺少文件名"))?;
        dunce::canonicalize(parent)?.join(file_name)
    };
    Ok(canonical_path == canonical_root || canonical_path.starts_with(&canonical_root))
}

fn collect_files(root: &Path, current: &Path, files: &mut Vec<PathBuf>) -> Result<(), io::Error> {
    if !current.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let relative = path.strip_prefix(root).unwrap_or(&path);
        if relative.components().any(|component| {
            matches!(component, Component::Normal(name) if name == "log" || name == IMPORT_DIRECTORY)
                || matches!(component, Component::Normal(name) if name == ".pending")
        }) {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_files(root, &path, files)?;
        } else if file_type.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

fn safe_join(root: &Path, relative: &str) -> Result<PathBuf, DataManagementError> {
    let path = Path::new(relative);
    if relative.is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(DataManagementError::InvalidDataFile(format!(
            "数据文件包含非法路径：{relative}"
        )));
    }
    Ok(root.join(path))
}

fn extract_archive(path: &Path, staging: &Path) -> Result<(), DataManagementError> {
    let mut file = File::open(path)?;
    let mut magic = vec![0; MAGIC.len()];
    file.read_exact(&mut magic)?;
    if magic != MAGIC {
        return Err(DataManagementError::InvalidDataFile(
            "文件头不匹配".to_owned(),
        ));
    }
    let count = read_u32(&mut file)?;
    if count > MAX_ENTRIES {
        return Err(DataManagementError::InvalidDataFile(
            "数据文件条目数量过多".to_owned(),
        ));
    }
    let mut paths = HashSet::with_capacity(count as usize);
    let mut has_database = false;
    for _ in 0..count {
        let path_length = read_u32(&mut file)?;
        let byte_length = read_u64(&mut file)?;
        if path_length == 0 || path_length > MAX_PATH_LENGTH || byte_length > MAX_ENTRY_BYTES {
            return Err(DataManagementError::InvalidDataFile(
                "数据文件条目大小无效".to_owned(),
            ));
        }
        let mut expected_hash = [0; 32];
        file.read_exact(&mut expected_hash)?;
        let mut path_bytes = vec![0; path_length as usize];
        file.read_exact(&mut path_bytes)?;
        let relative = String::from_utf8(path_bytes).map_err(|_| {
            DataManagementError::InvalidDataFile("数据文件路径不是 UTF-8".to_owned())
        })?;
        safe_join(Path::new("."), &relative)?;
        let mut components = Path::new(&relative).components();
        let first = components.next();
        let has_child = components.next().is_some();
        let allowed = match first {
            Some(Component::Normal(name)) if !has_child => MANAGED_ROOT_FILES
                .iter()
                .any(|managed_name| name.to_str() == Some(*managed_name)),
            Some(Component::Normal(name)) if has_child => MANAGED_ROOT_DIRECTORIES
                .iter()
                .any(|managed_name| name.to_str() == Some(*managed_name)),
            _ => false,
        };
        if !paths.insert(relative.clone()) {
            return Err(DataManagementError::InvalidDataFile(format!(
                "数据文件包含重复路径：{relative}"
            )));
        }
        if !allowed {
            let mut sink = io::sink();
            let actual = copy_file_contents(&mut file, &mut sink, byte_length)?;
            if actual != expected_hash {
                return Err(DataManagementError::InvalidDataFile(format!(
                    "数据文件条目校验失败：{relative}"
                )));
            }
            continue;
        }
        let target = safe_join(staging, &relative)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)?;
        let actual = copy_file_contents(&mut file, &mut output, byte_length)?;
        if actual != expected_hash {
            return Err(DataManagementError::InvalidDataFile(format!(
                "数据文件条目校验失败：{relative}"
            )));
        }
        output.sync_all()?;
        has_database |= relative == "qcclip.sqlite3";
    }
    let mut extra = [0; 1];
    if file.read(&mut extra)? != 0 {
        return Err(DataManagementError::InvalidDataFile(
            "数据文件包含未解析的数据".to_owned(),
        ));
    }
    if !has_database {
        return Err(DataManagementError::InvalidDataFile(
            "导入文件缺少数据库文件".to_owned(),
        ));
    }
    Ok(())
}

fn hash_file(path: &Path) -> Result<[u8; 32], io::Error> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher.finalize().into())
}

fn copy_file_contents<W: Write>(
    input: &mut File,
    output: &mut W,
    byte_length: u64,
) -> Result<[u8; 32], io::Error> {
    let mut hasher = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    let mut remaining = byte_length;
    while remaining > 0 {
        let count = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        input.read_exact(&mut buffer[..count])?;
        output.write_all(&buffer[..count])?;
        hasher.update(&buffer[..count]);
        remaining -= count as u64;
    }
    Ok(hasher.finalize().into())
}

fn read_u32(file: &mut File) -> Result<u32, io::Error> {
    let mut bytes = [0; 4];
    file.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64(file: &mut File) -> Result<u64, io::Error> {
    let mut bytes = [0; 8];
    file.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hot_import_keeps_existing_capture_service_writable_for_external_blobs() {
        use crate::services::{
            capture::{CaptureService, mock::MockClipboardSource},
            storage::RawBlob,
        };

        fn external_blob(database: &Database, item_id: i64) -> RawBlob {
            let connection = database.connect().expect("无法连接测试数据库");
            let (sha256, byte_length, relative_path): (Vec<u8>, i64, String) = connection
                .query_row(
                    "SELECT b.sha256, b.byte_length, b.relative_path
                     FROM clipboard_formats f JOIN binary_blobs b ON b.id = f.blob_id
                     WHERE f.item_id = ?1 AND f.storage_kind = 'external'",
                    [item_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .expect("捕获内容应存储为外部 blob");
            RawBlob::from_record(
                sha256.try_into().expect("blob 哈希长度应为 32 字节"),
                byte_length.try_into().expect("blob 字节长度不应为负数"),
                relative_path,
            )
        }

        let directory = tempfile::tempdir().expect("无法创建测试目录");
        let source_root = directory.path().join("source");
        let target_root = directory.path().join("target");
        let archive = directory.path().join("export.qcclip-data");
        let source = DataManagementService::legacy_test_service(&source_root);
        let target = DataManagementService::legacy_test_service(&target_root);
        let temporary_root = dunce::canonicalize(directory.path()).unwrap();
        for root in [&source_root, &target_root] {
            assert!(
                dunce::canonicalize(root)
                    .unwrap()
                    .starts_with(&temporary_root)
            );
        }

        let source_capture =
            CaptureService::new(source.database.clone(), source.blob_store.clone());
        let imported_text = "exported content ".repeat(8 * 1024);
        let imported = source_capture
            .store_captured_clipboard_with_status(
                &MockClipboardSource::text(&imported_text).capture(),
            )
            .expect("无法保存待导出的外部 blob");
        let imported_blob = external_blob(&source.database, imported.item_id);
        source.export_data(&archive).expect("无法导出测试归档");

        // 保留导入前的服务实例，模拟热导入后仍使用原有数据源句柄。
        let existing_blobs = target.blob_store.clone();
        let existing_capture = CaptureService::new(target.database.clone(), existing_blobs.clone());
        existing_capture
            .store_captured_clipboard(&MockClipboardSource::text("replaced target").capture())
            .expect("无法保存导入前的目标内容");
        assert!(target_root.join("data/.pending").is_dir());

        target.stage_import(&archive).expect("无法暂存测试归档");
        let pending_root = target_root.join(IMPORT_DIRECTORY);
        let staging_name = fs::read_to_string(pending_root.join(IMPORT_MARKER)).unwrap();
        let staging = pending_root.join(staging_name.trim());
        assert!(staging.join(imported_blob.relative_path()).is_file());
        assert!(!staging.join("data/.pending").exists());

        // 所有查询连接均已在 external_blob 返回时关闭，避免 Windows 文件锁干扰导入。
        let (applied, switch_guard) =
            apply_pending_import_for_reload(&target_root).expect("无法热导入测试归档");
        assert!(applied);
        drop(switch_guard);

        let new_text = "new content after import ".repeat(8 * 1024);
        let stored = existing_capture
            .store_captured_clipboard_with_status(&MockClipboardSource::text(&new_text).capture())
            .expect("热导入后原有捕获服务应能写入新的外部 blob");
        let stored_blob = external_blob(&target.database, stored.item_id);
        assert_eq!(
            existing_blobs
                .read(&stored_blob)
                .expect("无法读取新增 blob"),
            new_text.as_bytes()
        );
        assert_eq!(
            existing_blobs
                .read(&imported_blob)
                .expect("无法读取导入的 blob"),
            imported_text.as_bytes()
        );
        let connection = target.database.connect().unwrap();
        let item_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM clipboard_items", [], |row| row.get(0))
            .unwrap();
        assert_eq!(item_count, 2);
    }

    #[test]
    fn bulk_history_cleanup_reclaims_database_space_and_preserves_favorite_content() {
        use crate::services::{
            capture::{CaptureService, mock::MockClipboardSource},
            database::{FavoriteRepository, query::HistoryQuery},
        };

        for mode in ["all", "unpinned", "pending", "startup", "already_deleted"] {
            let directory = tempfile::tempdir().expect("无法创建测试目录");
            let service = DataManagementService::legacy_test_service(directory.path());
            let capture = CaptureService::with_inline_threshold(
                service.database.clone(),
                service.blob_store.clone(),
                usize::MAX,
            );
            let favorite_text = "需要保留的收藏原始内容";
            let pinned_id = capture
                .store_captured_clipboard(&MockClipboardSource::text(favorite_text).capture())
                .expect("无法保存收藏历史");
            let repository =
                HistoryRepository::new(service.database.clone(), service.blob_store.clone());
            repository
                .set_pinned(pinned_id, true)
                .expect("无法置顶历史");
            let connection = service.database.connect().expect("无法连接测试数据库");
            let item_id: i64 = connection
                .query_row(
                    "SELECT item_id FROM clipboard_history WHERE id = ?1",
                    [pinned_id],
                    |row| row.get(0),
                )
                .expect("无法读取收藏内容编号");
            drop(connection);
            FavoriteRepository::new(service.database.clone())
                .add_to_favorites(item_id)
                .expect("无法添加收藏");
            for index in 0..8 {
                let text = format!("{index}{}", "x".repeat(128 * 1024));
                capture
                    .store_captured_clipboard(&MockClipboardSource::text(&text).capture())
                    .expect("无法保存大文本历史");
            }
            let connection = service.database.connect().expect("无法连接测试数据库");
            connection.execute("UPDATE clipboard_history SET first_seen_at = 0, last_seen_at = 0 WHERE is_pinned = 0", [])
                .expect("无法设置过期历史");
            connection
                .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                .expect("无法写回测试数据库");
            drop(connection);
            let before = fs::metadata(service.database.path())
                .expect("无法读取清理前文件大小")
                .len();
            let policy = HistoryConfig {
                retention_days: Some(7),
                cleanup_on_startup: true,
                ..HistoryConfig::default()
            };
            let report = match mode {
                "all" => service.clear_all_history(),
                "unpinned" => service.clear_unpinned_history(),
                "pending" => service.cleanup_history(&policy),
                "startup" => service.cleanup_history_on_startup(&policy),
                "already_deleted" => {
                    repository.clear_all().expect("无法模拟旧版本清空历史");
                    service.clear_all_history()
                }
                _ => unreachable!(),
            }
            .expect("批量清理失败");
            let after = fs::metadata(service.database.path())
                .expect("无法读取清理后文件大小")
                .len();
            assert!(
                after < before / 2,
                "{mode} 未回收数据库空间：清理前 {before}，清理后 {after}"
            );
            let stats = service.stats(&policy).expect("无法读取数据统计");
            assert_eq!(stats.database_bytes, after, "{mode} 仍残留日志空间");
            assert_eq!(stats.favorite_count, 1);
            let keep_pinned = matches!(mode, "unpinned" | "pending" | "startup");
            assert_eq!(
                repository
                    .count(&HistoryQuery::default())
                    .expect("无法统计历史"),
                usize::from(keep_pinned)
            );
            assert_eq!(
                report.history_items_removed,
                if mode == "already_deleted" {
                    0
                } else if keep_pinned {
                    8
                } else {
                    9
                }
            );
            let connection = service.database.connect().expect("无法连接测试数据库");
            let saved: Vec<u8> = connection.query_row(
                "SELECT cf.inline_data FROM clipboard_formats cf JOIN favorites f ON f.item_id = cf.item_id WHERE cf.is_primary = 1",
                [], |row| row.get(0),
            ).expect("无法读取保留收藏的原始数据");
            assert_eq!(saved, favorite_text.as_bytes());
            let integrity: String = connection
                .query_row("PRAGMA integrity_check", [], |row| row.get(0))
                .expect("数据库完整性检查失败");
            assert_eq!(integrity, "ok");
        }
    }

    #[test]
    fn history_cleanup_on_startup_requires_opt_in_and_preserves_pins_and_favorites() {
        use crate::services::{
            capture::{CaptureService, mock::MockClipboardSource},
            database::{FavoriteRepository, query::HistoryQuery},
        };

        let directory = tempfile::tempdir().expect("无法创建测试目录");
        let service = DataManagementService::legacy_test_service(directory.path());
        let capture = CaptureService::with_inline_threshold(
            service.database.clone(),
            service.blob_store.clone(),
            0,
        );
        let mut ids = Vec::new();
        for text in [
            "过期置顶",
            "过期收藏",
            "过期普通历史",
            "超出条数上限",
            "保留的最新历史",
        ] {
            ids.push(
                capture
                    .store_captured_clipboard(&MockClipboardSource::text(text).capture())
                    .expect("无法保存测试历史"),
            );
        }
        let repository =
            HistoryRepository::new(service.database.clone(), service.blob_store.clone());
        repository
            .set_pinned(ids[0], true)
            .expect("无法置顶测试历史");
        let connection = service.database.connect().expect("无法连接测试数据库");
        connection.execute(
            "UPDATE clipboard_history SET first_seen_at = 0, last_seen_at = 0 WHERE id IN (?1, ?2, ?3)",
            rusqlite::params![ids[0], ids[1], ids[2]],
        ).expect("无法设置过期历史");
        let favorite_item_id: i64 = connection
            .query_row(
                "SELECT item_id FROM clipboard_history WHERE id = ?1",
                [ids[1]],
                |row| row.get(0),
            )
            .expect("无法读取收藏内容");
        drop(connection);
        let favorites = FavoriteRepository::with_blob_store(
            service.database.clone(),
            service.blob_store.clone(),
        );
        favorites
            .add_to_favorites(favorite_item_id)
            .expect("无法添加测试收藏");
        let mut policy = HistoryConfig {
            retention_days: Some(7),
            max_items: Some(1),
            ..HistoryConfig::default()
        };

        assert_eq!(
            service
                .cleanup_history_on_startup(&policy)
                .expect("关闭启动清理时不应失败"),
            HistoryCleanupReport::default()
        );
        assert_eq!(
            repository
                .count(&HistoryQuery::default())
                .expect("无法统计历史"),
            5
        );

        policy.cleanup_on_startup = true;
        let report = service
            .cleanup_history_on_startup(&policy)
            .expect("启动清理失败");
        assert_eq!(report.history_items_removed, 3);
        assert_eq!(report.orphan_items_removed, 2);
        assert_eq!(report.orphan_blob_files_removed, 2);
        let mut remaining = repository
            .query(&HistoryQuery::default())
            .expect("无法读取剩余历史")
            .into_iter()
            .map(|record| record.history_id)
            .collect::<Vec<_>>();
        remaining.sort_unstable();
        assert_eq!(remaining, vec![ids[0], ids[4]]);
        assert_eq!(
            favorites
                .query(&HistoryQuery::default())
                .expect("无法读取保留收藏")
                .len(),
            1
        );
    }

    #[test]
    fn application_reset_clears_managed_data_and_restores_default_location() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("custom");
        let default_root = directory.path().join("default");
        let mut service = DataManagementService::legacy_test_service(&root);
        service.default_data_root = default_root.clone();
        service.location_path = directory.path().join("location.json");
        let format = crate::domain::capture::RawFormat::captured(
            "mime",
            "text/plain",
            "文本",
            crate::domain::capture::FormatKind::Text,
            b"old-history".to_vec(),
            0,
            true,
        );
        let history_id = crate::services::capture::CaptureService::new(
            service.database.clone(),
            service.blob_store.clone(),
        )
        .store_captured_clipboard(&crate::domain::capture::CapturedClipboard {
            formats: vec![format],
            primary_format_identity: Some(("mime".to_owned(), "text/plain".to_owned())),
            preview_candidate: Some("old-history".to_owned()),
            source_application: crate::domain::capture::SourceApplication::default(),
        })
        .unwrap();
        let connection = service.database.connect().unwrap();
        let item_id: i64 = connection
            .query_row(
                "SELECT item_id FROM clipboard_history WHERE id = ?1",
                [history_id],
                |row| row.get(0),
            )
            .unwrap();
        drop(connection);
        crate::services::database::FavoriteRepository::new(service.database.clone())
            .add_to_favorites(item_id)
            .unwrap();
        write_location(&service.location_path, &root).unwrap();
        for data_root in [&root, &default_root] {
            fs::create_dir_all(data_root).unwrap();
            for name in ["config.json", "state.json"] {
                fs::write(data_root.join(name), b"old-settings").unwrap();
            }
            for name in DATA_LOCATION_DIRECTORIES
                .iter()
                .copied()
                .chain([IMPORT_DIRECTORY])
            {
                fs::create_dir_all(data_root.join(name)).unwrap();
                fs::write(data_root.join(name).join("old-data"), b"old-data").unwrap();
            }
            fs::write(data_root.join("user-file.txt"), b"user-data").unwrap();
        }
        let prepared = service.prepare_application_reset().expect("重置应用失败");
        assert_eq!(prepared.data_root, default_root);
        assert!(prepared.cleanup_error.is_none());
        drop(prepared.switch_guard);
        assert!(!service.location_path.exists());
        for data_root in [&root, &default_root] {
            assert!(!data_root.join("config.json").exists());
            assert!(!data_root.join("state.json").exists());
            for name in DATA_LOCATION_DIRECTORIES
                .iter()
                .copied()
                .chain([IMPORT_DIRECTORY])
            {
                assert!(!data_root.join(name).join("old-data").exists());
            }
            assert_eq!(
                fs::read(data_root.join("user-file.txt")).unwrap(),
                b"user-data"
            );
        }
        assert!(!root.join("qcclip.sqlite3").exists());
        let connection = prepared.database.connect().unwrap();
        for table in [
            "clipboard_history",
            "clipboard_items",
            "favorites",
            "tags",
            "binary_blobs",
        ] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 0);
        }
    }

    #[test]
    fn application_reset_restores_data_when_location_cannot_be_reset() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("data-root");
        let mut service = DataManagementService::legacy_test_service(&root);
        service.location_path = directory.path().join("invalid-location");
        fs::create_dir(&service.location_path).unwrap();
        fs::write(root.join("config.json"), b"old-settings").unwrap();
        fs::write(root.join("data").join("old-data"), b"old-data").unwrap();
        assert!(service.prepare_application_reset().is_err());
        assert_eq!(fs::read(root.join("config.json")).unwrap(), b"old-settings");
        assert_eq!(
            fs::read(root.join("data").join("old-data")).unwrap(),
            b"old-data"
        );
        assert!(service.database.connect().is_ok());
    }

    #[test]
    fn portable_application_reset_preserves_portable_marker_and_user_files() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("QcClipData");
        let mut service = DataManagementService::legacy_test_service(&root);
        service.portable = true;
        service.location_path = directory.path().join("location.json");
        fs::write(directory.path().join("QcClip.portable"), b"").unwrap();
        fs::write(root.join("config.json"), b"old-settings").unwrap();
        let prepared = service.prepare_application_reset().unwrap();
        assert_eq!(prepared.data_root, root);
        assert!(!root.join("config.json").exists());
        assert!(directory.path().join("QcClip.portable").exists());
    }

    #[test]
    fn portable_data_location_switch_is_rejected_before_migration() {
        let directory = tempfile::tempdir().expect("应创建临时目录");
        let root = directory.path().join("QcClipData");
        let database = Database::open(root.join("qcclip.sqlite3")).expect("应打开数据库");
        let blob_store = RawBlobStore::new(&root).expect("应创建数据存储");
        let mut service = DataManagementService {
            database,
            blob_store,
            data_root: root.clone(),
            default_data_root: root.clone(),
            location_path: directory.path().join("location.json"),
            portable: true,
        };
        let target = directory.path().join("other-data");

        assert!(!service.can_change_data_location());
        assert!(matches!(
            service.prepare_data_location_switch(&target),
            Err(DataManagementError::PortableDataLocation)
        ));
        assert!(!target.exists());
        assert!(!service.location_path.exists());
        assert!(root.join("qcclip.sqlite3").is_file());

        service.portable = false;
        assert!(service.can_change_data_location());
    }
}
