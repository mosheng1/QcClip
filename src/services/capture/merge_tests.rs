use rusqlite::params;

use super::{CaptureService, hash_clipboard, mock::MockClipboardSource};
use crate::{
    domain::capture::{CapturedClipboard, FormatKind, RawFormat},
    services::{
        database::query::HistoryQuery,
        database::{Database, HistoryRepository},
        paste::PasteService,
        storage::RawBlobStore,
    },
};

struct Fixture {
    _directory: tempfile::TempDir,
    database: Database,
    blobs: RawBlobStore,
    capture: CaptureService,
}

impl Fixture {
    fn new(threshold: usize) -> Self {
        let directory = tempfile::tempdir().expect("无法创建临时目录");
        let database =
            Database::open(directory.path().join("test.sqlite3")).expect("无法创建数据库");
        let blobs = RawBlobStore::new(directory.path()).expect("无法创建文件存储");
        let capture =
            CaptureService::with_inline_threshold(database.clone(), blobs.clone(), threshold);
        Self {
            _directory: directory,
            database,
            blobs,
            capture,
        }
    }

    fn store(&self, captured: &CapturedClipboard) -> i64 {
        self.capture
            .store_captured_clipboard(captured)
            .expect("无法保存剪贴板内容")
    }

    fn bytes(&self, history_id: i64, key: &str) -> Option<Vec<u8>> {
        let connection = self.database.connect().expect("无法连接数据库");
        use rusqlite::OptionalExtension;
        let stored = connection
            .query_row(
                "SELECT f.inline_data, b.sha256, b.byte_length, b.relative_path
             FROM clipboard_formats f JOIN clipboard_history h ON h.item_id = f.item_id
             LEFT JOIN binary_blobs b ON b.id = f.blob_id
             WHERE h.id = ?1 AND f.format_key = ?2",
                params![history_id, key],
                |row| {
                    Ok((
                        row.get::<_, Option<Vec<u8>>>(0)?,
                        row.get::<_, Option<Vec<u8>>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()
            .expect("无法查询原始格式")?;
        stored.0.or_else(|| {
            let blob = crate::services::storage::RawBlob::from_record(
                stored.1?.try_into().ok()?,
                stored.2?.try_into().ok()?,
                stored.3?,
            );
            Some(self.blobs.read(&blob).expect("无法读取外部格式"))
        })
    }
}

fn text(value: &str) -> CapturedClipboard {
    MockClipboardSource::text(value).capture()
}

fn add_format(captured: &mut CapturedClipboard, key: &str, kind: FormatKind, bytes: &[u8]) {
    captured.formats.push(RawFormat::captured(
        "mime",
        key,
        key,
        kind,
        bytes.to_vec(),
        captured.formats.len() as u32,
        true,
    ));
}

fn rich(value: &str, html: &str) -> CapturedClipboard {
    let mut captured = text(value);
    add_format(
        &mut captured,
        "text/html",
        FormatKind::Html,
        html.as_bytes(),
    );
    captured
}

#[test]
fn candidate_lookup_uses_indexes_instead_of_scanning_all_items() {
    let fixture = Fixture::new(65536);
    fixture.store(&text("hello"));
    let connection = fixture.database.connect().expect("无法连接数据库");
    let mut statement = connection
        .prepare(&format!(
            "EXPLAIN QUERY PLAN {}",
            super::text_merge::CANDIDATE_QUERY
        ))
        .expect("无法检查查询计划");
    let plan = statement
        .query_map(params!["hello", Option::<i64>::None, "hello"], |row| {
            row.get::<_, String>(3)
        })
        .expect("无法执行查询计划")
        .collect::<Result<Vec<_>, _>>()
        .expect("无法读取查询计划");
    assert_eq!(
        plan.iter()
            .filter(|line| line
                .contains("SEARCH clipboard_formats USING INDEX ix_clipboard_formats_text_merge"))
            .count(),
        2,
        "查询必须通过索引分别查找相同预览及缺失预览：{plan:?}"
    );
    assert!(
        !plan
            .iter()
            .any(|line| line.starts_with("SCAN i ") || line.starts_with("SCAN clipboard_formats")),
        "不应遍历全库：{plan:?}"
    );
}

#[test]
fn same_text_supplements_formats_and_preserves_rich_text_on_plain_copy() {
    for threshold in [4, 65536] {
        let fixture = Fixture::new(threshold);
        let id = fixture.store(&text("你好"));
        let rich = rich("你好", "<b>你好</b>");
        assert_eq!(fixture.store(&rich), id);
        assert_eq!(fixture.store(&text("你好")), id);
        assert_eq!(
            fixture.bytes(id, "text/html"),
            Some("<b>你好</b>".as_bytes().to_vec())
        );
        let connection = fixture.database.connect().expect("无法连接数据库");
        let values: (i64, i64, i64, Vec<u8>) = connection.query_row(
            "SELECT (SELECT COUNT(*) FROM clipboard_items), i.format_count, h.capture_count, i.content_hash
             FROM clipboard_items i JOIN clipboard_history h ON h.item_id = i.id WHERE h.id = ?1",
            [id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        ).expect("无法查询合并结果");
        assert_eq!((values.0, values.1, values.2), (1, 2, 3));
        assert_eq!(
            values.3,
            hash_clipboard(&rich).expect("无法计算哈希").content_hash
        );
        assert_eq!(
            PasteService::new(fixture.database.clone(), fixture.blobs.clone())
                .item_plain_text(
                    connection
                        .query_row(
                            "SELECT item_id FROM clipboard_history WHERE id = ?1",
                            [id],
                            |row| row.get(0)
                        )
                        .expect("无法查询内容 ID")
                )
                .expect("无法读取粘贴文本"),
            Some("你好".to_owned())
        );
    }
}

#[test]
fn capture_feedback_requires_new_history_reordering_or_changed_formats() {
    let fixture = Fixture::new(65536);
    let store = |captured: &CapturedClipboard| {
        fixture
            .capture
            .store_captured_clipboard_with_status(captured)
            .expect("无法保存剪贴板内容")
    };
    let first = store(&text("hello"));
    assert!(first.should_notify());
    let pinned = fixture.store(&text("置顶"));
    let repository = HistoryRepository::new(fixture.database.clone(), fixture.blobs.clone());
    repository.set_pinned(pinned, true).expect("无法置顶");

    for (captured, should_notify) in [
        (text("hello"), false),
        (text("其他"), true),
        (text("hello"), true),
        (text("hello"), false),
        (rich("hello", "<b>hello</b>"), true),
        (rich("hello", "<b>hello</b>"), false),
        (rich("hello", "<i>hello</i>"), true),
        (text("hello"), false),
    ] {
        let result = store(&captured);
        assert_eq!(
            result.should_notify(),
            should_notify,
            "通知判断错误：{result:?}"
        );
    }
    let rows = repository
        .query(&HistoryQuery::default())
        .expect("无法查询列表");
    assert_eq!(
        rows.iter()
            .take(2)
            .map(|row| row.history_id)
            .collect::<Vec<_>>(),
        [pinned, first.history_id]
    );
    let capture_count: i64 = fixture
        .database
        .connect()
        .expect("无法连接数据库")
        .query_row(
            "SELECT capture_count FROM clipboard_history WHERE id = ?1",
            [first.history_id],
            |row| row.get(0),
        )
        .expect("无法查询复制次数");
    assert_eq!(capture_count, 8);
}

#[test]
fn latest_restorable_format_wins_and_failed_copy_does_not_remove_it() {
    let fixture = Fixture::new(4);
    let id = fixture.store(&rich("hello", "<b>hello</b>"));
    assert_eq!(fixture.store(&rich("hello", "<i>hello</i>")), id);
    let mut failed = text("hello");
    failed.formats.push(RawFormat::failed(
        "mime",
        "text/html",
        "HTML",
        FormatKind::Html,
        1,
        "读取失败",
        "模拟格式读取失败",
    ));
    assert_eq!(fixture.store(&failed), id);
    let mut unrestorable = rich("hello", "<u>hello</u>");
    unrestorable.formats[1].restore_state = crate::domain::capture::RestoreState::NotRestorable;
    assert_eq!(fixture.store(&unrestorable), id);
    assert_eq!(
        fixture.bytes(id, "text/html"),
        Some(b"<i>hello</i>".to_vec())
    );
}

#[test]
fn merge_keeps_identity_metadata_and_moves_below_pinned_items() {
    let fixture = Fixture::new(65536);
    let id = fixture.store(&text("hello"));
    let connection = fixture.database.connect().expect("无法连接数据库");
    let item_id: i64 = connection
        .query_row(
            "SELECT item_id FROM clipboard_history WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .expect("无法查询内容 ID");
    connection
        .execute(
            "UPDATE clipboard_history SET paste_count = 7 WHERE id = ?1",
            [id],
        )
        .expect("无法写入粘贴次数");
    connection.execute("INSERT INTO favorites (uuid, item_id, is_pinned, sort_order, added_at, updated_at) VALUES ('收藏', ?1, 0, 0, 1, 1)", [item_id]).expect("无法创建收藏");
    connection.execute("INSERT INTO clipboard_item_notes (uuid, item_id, note_text, created_at, updated_at) VALUES ('备注', ?1, '保留备注', 1, 1)", [item_id]).expect("无法创建备注");
    connection.execute("INSERT INTO tags (uuid, name, normalized_name, color, created_at, updated_at) VALUES ('标签', '工作', '工作', '#1677FF', 1, 1)", []).expect("无法创建标签");
    let tag_id = connection.last_insert_rowid();
    connection.execute("INSERT INTO clipboard_item_tags (uuid, item_id, tag_id, created_at, updated_at) VALUES ('关联', ?1, ?2, 1, 1)", [item_id, tag_id]).expect("无法关联标签");
    drop(connection);
    let pinned = fixture.store(&text("置顶"));
    let repository = HistoryRepository::new(fixture.database.clone(), fixture.blobs.clone());
    repository.set_pinned(pinned, true).expect("无法置顶");
    let other = fixture.store(&text("其他"));
    let mut incoming = rich("hello", "<b>hello</b>");
    incoming.source_application.name = Some("最新应用".to_owned());
    assert_eq!(fixture.store(&incoming), id);
    let rows = repository
        .query(&HistoryQuery::default())
        .expect("无法查询列表");
    assert_eq!(
        rows.iter().map(|row| row.history_id).collect::<Vec<_>>(),
        [pinned, id, other]
    );
    assert_eq!(rows[1].item_id, item_id);
    assert_eq!(rows[1].paste_count, 7);
    assert!(rows[1].favorite_id.is_some());
    assert_eq!(rows[1].tag_ids, [tag_id]);
    assert_eq!(rows[1].note_text, "保留备注");
    assert_eq!(rows[1].source_app_name.as_deref(), Some("最新应用"));
    repository.set_pinned(id, true).expect("无法置顶合并条目");
    assert_eq!(fixture.store(&text("hello")), id);
    assert!(
        repository
            .find_by_id(id, &HistoryQuery::default())
            .expect("无法查询条目")
            .expect("条目丢失")
            .is_pinned
    );
}

#[test]
fn preview_equality_does_not_merge_different_full_text() {
    let fixture = Fixture::new(65536);
    let values = [
        "hello",
        " hello",
        "hello ",
        "hello\n",
        "Hello",
        "hello\n\nworld",
        "hello\nworld",
    ];
    let ids = values
        .iter()
        .map(|value| fixture.store(&text(value)))
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(ids.len(), values.len());
    let prefix = "相同前缀".repeat(1000);
    assert_ne!(
        fixture.store(&text(&format!("{prefix}甲"))),
        fixture.store(&text(&format!("{prefix}乙")))
    );
}

#[test]
fn images_files_embedded_objects_and_opaque_previews_remain_separate() {
    let fixture = Fixture::new(65536);
    let plain = fixture.store(&text("hello"));
    for (key, kind, first, second) in [
        (
            "image/png",
            FormatKind::Image,
            b"image-a".as_slice(),
            b"image-b".as_slice(),
        ),
        ("files", FormatKind::FileList, b"file-a", b"file-b"),
        (
            "text/html",
            FormatKind::Html,
            b"hello<img src='a'>",
            b"hello<img src='b'>",
        ),
        (
            "text/rtf",
            FormatKind::Rtf,
            br"{\rtf1 hello {\pict a}}",
            br"{\rtf1 hello {\pict b}}",
        ),
    ] {
        let mut a = text("hello");
        add_format(&mut a, key, kind, first);
        let mut b = text("hello");
        add_format(&mut b, key, kind, second);
        let a_id = fixture.store(&a);
        let b_id = fixture.store(&b);
        assert_ne!(a_id, plain, "不应按文本合并 {key}");
        assert_ne!(a_id, b_id, "不同的非文本内容不应合并 {key}");
    }
    for kind in [FormatKind::Rtf, FormatKind::Custom] {
        let mut a = text("hello");
        a.formats.clear();
        a.primary_format_identity = None;
        add_format(&mut a, "opaque", kind, b"a");
        let mut b = a.clone();
        b.formats[0].bytes = Some(b"b".to_vec());
        assert_ne!(fixture.store(&a), fixture.store(&b));
    }
}

#[test]
fn unicode_text_matches_utf8_without_merging_invalid_encodings() {
    let fixture = Fixture::new(65536);
    let id = fixture.store(&text("你好"));
    let mut unicode = text("你好");
    unicode.formats[0] = RawFormat::captured(
        "windows",
        "CF_UNICODETEXT",
        "Unicode 文本",
        FormatKind::Text,
        "你好\0".encode_utf16().flat_map(u16::to_le_bytes).collect(),
        0,
        true,
    );
    unicode.primary_format_identity = Some(("windows".to_owned(), "CF_UNICODETEXT".to_owned()));
    assert_eq!(fixture.store(&unicode), id);
    let mut invalid = text("替换字符");
    invalid.formats[0].bytes = Some(vec![0xff]);
    assert_ne!(fixture.store(&invalid), fixture.store(&text("�")));
}

#[test]
fn private_formats_are_combined_and_newer_data_replaces_only_the_same_format() {
    let fixture = Fixture::new(65536);
    let mut first = text("hello");
    add_format(&mut first, "app-a", FormatKind::Custom, b"old-private");
    let id = fixture.store(&first);
    let mut second = rich("hello", "<b>hello</b>");
    add_format(&mut second, "app-b", FormatKind::Custom, b"new-private");
    assert_eq!(fixture.store(&second), id);
    assert_eq!(fixture.bytes(id, "app-a"), Some(b"old-private".to_vec()));
    assert_eq!(fixture.bytes(id, "app-b"), Some(b"new-private".to_vec()));
    let mut third = text("hello");
    add_format(&mut third, "app-a", FormatKind::Custom, b"updated-private");
    assert_eq!(fixture.store(&third), id);
    assert_eq!(
        fixture.bytes(id, "app-a"),
        Some(b"updated-private".to_vec())
    );
    assert_eq!(fixture.bytes(id, "app-b"), Some(b"new-private".to_vec()));
}

#[test]
fn failed_private_recapture_preserves_saved_bytes_and_missing_format_records() {
    use crate::domain::capture::RestoreState;

    let fixture = Fixture::new(4);
    let mut first = text("hello");
    add_format(&mut first, "app-a", FormatKind::Custom, b"saved-private");
    first.formats[1].restore_state = RestoreState::NotRestorable;
    first.formats.push(RawFormat::failed(
        "mime",
        "app-b",
        "应用格式 B",
        FormatKind::Custom,
        2,
        "读取失败",
        "模拟格式读取失败",
    ));
    let id = fixture.store(&first);
    let mut second = text("hello");
    second.formats.push(RawFormat::failed(
        "mime",
        "app-a",
        "应用格式 A",
        FormatKind::Custom,
        1,
        "读取失败",
        "模拟再次读取失败",
    ));
    assert_eq!(fixture.store(&second), id);
    assert_eq!(fixture.bytes(id, "app-a"), Some(b"saved-private".to_vec()));
    let connection = fixture.database.connect().expect("无法连接数据库");
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM clipboard_formats f JOIN clipboard_history h ON h.item_id = f.item_id WHERE h.id = ?1",
        [id], |row| row.get(0),
    ).expect("无法查询保留的格式记录");
    assert_eq!(count, 3);
}

#[test]
fn unchanged_bytes_can_upgrade_a_format_to_restorable() {
    let fixture = Fixture::new(65536);
    let mut first = rich("hello", "<b>hello</b>");
    first.formats[1].restore_state = crate::domain::capture::RestoreState::NotRestorable;
    let id = fixture.store(&first);
    let result = fixture
        .capture
        .store_captured_clipboard_with_status(&rich("hello", "<b>hello</b>"))
        .expect("无法更新格式恢复状态");
    assert_eq!(result.history_id, id);
    assert!(!result.is_new);
    assert!(result.formats_changed);
    let connection = fixture.database.connect().expect("无法连接数据库");
    let state: String = connection
        .query_row(
            "SELECT restore_state FROM clipboard_formats WHERE format_key = 'text/html'",
            [],
            |row| row.get(0),
        )
        .expect("无法查询格式恢复状态");
    assert_eq!(state, "restorable");
    let repeated = fixture
        .capture
        .store_captured_clipboard_with_status(&rich("hello", "<b>hello</b>"))
        .expect("无法重复保存内容");
    assert!(!repeated.formats_changed);
}

#[test]
fn unchanged_formats_keep_their_records_when_capture_order_changes() {
    let fixture = Fixture::new(4);
    let captured = rich("hello", "<b>hello</b>");
    let id = fixture.store(&captured);
    fixture
        .database
        .connect_write()
        .expect("无法连接数据库")
        .execute_batch(
            "CREATE TRIGGER forbid_format_rewrite BEFORE DELETE ON clipboard_formats
         BEGIN SELECT RAISE(ABORT, '未变化格式不应重写'); END;",
        )
        .expect("无法安装格式重写检查");
    assert_eq!(fixture.store(&captured), id);
    let mut reordered = captured.clone();
    reordered.formats.reverse();
    for (index, format) in reordered.formats.iter_mut().enumerate() {
        format.format_order = index as u32;
    }
    assert_eq!(fixture.store(&reordered), id);
    assert_eq!(fixture.store(&text("hello")), id);
    assert_eq!(
        fixture.bytes(id, "text/html"),
        Some(b"<b>hello</b>".to_vec())
    );
}

#[test]
fn published_records_without_preview_can_merge_and_hash_conflicts_reuse_the_owner() {
    let fixture = Fixture::new(65536);
    let plain_id = fixture.store(&text("hello"));
    let connection = fixture.database.connect().expect("无法连接数据库");
    connection
        .execute("UPDATE clipboard_items SET origin_kind = 'manual'", [])
        .expect("无法准备旧版条目");
    drop(connection);
    let rich_id = fixture.store(&rich("hello", "<b>hello</b>"));
    assert_ne!(plain_id, rich_id);
    let connection = fixture.database.connect().expect("无法连接数据库");
    connection.execute_batch("UPDATE clipboard_items SET origin_kind = 'captured';
        UPDATE clipboard_formats SET preview_text = NULL, preview_char_count = NULL, preview_content_kind = NULL;")
        .expect("无法准备旧版预览字段");
    drop(connection);
    let mut incoming = rich("hello", "<b>hello</b>");
    incoming.formats[1].capture_state = crate::domain::capture::FormatCaptureState::Generated;
    // 原始格式哈希已有所属条目，元数据变化仍应更新该条目。
    assert_eq!(fixture.store(&incoming), rich_id);
    assert_eq!(fixture.store(&text("hello")), plain_id);
    let connection = fixture.database.connect().expect("无法连接数据库");
    let count: i64 = connection
        .query_row("SELECT COUNT(*) FROM clipboard_items", [], |row| row.get(0))
        .expect("无法检查旧版条目数量");
    assert_eq!(count, 2);
}

#[test]
fn user_edited_text_is_not_overwritten_by_captured_content() {
    let fixture = Fixture::new(65536);
    let id = fixture.store(&text("hello"));
    let connection = fixture.database.connect().expect("无法连接数据库");
    let item_id = connection
        .query_row(
            "SELECT item_id FROM clipboard_history WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .expect("无法查询内容 ID");
    drop(connection);
    let custom = crate::services::custom_text::CustomTextService::new(
        fixture.database.clone(),
        fixture.blobs.clone(),
    );
    custom
        .save_for_item(item_id, "用户编辑")
        .expect("无法编辑条目");
    assert_ne!(fixture.store(&rich("hello", "<b>hello</b>")), id);
    assert_ne!(fixture.store(&text("用户编辑")), id);
    assert_eq!(
        PasteService::new(fixture.database.clone(), fixture.blobs.clone())
            .item_plain_text(item_id)
            .expect("无法读取用户编辑文本"),
        Some("用户编辑".to_owned())
    );
}

#[test]
fn failed_merge_rolls_back_original_formats_hash_and_history() {
    let fixture = Fixture::new(65536);
    let id = fixture.store(&rich("hello", "<b>hello</b>"));
    let connection = fixture.database.connect().expect("无法连接数据库");
    connection
        .execute_batch(
            "CREATE TRIGGER fail_merge BEFORE INSERT ON clipboard_formats
        WHEN NEW.format_key = 'text/html' BEGIN SELECT RAISE(ABORT, '模拟合并失败'); END;",
        )
        .expect("无法设置失败条件");
    drop(connection);
    assert!(
        fixture
            .capture
            .store_captured_clipboard(&rich("hello", "<i>hello</i>"))
            .is_err()
    );
    assert_eq!(
        fixture.bytes(id, "text/html"),
        Some(b"<b>hello</b>".to_vec())
    );
    let connection = fixture.database.connect().expect("无法连接数据库");
    let values: (String, i64, Vec<u8>) = connection
        .query_row(
            "SELECT i.capture_state, h.capture_count, i.content_hash FROM clipboard_history h
         JOIN clipboard_items i ON i.id = h.item_id WHERE h.id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("无法检查事务回滚");
    assert_eq!(values.0, "ready");
    assert_eq!(values.1, 1);
    assert_eq!(
        values.2,
        hash_clipboard(&rich("hello", "<b>hello</b>"))
            .expect("无法计算原始哈希")
            .content_hash
    );
}

#[test]
fn updating_html_reuses_unchanged_external_formats() {
    let fixture = Fixture::new(65536);
    let mut first = rich("hello", "<b>hello</b>");
    let bytes = vec![42; 128 * 1024];
    add_format(&mut first, "app-private", FormatKind::Custom, &bytes);
    let id = fixture.store(&first);
    fixture
        .database
        .connect_write()
        .expect("无法连接数据库")
        .execute_batch(
            "CREATE TRIGGER forbid_blob_rewrite BEFORE UPDATE ON binary_blobs
         BEGIN SELECT RAISE(ABORT, '未变化的外部格式不应重新落盘'); END;",
        )
        .expect("无法安装外部格式重写检查");
    let mut second = first.clone();
    second.formats[1].bytes = Some(b"<i>hello</i>".to_vec());
    assert_eq!(fixture.store(&second), id);
    assert_eq!(fixture.bytes(id, "app-private"), Some(bytes));
    assert_eq!(
        fixture.bytes(id, "text/html"),
        Some(b"<i>hello</i>".to_vec())
    );
}

#[test]
fn replacing_external_formats_reclaims_only_unreferenced_blobs() {
    let fixture = Fixture::new(0);
    let first = rich("hello", "<b>hello</b>");
    let id = fixture.store(&first);
    let connection = fixture.database.connect().expect("无法连接数据库");
    let old_path: String = connection
        .query_row(
            "SELECT b.relative_path FROM binary_blobs b
        JOIN clipboard_formats f ON f.blob_id = b.id WHERE f.format_key = 'text/html'",
            [],
            |row| row.get(0),
        )
        .expect("无法查询旧格式文件");
    drop(connection);
    let mut other = rich("other", "<b>hello</b>");
    let other_id = fixture.store(&other);
    assert_eq!(fixture.store(&rich("hello", "<i>hello</i>")), id);
    assert!(fixture._directory.path().join(&old_path).exists());
    assert_eq!(
        fixture.bytes(other_id, "text/html"),
        Some(b"<b>hello</b>".to_vec())
    );
    other.formats[1].bytes = Some(b"<i>other</i>".to_vec());
    assert_eq!(fixture.store(&other), other_id);
    assert!(!fixture._directory.path().join(&old_path).exists());
    let connection = fixture.database.connect().expect("无法连接数据库");
    let orphans: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM binary_blobs b
        WHERE NOT EXISTS (SELECT 1 FROM clipboard_formats WHERE blob_id = b.id)",
            [],
            |row| row.get(0),
        )
        .expect("无法检查文件索引");
    assert_eq!(orphans, 0);
}

#[test]
fn compatible_future_database_preserves_its_formats_without_rewriting() {
    let fixture = Fixture::new(65536);
    let id = fixture.store(&rich("hello", "<b>hello</b>"));
    let connection = fixture.database.connect().expect("无法连接数据库");
    connection
        .pragma_update(
            None,
            "user_version",
            crate::services::database::CURRENT_SCHEMA_VERSION + 1,
        )
        .expect("无法模拟新版数据库");
    drop(connection);
    assert_ne!(fixture.store(&rich("hello", "<i>hello</i>")), id);
    assert_eq!(
        fixture.bytes(id, "text/html"),
        Some(b"<b>hello</b>".to_vec())
    );
}

#[test]
fn missing_legacy_previews_and_old_hash_versions_still_support_text_merge() {
    let fixture = Fixture::new(65536);
    let id = fixture.store(&text("hello"));
    let connection = fixture.database.connect().expect("无法连接数据库");
    connection
        .execute_batch(
            "UPDATE clipboard_formats SET preview_text = NULL,
        preview_char_count = NULL, preview_content_kind = NULL;
        UPDATE clipboard_items SET hash_version = 1, content_hash = zeroblob(32);",
        )
        .expect("无法准备旧版格式记录");
    drop(connection);
    assert_eq!(fixture.store(&rich("hello", "<b>hello</b>")), id);
    assert_eq!(
        fixture.bytes(id, "text/html"),
        Some(b"<b>hello</b>".to_vec())
    );
}

#[test]
fn existing_merged_hash_is_reused_without_creating_or_overwriting_another_item() {
    let fixture = Fixture::new(65536);
    let html = rich("hello", "<b>hello</b>");
    let first = fixture.store(&html);
    let mut combined = html.clone();
    add_format(
        &mut combined,
        "text/rtf",
        FormatKind::Rtf,
        br"{\rtf1 hello}",
    );
    let connection = fixture.database.connect().expect("无法连接数据库");
    connection
        .execute("UPDATE clipboard_items SET origin_kind = 'manual'", [])
        .expect("无法准备旧条目");
    drop(connection);
    let owner = fixture.store(&combined);
    let connection = fixture.database.connect().expect("无法连接数据库");
    connection
        .execute_batch("UPDATE clipboard_items SET origin_kind = 'captured';")
        .expect("无法恢复旧条目来源");
    drop(connection);
    HistoryRepository::new(fixture.database.clone(), fixture.blobs.clone())
        .set_pinned(first, true)
        .expect("无法置顶旧条目");
    let mut incoming = text("hello");
    add_format(
        &mut incoming,
        "text/rtf",
        FormatKind::Rtf,
        br"{\rtf1 hello}",
    );
    assert_eq!(fixture.store(&incoming), owner);
    assert!(fixture.bytes(first, "text/rtf").is_none());
    assert_eq!(
        fixture.bytes(owner, "text/html"),
        Some(b"<b>hello</b>".to_vec())
    );
}

#[test]
fn html_embedded_content_and_unknown_text_encoding_do_not_merge_by_preview() {
    let fixture = Fixture::new(65536);
    let id = fixture.store(&text("hello"));
    for html in [
        "hello<svg></svg>",
        "hello<object data='a'></object>",
        "<span style='background:url(a)'>hello</span>",
    ] {
        assert_ne!(fixture.store(&rich("hello", html)), id);
    }
    let mut unknown = text("hello");
    unknown.formats[0].encoding = Some("unknown-encoding".to_owned());
    add_format(&mut unknown, "text/html", FormatKind::Html, b"<b>hello</b>");
    assert_ne!(fixture.store(&unknown), id);
}

fn windows_text(value: &str) -> CapturedClipboard {
    let mut captured = text(value);
    captured.formats[0] = RawFormat::captured(
        "windows",
        "CF_UNICODETEXT",
        "CF_UNICODETEXT",
        FormatKind::Text,
        value
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect(),
        0,
        true,
    );
    captured.formats[0].encoding = Some("utf-16le".to_owned());
    captured.primary_format_identity = Some(("windows".to_owned(), "CF_UNICODETEXT".to_owned()));
    captured
}

fn excel_cell(value: &str) -> CapturedClipboard {
    let mut captured = windows_text(&format!("{value}\r\n"));
    captured.source_application.name = Some("EXCEL".to_owned());
    for (key, kind, bytes) in [
        ("registered:XML Spreadsheet", FormatKind::Custom,
            format!(r#"<Workbook xmlns="urn:schemas-microsoft-com:office:spreadsheet" xmlns:ss="urn:schemas-microsoft-com:office:spreadsheet"><Worksheet ss:Name="Sheet1"><Table ss:ExpandedColumnCount="1" ss:ExpandedRowCount="1"><Row><Cell><Data ss:Type="String">{value}</Data></Cell></Row></Table></Worksheet></Workbook>"#).into_bytes()),
        ("registered:HTML Format", FormatKind::Html,
            format!("<html><style>.xl65{{color:red;}}</style><table><tr><!--StartFragment--><td class=xl65>{value}</td><!--EndFragment--></tr></table></html>").into_bytes()),
        ("registered:Rich Text Format", FormatKind::Rtf, br"{\rtf1\ansi{\colortbl;\red255\green0\blue0;}\cf1 123456}".to_vec()),
        ("CF_DIB", FormatKind::Image, "单元格位图".as_bytes().to_vec()),
        ("CF_DIBV5", FormatKind::Image, "单元格位图 V5".as_bytes().to_vec()),
    ] {
        captured.formats.push(RawFormat::captured("windows", key, key, kind, bytes, captured.formats.len() as u32, true));
    }
    captured.formats.push(RawFormat::unsupported(
        "windows",
        "CF_BITMAP",
        "CF_BITMAP",
        FormatKind::Image,
        captured.formats.len() as u32,
        "native_handle_unsupported",
        "平台位图句柄不支持保存",
    ));
    captured
}

#[test]
fn excel_single_cell_merges_with_notepad_and_qq_while_preserving_red_text() {
    let fixture = Fixture::new(65536);
    let mut plain = windows_text("123456");
    plain.source_application.name = Some("记事本".to_owned());
    let id = fixture.store(&plain);
    plain.source_application.name = Some("QQ".to_owned());
    assert_eq!(fixture.store(&plain), id);
    let excel = excel_cell("123456");
    assert_eq!(fixture.store(&excel), id);
    assert_eq!(
        fixture.bytes(id, "registered:HTML Format"),
        excel.formats[2].bytes
    );
    assert_eq!(
        fixture.bytes(id, "registered:XML Spreadsheet"),
        excel.formats[1].bytes
    );
    assert_eq!(
        fixture.bytes(id, "CF_DIB"),
        Some("单元格位图".as_bytes().to_vec())
    );
    assert_eq!(fixture.store(&excel), id);
    assert_eq!(fixture.store(&plain), id);
    assert_eq!(fixture.store(&excel), id);
}

#[test]
fn excel_single_cell_can_be_the_first_capture_without_losing_real_whitespace() {
    let fixture = Fixture::new(65536);
    let id = fixture.store(&excel_cell("123456"));
    assert_eq!(fixture.store(&windows_text("123456")), id);
    assert_eq!(fixture.store(&excel_cell("123456")), id);
    assert_ne!(fixture.store(&windows_text("123456\r\n")), id);
    assert_ne!(fixture.store(&windows_text(" 123456")), id);
    assert_ne!(fixture.store(&excel_cell("123456 ")), id);
}

#[test]
fn excel_formats_survive_each_plain_copy_and_remain_restorable() {
    for threshold in [4, 65536] {
        let fixture = Fixture::new(threshold);
        let mut excel = excel_cell("123456");
        excel.formats.push(RawFormat::captured(
            "windows",
            "registered:Biff12",
            "Biff12",
            FormatKind::Custom,
            b"excel-native-cell".to_vec(),
            excel.formats.len() as u32,
            true,
        ));
        let id = fixture.store(&excel);
        let mut qq = windows_text("123456");
        qq.source_application.name = Some("QQ".to_owned());
        for copied in [&qq, &qq, &excel, &qq] {
            assert_eq!(fixture.store(copied), id);
            for format in excel.formats.iter().skip(1) {
                assert_eq!(
                    fixture.bytes(id, &format.format_key),
                    format.bytes,
                    "复制纯文本后丢失格式：{}",
                    format.format_key
                );
            }
            let connection = fixture.database.connect().expect("无法连接数据库");
            let (items, formats, actual_formats, restorable, total_bytes): (i64, i64, i64, i64, i64) = connection.query_row(
                "SELECT (SELECT COUNT(*) FROM clipboard_items), i.format_count,
                    (SELECT COUNT(*) FROM clipboard_formats f WHERE f.item_id = i.id),
                    (SELECT COUNT(*) FROM clipboard_formats f WHERE f.item_id = i.id AND f.restore_state = 'restorable'), i.total_bytes
                 FROM clipboard_items i JOIN clipboard_history h ON h.item_id = i.id WHERE h.id = ?1",
                [id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            ).expect("无法检查合并格式统计");
            let expected_bytes: usize = copied.formats[0]
                .bytes
                .as_ref()
                .expect("缺少文本字节")
                .len()
                + excel
                    .formats
                    .iter()
                    .skip(1)
                    .filter_map(|format| format.bytes.as_ref())
                    .map(Vec::len)
                    .sum::<usize>();
            assert_eq!(items, 1);
            assert_eq!(formats, excel.formats.len() as i64);
            assert_eq!(actual_formats, formats);
            assert_eq!(restorable, formats - 1);
            assert_eq!(total_bytes, expected_bytes as i64);
        }
    }
}

#[test]
fn excel_multi_cell_ranges_and_embedded_images_are_not_treated_as_single_text() {
    let fixture = Fixture::new(65536);
    let id = fixture.store(&windows_text("123456"));
    for (from, to) in [
        ("ExpandedColumnCount=\"1\"", "ExpandedColumnCount=\"2\""),
        ("ExpandedRowCount=\"1\"", "ExpandedRowCount=\"2\""),
        ("<Cell>", "<Cell ss:MergeAcross=\"1\">"),
        (
            "</Cell>",
            "</Cell><Cell><Data ss:Type=\"String\"></Data></Cell>",
        ),
        (
            "</Row>",
            "</Row><Row><Cell><Data ss:Type=\"String\"></Data></Cell></Row>",
        ),
    ] {
        let mut captured = excel_cell("123456");
        let xml = String::from_utf8(captured.formats[1].bytes.clone().expect("缺少表格 XML"))
            .expect("XML 编码无效");
        captured.formats[1].bytes = Some(xml.replace(from, to).into_bytes());
        assert_ne!(fixture.store(&captured), id, "不应合并多单元格内容：{to}");
    }
    let mut image = excel_cell("123456");
    image.formats[2].bytes =
        Some(b"<table><tr><td>123456<img src='image.png'></td></tr></table>".to_vec());
    assert_ne!(fixture.store(&image), id);
    let mut unknown = excel_cell("123456");
    unknown.formats[1].bytes = Some("损坏的 XML".as_bytes().to_vec());
    assert_ne!(fixture.store(&unknown), id);
    let mut opaque = excel_cell("123456");
    opaque
        .formats
        .retain(|format| format.format_key != "registered:XML Spreadsheet");
    assert_ne!(fixture.store(&opaque), id);
}
