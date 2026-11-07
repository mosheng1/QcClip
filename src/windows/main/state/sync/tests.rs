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
fn visibility_anchor_preserves_screen_position_on_both_axes() {
    let original = [1, 2, 3, 4, 5];
    let offsets = [0.0, 40.0, 80.0, 120.0, 160.0, 200.0];
    let old_axis_offset = px(-85.0);
    let item_offset = |index: usize| offsets.get(index).copied().map(px);
    for horizontal in [false, true] {
        let old_offset = if horizontal {
            gpui::point(old_axis_offset, px(17.0))
        } else {
            gpui::point(px(17.0), old_axis_offset)
        };
        let first_visible = HashSet::from([3, 4, 5]);
        let (history_id, origin) = anchor_in_layout(
            &original,
            Some(&first_visible),
            old_axis_offset,
            px(0.0),
            item_offset,
        )
        .unwrap();
        assert_eq!(history_id, 3);
        let anchor = PendingListAnchor {
            history_id,
            screen_axis: origin + old_axis_offset,
            horizontal,
            layout_order_ids: original.to_vec(),
            axis_offset: old_axis_offset,
            leading_padding: px(0.0),
        };
        let new_offset = anchor.restored_offset(old_offset, px(0.0));
        if horizontal {
            assert_eq!(new_offset, gpui::point(px(-5.0), px(17.0)));
        } else {
            assert_eq!(new_offset, gpui::point(px(17.0), px(-5.0)));
        }

        // Another scan can remove the chosen row before the virtual list renders.
        let second_visible = HashSet::from([4, 5]);
        let (next_id, next_origin) = anchor_in_layout(
            &anchor.layout_order_ids,
            Some(&second_visible),
            anchor.axis_offset,
            anchor.leading_padding,
            item_offset,
        )
        .unwrap();
        assert_eq!(next_id, 4);
        let next_anchor = PendingListAnchor {
            history_id: next_id,
            screen_axis: next_origin + anchor.axis_offset,
            ..anchor
        };
        let next_offset = next_anchor.restored_offset(old_offset, px(0.0));
        if horizontal {
            assert_eq!(next_offset, gpui::point(px(35.0), px(17.0)));
        } else {
            assert_eq!(next_offset, gpui::point(px(17.0), px(35.0)));
        }

        // Restoring the rows above the viewport returns the original offset.
        let (restored_id, restored_origin) =
            anchor_in_layout(&original, None, old_axis_offset, px(0.0), item_offset).unwrap();
        assert_eq!(restored_id, 3);
        assert_eq!(restored_origin + old_axis_offset, px(-5.0));
        let restored_anchor = PendingListAnchor {
            history_id: restored_id,
            screen_axis: restored_origin + old_axis_offset,
            horizontal,
            layout_order_ids: original.to_vec(),
            axis_offset: old_axis_offset,
            leading_padding: px(0.0),
        };
        assert_eq!(
            restored_anchor.restored_offset(old_offset, restored_origin),
            old_offset
        );
    }
}

#[test]
fn visibility_anchor_keeps_partially_visible_card_inside_leading_padding() {
    let offsets = [0.0, 40.0, 80.0, 120.0];
    let (history_id, origin) = anchor_in_layout(&[1, 2, 3], None, px(-45.0), px(10.0), |index| {
        offsets.get(index).copied().map(px)
    })
    .unwrap();
    assert_eq!((history_id, origin), (1, px(0.0)));
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

#[cfg(any())] // 旧的局部可见性刷新路径已回滚，仅保留历史实现
fn missing_visibility_scan_reaches_both_split_panes() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_path_buf();
    let database = Database::open(root.join("qcclip.sqlite3")).unwrap();
    let blobs = RawBlobStore::new(&root).unwrap();
    gpui_platform::application().run(move |cx| {
        gpui_component::init(cx);
        crate::config_runtime::ConfigRuntime::install(root.join("config.json"), cx);
        crate::state_runtime::StateRuntime::install(root.join("state.json"), cx);
        let main = cx.new(|cx| create_main(&database, &blobs, &root, cx));
        let panes = [TabId::Clipboard, TabId::Favorites].map(|tab| {
            cx.new(|cx| {
                let mut pane = create_main(&database, &blobs, &root, cx);
                pane.active_tab = tab;
                pane.is_list_pane = true;
                pane
            })
        });
        main.update(cx, |main, cx| {
            main.split_panes = Some(panes.clone());
            main.refresh_missing_visibility(cx);
        });
        assert_eq!(main.read(cx).missing_visibility_generation, 0);
        for pane in &panes {
            assert_eq!(pane.read(cx).missing_visibility_generation, 1);
            assert!(pane.read(cx).missing_visibility_dirty);
            pane.update(cx, |pane, _| pane.missing_visibility_task = None);
        }
        main.update(cx, |main, cx| main.retry_missing_visibility(cx));
        for pane in &panes {
            assert_eq!(pane.read(cx).missing_visibility_generation, 2);
            assert!(pane.read(cx).missing_visibility_task.is_some());
        }
        cx.quit();
    });
}

#[cfg(any())] // 旧的局部可见性刷新路径已回滚，仅保留历史实现
fn missing_visibility_query_failure_retries_on_unchanged_scan() {
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
        CustomTextService::new(database.clone(), blobs.clone())
            .create_manual_text("可见项目")
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
            main
        });
        let original_order = main.read(cx).list_order_ids.clone();
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
            let failure_guard = main.read_with(cx, |main, _| {
                main.history_repository.fail_next_visibility_query()
            });
            main.update(cx, |main, cx| main.refresh_missing_visibility(cx));
            for _ in 0..100 {
                if main.read_with(cx, |main, _| main.missing_visibility_task.is_none()) {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(10))
                    .await;
            }
            main.read_with(cx, |main, _| {
                assert!(main.missing_visibility_task.is_none());
                assert!(main.missing_visibility_dirty);
            });
            drop(failure_guard);
            let failed_generation =
                main.read_with(cx, |main, _| main.missing_visibility_generation);
            cx.update(MainWindowService::request_missing_scan_now);
            for _ in 0..100 {
                if main.read_with(cx, |main, _| !main.missing_visibility_dirty) {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(10))
                    .await;
            }
            main.read_with(cx, |main, _| {
                assert!(!main.missing_visibility_dirty);
                assert!(main.missing_visibility_generation > failed_generation);
                assert_eq!(main.query_generation, 0);
                assert_eq!(main.list_order_ids, original_order);
            });
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

#[cfg(any())] // 旧的局部可见性刷新路径已回滚，仅保留历史实现
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
