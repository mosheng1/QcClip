use std::{
    fs,
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
};

use rusqlite::{Connection, OpenFlags};
use thiserror::Error;

use super::{migrations, models::CURRENT_SCHEMA_VERSION, repository::DatabaseRepository};
use crate::services::data_runtime::{self, DataOperationGuard};

#[derive(Debug, Error)]
pub enum DatabaseError {
    #[error("无法创建数据库目录“{path}”：{source}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("无法打开数据库“{path}”：{source}")]
    Open {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    #[error("无法配置 SQLite 连接：{source}")]
    Configure {
        #[source]
        source: rusqlite::Error,
    },
    #[error("无法读取数据库状态：{source}")]
    Inspect {
        #[source]
        source: rusqlite::Error,
    },
    #[error("数据库需要读取兼容版本 {required}，当前程序支持 {supported}，请使用新版程序打开")]
    UnsupportedReadVersion { required: i32, supported: i32 },
    #[error("数据库需要写入兼容版本 {required}，当前程序支持 {supported}，已按只读方式打开")]
    UnsupportedWriteVersion { required: i32, supported: i32 },
    #[error("数据库结构较新，当前程序不能重写已有格式，请使用新版程序编辑")]
    UnsupportedRewrite,
    #[error("执行第 {version} 版数据库迁移失败：{source}")]
    Migration {
        version: i32,
        #[source]
        source: rusqlite::Error,
    },
    #[error("数据库结构校验失败：{0}")]
    Validation(String),
    #[error("数据正在导入，请稍后重试")]
    DataSwitching,
}

#[derive(Clone, Debug)]
pub struct Database {
    path: PathBuf,
    /// `hidden_missing_items` is created on writable opens. A database opened
    /// read-only without the auxiliary table keeps missing-item filtering off.
    has_hidden_items_table: bool,
}

pub(crate) struct DatabaseConnection {
    connection: Connection,
    _operation: DataOperationGuard,
}

impl Deref for DatabaseConnection {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        &self.connection
    }
}

impl DerefMut for DatabaseConnection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.connection
    }
}

impl Database {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DatabaseError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| DatabaseError::CreateDirectory {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let mut database = Self {
            path,
            has_hidden_items_table: false,
        };
        let initialize = if database.path.exists() {
            let connection = database.open_connection(true, false)?;
            let version: i32 = connection
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .map_err(|source| DatabaseError::Inspect { source })?;
            if version == 0 {
                let tables: i64 = connection.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
                    [], |row| row.get(0),
                ).map_err(|source| DatabaseError::Inspect { source })?;
                if tables != 0 {
                    return Err(DatabaseError::Validation(
                        "未标记版本的数据库已包含数据表，请先备份并核验结构".to_owned(),
                    ));
                }
                true
            } else {
                migrations::validate_schema(&connection)?;
                version < CURRENT_SCHEMA_VERSION
            }
        } else {
            true
        };
        if initialize {
            let mut connection = database.open_connection(false, true)?;
            migrations::migrate(&mut connection)?;
            database.validate_connection(&connection)?;
        } else {
            let connection = database.open_connection(true, false)?;
            database.validate_connection(&connection)?;
            match migrations::validate_write_schema(&connection) {
                Ok(()) => {
                    let version: i32 = connection
                        .query_row("PRAGMA user_version", [], |row| row.get(0))
                        .map_err(|source| DatabaseError::Inspect { source })?;
                    if version == CURRENT_SCHEMA_VERSION {
                        let writable = database.open_connection(false, false)?;
                        migrations::ensure_capture_indexes(&writable)?;
                        writable
                            .execute_batch(include_str!("sync.sql"))
                            .map_err(|source| DatabaseError::Configure { source })?;
                        migrations::ensure_missing_files_table(&writable)?;
                    }
                }
                Err(error) => tracing::warn!("数据库已按只读兼容模式打开：{error}"),
            }
        }
        database.has_hidden_items_table = database.detect_hidden_items_table()?;
        Ok(database)
    }

    pub(crate) fn has_hidden_items_table(&self) -> bool {
        self.has_hidden_items_table
    }

    fn detect_hidden_items_table(&self) -> Result<bool, DatabaseError> {
        self.open_connection(true, false)?
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_master
                    WHERE type = 'table' AND name = 'hidden_missing_items'
                )",
                [],
                |row| row.get(0),
            )
            .map_err(|source| DatabaseError::Inspect { source })
    }

    pub(crate) fn connect_read(&self) -> Result<DatabaseConnection, DatabaseError> {
        let operation =
            data_runtime::begin_operation().map_err(|_| DatabaseError::DataSwitching)?;
        let connection = self.open_connection(true, false)?;
        migrations::validate_schema(&connection)?;
        self.validate_connection(&connection)?;
        Ok(DatabaseConnection {
            connection,
            _operation: operation,
        })
    }

    pub(crate) fn connect(&self) -> Result<DatabaseConnection, DatabaseError> {
        let operation =
            data_runtime::begin_operation().map_err(|_| DatabaseError::DataSwitching)?;
        let connection = self.open_connection(true, false)?;
        migrations::validate_schema(&connection)?;
        self.validate_connection(&connection)?;
        let connection = if migrations::validate_write_schema(&connection).is_ok() {
            drop(connection);
            let connection = self.open_connection(false, false)?;
            migrations::validate_schema(&connection)?;
            migrations::validate_write_schema(&connection)?;
            self.validate_connection(&connection)?;
            connection
        } else {
            connection
        };
        Ok(DatabaseConnection {
            connection,
            _operation: operation,
        })
    }

    pub(crate) fn connect_write(&self) -> Result<DatabaseConnection, DatabaseError> {
        let connection = self.connect()?;
        migrations::validate_write_schema(&connection)?;
        Ok(connection)
    }

    pub(crate) fn connect_rewrite(&self) -> Result<DatabaseConnection, DatabaseError> {
        let connection = self.connect_write()?;
        if !Self::maintenance_supported(&connection)? {
            return Err(DatabaseError::UnsupportedRewrite);
        }
        Ok(connection)
    }

    pub(crate) fn maintenance_supported(connection: &Connection) -> Result<bool, DatabaseError> {
        let version: i32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|source| DatabaseError::Inspect { source })?;
        let read_only = connection
            .is_readonly(rusqlite::MAIN_DB)
            .map_err(|source| DatabaseError::Inspect { source })?;
        Ok(version == CURRENT_SCHEMA_VERSION && !read_only)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn open_connection(
        &self,
        read_only: bool,
        initialize: bool,
    ) -> Result<Connection, DatabaseError> {
        let flags = if read_only {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        } else if initialize {
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE
        } else {
            OpenFlags::SQLITE_OPEN_READ_WRITE
        };
        let connection = Connection::open_with_flags(&self.path, flags).map_err(|source| {
            DatabaseError::Open {
                path: self.path.clone(),
                source,
            }
        })?;
        connection
            .execute_batch("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;")
            .map_err(|source| DatabaseError::Configure { source })?;
        if initialize {
            connection
                .pragma_update(None, "journal_mode", "WAL")
                .map_err(|source| DatabaseError::Configure { source })?;
        }
        Ok(connection)
    }

    fn validate_connection(&self, connection: &Connection) -> Result<(), DatabaseError> {
        let info = DatabaseRepository::new(connection).database_info()?;
        if !info.foreign_keys_enabled {
            return Err(DatabaseError::Validation("当前连接未启用外键".to_owned()));
        }
        if !info.journal_mode.eq_ignore_ascii_case("wal") {
            return Err(DatabaseError::Validation(format!(
                "日志模式应为 WAL，实际为 {}",
                info.journal_mode,
            )));
        }
        if info.busy_timeout_millis != 5_000 {
            return Err(DatabaseError::Validation(format!(
                "忙碌超时应为 5000 毫秒，实际为 {} 毫秒",
                info.busy_timeout_millis,
            )));
        }
        Ok(())
    }
}
