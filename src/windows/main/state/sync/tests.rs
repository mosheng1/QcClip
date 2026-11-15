use super::*;
use crate::services::{database::Database, image_library::ImageLibraryService};

fn create_main(
    database: &Database,
    blobs: &RawBlobStore,
    root: &Path,
    cx: &mut Context<MainWindow>,
) -> MainWindow {
    MainWindow::new(
        HistoryRepository::new(database.clone(), blobs.clone()),
        FavoriteRepository::with_blob_store(database.clone(), blobs.clone()),
        TagRepository::new(database.clone()),
        CaptureService::new(database.clone(), blobs.clone()),
        CustomTextService::new(database.clone(), blobs.clone()),
        PasteService::new(database.clone(), blobs.clone()),
        blobs.clone(),
        ImageLibraryService::new(root.join("images")),
        None,
        ClipboardWriteSuppressor::default(),
        cx,
    )
}

#[test]
fn removed_selection_chooses_nearest_survivor() {
    let order = [1, 2, 3, 4, 5, 6];
    assert_eq!(
        nearest_survivor(&order, 3, &HashSet::from([1, 2, 5, 6])),
        Some(5)
    );
    assert_eq!(
        nearest_survivor(&order, 3, &HashSet::from([1, 2, 6])),
        Some(6)
    );
    assert_eq!(nearest_survivor(&order, 3, &HashSet::from([1, 2])), Some(2));
}

#[test]
fn first_missing_scan_only_refreshes_cached_file_cards_and_all_membership_changes() {
    let change = |id| ReceivedChange {
        item_id: id,
        history_id: Some(id),
        favorite_id: None,
        tags_changed: false,
    };
    let cached = HashSet::from([7, 901]);
    let file_list_changes = (1..=1_000).map(change).collect();
    let selected =
        select_cached_missing_changes(vec![change(500), change(901)], file_list_changes, &cached);
    let ids: HashSet<_> = selected.iter().map(|change| change.item_id).collect();
    assert_eq!(ids, HashSet::from([7, 500, 901]));
    assert_eq!(
        selected.len(),
        3,
        "membership and metadata must not duplicate a query"
    );
}

#[cfg(target_os = "windows")]
#[test]
#[ignore = "需要 GPUI 桌面会话，验证首轮文件扫描保持按需加载"]
fn first_missing_scan_does_not_load_all_file_list_history() {
    use crate::domain::capture::{CapturedClipboard, RawFormat, SourceApplication};

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_path_buf();
    let database = Database::open(root.join("qcclip.sqlite3")).unwrap();
    let blobs = RawBlobStore::new(&root).unwrap();
    std::fs::write(
        root.join("config.json"),
        r#"{"history":{"autoHideMissingFiles":true}}"#,
    )
    .unwrap();
    gpui_platform::application().run(move |cx| {
        gpui_component::init(cx);
        crate::config_runtime::ConfigRuntime::install(root.join("config.json"), cx);
        crate::state_runtime::StateRuntime::install(root.join("state.json"), cx);
        let capture = CaptureService::new(database.clone(), blobs.clone());
        for index in 0..24 {
            let file = root.join(format!("file-{index}.txt"));
            std::fs::write(&file, b"present").unwrap();
            capture
                .store_captured_clipboard(&CapturedClipboard {
                    primary_format_identity: Some(("windows".to_owned(), "CF_HDROP".to_owned())),
                    formats: vec![RawFormat::captured(
                        "windows",
                        "CF_HDROP",
                        "CF_HDROP",
                        FormatKind::FileList,
                        crate::platform::image_temp_file::hdrop_bytes(&file),
                        0,
                        true,
                    )],
                    preview_candidate: Some(format!("file-{index}.txt")),
                    source_application: SourceApplication::default(),
                })
                .unwrap();
        }
        let main = cx.new(|cx| {
            let mut main = create_main(&database, &blobs, &root, cx);
            main.active_tab = TabId::Clipboard;
            let query = HistoryQuery::default();
            let policy = crate::config_runtime::history_config(cx);
            let snapshot = main
                .history_repository
                .ordered_snapshot_scoped(&query, &policy)
                .unwrap();
            assert_eq!(snapshot.ids.len(), 24);
            let records = main
                .history_repository
                .query_by_ids_scoped(&query, &snapshot.ids[..1], &policy)
                .unwrap();
            assert!(main.accept_query_result(0, Ok((snapshot, records))));
            assert_eq!(main.loaded_items.len(), 1);
            main
        });
        let order = main.read(cx).list_order_ids.clone();
        cx.set_global(MainWindowService::default());
        MainWindowService::set_main_view(main.clone(), cx);
        cx.spawn(async move |cx| {
            for _ in 0..100 {
                if !cx.update(|cx| cx.global::<MainWindowService>().missing_scan_pending) {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(10))
                    .await;
            }
            assert!(!cx.update(|cx| cx.global::<MainWindowService>().missing_scan_pending));
            wait_for_updates(&main, cx).await;
            main.read_with(cx, |main, _| {
                assert_eq!(main.list_order_ids, order);
                assert_eq!(main.loaded_items.len(), 1);
                assert_eq!(main.items.len(), 1);
                assert!(main.has_more_items);
                assert_eq!(main.query_generation, 0);
            });
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
}

#[cfg(target_os = "windows")]
#[test]
#[ignore = "需要 GPUI 桌面会话，验证图片文件恢复时清除失败缓存"]
fn visible_image_hdrop_with_text_fallback_retries_after_file_recovery() {
    use crate::domain::capture::{CapturedClipboard, RawFormat, SourceApplication};

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_path_buf();
    let file = root.join("picture.png");
    std::fs::write(&file, b"image bytes").unwrap();
    let database = Database::open(root.join("qcclip.sqlite3")).unwrap();
    let blobs = RawBlobStore::new(&root).unwrap();
    std::fs::write(
        root.join("config.json"),
        r#"{"history":{"autoHideMissingFiles":true},"clipboard":{"displayPriority":"image-text-html"}}"#,
    )
    .unwrap();
    gpui_platform::application().run(move |cx| {
        gpui_component::init(cx);
        crate::config_runtime::ConfigRuntime::install(root.join("config.json"), cx);
        crate::state_runtime::StateRuntime::install(root.join("state.json"), cx);
        let captured = CapturedClipboard {
            primary_format_identity: Some(("windows".to_owned(), "CF_HDROP".to_owned())),
            formats: vec![
                RawFormat::captured(
                    "windows",
                    "CF_HDROP",
                    "CF_HDROP",
                    FormatKind::FileList,
                    crate::platform::image_temp_file::hdrop_bytes(&file),
                    0,
                    true,
                ),
                RawFormat::captured(
                    "windows",
                    "CF_UNICODETEXT",
                    "CF_UNICODETEXT",
                    FormatKind::Text,
                    "fallback"
                        .encode_utf16()
                        .flat_map(u16::to_le_bytes)
                        .collect(),
                    1,
                    true,
                ),
            ],
            preview_candidate: Some("picture.png".to_owned()),
            source_application: SourceApplication::default(),
        };
        let history_id = CaptureService::new(database.clone(), blobs.clone())
            .store_captured_clipboard(&captured)
            .unwrap();
        let main = cx.new(|cx| {
            let mut main = create_main(&database, &blobs, &root, cx);
            main.active_tab = TabId::Clipboard;
            let query = HistoryQuery::default();
            let policy = crate::config_runtime::history_config(cx);
            let snapshot = main
                .history_repository
                .ordered_snapshot_scoped(&query, &policy)
                .unwrap();
            let records = main
                .history_repository
                .query_by_ids_scoped(&query, &[history_id], &policy)
                .unwrap();
            assert!(main.accept_query_result(0, Ok((snapshot, records))));
            assert_eq!(main.list_order_ids, vec![history_id]);
            main
        });
        let item_id = main.read(cx).loaded_items[&history_id].item_id;
        let key = ImagePreviewKey::new(item_id, "windows", "CF_HDROP");
        cx.set_global(MainWindowService::default());
        MainWindowService::set_main_view(main.clone(), cx);
        cx.spawn(async move |cx| {
            for missing in [true, false] {
                if missing {
                    std::fs::remove_file(&file).unwrap();
                } else {
                    std::fs::write(&file, b"restored image bytes").unwrap();
                }
                main.update(cx, |main, _| {
                    main.image_load_failures.insert(key.clone());
                    main.image_loading.insert(key.clone(), 1);
                });
                cx.update(MainWindowService::request_missing_scan_now);
                for _ in 0..100 {
                    if !cx.update(|cx| cx.global::<MainWindowService>().missing_scan_pending) {
                        break;
                    }
                    cx.background_executor()
                        .timer(Duration::from_millis(10))
                        .await;
                }
                assert!(!cx.update(|cx| cx.global::<MainWindowService>().missing_scan_pending));
                wait_for_updates(&main, cx).await;
                main.read_with(cx, |main, _| {
                    assert_eq!(main.list_order_ids, vec![history_id]);
                    assert!(!main.image_load_failures.contains(&key));
                    assert!(!main.image_loading.contains_key(&key));
                });
            }
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
}

#[test]
#[ignore = "需要 GPUI 桌面会话，验证增量查询失败后自动重试"]
fn missing_visibility_query_failure_retries_without_new_scan() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_path_buf();
    let database = Database::open(root.join("qcclip.sqlite3")).unwrap();
    let blobs = RawBlobStore::new(&root).unwrap();
    gpui_platform::application().run(move |cx| {
        gpui_component::init(cx);
        crate::config_runtime::ConfigRuntime::install(root.join("config.json"), cx);
        crate::state_runtime::StateRuntime::install(root.join("state.json"), cx);
        CustomTextService::new(database.clone(), blobs.clone())
            .create_manual_text("可见项目")
            .unwrap();
        let main = cx.new(|cx| {
            let mut main = create_main(&database, &blobs, &root, cx);
            let query = HistoryQuery::default();
            let policy = crate::config_runtime::history_config(cx);
            let snapshot = main
                .history_repository
                .ordered_snapshot_scoped(&query, &policy)
                .unwrap();
            let records = main
                .history_repository
                .query_by_ids_scoped(&query, &snapshot.ids, &policy)
                .unwrap();
            assert!(main.accept_query_result(0, Ok((snapshot, records))));
            main
        });
        let (item_id, history_id) = main.read_with(cx, |main, _| {
            let item = &main.items[0];
            (item.item_id, item.history_id)
        });
        let failure_guard = main.read_with(cx, |main, _| {
            main.history_repository.fail_next_visibility_query()
        });
        main.update(cx, |main, cx| {
            main.receive_missing_changes(
                vec![ReceivedChange {
                    item_id,
                    history_id: Some(history_id),
                    favorite_id: None,
                    tags_changed: false,
                }],
                Vec::new(),
                cx,
            );
            assert!(main.sync_refresh_task.is_some());
        });
        cx.spawn(async move |cx| {
            for _ in 0..100 {
                if main.read_with(cx, |main, _| main.sync_refresh_task.is_none()) {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(20))
                    .await;
            }
            main.read_with(cx, |main, _| {
                assert!(main.sync_refresh_task.is_none());
                assert!(main.sync_changes.is_empty());
                assert_eq!(main.list_order_ids, vec![history_id]);
                assert_eq!(main.query_generation, 0);
            });
            drop(failure_guard);
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
}

#[test]
#[ignore = "需要 GPUI 桌面会话，验证重复失效文件扫描不重置已有卡片"]
fn missing_scan_preserves_unchanged_browsing() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_path_buf();
    let database = Database::open(root.join("qcclip.sqlite3")).unwrap();
    let blobs = RawBlobStore::new(&root).unwrap();
    std::fs::write(
        root.join("config.json"),
        r#"{"history":{"autoHideMissingFiles":true}}"#,
    )
    .unwrap();
    gpui_platform::application().run(move |cx| {
        gpui_component::init(cx);
        crate::config_runtime::ConfigRuntime::install(root.join("config.json"), cx);
        crate::state_runtime::StateRuntime::install(root.join("state.json"), cx);
        assert!(crate::config_runtime::history_config(cx).auto_hide_missing_files);
        let custom = CustomTextService::new(database.clone(), blobs.clone());
        for text in ["原有内容一", "原有内容二", "原有内容三"] {
            custom.create_manual_text(text).unwrap();
        }
        let main = cx.new(|cx| {
            let mut main = create_main(&database, &blobs, &root, cx);
            main.active_tab = TabId::Clipboard;
            let query = HistoryQuery::default();
            let policy = crate::config_runtime::history_config(cx);
            let snapshot = main
                .history_repository
                .ordered_snapshot_scoped(&query, &policy)
                .unwrap();
            let records = main
                .history_repository
                .query_by_ids_scoped(&query, &snapshot.ids, &policy)
                .unwrap();
            assert!(main.accept_query_result(0, Ok((snapshot, records.clone()))));
            main.replace_items(records.into_iter().map(record_to_list_item).collect());
            assert_eq!(main.list_order_ids.len(), 3);
            main.selected_item_id = Some(main.list_order_ids[1]);
            main.multi_selected_item_ids.push(main.list_order_ids[1]);
            main.multi_selection_anchor = main.selected_item_id;
            main.list_scroll_handle
                .as_ref()
                .set_offset(gpui::point(px(0.0), px(-120.0)));
            main.adaptive_item_heights
                .insert(main.list_order_ids[1], px(65.0));
            main
        });
        let initial = main.read(cx).list_order_ids.clone();
        cx.set_global(MainWindowService::default());
        MainWindowService::set_main_view(main.clone(), cx);
        cx.spawn(async move |cx| {
            for scan_index in 0..2 {
                for _ in 0..100 {
                    if !cx.update(|cx| cx.global::<MainWindowService>().missing_scan_pending) {
                        break;
                    }
                    cx.background_executor()
                        .timer(Duration::from_millis(10))
                        .await;
                }
                assert!(!cx.update(|cx| cx.global::<MainWindowService>().missing_scan_pending));
                main.read_with(cx, |main, _| {
                    assert_eq!(main.query_generation, 0, "未变化的扫描不应重置列表");
                    assert_eq!(main.list_order_ids, initial);
                    assert_eq!(main.loaded_items.len(), 3);
                    assert_eq!(main.selected_item_id, Some(initial[1]));
                    assert_eq!(main.multi_selected_item_ids, vec![initial[1]]);
                    assert_eq!(
                        main.list_scroll_handle.as_ref().offset(),
                        gpui::point(px(0.0), px(-120.0))
                    );
                    assert_eq!(main.adaptive_item_heights.get(&initial[1]), Some(&px(65.0)));
                });
                if scan_index == 0 {
                    cx.update(MainWindowService::request_missing_scan_now);
                }
            }
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
}

#[cfg(target_os = "windows")]
#[test]
#[ignore = "需要 GPUI 桌面会话，验证失效条目恢复后移到首行"]
fn missing_recovery_moves_row_to_top() {
    use crate::domain::capture::{CapturedClipboard, RawFormat, SourceApplication};

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_path_buf();
    let missing = root.join("missing-recovered.txt");
    let present = root.join("present-recovered.txt");
    std::fs::write(&present, b"present").unwrap();
    let database = Database::open(root.join("qcclip.sqlite3")).unwrap();
    let blobs = RawBlobStore::new(&root).unwrap();
    std::fs::write(
        root.join("config.json"),
        r#"{"history":{"autoHideMissingFiles":true,"moveRecoveredMissingFilesToTop":true}}"#,
    )
    .unwrap();
    gpui_platform::application().run(move |cx| {
        gpui_component::init(cx);
        crate::config_runtime::ConfigRuntime::install(root.join("config.json"), cx);
        crate::state_runtime::StateRuntime::install(root.join("state.json"), cx);
        let capture = CaptureService::new(database.clone(), blobs.clone());
        let file_capture = |path: &Path, preview: &str| CapturedClipboard {
            primary_format_identity: Some(("windows".to_owned(), "CF_HDROP".to_owned())),
            formats: vec![RawFormat::captured(
                "windows",
                "CF_HDROP",
                "CF_HDROP",
                FormatKind::FileList,
                crate::platform::image_temp_file::hdrop_bytes(path),
                0,
                true,
            )],
            preview_candidate: Some(preview.to_owned()),
            source_application: SourceApplication::default(),
        };
        let missing_id = capture
            .store_captured_clipboard(&file_capture(&missing, "missing-recovered.txt"))
            .unwrap();
        let present_id = capture
            .store_captured_clipboard(&file_capture(&present, "present-recovered.txt"))
            .unwrap();
        let main = cx.new(|cx| {
            let mut main = create_main(&database, &blobs, &root, cx);
            main.active_tab = TabId::Clipboard;
            main.history_repository.publish_missing_files_with_recovery(
                main.history_repository.scan_missing_files().unwrap(),
                true,
            );
            let query = HistoryQuery::default();
            let policy = crate::config_runtime::history_config(cx);
            let snapshot = main
                .history_repository
                .ordered_snapshot_scoped(&query, &policy)
                .unwrap();
            let records = main
                .history_repository
                .query_by_ids_scoped(&query, &snapshot.ids, &policy)
                .unwrap();
            assert!(main.accept_query_result(0, Ok((snapshot, records.clone()))));
            main.replace_items(records.into_iter().map(record_to_list_item).collect());
            main
        });
        main.read_with(cx, |main, _| {
            assert_eq!(main.list_order_ids, vec![present_id]);
        });
        cx.set_global(MainWindowService::default());
        MainWindowService::set_main_view(main.clone(), cx);
        cx.spawn(async move |cx| {
            std::fs::write(&missing, b"restored").unwrap();
            cx.update(MainWindowService::request_missing_scan_now);
            for _ in 0..100 {
                if !cx.update(|cx| cx.global::<MainWindowService>().missing_scan_pending) {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(10))
                    .await;
            }
            assert!(!cx.update(|cx| cx.global::<MainWindowService>().missing_scan_pending));
            for _ in 0..100 {
                if main.read_with(cx, |main, _| {
                    main.list_order_ids == vec![missing_id, present_id]
                }) {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(10))
                    .await;
            }
            main.read_with(cx, |main, _| {
                assert_eq!(main.list_order_ids, vec![missing_id, present_id]);
                assert_eq!(
                    main.items.first().map(|item| item.history_id),
                    Some(missing_id)
                );
                assert_eq!(
                    main.items.first().map(|item| item.shortcut.as_str()),
                    Some("Ctrl+1")
                );
            });
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
}

#[cfg(target_os = "windows")]
#[test]
#[ignore = "需要 GPUI 桌面会话，验证失效项增量隐藏及恢复保留浏览状态"]
fn missing_scan_reconciles_membership_without_resetting_browsing() {
    use crate::domain::capture::{CapturedClipboard, RawFormat, SourceApplication};

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_path_buf();
    let file = root.join("temporary.txt");
    std::fs::write(&file, b"present").unwrap();
    let database = Database::open(root.join("qcclip.sqlite3")).unwrap();
    let blobs = RawBlobStore::new(&root).unwrap();
    std::fs::write(
        root.join("config.json"),
        r#"{"history":{"autoHideMissingFiles":true}}"#,
    )
    .unwrap();
    gpui_platform::application().run(move |cx| {
        gpui_component::init(cx);
        crate::config_runtime::ConfigRuntime::install(root.join("config.json"), cx);
        crate::state_runtime::StateRuntime::install(root.join("state.json"), cx);
        let custom = CustomTextService::new(database.clone(), blobs.clone());
        for text in ["保留一", "保留二", "保留三"] {
            custom.create_manual_text(text).unwrap();
        }
        let captured = CapturedClipboard {
            primary_format_identity: Some(("windows".to_owned(), "CF_HDROP".to_owned())),
            formats: vec![RawFormat::captured(
                "windows",
                "CF_HDROP",
                "CF_HDROP",
                FormatKind::FileList,
                crate::platform::image_temp_file::hdrop_bytes(&file),
                0,
                true,
            )],
            preview_candidate: Some("temporary.txt".to_owned()),
            source_application: SourceApplication::default(),
        };
        let hidden_id = CaptureService::new(database.clone(), blobs.clone())
            .store_captured_clipboard(&captured)
            .unwrap();
        let main = cx.new(|cx| {
            let mut main = create_main(&database, &blobs, &root, cx);
            main.active_tab = TabId::Clipboard;
            let query = HistoryQuery::default();
            let policy = crate::config_runtime::history_config(cx);
            let snapshot = main
                .history_repository
                .ordered_snapshot_scoped(&query, &policy)
                .unwrap();
            let records = main
                .history_repository
                .query_by_ids_scoped(&query, &snapshot.ids, &policy)
                .unwrap();
            assert!(main.accept_query_result(0, Ok((snapshot, records.clone()))));
            main.replace_items(records.into_iter().map(record_to_list_item).collect());
            assert_eq!(main.list_order_ids.len(), 4);
            let selected = *main
                .list_order_ids
                .iter()
                .find(|id| **id != hidden_id)
                .unwrap();
            main.selected_item_id = Some(selected);
            main.multi_selected_item_ids.push(selected);
            main.multi_selection_anchor = Some(selected);
            main.list_scroll_handle
                .as_ref()
                .set_offset(gpui::point(px(0.0), px(-120.0)));
            main.adaptive_item_heights.insert(selected, px(65.0));
            main
        });
        let retained = main.read(cx).selected_item_id.unwrap();
        cx.set_global(MainWindowService::default());
        MainWindowService::set_main_view(main.clone(), cx);
        cx.spawn(async move |cx| {
            for should_hide in [false, true, false] {
                if should_hide {
                    std::fs::remove_file(&file).unwrap();
                    cx.update(MainWindowService::request_missing_scan_now);
                } else if !file.exists() {
                    std::fs::write(&file, b"restored").unwrap();
                    cx.update(MainWindowService::request_missing_scan_now);
                }
                for _ in 0..100 {
                    if !cx.update(|cx| cx.global::<MainWindowService>().missing_scan_pending) {
                        break;
                    }
                    cx.background_executor()
                        .timer(Duration::from_millis(10))
                        .await;
                }
                assert!(!cx.update(|cx| cx.global::<MainWindowService>().missing_scan_pending));
                for _ in 0..100 {
                    if main.read_with(cx, |main, _| {
                        main.list_order_ids.contains(&hidden_id) != should_hide
                    }) {
                        break;
                    }
                    cx.background_executor()
                        .timer(Duration::from_millis(10))
                        .await;
                }
                main.read_with(cx, |main, _| {
                    assert_eq!(main.query_generation, 0);
                    assert_eq!(main.list_order_ids.contains(&hidden_id), !should_hide);
                    assert_eq!(main.selected_item_id, Some(retained));
                    assert_eq!(main.multi_selected_item_ids, vec![retained]);
                    assert_eq!(main.multi_selection_anchor, Some(retained));
                    assert_eq!(
                        main.list_scroll_handle.as_ref().offset(),
                        gpui::point(px(0.0), px(-120.0))
                    );
                    assert_eq!(main.adaptive_item_heights.get(&retained), Some(&px(65.0)));
                    assert!(main.loaded_items.contains_key(&retained));
                });
            }
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
}

#[test]
#[ignore = "需要 GPUI 桌面会话，验证同步局部更新保留列表交互状态"]
fn remote_updates_preserve_browsing_and_only_update_related_rows() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_path_buf();
    let database = Database::open(root.join("qcclip.sqlite3")).unwrap();
    let blobs = RawBlobStore::new(&root).unwrap();
    gpui_platform::application().run(move |cx| {
        gpui_component::init(cx);
        crate::config_runtime::ConfigRuntime::install(root.join("config.json"), cx);
        crate::state_runtime::StateRuntime::install(root.join("state.json"), cx);
        let custom = CustomTextService::new(database.clone(), blobs.clone());
        for text in ["原有内容一", "原有内容二", "原有内容三"] {
            custom.create_manual_text(text).unwrap();
        }
        let main = cx.new(|cx| {
            let mut main = create_main(&database, &blobs, &root, cx);
            main.active_tab = TabId::Clipboard;
            let query = HistoryQuery::default();
            let policy = crate::config::HistoryConfig::default();
            let snapshot = main
                .history_repository
                .ordered_snapshot_scoped(&query, &policy)
                .unwrap();
            let records = main
                .history_repository
                .query_by_ids_scoped(&query, &snapshot.ids, &policy)
                .unwrap();
            assert!(main.accept_query_result(0, Ok((snapshot, records.clone()))));
            main.replace_items(records.into_iter().map(record_to_list_item).collect());
            main.selected_item_id = Some(main.list_order_ids[1]);
            main.multi_selected_item_ids.push(main.list_order_ids[1]);
            main.multi_selection_anchor = main.selected_item_id;
            main.list_scroll_handle
                .as_ref()
                .set_offset(gpui::point(px(0.0), px(-120.0)));
            main.adaptive_item_heights
                .insert(main.list_order_ids[1], px(65.0));
            main
        });
        cx.spawn(async move |cx| {
            let initial = main.read_with(cx, |main, _| {
                (
                    main.list_order_ids.clone(),
                    main.selected_item_id,
                    main.multi_selected_item_ids.clone(),
                    main.list_scroll_handle.as_ref().offset(),
                )
            });
            let history_id = custom
                .create_manual_text_and_favorite("远端收藏专用内容")
                .unwrap();
            let item_id: i64 = database
                .connect()
                .unwrap()
                .query_row(
                    "SELECT item_id FROM clipboard_history WHERE id = ?1",
                    [history_id],
                    |row| row.get(0),
                )
                .unwrap();
            let favorite_id = FavoriteRepository::new(database.clone())
                .favorite_id_for_item(item_id)
                .unwrap()
                .unwrap();
            // 模拟仅在收藏表接收，不把测试用创建器附带的历史关系留在数据库。
            database
                .connect()
                .unwrap()
                .execute("DELETE FROM clipboard_history WHERE id = ?1", [history_id])
                .unwrap();
            main.update(cx, |main, cx| {
                main.receive_sync_changes(
                    vec![ReceivedChange {
                        item_id,
                        history_id: None,
                        favorite_id: Some(favorite_id),
                        tags_changed: false,
                    }],
                    cx,
                )
            });
            wait_for_updates(&main, cx).await;
            main.update(cx, |main, _| {
                assert_eq!(main.list_order_ids, initial.0);
                assert_eq!(main.selected_item_id, initial.1);
                assert_eq!(main.query_generation, 0);
            });

            let new_history = custom.create_manual_text("远端新增历史").unwrap();
            let new_item = database
                .connect()
                .unwrap()
                .query_row(
                    "SELECT item_id FROM clipboard_history WHERE id = ?1",
                    [new_history],
                    |row| row.get(0),
                )
                .unwrap();
            main.update(cx, |main, cx| {
                main.receive_sync_changes(
                    vec![ReceivedChange {
                        item_id: new_item,
                        history_id: Some(new_history),
                        favorite_id: None,
                        tags_changed: false,
                    }],
                    cx,
                )
            });
            wait_for_updates(&main, cx).await;
            main.update(cx, |main, _| {
                assert_eq!(main.list_order_ids[0], new_history);
                assert_eq!(main.list_order_ids.len(), 4);
                assert_eq!(main.loaded_items.len(), 4);
                assert_eq!(main.selected_item_id, initial.1);
                assert_eq!(main.multi_selected_item_ids, initial.2);
                assert_eq!(main.list_scroll_handle.as_ref().offset(), initial.3);
                assert_eq!(
                    main.adaptive_item_heights.get(&initial.1.unwrap()),
                    Some(&px(65.0))
                );
                assert_eq!(main.query_generation, 0);
            });

            // 收藏页收到只属于历史的条目时，也不更改已展示列表。
            main.update(cx, |main, _| {
                main.active_tab = TabId::Favorites;
                let query = HistoryQuery::default();
                let snapshot = main.favorite_repository.ordered_snapshot(&query).unwrap();
                let records = main
                    .favorite_repository
                    .query_by_ids(&query, &snapshot.ids)
                    .unwrap();
                main.accept_query_result(0, Ok((snapshot, records)));
                main.selected_item_id = Some(favorite_id);
            });
            main.update(cx, |main, cx| {
                main.receive_sync_changes(
                    vec![ReceivedChange {
                        item_id: new_item,
                        history_id: Some(new_history),
                        favorite_id: None,
                        tags_changed: false,
                    }],
                    cx,
                )
            });
            wait_for_updates(&main, cx).await;
            main.update(cx, |main, _| {
                assert_eq!(main.list_order_ids, vec![favorite_id]);
                assert_eq!(main.selected_item_id, Some(favorite_id));
                assert_eq!(main.query_generation, 0);
            });

            let tags = TagRepository::new(database.clone());
            let tag = tags.create("远端标签", "#123456").unwrap();
            tags.add_to_item(item_id, tag.id).unwrap();
            crate::services::database::NoteRepository::new(database.clone())
                .save(item_id, "远端补入的备注")
                .unwrap();
            main.update(cx, |main, cx| {
                main.list_drag_source_id = Some(favorite_id);
                main.receive_sync_changes(
                    vec![ReceivedChange {
                        item_id,
                        history_id: None,
                        favorite_id: Some(favorite_id),
                        tags_changed: true,
                    }],
                    cx,
                );
            });
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            main.update(cx, |main, _| {
                assert!(main.sync_refresh_task.is_some());
                assert_eq!(main.list_drag_source_id, Some(favorite_id));
                assert!(main.tags.is_empty());
                main.list_drag_source_id = None;
            });
            wait_for_updates(&main, cx).await;
            main.update(cx, |main, _| {
                assert_eq!(main.list_order_ids, vec![favorite_id]);
                assert_eq!(main.selected_item_id, Some(favorite_id));
                assert_eq!(main.loaded_items[&favorite_id].note_text, "远端补入的备注");
                assert_eq!(main.loaded_items[&favorite_id].tag_ids, vec![tag.id]);
                assert!(main.tags.iter().any(|value| value.id == tag.id));
                assert_eq!(main.query_generation, 0);
            });
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
}

async fn wait_for_updates(main: &Entity<MainWindow>, cx: &mut gpui::AsyncApp) {
    for _ in 0..100 {
        if main.read_with(cx, |main, _| main.sync_refresh_task.is_none()) {
            return;
        }
        cx.background_executor()
            .timer(Duration::from_millis(50))
            .await;
    }
    panic!("同步列表局部更新超时");
}
