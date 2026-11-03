use super::*;
use crate::{
    config::ClipboardWindowPosition,
    services::{database::Database, image_library::ImageLibraryService},
};

struct Fixture {
    directory: tempfile::TempDir,
    main: Entity<MainWindow>,
    history: HistoryRepository,
    favorites: FavoriteRepository,
    tags: TagRepository,
}

impl Fixture {
    fn new(cx: &mut gpui::TestAppContext) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let database = Database::open(root.join("test.sqlite3")).unwrap();
        let blobs = RawBlobStore::new(root).unwrap();
        let custom = CustomTextService::new(database.clone(), blobs.clone());
        custom.create_manual_text_and_favorite("目标收藏").unwrap();
        custom
            .create_manual_text_and_favorite("另一条收藏")
            .unwrap();
        custom.create_manual_text("普通内容").unwrap();
        let history = HistoryRepository::new(database.clone(), blobs.clone());
        let favorites = FavoriteRepository::with_blob_store(database.clone(), blobs.clone());
        let tags = TagRepository::new(database.clone());
        let mut config = crate::config::AppConfig::default();
        config.clipboard.list_layout = ClipboardListLayout::SideBySide;
        config.clipboard.clear_tags_on_tab_switch = true;
        std::fs::write(
            root.join("config.json"),
            serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::i18n::install(Locale::zh_cn(), cx);
            crate::config_runtime::ConfigRuntime::install(root.join("config.json"), cx);
            state_runtime::StateRuntime::install(root.join("state.json"), cx);
            PreviewWindowService::install(ImagePreviewService::new(blobs.clone()), cx);
            cx.set_global(MainWindowService::default());
        });
        let main = cx.new(|cx| {
            MainWindow::new(
                history.clone(),
                favorites.clone(),
                tags.clone(),
                CaptureService::new(database.clone(), blobs.clone()),
                custom.clone(),
                PasteService::new(database, blobs.clone()),
                blobs,
                ImageLibraryService::new(root.join("images")),
                None,
                ClipboardWriteSuppressor::default(),
                cx,
            )
        });
        main.update(cx, |main, cx| main.sync_split_layout(cx));
        cx.run_until_parked();
        Self {
            directory,
            main,
            history,
            favorites,
            tags,
        }
    }

    fn panes(&self, cx: &gpui::TestAppContext) -> [Entity<MainWindow>; 2] {
        self.main
            .read_with(cx, |main, _| main.split_panes.clone().unwrap())
    }

    fn change_layout(
        &self,
        layout: ClipboardListLayout,
        position: ClipboardWindowPosition,
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            let mut config = crate::config::AppConfig {
                clipboard: crate::config_runtime::clipboard_config(cx),
                ..Default::default()
            };
            config.clipboard.list_layout = layout;
            config.clipboard.window_position = position;
            let path = self.directory.path().join("config.json");
            std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
            crate::config_runtime::ConfigRuntime::reload(path, cx);
        });
        self.main.update(cx, |main, cx| main.sync_split_layout(cx));
        cx.run_until_parked();
    }
}

#[gpui::test]
fn switching_active_pane_preserves_both_selections_scroll_and_shared_tags(
    cx: &mut gpui::TestAppContext,
) {
    let fixture = Fixture::new(cx);
    let panes = fixture.panes(cx);
    let generations = panes
        .each_ref()
        .map(|pane| pane.read_with(cx, |pane, _| pane.query_generation));
    let selected = panes.each_ref().map(|pane| {
        pane.update(cx, |pane, _| {
            pane.selected_item_id = pane.list_order_ids.first().copied();
            pane.list_scroll_handle
                .as_ref()
                .set_offset(gpui::point(px(0.0), px(-90.0)));
            pane.selected_item_id
        })
    });
    fixture.main.update(cx, |main, cx| {
        main.selected_tag_ids.insert(123);
        main.move_tab(1, cx);
        assert_eq!(main.active_tab, TabId::Favorites);
        assert_eq!(main.active_list_entity(cx), Some(panes[1].clone()));
        main.select_tab(TabId::Clipboard, cx);
        assert!(main.selected_tag_ids.contains(&123));
        main.select_tab(TabId::Emoji, cx);
        main.select_tab(TabId::Favorites, cx);
    });
    for index in 0..2 {
        panes[index].read_with(cx, |pane, _| {
            assert_eq!(pane.query_generation, generations[index]);
            assert_eq!(pane.selected_item_id, selected[index]);
            assert_eq!(pane.list_scroll_handle.offset().y, px(-90.0));
            assert_eq!(pane.pane_active, index == 1);
        });
    }
    panes[0].update(cx, |pane, _| {
        pane.list_scroll_handle
            .as_ref()
            .set_offset(gpui::point(px(0.0), px(-150.0)))
    });
    assert_eq!(
        panes[1].read_with(cx, |pane, _| pane.list_scroll_handle.offset().y),
        px(-90.0)
    );
}

#[gpui::test]
fn search_content_filters_and_tag_filters_apply_to_both_panes(cx: &mut gpui::TestAppContext) {
    let fixture = Fixture::new(cx);
    let tag = fixture.tags.create("筛选标签", "#FF0000").unwrap();
    let query = HistoryQuery::default();
    let record = fixture
        .history
        .query(&query)
        .unwrap()
        .into_iter()
        .find(|record| record.preview_text.contains("目标"))
        .unwrap();
    fixture.tags.add_to_item(record.item_id, tag.id).unwrap();
    fixture.main.update(cx, |main, cx| {
        main.search_query = "目标".to_owned();
        main.filters.toggle(FilterId::Text);
        main.selected_tag_ids.insert(tag.id);
        main.tags_match_all = true;
        main.refresh_history(cx);
    });
    cx.run_until_parked();
    let panes = fixture.panes(cx);
    for pane in &panes {
        pane.read_with(cx, |pane, _| {
            assert_eq!(pane.list_status, ListStatus::Ready);
            assert_eq!(pane.total_item_count, 1);
            assert_eq!(pane.history_query().tag_ids, vec![tag.id]);
            assert!(pane.history_query().tag_match_all);
            assert_eq!(pane.items[0].item_id, record.item_id);
        });
    }
    fixture.main.update(cx, |main, cx| {
        main.filters.toggle(FilterId::Image);
        main.refresh_history(cx);
    });
    cx.run_until_parked();
    let queries = panes
        .each_ref()
        .map(|pane| pane.read_with(cx, |pane, _| pane.history_query()));
    assert_eq!(queries[0], queries[1]);
}

#[gpui::test]
fn favorite_changes_update_peer_without_resetting_its_scroll(cx: &mut gpui::TestAppContext) {
    let fixture = Fixture::new(cx);
    let panes = fixture.panes(cx);
    let item_id = panes[0].read_with(cx, |pane, _| {
        pane.items
            .iter()
            .find(|item| item.favorite_id.is_none())
            .unwrap()
            .item_id
    });
    panes[1].update(cx, |pane, _| {
        pane.list_scroll_handle
            .as_ref()
            .set_offset(gpui::point(px(0.0), px(-48.0)))
    });
    panes[0].update(cx, |pane, cx| pane.add_to_favorites(item_id, cx));
    cx.run_until_parked();
    let favorite_id = fixture
        .favorites
        .favorite_id_for_item(item_id)
        .unwrap()
        .unwrap();
    panes[1].read_with(cx, |pane, _| {
        assert_eq!(pane.total_item_count, 3);
        assert!(pane.list_order_ids.contains(&favorite_id));
        assert_eq!(pane.list_scroll_handle.offset().y, px(-48.0));
    });
    panes[1].update(cx, |pane, cx| {
        pane.remove_from_favorites(favorite_id, favorite_id, cx);
        pane.items.clear();
        pane.loaded_items.clear();
    });
    cx.run_until_parked();
    panes[0].read_with(cx, |pane, _| {
        assert!(
            pane.items
                .iter()
                .find(|item| item.item_id == item_id)
                .unwrap()
                .favorite_id
                .is_none()
        );
    });
    assert_eq!(panes[1].read_with(cx, |pane, _| pane.total_item_count), 2);
}

#[gpui::test]
fn split_orientation_reuses_panes_and_edge_position_suspends_them(cx: &mut gpui::TestAppContext) {
    let fixture = Fixture::new(cx);
    let panes = fixture.panes(cx);
    fixture.change_layout(
        ClipboardListLayout::Stacked,
        ClipboardWindowPosition::MouseCenter,
        cx,
    );
    assert_eq!(fixture.panes(cx), panes);
    fixture.change_layout(
        ClipboardListLayout::Stacked,
        ClipboardWindowPosition::ScreenLeftCenter,
        cx,
    );
    fixture.main.read_with(cx, |main, cx| {
        assert!(main.split_panes.is_none());
        assert_eq!(main.split_layout(cx), ClipboardListLayout::Single);
        assert_eq!(
            crate::config_runtime::clipboard_config(cx).list_layout,
            ClipboardListLayout::Stacked
        );
    });
    fixture.change_layout(
        ClipboardListLayout::Stacked,
        ClipboardWindowPosition::Remember,
        cx,
    );
    assert!(
        fixture
            .main
            .read_with(cx, |main, _| main.split_panes.is_some())
    );
}

#[gpui::test]
fn layout_sizes_are_saved_independently_and_old_state_keeps_its_single_size(
    cx: &mut gpui::TestAppContext,
) {
    let fixture = Fixture::new(cx);
    let side_size = state_runtime::PersistedWindowSize {
        width: 920.0,
        height: 600.0,
    };
    let stacked_size = state_runtime::PersistedWindowSize {
        width: 460.0,
        height: 820.0,
    };
    cx.update(|cx| state_runtime::set_main_window_size(side_size, cx));
    fixture.change_layout(
        ClipboardListLayout::Stacked,
        ClipboardWindowPosition::Remember,
        cx,
    );
    cx.update(|cx| state_runtime::set_main_window_size(stacked_size, cx));
    cx.update(|cx| {
        let state = state_runtime::state(cx).main_window;
        assert_eq!(
            state.restored_layout_size(ClipboardListLayout::SideBySide),
            side_size
        );
        assert_eq!(
            state.restored_layout_size(ClipboardListLayout::Stacked),
            stacked_size
        );
        assert_eq!(
            state.restored_size(),
            state_runtime::PersistedWindowSize::DEFAULT
        );
        let old: state_runtime::PersistedMainWindowState =
            serde_json::from_str(r#"{"size":{"width":480,"height":640}}"#).unwrap();
        assert_eq!(
            old.restored_size(),
            state_runtime::PersistedWindowSize {
                width: 480.0,
                height: 640.0
            }
        );
        assert_eq!(
            old.restored_layout_size(ClipboardListLayout::SideBySide)
                .width,
            860.0
        );
        assert_eq!(
            old.restored_layout_size(ClipboardListLayout::Stacked)
                .height,
            760.0
        );
    });
}

#[gpui::test]
fn rendered_panes_follow_orientation_and_mouse_and_keyboard_target_the_active_list(
    cx: &mut gpui::TestAppContext,
) {
    use gpui::BorrowAppContext as _;
    let fixture = Fixture::new(cx);
    let main = fixture.main.clone();
    cx.update(MenuService::install);
    let window = cx.open_window(gpui::size(px(860.0), px(560.0)), |window, cx| {
        gpui_component::Root::new(main.clone(), window, cx)
    });
    cx.update(|cx| {
        MainWindowService::set_main_view(main.clone(), cx);
        cx.update_global::<MainWindowService, _>(|service, _| {
            service.window = Some(window.into());
            service.window_state.set_visibility(true);
        });
    });
    for _ in 0..3 {
        cx.update_window(window.into(), |_, window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        })
        .unwrap();
        cx.run_until_parked();
    }
    let panes = fixture.panes(cx);
    let bounds = panes
        .each_ref()
        .map(|pane| pane.read_with(cx, |pane, _| pane.list_scroll_handle.bounds()));
    assert!(bounds[0].size.width > px(300.0));
    assert!(bounds[0].size.width < px(450.0));
    assert!(bounds[1].origin.x >= bounds[0].origin.x + bounds[0].size.width);
    assert_eq!(bounds[0].origin.y, bounds[1].origin.y);
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(bounds[1].center(), None, Modifiers::default());
    assert_eq!(
        main.read_with(cx, |main, _| main.active_tab),
        TabId::Favorites
    );
    let clipboard_selection = panes[0].read_with(cx, |pane, _| pane.selected_item_id);
    cx.update(|cx| {
        crate::windows::main::dispatch_session_shortcut(
            crate::config::SessionShortcutId::SelectNextItem,
            cx,
        )
    });
    assert!(panes[1].read_with(cx, |pane, _| pane.selected_item_id.is_some()));
    assert_eq!(
        panes[0].read_with(cx, |pane, _| pane.selected_item_id),
        clipboard_selection
    );

    fixture.change_layout(
        ClipboardListLayout::Stacked,
        ClipboardWindowPosition::Remember,
        cx,
    );
    for _ in 0..3 {
        cx.update_window(window.into(), |_, window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        })
        .unwrap();
        cx.run_until_parked();
    }
    let bounds = panes
        .each_ref()
        .map(|pane| pane.read_with(cx, |pane, _| pane.list_scroll_handle.bounds()));
    assert_eq!(bounds[0].origin.x, bounds[1].origin.x);
    assert!(bounds[1].origin.y >= bounds[0].origin.y + bounds[0].size.height);
    assert!(bounds[0].size.width > px(800.0));
}
