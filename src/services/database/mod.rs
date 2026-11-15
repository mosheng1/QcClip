#[cfg(test)]
mod compatibility_tests;
mod connection;
mod favorite_repository;
mod history_repository;
mod migrations;
pub(crate) mod missing_files;
mod models;
mod note_repository;
pub mod query;
mod repository;
mod tag_repository;

#[cfg(test)]
pub(crate) use connection::DatabaseConnection;
pub use connection::{Database, DatabaseError};
pub use favorite_repository::{FavoriteDeleteError, FavoriteReorderError, FavoriteRepository};
pub use history_repository::{
    HistoryCleanupError, HistoryCleanupReport, HistoryReorderError, HistoryRepository,
    HistoryRetentionCounts,
};
pub use models::DatabaseInfo;
pub(crate) use models::LIST_ORDER_GAP;
#[cfg(test)]
pub(crate) use models::{CURRENT_SCHEMA_VERSION, DATABASE_WRITE_VERSION};
pub use note_repository::NoteRepository;
pub use tag_repository::{
    TagCreateError, TagDeleteError, TagItemDeleteError, TagItemWriteError, TagQueryError,
    TagReorderError, TagRepository, TagUpdateError,
};
