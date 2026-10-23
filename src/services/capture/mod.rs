mod hash;
mod persistence;
mod preview;
mod text_merge;

#[cfg(test)]
mod merge_tests;

#[cfg(test)]
mod perf_tests;

#[cfg(test)]
pub(crate) mod mock;

pub(crate) use hash::hash_format_set;
pub use hash::{CONTENT_HASH_VERSION, HashedClipboard, hash_clipboard};
pub use persistence::CaptureService;
pub(crate) use persistence::CaptureStoreResult;
pub(crate) use preview::extract_links;
pub use preview::{ClipboardPreview, generate_preview, select_default_preview};

use thiserror::Error;

use super::{database::DatabaseError, storage::RawBlobError};

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("捕获结果中没有成功读取的原始格式")]
    NoCapturedFormats,
    #[error("捕获结果中没有可恢复的原始格式")]
    NoRestorableFormats,
    #[error("捕获格式无效：{0}")]
    InvalidFormat(String),
    #[error("数据库操作失败：{0}")]
    Database(#[from] DatabaseError),
    #[error("数据库写入失败：{0}")]
    Sql(#[from] rusqlite::Error),
    #[error("原始数据存储失败：{0}")]
    Storage(#[from] RawBlobError),
}
