use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use gpui::{App, AppContext as _, BorrowAppContext as _, DisplayId, Global, WindowHandle};
use gpui_component::Root;

use crate::{
    config::NotificationKind,
    domain::capture::CapturedClipboard,
    domain::clipboard::{ClipboardItemId, HistoryItemId, Tag},
    platform::clipboard::{ClipboardWriteSuppressor, capture_selection, captured_plain_text},
    services::{
        capture::{CaptureError, CaptureService, CaptureStoreResult},
        custom_text::CustomTextService,
        data_runtime::{self, DataOperationGuard},
        database::{Database, FavoriteRepository, TagQueryError, TagRepository},
        storage::RawBlobStore,
    },
    windows::notification::{NotificationLevel, NotificationService},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct QuickFavoriteSaveResult {
    is_new: bool,
    added: bool,
}

#[derive(Clone, Debug)]
struct BoundFavoriteItem {
    item_id: ClipboardItemId,
    history_id: HistoryItemId,
    original_text: String,
    is_new: bool,
}

#[derive(Clone)]
struct QuickFavoriteRepositories {
    custom_text: CustomTextService,
    favorites: FavoriteRepository,
    tags: TagRepository,
}

/// A token belongs to one data-source session and is never reactivated.
#[derive(Clone)]
struct FavoriteSession(Arc<AtomicBool>);

impl FavoriteSession {
    fn new() -> Self {
        Self(Arc::new(AtomicBool::new(true)))
    }

    fn retire(&self) {
        self.0.store(false, Ordering::Release);
    }

    fn is_current(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    fn begin_operation(&self) -> Result<DataOperationGuard, String> {
        // Acquire the runtime guard before checking the token: replacement must
        // wait for the entire save, including all repository/blob operations.
        let operation = data_runtime::begin_operation().map_err(|error| error.to_string())?;
        if !self.is_current() {
            return Err("数据源已切换，请重新打开快捷收藏".to_owned());
        }
        Ok(operation)
    }

    fn save(
        &self,
        bound: Option<BoundFavoriteItem>,
        text: &str,
        tag_ids: &[i64],
        repositories: &QuickFavoriteRepositories,
    ) -> Result<(QuickFavoriteSaveResult, Option<HistoryItemId>), String> {
        let _operation = self.begin_operation()?;
        match bound {
            Some(bound) => {
                let history_id = bound.history_id;
                persist_bound_favorite(bound, text, tag_ids, repositories)
                    .map(|result| (result, Some(history_id)))
            }
            None => persist_manual_favorite(text, tag_ids, repositories),
        }
    }

    fn capture(
        &self,
        capture_service: &CaptureService,
        captured: Option<&CapturedClipboard>,
    ) -> Result<SelectionOutcome, String> {
        let _operation = self.begin_operation()?;
        store_selection(capture_service, captured).map_err(|error| error.to_string())
    }
}

#[derive(Debug)]
enum SelectionOutcome {
    Empty,
    Stored {
        text: Option<String>,
        store: CaptureStoreResult,
    },
}

fn store_selection(
    capture_service: &CaptureService,
    captured: Option<&CapturedClipboard>,
) -> Result<SelectionOutcome, CaptureError> {
    let Some(captured) = captured else {
        return Ok(SelectionOutcome::Empty);
    };
    let store = capture_service.store_captured_clipboard_with_status(captured)?;
    Ok(SelectionOutcome::Stored {
        text: captured_plain_text(captured),
        store,
    })
}

pub(crate) struct QuickFavoriteService {
    repositories: QuickFavoriteRepositories,
    capture_service: CaptureService,
    clipboard_write_suppressor: ClipboardWriteSuppressor,
    editor_window: Option<WindowHandle<Root>>,
    opening: bool,
    session: FavoriteSession,
}

impl Global for QuickFavoriteService {}

impl QuickFavoriteService {
    pub(crate) fn install(
        tag_repository: TagRepository,
        custom_text_service: CustomTextService,
        favorite_repository: FavoriteRepository,
        capture_service: CaptureService,
        clipboard_write_suppressor: ClipboardWriteSuppressor,
        cx: &mut App,
    ) {
        cx.set_global(Self {
            repositories: QuickFavoriteRepositories {
                custom_text: custom_text_service,
                favorites: favorite_repository,
                tags: tag_repository,
            },
            capture_service,
            clipboard_write_suppressor,
            editor_window: None,
            opening: false,
            session: FavoriteSession::new(),
        });
    }

    /// Retire queued work before the data runtime begins replacing files.
    pub(crate) fn suspend(cx: &mut App) {
        cx.update_global::<Self, _>(|service, cx| {
            service.session.retire();
            service.opening = false;
            if let Some(handle) = service.editor_window.take() {
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            }
        });
    }

    pub(crate) fn replace_data_sources(database: Database, blob_store: RawBlobStore, cx: &mut App) {
        cx.update_global::<Self, _>(|service, _| service.replace_sources(database, blob_store));
    }

    fn replace_sources(&mut self, database: Database, blob_store: RawBlobStore) {
        self.repositories = QuickFavoriteRepositories {
            custom_text: CustomTextService::new(database.clone(), blob_store.clone()),
            favorites: FavoriteRepository::new(database.clone()),
            tags: TagRepository::new(database.clone()),
        };
        self.capture_service = CaptureService::new(database, blob_store);
    }

    pub(crate) fn resume(cx: &mut App) {
        cx.update_global::<Self, _>(|service, _| {
            if !service.session.is_current() {
                service.session = FavoriteSession::new();
            }
        });
    }

    /// 快捷键与悬浮 + 共享单个编辑器：已有窗口时只聚焦，不重新捕获或新建。
    pub(crate) fn open(from_hotkey: bool, cx: &mut App) {
        if !cx.global::<Self>().session.is_current() || Self::focus_open_editor(cx) {
            return;
        }
        let display_id =
            crate::platform::window::display_id_for_foreground_window().or_else(|| {
                let point = crate::platform::window::physical_cursor_position()?;
                crate::platform::window::display_for_physical_point(cx, point)
                    .map(|placement| placement.display_id)
            });
        let opening = cx.update_global::<Self, _>(|service, _| {
            if service.opening {
                return false;
            }
            service.opening = true;
            true
        });
        if !opening {
            return;
        }

        let (repositories, capture_service, clipboard_write_suppressor, session) = {
            let service = cx.global::<Self>();
            (
                service.repositories.clone(),
                service.capture_service.clone(),
                service.clipboard_write_suppressor.clone(),
                service.session.clone(),
            )
        };
        let task_session = session.clone();
        let task = cx.background_spawn(async move {
            let _operation = task_session.begin_operation()?;
            let captured = if from_hotkey {
                capture_selection(&clipboard_write_suppressor).map_err(|error| error.to_string())?
            } else {
                None
            };
            let selection = task_session.capture(&capture_service, captured.as_ref())?;
            let tags = repositories.tags.list();
            Ok((selection, tags))
        });
        cx.spawn(async move |cx| {
            let result = task.await;
            cx.update(|cx| {
                if !session.is_current() {
                    return;
                }
                cx.update_global::<Self, _>(|service, _| service.opening = false);
                match result {
                    Ok((selection, tags)) => {
                        Self::finish_open(from_hotkey, display_id, selection, tags, session, cx);
                    }
                    Err(error) => handle_save_result(from_hotkey, Err(error), cx),
                }
            });
        })
        .detach();
    }

    fn focus_open_editor(cx: &mut App) -> bool {
        let focused = cx.update_global::<Self, _>(|service, cx| {
            let Some(handle) = service.editor_window else {
                return false;
            };
            if handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
            {
                true
            } else {
                service.editor_window = None;
                false
            }
        });
        if focused {
            cx.activate(true);
        }
        focused
    }

    fn finish_open(
        from_hotkey: bool,
        display_id: Option<DisplayId>,
        selection: SelectionOutcome,
        tags: Result<Vec<Tag>, TagQueryError>,
        session: FavoriteSession,
        cx: &mut App,
    ) {
        let tags = tags.unwrap_or_else(|error| {
            tracing::error!("读取快捷收藏标签失败：{error}");
            Vec::new()
        });
        let (editor_text, bound) = match selection {
            SelectionOutcome::Empty => {
                if from_hotkey
                    && !crate::config_runtime::shortcuts_config(cx)
                        .add_favorite_open_editor_on_empty
                {
                    let translator = crate::i18n::translator(cx);
                    NotificationService::show_message_for(
                        NotificationKind::Attention,
                        translator.translate("notification.quick_favorite.failed_title"),
                        translator.translate("notification.quick_favorite.no_selection"),
                        NotificationLevel::Warning,
                        cx,
                    );
                    return;
                }
                (String::new(), None)
            }
            SelectionOutcome::Stored { text, store } => {
                crate::windows::main::MainWindowService::refresh_captured_history_item(
                    cx,
                    store.history_id,
                );
                let Some(text) = text else {
                    let repositories = cx.global::<Self>().repositories.clone();
                    let task_session = session.clone();
                    let task = cx.background_spawn(async move {
                        let _operation = task_session.begin_operation()?;
                        favorite_captured_item(&store, &repositories)
                            .map(|result| (result, Some(store.history_id)))
                    });
                    finish_save_in_background(from_hotkey, session, task, cx);
                    return;
                };
                let bound = BoundFavoriteItem {
                    item_id: store.item_id,
                    history_id: store.history_id,
                    original_text: text.clone(),
                    is_new: store.is_new,
                };
                (text, Some(bound))
            }
        };
        let repositories = cx.global::<Self>().repositories.clone();
        let on_confirm = Box::new(move |text: String, tag_ids: Vec<i64>, cx: &mut App| {
            if session.is_current() {
                let task_session = session.clone();
                let task = cx.background_spawn(async move {
                    task_session.save(bound, &text, &tag_ids, &repositories)
                });
                finish_save_in_background(from_hotkey, session, task, cx);
            }
        });
        let opened = crate::windows::custom_text_editor::open_new_favorite(
            editor_text,
            tags,
            display_id,
            on_confirm,
            cx,
        );
        match opened {
            Ok(handle) => {
                cx.update_global::<Self, _>(|service, _| service.editor_window = Some(handle));
            }
            Err(error) => tracing::error!("打开快捷收藏窗口失败：{error}"),
        }
    }
}

fn finish_save_in_background(
    from_hotkey: bool,
    session: FavoriteSession,
    task: gpui::Task<Result<(QuickFavoriteSaveResult, Option<HistoryItemId>), String>>,
    cx: &mut App,
) {
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| {
            if session.is_current() {
                handle_save_result(from_hotkey, result, cx);
            }
        });
    })
    .detach();
}

fn persist_bound_favorite(
    bound: BoundFavoriteItem,
    text: &str,
    tag_ids: &[i64],
    repositories: &QuickFavoriteRepositories,
) -> Result<QuickFavoriteSaveResult, String> {
    if text == bound.original_text {
        append_tags(bound.item_id, tag_ids, repositories)?;
    } else {
        let mut merged_tag_ids = repositories
            .tags
            .item_tag_ids(bound.item_id)
            .map_err(|error| error.to_string())?;
        merged_tag_ids.extend_from_slice(tag_ids);
        repositories
            .custom_text
            .save_for_item_with_tags(bound.item_id, text, &merged_tag_ids)
            .map_err(|error| error.to_string())?;
    }
    Ok(QuickFavoriteSaveResult {
        is_new: bound.is_new,
        added: add_to_favorites(bound.item_id, repositories)?,
    })
}

fn persist_manual_favorite(
    text: &str,
    tag_ids: &[i64],
    repositories: &QuickFavoriteRepositories,
) -> Result<(QuickFavoriteSaveResult, Option<HistoryItemId>), String> {
    let item_id = match repositories
        .custom_text
        .find_manual_text_item(text)
        .map_err(|error| error.to_string())?
    {
        Some(item_id) => item_id,
        None => match repositories
            .custom_text
            .create_manual_text_and_favorite_with_tags(text, tag_ids)
        {
            Ok(history_id) => {
                return Ok((
                    QuickFavoriteSaveResult {
                        is_new: true,
                        added: true,
                    },
                    Some(history_id),
                ));
            }
            Err(error) if is_unique_violation(&error) => repositories
                .custom_text
                .find_manual_text_item(text)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| error.to_string())?,
            Err(error) => return Err(error.to_string()),
        },
    };
    append_tags(item_id, tag_ids, repositories)?;
    Ok((
        QuickFavoriteSaveResult {
            is_new: false,
            added: add_to_favorites(item_id, repositories)?,
        },
        None,
    ))
}

fn favorite_captured_item(
    store: &CaptureStoreResult,
    repositories: &QuickFavoriteRepositories,
) -> Result<QuickFavoriteSaveResult, String> {
    Ok(QuickFavoriteSaveResult {
        is_new: store.is_new,
        added: add_to_favorites(store.item_id, repositories)?,
    })
}

fn append_tags(
    item_id: ClipboardItemId,
    tag_ids: &[i64],
    repositories: &QuickFavoriteRepositories,
) -> Result<(), String> {
    for tag_id in tag_ids {
        repositories
            .tags
            .add_to_item(item_id, *tag_id)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn add_to_favorites(
    item_id: ClipboardItemId,
    repositories: &QuickFavoriteRepositories,
) -> Result<bool, String> {
    repositories
        .favorites
        .add_to_favorites(item_id)
        .map_err(|error| error.to_string())
}

fn is_unique_violation(error: &crate::services::custom_text::CustomTextError) -> bool {
    matches!(
        error,
        crate::services::custom_text::CustomTextError::Sql(
            rusqlite::Error::SqliteFailure(inner, _)
        ) if inner.code == rusqlite::ErrorCode::ConstraintViolation
    )
}

fn notification_message(result: QuickFavoriteSaveResult) -> (&'static str, NotificationLevel) {
    if !result.added {
        (
            "notification.quick_favorite.exists_present",
            NotificationLevel::Info,
        )
    } else if result.is_new {
        (
            "notification.quick_favorite.added",
            NotificationLevel::Success,
        )
    } else {
        (
            "notification.quick_favorite.exists_added",
            NotificationLevel::Success,
        )
    }
}

fn handle_save_result(
    from_hotkey: bool,
    result: Result<(QuickFavoriteSaveResult, Option<HistoryItemId>), String>,
    cx: &mut App,
) {
    match result {
        Ok((result, history_id)) => {
            if from_hotkey {
                let translator = crate::i18n::translator(cx);
                let (message_key, level) = notification_message(result);
                NotificationService::show_message_for(
                    NotificationKind::OperationResult,
                    translator.translate("notification.quick_favorite.title"),
                    translator.translate(message_key),
                    level,
                    cx,
                );
            }
            if let Some(history_id) = history_id {
                crate::windows::main::MainWindowService::refresh_history_item(cx, history_id);
            } else {
                crate::windows::main::MainWindowService::refresh_history(cx);
            }
        }
        Err(error) => {
            tracing::error!("快捷收藏失败：{error}");
            let translator = crate::i18n::translator(cx);
            NotificationService::show_message_for(
                NotificationKind::Attention,
                translator.translate("notification.quick_favorite.failed_title"),
                error,
                NotificationLevel::Error,
                cx,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::services::{database::Database, storage::RawBlobStore};

    struct TestContext {
        _directory: TempDir,
        database: Database,
        custom_text_service: CustomTextService,
        favorite_repository: FavoriteRepository,
        tag_repository: TagRepository,
        capture_service: crate::services::capture::CaptureService,
    }

    impl TestContext {
        fn new() -> Self {
            let directory = tempfile::tempdir().expect("无法创建测试目录");
            let database =
                Database::open(directory.path().join("test.sqlite3")).expect("无法创建测试数据库");
            let blob_store = RawBlobStore::new(directory.path()).expect("无法创建 blob 存储");
            let custom_text_service = CustomTextService::new(database.clone(), blob_store.clone());
            let favorite_repository = FavoriteRepository::new(database.clone());
            let tag_repository = TagRepository::new(database.clone());
            let capture_service =
                crate::services::capture::CaptureService::new(database.clone(), blob_store);
            Self {
                _directory: directory,
                database,
                custom_text_service,
                favorite_repository,
                tag_repository,
                capture_service,
            }
        }

        fn capture(&self, text: &str) -> CaptureStoreResult {
            self.capture_service
                .store_captured_clipboard_with_status(
                    &crate::services::capture::mock::MockClipboardSource::text(text).capture(),
                )
                .expect("无法保存测试捕获")
        }

        fn favorite_count(&self) -> i64 {
            self.count("favorites")
        }

        fn preview_text(&self, item_id: ClipboardItemId) -> String {
            self.database
                .connect()
                .expect("无法连接测试数据库")
                .query_row(
                    "SELECT preview_text FROM clipboard_items WHERE id = ?1",
                    [item_id],
                    |row| row.get(0),
                )
                .expect("无法查询预览")
        }

        fn repositories(&self) -> QuickFavoriteRepositories {
            QuickFavoriteRepositories {
                custom_text: self.custom_text_service.clone(),
                favorites: self.favorite_repository.clone(),
                tags: self.tag_repository.clone(),
            }
        }

        fn persist(&self, text: &str, tag_ids: &[i64]) -> Result<QuickFavoriteSaveResult, String> {
            persist_manual_favorite(text, tag_ids, &self.repositories()).map(|(result, _)| result)
        }

        fn count(&self, table: &str) -> i64 {
            self.database
                .connect()
                .expect("无法连接测试数据库")
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("无法统计表行数")
        }
    }

    #[test]
    fn duplicate_text_reuses_the_existing_item_and_merges_new_tags() {
        let context = TestContext::new();
        let history_id = context
            .custom_text_service
            .create_manual_text("想法")
            .expect("无法创建手动文本");
        let item_id = context
            .custom_text_service
            .find_manual_text_item("想法")
            .expect("无法查询手动文本")
            .expect("手动文本不存在");
        let existing_tag = context
            .tag_repository
            .create("工作", "#1677FF")
            .expect("无法创建已有标签");
        let new_tag = context
            .tag_repository
            .create("开发", "#00FF00")
            .expect("无法创建新标签");
        context
            .tag_repository
            .add_to_item(item_id, existing_tag.id)
            .expect("无法添加已有标签");

        let outcome = context
            .persist("想法", &[new_tag.id])
            .expect("快捷收藏失败");

        assert_eq!(
            outcome,
            QuickFavoriteSaveResult {
                is_new: false,
                added: true
            }
        );
        assert_eq!(context.count("clipboard_items"), 1);
        assert_eq!(context.count("clipboard_history"), 1);
        assert_eq!(context.count("favorites"), 1);
        assert_eq!(
            context
                .tag_repository
                .item_tag_ids(item_id)
                .expect("无法查询标签"),
            vec![existing_tag.id, new_tag.id]
        );
        assert!(
            context
                .favorite_repository
                .favorite_id_for_item(history_id)
                .expect("无法查询收藏关系")
                .is_some()
        );
    }

    #[test]
    fn duplicate_text_already_in_favorites_is_idempotent() {
        let context = TestContext::new();
        let first = context.persist("想法", &[]).expect("首次快捷收藏失败");
        assert_eq!(
            first,
            QuickFavoriteSaveResult {
                is_new: true,
                added: true
            }
        );

        let sort_order_before: i64 = context
            .database
            .connect()
            .expect("无法连接测试数据库")
            .query_row("SELECT sort_order FROM favorites LIMIT 1", [], |row| {
                row.get(0)
            })
            .expect("无法查询收藏顺序");
        let second = context.persist("想法", &[]).expect("重复快捷收藏失败");

        assert_eq!(
            second,
            QuickFavoriteSaveResult {
                is_new: false,
                added: false
            }
        );
        assert_eq!(context.count("clipboard_items"), 1);
        assert_eq!(context.count("clipboard_history"), 1);
        assert_eq!(context.count("favorites"), 1);
        let sort_order_after: i64 = context
            .database
            .connect()
            .expect("无法连接测试数据库")
            .query_row("SELECT sort_order FROM favorites LIMIT 1", [], |row| {
                row.get(0)
            })
            .expect("无法查询收藏顺序");
        assert_eq!(sort_order_after, sort_order_before);
    }

    #[test]
    fn trailing_whitespace_is_a_different_text() {
        let context = TestContext::new();
        let first = context.persist("想法", &[]).expect("首次快捷收藏失败");
        let second = context.persist("想法 ", &[]).expect("空白差异收藏失败");

        let created = QuickFavoriteSaveResult {
            is_new: true,
            added: true,
        };
        assert_eq!(first, created);
        assert_eq!(second, created);
        assert_eq!(context.count("clipboard_items"), 2);
        assert_eq!(context.count("clipboard_history"), 2);
        assert_eq!(context.count("favorites"), 2);
    }

    #[test]
    fn empty_text_still_creates_a_favorite() {
        let context = TestContext::new();
        let outcome = context.persist("", &[]).expect("空内容收藏失败");

        assert_eq!(
            outcome,
            QuickFavoriteSaveResult {
                is_new: true,
                added: true
            }
        );
        assert_eq!(context.count("clipboard_items"), 1);
        assert_eq!(context.count("favorites"), 1);
    }

    #[test]
    fn bound_item_edit_preserves_existing_tags_and_adds_new_ones() {
        let context = TestContext::new();
        let store = context.capture("原始文本");
        let bound = BoundFavoriteItem {
            item_id: store.item_id,
            history_id: store.history_id,
            original_text: "原始文本".to_owned(),
            is_new: true,
        };
        let first_tag = context
            .tag_repository
            .create("工作", "#1677FF")
            .expect("无法创建第一个标签");
        let second_tag = context
            .tag_repository
            .create("开发", "#00FF00")
            .expect("无法创建第二个标签");

        let created = persist_bound_favorite(
            bound.clone(),
            "原始文本",
            &[first_tag.id],
            &context.repositories(),
        )
        .expect("无法收藏绑定内容");
        assert_eq!(
            created,
            QuickFavoriteSaveResult {
                is_new: true,
                added: true
            }
        );

        let edited = persist_bound_favorite(
            BoundFavoriteItem {
                is_new: false,
                ..bound
            },
            "修改后的文本",
            &[second_tag.id],
            &context.repositories(),
        )
        .expect("无法更新绑定内容");
        assert_eq!(
            edited,
            QuickFavoriteSaveResult {
                is_new: false,
                added: false
            }
        );
        assert_eq!(context.favorite_count(), 1);
        assert_eq!(context.count("clipboard_items"), 1);
        assert_eq!(
            context
                .tag_repository
                .item_tag_ids(store.item_id)
                .expect("无法查询标签"),
            {
                let mut expected = vec![first_tag.id, second_tag.id];
                expected.sort_unstable();
                expected
            }
        );
        assert_eq!(context.preview_text(store.item_id), "修改后的文本");
    }

    #[test]
    fn captured_item_without_editor_is_favorited_directly() {
        let context = TestContext::new();
        let store = context.capture("图片占位");

        let outcome =
            favorite_captured_item(&store, &context.repositories()).expect("无法直接收藏捕获条目");

        assert_eq!(
            outcome,
            QuickFavoriteSaveResult {
                is_new: true,
                added: true
            }
        );
        assert_eq!(context.favorite_count(), 1);
        assert_eq!(context.count("clipboard_items"), 1);
        assert_eq!(context.count("clipboard_history"), 1);
    }

    #[test]
    fn bound_item_edit_hash_conflict_fails_without_favoriting() {
        let context = TestContext::new();
        let history_id = context
            .custom_text_service
            .create_manual_text("原始文本")
            .expect("无法创建待编辑文本");
        let item_id = context
            .custom_text_service
            .find_manual_text_item("原始文本")
            .expect("无法查询待编辑文本")
            .expect("待编辑文本不存在");
        context
            .custom_text_service
            .create_manual_text("已存在")
            .expect("无法创建冲突文本");
        let bound = BoundFavoriteItem {
            item_id,
            history_id,
            original_text: "原始文本".to_owned(),
            is_new: false,
        };

        let result = persist_bound_favorite(bound, "已存在", &[], &context.repositories());

        assert!(result.is_err());
        assert_eq!(context.favorite_count(), 0);
    }

    #[test]
    fn notification_messages_follow_the_three_outcomes() {
        assert_eq!(
            notification_message(QuickFavoriteSaveResult {
                is_new: true,
                added: true
            }),
            (
                "notification.quick_favorite.added",
                NotificationLevel::Success
            )
        );
        assert_eq!(
            notification_message(QuickFavoriteSaveResult {
                is_new: false,
                added: true
            }),
            (
                "notification.quick_favorite.exists_added",
                NotificationLevel::Success
            )
        );
        assert_eq!(
            notification_message(QuickFavoriteSaveResult {
                is_new: false,
                added: false
            }),
            (
                "notification.quick_favorite.exists_present",
                NotificationLevel::Info
            )
        );
    }
    fn windows_capture(image: bool) -> CapturedClipboard {
        use crate::domain::capture::{FormatKind, RawFormat, SourceApplication};
        let formats = if image {
            vec![RawFormat::captured(
                "windows",
                "PNG",
                "PNG",
                FormatKind::Image,
                vec![137, 80, 78, 71, 13, 10, 26, 10],
                0,
                true,
            )]
        } else {
            vec![
                RawFormat::captured(
                    "windows",
                    "CF_UNICODETEXT",
                    "Unicode text",
                    FormatKind::Text,
                    "富文本\0"
                        .encode_utf16()
                        .flat_map(u16::to_le_bytes)
                        .collect(),
                    0,
                    true,
                ),
                RawFormat::captured(
                    "windows",
                    "HTML Format",
                    "HTML",
                    FormatKind::Html,
                    b"<b>rich</b>".to_vec(),
                    1,
                    true,
                ),
                RawFormat::captured(
                    "windows",
                    "Rich Text Format",
                    "RTF",
                    FormatKind::Rtf,
                    br"{\rtf1 rich}".to_vec(),
                    2,
                    true,
                ),
            ]
        };
        CapturedClipboard {
            primary_format_identity: Some((
                formats[0].format_namespace.clone(),
                formats[0].format_key.clone(),
            )),
            formats,
            preview_candidate: None,
            source_application: SourceApplication::default(),
        }
    }

    #[test]
    fn failed_rich_capture_is_not_a_manual_favorite_and_retry_preserves_formats() {
        let context = TestContext::new();
        let captured = windows_capture(false);
        let connection = context.database.connect().unwrap();
        connection
            .execute_batch(
                "CREATE TRIGGER reject_capture BEFORE INSERT ON clipboard_items
             BEGIN SELECT RAISE(ABORT, 'temporary capture failure'); END;",
            )
            .unwrap();
        let failed = store_selection(&context.capture_service, Some(&captured));
        assert!(
            matches!(failed, Err(CaptureError::Sql(_))),
            "storage error must reach the UI: {failed:?}"
        );
        assert_eq!(context.favorite_count(), 0);
        connection
            .execute_batch("DROP TRIGGER reject_capture")
            .unwrap();
        let SelectionOutcome::Stored { text, store } =
            store_selection(&context.capture_service, Some(&captured)).unwrap()
        else {
            panic!("retry must produce the captured item")
        };
        let bound = BoundFavoriteItem {
            item_id: store.item_id,
            history_id: store.history_id,
            original_text: text.unwrap(),
            is_new: store.is_new,
        };
        let session = FavoriteSession::new();
        assert!(
            session
                .save(Some(bound.clone()), "编辑后", &[], &context.repositories())
                .unwrap()
                .0
                .added
        );
        assert!(
            !session
                .save(Some(bound), "编辑后", &[], &context.repositories())
                .unwrap()
                .0
                .added
        );
        for format in &captured.formats {
            let actual: Vec<u8> = connection.query_row(
                "SELECT inline_data FROM clipboard_formats WHERE item_id = ?1 AND format_key = ?2",
                rusqlite::params![store.item_id, format.format_key], |row| row.get(0),
            ).unwrap();
            assert_eq!(actual, *format.bytes.as_ref().unwrap());
        }
        assert_eq!(context.count("clipboard_items"), 1);
        assert_eq!(context.favorite_count(), 1);
    }

    #[test]
    fn failed_image_capture_is_not_reported_as_no_selection() {
        let context = TestContext::new();
        context
            .database
            .connect()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER reject_capture BEFORE INSERT ON clipboard_items
             BEGIN SELECT RAISE(ABORT, 'temporary capture failure'); END;",
            )
            .unwrap();
        let result = store_selection(&context.capture_service, Some(&windows_capture(true)));
        assert!(matches!(result, Err(CaptureError::Sql(_))), "{result:?}");
    }

    #[test]
    fn retired_editor_cannot_save_a_bound_item_into_replacement_database() {
        let old = TestContext::new();
        let new = TestContext::new();
        let old_item = old.capture("old");
        let new_item = new.capture("new");
        assert_eq!(old_item.item_id, new_item.item_id);
        let session = FavoriteSession::new();
        session.retire();
        let bound = BoundFavoriteItem {
            item_id: old_item.item_id,
            history_id: old_item.history_id,
            original_text: "old".into(),
            is_new: true,
        };
        let result = session.save(Some(bound), "overwrite", &[], &new.repositories());
        assert!(
            result.is_err(),
            "retired editor must not reuse the ID in the replacement database"
        );
        assert_eq!(new.preview_text(new_item.item_id), "new");
        assert_eq!(old.favorite_count(), 0);
        assert_eq!(new.favorite_count(), 0);
    }

    #[test]
    fn queued_capture_cannot_write_to_retired_data_source() {
        let context = TestContext::new();
        let session = FavoriteSession::new();
        let queued = session.clone();
        session.retire();
        let result = queued.capture(&context.capture_service, Some(&windows_capture(false)));
        assert!(result.is_err());
        assert_eq!(context.count("clipboard_items"), 0);
    }

    #[test]
    fn replacement_routes_manual_capture_and_external_blobs_to_new_sources() {
        let old = TestContext::new();
        let new = TestContext::new();
        let mut service = QuickFavoriteService {
            repositories: old.repositories(),
            capture_service: old.capture_service.clone(),
            clipboard_write_suppressor: ClipboardWriteSuppressor::default(),
            editor_window: None,
            opening: false,
            session: FavoriteSession::new(),
        };
        service.session.retire();
        service.replace_sources(
            new.database.clone(),
            RawBlobStore::new(new._directory.path()).unwrap(),
        );
        service.session = FavoriteSession::new();
        let tag = new.tag_repository.create("new source", "#1677FF").unwrap();
        service
            .session
            .save(None, "manual", &[tag.id], &service.repositories)
            .unwrap();
        let mut captured = windows_capture(false);
        captured.formats[1].bytes = Some(vec![b'x'; 80 * 1024]);
        let SelectionOutcome::Stored { text, store } = service
            .session
            .capture(&service.capture_service, Some(&captured))
            .unwrap()
        else {
            panic!("capture must be stored")
        };
        let bound = BoundFavoriteItem {
            item_id: store.item_id,
            history_id: store.history_id,
            original_text: text.unwrap(),
            is_new: store.is_new,
        };
        service
            .session
            .save(Some(bound), "edited", &[tag.id], &service.repositories)
            .unwrap();
        assert_eq!(old.count("clipboard_items"), 0);
        assert_eq!(old.count("binary_blobs"), 0);
        assert_eq!(new.count("clipboard_items"), 2);
        assert_eq!(new.favorite_count(), 2);
        assert_eq!(
            new.tag_repository.item_tag_ids(store.item_id).unwrap(),
            vec![tag.id]
        );
        let connection = new.database.connect().unwrap();
        let relative_path: String = connection
            .query_row("SELECT relative_path FROM binary_blobs", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(new._directory.path().join(&relative_path).is_file());
        assert!(!old._directory.path().join(&relative_path).exists());
    }

    #[test]
    fn only_absent_selection_is_empty_and_non_text_capture_stays_bound() {
        let context = TestContext::new();
        assert!(matches!(
            store_selection(&context.capture_service, None),
            Ok(SelectionOutcome::Empty)
        ));
        let mut failed_formats = windows_capture(false);
        failed_formats.formats.clear();
        assert!(matches!(
            store_selection(&context.capture_service, Some(&failed_formats)),
            Err(CaptureError::NoCapturedFormats)
        ));
        for kind in [
            crate::domain::capture::FormatKind::Image,
            crate::domain::capture::FormatKind::FileList,
        ] {
            let mut captured = windows_capture(true);
            if kind == crate::domain::capture::FormatKind::FileList {
                captured.formats[0] = crate::domain::capture::RawFormat::captured(
                    "mime",
                    "text/uri-list",
                    "Files",
                    kind,
                    b"file:///C:/example.txt".to_vec(),
                    0,
                    true,
                );
                captured.primary_format_identity = Some(("mime".into(), "text/uri-list".into()));
            }
            let SelectionOutcome::Stored { text, store } =
                store_selection(&context.capture_service, Some(&captured)).unwrap()
            else {
                panic!("non-text capture must be bound")
            };
            assert_eq!(text, None);
            let result = favorite_captured_item(&store, &context.repositories()).unwrap();
            assert!(result.added);
            assert!(
                !favorite_captured_item(&store, &context.repositories())
                    .unwrap()
                    .added
            );
        }
        assert_eq!(context.count("clipboard_items"), 2);
        assert_eq!(context.favorite_count(), 2);
    }

    #[test]
    fn failed_switch_resumes_with_a_fresh_session_only() {
        let context = TestContext::new();
        let old = FavoriteSession::new();
        old.retire();
        let resumed = FavoriteSession::new();
        resumed
            .save(None, "after failed switch", &[], &context.repositories())
            .unwrap();
        assert!(
            old.save(None, "stale", &[], &context.repositories())
                .is_err()
        );
        assert_eq!(context.count("clipboard_items"), 1);
        assert_eq!(context.favorite_count(), 1);
    }

    #[test]
    fn runtime_switch_waits_for_active_session_and_rejects_queued_work() {
        // Isolate the process-global runtime so parallel database tests remain independent.
        const CHILD: &str = "QCCLIP_TEST_FAVORITE_RUNTIME";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "services::quick_favorite::tests::runtime_switch_waits_for_active_session_and_rejects_queued_work", "--test-threads=1"])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        use std::{sync::mpsc, time::Duration};
        let context = TestContext::new();
        data_runtime::initialize();
        let old = FavoriteSession::new();
        let active = old.begin_operation().unwrap();
        old.retire();
        let (started_tx, started_rx) = mpsc::channel();
        let (switched_tx, switched_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let switch = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let _switch = data_runtime::begin_switch();
            switched_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(
            switched_rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        drop(active);
        switched_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let current = FavoriteSession::new();
        let captured = windows_capture(false);
        assert!(
            current
                .capture(&context.capture_service, Some(&captured))
                .is_err()
        );
        release_tx.send(()).unwrap();
        switch.join().unwrap();
        assert!(
            old.capture(&context.capture_service, Some(&captured))
                .is_err()
        );
        assert_eq!(context.count("clipboard_items"), 0);
        assert!(matches!(
            current.capture(&context.capture_service, Some(&captured)),
            Ok(SelectionOutcome::Stored { .. })
        ));
        assert_eq!(context.count("clipboard_items"), 1);
    }
}
