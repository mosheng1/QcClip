pub(crate) mod activity;
pub(crate) mod config;
mod dav_cache;
mod diagnostics;
mod lan;
pub(crate) mod local_pairing;
pub(crate) mod pairing;
pub(crate) mod protocol;
mod repository;
mod runtime;
pub(crate) mod ui;
pub(crate) mod webdav;

#[cfg(test)]
mod behavior_tests;
#[cfg(test)]
mod relay_tests;
#[cfg(test)]
mod tests;

use crate::services::{capture::CaptureStoreResult, database::Database};
use std::sync::{OnceLock, Weak};

pub(crate) use repository::ReceivedChange;
pub(crate) use repository::managed_file_list;
pub(crate) use runtime::{Runtime, Snapshot, SyncService};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Message(String),
    #[error("{message}")]
    Http { status: u16, message: String },
    #[error("同步文件操作失败")]
    Io(#[from] std::io::Error),
    #[error("同步数据格式无法识别，可能是设备版本不一致或缓存数据损坏")]
    Json(#[from] serde_json::Error),
    #[error("同步数据库操作失败：{0}")]
    Sql(#[from] rusqlite::Error),
    #[error("同步数据库不可用：{0}")]
    Database(#[from] crate::services::database::DatabaseError),
    #[error("同步内容入库失败：{0}")]
    Capture(#[from] crate::services::capture::CaptureError),
    #[error("同步原始资源读取失败")]
    Blob(#[from] crate::services::storage::RawBlobError),
}
pub type Result<T> = std::result::Result<T, Error>;
pub fn fail<T>(message: &str) -> Result<T> {
    Err(Error::Message(message.into()))
}

static ACTIVE: OnceLock<Weak<runtime::Runtime>> = OnceLock::new();

pub(crate) fn captured(database: &Database, result: CaptureStoreResult) {
    if let Some(runtime) = ACTIVE.get().and_then(Weak::upgrade) {
        if result.formats_changed {
            return;
        }
        runtime.automatic(
            database,
            result.item_id,
            if result.is_new {
                protocol::Mode::NewContent
            } else {
                protocol::Mode::Touch
            },
        );
    }
}
pub(crate) fn favorited(database: &Database, item_id: i64) {
    if let Some(runtime) = ACTIVE.get().and_then(Weak::upgrade) {
        runtime.automatic(database, item_id, protocol::Mode::Favorite);
    }
}
