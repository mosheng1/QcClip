use super::*;
use crate::{
    config::ClipboardWindowPosition,
    services::{database::Database, image_library::ImageLibraryService},
};
use gpui::BorrowAppContext as _;

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
        config.clipboard.hover_preview_enabled = false;
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
            crate::window_material::install(cx);
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
            config.clipboard.window_position = position;
            config.clipboard.set_list_layout(layout);
            let path = self.directory.path().join("config.json");
            std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
            crate::config_runtime::ConfigRuntime::reload(path, cx);
        });
        self.main.update(cx, |main, cx| main.sync_split_layout(cx));
        cx.run_until_parked();
    }

    fn open_window(&self, cx: &mut gpui::TestAppContext) -> gpui::AnyWindowHandle {
        let main = self.main.clone();
        cx.update(MenuService::install);
        #[cfg(target_os = "windows")]
        cx.update(|cx| {
            crate::platform::drag_preview::DragPreviewService::install(cx);
            crate::platform::external_drag::ExternalDragService::install(cx);
        });
        let window = cx.open_window(gpui::size(px(860.0), px(560.0)), |window, cx| {
            gpui_component::Root::new(main.clone(), window, cx)
        });
        cx.update(|cx| {
            MainWindowService::set_main_view(main, cx);
            cx.update_global::<MainWindowService, _>(|service, _| {
                service.window = Some(window.into());
                service.window_state.set_visibility(true);
            });
        });
        draw_window(window.into(), cx);
        window.into()
    }
}

fn draw_window(window: gpui::AnyWindowHandle, cx: &mut gpui::TestAppContext) {
    for _ in 0..3 {
        cx.update_window(window, |_, window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        })
        .unwrap();
        cx.run_until_parked();
    }
}

#[gpui::test]
fn twemoji_images_are_released_after_scrolling_leaving_and_hiding(cx: &mut gpui::TestAppContext) {
    let fixture = Fixture::new(cx);
    fixture.main.update(cx, |main, cx| {
        main.select_symbol_category(SymbolCategoryId::Emoji, cx);
        main.select_tab(TabId::Emoji, cx);
    });
    let window = fixture.open_window(cx);
    let first_image = fixture.main.read_with(cx, |main, _| {
        let (index, image) = (0..crate::emoji::data().items.len())
            .find_map(|index| main.emoji_image(index).map(|image| (index, image)))
            .expect("表情页应加载可见图片");
        (index, Arc::downgrade(&image))
    });
    fixture.main.update(cx, |main, cx| {
        main.emoji_scroll_handle
            .set_offset(gpui::point(px(0.0), px(-6000.0)));
        cx.notify();
    });
    draw_window(window, cx);
    assert!(
        fixture
            .main
            .read_with(cx, |main, _| main.emoji_image(first_image.0).is_none())
    );
    assert!(first_image.1.upgrade().is_none(), "滚出视口的图片应释放");
    let next_image = fixture.main.read_with(cx, |main, _| {
        let image = (0..crate::emoji::data().items.len())
            .find_map(|index| main.emoji_image(index))
            .unwrap();
        Arc::downgrade(&image)
    });
    fixture
        .main
        .update(cx, |main, cx| main.select_tab(TabId::Clipboard, cx));
    draw_window(window, cx);
    assert!(
        fixture
            .main
            .read_with(cx, |main, _| main.emoji_resources.is_none())
    );
    assert!(next_image.upgrade().is_none(), "离开表情页应释放图片");

    fixture
        .main
        .update(cx, |main, cx| main.select_tab(TabId::Emoji, cx));
    draw_window(window, cx);
    assert!(
        fixture
            .main
            .read_with(cx, |main, _| main.emoji_resources.is_some())
    );
    let hidden_image = fixture.main.read_with(cx, |main, _| {
        let image = (0..crate::emoji::data().items.len())
            .find_map(|index| main.emoji_image(index))
            .unwrap();
        Arc::downgrade(&image)
    });
    cx.update_window(window, |_, window, cx| {
        cx.update_global::<MainWindowService, _>(|service, _| {
            service.window_state.set_visibility(false)
        });
        MainWindowService::handle_window_hidden(window, cx);
    })
    .unwrap();
    assert!(
        hidden_image.upgrade().is_none(),
        "隐藏后无需再次绘制即可释放图片"
    );
    draw_window(window, cx);
    assert!(
        fixture
            .main
            .read_with(cx, |main, _| main.emoji_resources.is_none()),
        "隐藏后的绘制不能重新加载资源"
    );
    cx.update(|cx| {
        cx.update_global::<MainWindowService, _>(|service, _| {
            service.window_state.set_visibility(true)
        });
    });
    draw_window(window, cx);
    assert!(
        fixture.main.read_with(cx, |main, _| {
            (0..crate::emoji::data().items.len()).any(|index| main.emoji_image(index).is_some())
        }),
        "再次显示后应能重新加载图片"
    );
}

#[gpui::test]
fn twemoji_pending_loads_do_not_restore_released_resources(cx: &mut gpui::TestAppContext) {
    let fixture = Fixture::new(cx);
    let window = fixture.open_window(cx);
    cx.update_window(window, |_, window, cx| {
        fixture.main.update(cx, |main, cx| {
            main.ensure_emoji_resources(cx);
            main.clear_emoji_resources(window, cx);
        });
    })
    .unwrap();
    cx.run_until_parked();
    assert!(
        fixture
            .main
            .read_with(cx, |main, _| main.emoji_resources.is_none())
    );

    fixture.main.update(cx, |main, cx| {
        main.select_symbol_category(SymbolCategoryId::Emoji, cx);
        main.select_tab(TabId::Emoji, cx);
    });
    draw_window(window, cx);
    cx.update_window(window, |_, window, cx| {
        fixture.main.update(cx, |main, cx| {
            let index = crate::emoji::data().items.len() - 1;
            assert!(main.emoji_image(index).is_none());
            main.ensure_emoji_images(HashSet::from([index]), window, cx);
            main.clear_emoji_resources(window, cx);
        });
    })
    .unwrap();
    cx.update(|cx| {
        cx.update_global::<MainWindowService, _>(|service, _| {
            service.window_state.set_visibility(false)
        });
    });
    cx.run_until_parked();
    assert!(
        fixture
            .main
            .read_with(cx, |main, _| main.emoji_resources.is_none())
    );
}

#[test]
fn split_ratios_default_for_old_state_and_validate_saved_values() {
    use state_runtime::PersistedMainWindowState;
    let old: PersistedMainWindowState = serde_json::from_str("{}").unwrap();
    for layout in [
        ClipboardListLayout::SideBySide,
        ClipboardListLayout::Stacked,
    ] {
        assert_eq!(old.restored_split_ratio(layout), 0.5);
    }
    let state = PersistedMainWindowState {
        split_ratios: [-1.0, 2.0],
        ..Default::default()
    };
    assert_eq!(
        state.restored_split_ratio(ClipboardListLayout::SideBySide),
        0.2
    );
    assert_eq!(
        state.restored_split_ratio(ClipboardListLayout::Stacked),
        0.8
    );
    let state = PersistedMainWindowState {
        split_ratios: [f32::NAN, f32::INFINITY],
        ..Default::default()
    };
    assert_eq!(
        state.restored_split_ratio(ClipboardListLayout::SideBySide),
        0.5
    );
    assert_eq!(
        state.restored_split_ratio(ClipboardListLayout::Stacked),
        0.5
    );
}

#[gpui::test]
fn dragging_split_divider_resizes_both_orientations_and_persists_on_release(
    cx: &mut gpui::TestAppContext,
) {
    let fixture = Fixture::new(cx);
    let window = fixture.open_window(cx);
    fixture.main.update(cx, |main, cx| main.persist_state(cx));
    let state_path = fixture.directory.path().join("state.json");
    for (layout, target) in [
        (ClipboardListLayout::SideBySide, 0.65),
        (ClipboardListLayout::Stacked, 0.35),
    ] {
        fixture.change_layout(layout, ClipboardWindowPosition::Remember, cx);
        draw_window(window, cx);
        let panes = fixture.panes(cx);
        let active = fixture.main.read_with(cx, |main, _| main.active_tab);
        let selections = panes.each_ref().map(|pane| {
            pane.update(cx, |pane, _| {
                pane.selected_item_id = pane.list_order_ids.last().copied();
                pane.selected_item_id
            })
        });
        let before = std::fs::read(&state_path).unwrap();
        let start = fixture
            .main
            .read_with(cx, |main, _| main.split_content_bounds.unwrap().center());
        let mut visual = gpui::VisualTestContext::from_window(window, cx);
        visual.simulate_mouse_down(start, gpui::MouseButton::Left, Modifiers::default());
        let available = fixture.main.read_with(cx, |main, _| {
            assert!(main.window_drag_start.is_none());
            main.split_resize.unwrap().available
        });
        let mut end = start;
        if layout == ClipboardListLayout::SideBySide {
            end.x += available * (target - 0.5);
        } else {
            end.y += available * (target - 0.5);
        }
        visual.simulate_mouse_move(end, Some(gpui::MouseButton::Left), Modifiers::default());
        draw_window(window, cx);
        fixture.main.read_with(cx, |main, cx| {
            assert!((main.split_ratio(cx) - target).abs() < 0.001);
            assert_eq!(main.active_tab, active);
            assert!(main.window_drag_start.is_none());
        });
        assert_eq!(std::fs::read(&state_path).unwrap(), before);
        let bounds = panes
            .each_ref()
            .map(|pane| pane.read_with(cx, |pane, _| pane.list_scroll_handle.bounds()));
        let spacing = if layout == ClipboardListLayout::SideBySide {
            bounds[1].origin.x - bounds[0].origin.x
        } else {
            bounds[1].origin.y - bounds[0].origin.y
        };
        assert!((f32::from(spacing - px(6.0)) / f32::from(available) - target).abs() < 0.01);
        for (index, pane) in panes.iter().enumerate() {
            assert_eq!(
                pane.read_with(cx, |pane, _| pane.selected_item_id),
                selections[index]
            );
        }
        visual.simulate_mouse_up(end, gpui::MouseButton::Left, Modifiers::default());
        fixture
            .main
            .read_with(cx, |main, _| assert!(main.split_resize.is_none()));
        let saved: state_runtime::PersistedState =
            serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
        assert!((saved.main_window.restored_split_ratio(layout) - target).abs() < 0.001);
        visual.simulate_resize(gpui::size(px(1000.0), px(700.0)));
        draw_window(window, cx);
        fixture.main.read_with(cx, |main, cx| {
            assert!((main.split_ratio(cx) - target).abs() < 0.001);
        });
    }
    cx.update(|cx| state_runtime::StateRuntime::install(state_path.clone(), cx));
    fixture.change_layout(
        ClipboardListLayout::SideBySide,
        ClipboardWindowPosition::Remember,
        cx,
    );
    draw_window(window, cx);
    let start = fixture.main.read_with(cx, |main, cx| {
        assert!((main.split_ratio(cx) - 0.65).abs() < 0.001);
        let bounds = main.split_content_bounds.unwrap();
        gpui::point(
            bounds.origin.x + (bounds.size.width - px(6.0)) * main.split_ratio(cx) + px(3.0),
            bounds.center().y,
        )
    });
    let mut visual = gpui::VisualTestContext::from_window(window, cx);
    visual.simulate_event(gpui::MouseDownEvent {
        position: start,
        button: gpui::MouseButton::Left,
        modifiers: Modifiers::default(),
        click_count: 2,
        first_mouse: false,
    });
    visual.simulate_mouse_up(start, gpui::MouseButton::Left, Modifiers::default());
    let saved: state_runtime::PersistedState =
        serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
    assert_eq!(saved.main_window.split_ratios[0], 0.5);
    assert!((saved.main_window.split_ratios[1] - 0.35).abs() < 0.001);
    fixture.change_layout(
        ClipboardListLayout::Stacked,
        ClipboardWindowPosition::ScreenLeftCenter,
        cx,
    );
    fixture.change_layout(
        ClipboardListLayout::Stacked,
        ClipboardWindowPosition::Remember,
        cx,
    );
    fixture.main.read_with(cx, |main, cx| {
        assert!((main.split_ratio(cx) - 0.35).abs() < 0.001);
    });
}

#[gpui::test]
fn split_divider_snaps_to_center_with_a_release_buffer_and_saves_exactly_half(
    cx: &mut gpui::TestAppContext,
) {
    let fixture = Fixture::new(cx);
    cx.update(|cx| {
        let mut state = state_runtime::state(cx);
        state.main_window.split_ratios = [0.6, 0.4];
        state_runtime::set_state(state, cx).unwrap();
    });
    let window = fixture.open_window(cx);
    for layout in [
        ClipboardListLayout::SideBySide,
        ClipboardListLayout::Stacked,
    ] {
        fixture.change_layout(layout, ClipboardWindowPosition::Remember, cx);
        draw_window(window, cx);
        let (start, start_ratio) = fixture.main.read_with(cx, |main, cx| {
            let bounds = main.split_content_bounds.unwrap();
            let ratio = main.split_ratio(cx);
            let mut position = bounds.center();
            if layout == ClipboardListLayout::SideBySide {
                position.x = bounds.origin.x + (bounds.size.width - px(6.0)) * ratio + px(3.0);
            } else {
                position.y = bounds.origin.y + (bounds.size.height - px(6.0)) * ratio + px(3.0);
            }
            (position, ratio)
        });
        let mut visual = gpui::VisualTestContext::from_window(window, cx);
        visual.simulate_mouse_down(start, gpui::MouseButton::Left, Modifiers::default());
        let available = fixture
            .main
            .read_with(cx, |main, _| main.split_resize.unwrap().available);
        let mut end = start;
        for (target, expected) in [
            (0.60, 0.60),
            (0.511, 0.511),
            (0.509, 0.5),
            (0.514, 0.5),
            (0.516, 0.516),
            (0.509, 0.5),
            (0.486, 0.5),
            (0.484, 0.484),
            (0.489, 0.489),
            (0.491, 0.5),
        ] {
            if layout == ClipboardListLayout::SideBySide {
                end.x = start.x + available * (target - start_ratio);
            } else {
                end.y = start.y + available * (target - start_ratio);
            }
            visual.simulate_mouse_move(end, Some(gpui::MouseButton::Left), Modifiers::default());
            fixture.main.read_with(cx, |main, cx| {
                assert!((main.split_ratio(cx) - expected).abs() < 0.0001);
            });
        }
        draw_window(window, cx);
        let bounds = fixture
            .panes(cx)
            .each_ref()
            .map(|pane| pane.read_with(cx, |pane, _| pane.list_scroll_handle.bounds()));
        let spacing = if layout == ClipboardListLayout::SideBySide {
            bounds[1].origin.x - bounds[0].origin.x
        } else {
            bounds[1].origin.y - bounds[0].origin.y
        };
        assert!(f32::from(spacing - px(6.0) - available * 0.5).abs() < 1.0);
        visual.simulate_mouse_up(end, gpui::MouseButton::Left, Modifiers::default());
        let saved: state_runtime::PersistedState = serde_json::from_slice(
            &std::fs::read(fixture.directory.path().join("state.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(saved.main_window.restored_split_ratio(layout), 0.5);
    }
}

#[gpui::test]
fn split_resize_releases_outside_the_window_and_recovers_a_missing_mouse_up(
    cx: &mut gpui::TestAppContext,
) {
    let fixture = Fixture::new(cx);
    let window = fixture.open_window(cx);
    let start = fixture
        .main
        .read_with(cx, |main, _| main.split_content_bounds.unwrap().center());
    let mut visual = gpui::VisualTestContext::from_window(window, cx);
    visual.simulate_mouse_down(start, gpui::MouseButton::Left, Modifiers::default());
    let outside = gpui::point(px(-100.0), px(-100.0));
    visual.simulate_mouse_move(outside, Some(gpui::MouseButton::Left), Modifiers::default());
    visual.simulate_mouse_up(outside, gpui::MouseButton::Left, Modifiers::default());
    fixture.main.read_with(cx, |main, cx| {
        assert!(main.split_resize.is_none());
        assert_eq!(main.split_ratio(cx), 0.2);
    });
    draw_window(window, cx);
    let start = fixture.main.read_with(cx, |main, _| {
        let bounds = main.split_content_bounds.unwrap();
        gpui::point(
            bounds.origin.x + (bounds.size.width - px(6.0)) * 0.2 + px(3.0),
            bounds.center().y,
        )
    });
    visual.simulate_mouse_down(start, gpui::MouseButton::Left, Modifiers::default());
    let outside = gpui::point(px(2000.0), start.y);
    visual.simulate_mouse_move(outside, Some(gpui::MouseButton::Left), Modifiers::default());
    visual.simulate_mouse_move(outside, None, Modifiers::default());
    fixture.main.read_with(cx, |main, cx| {
        assert!(main.split_resize.is_none());
        assert_eq!(main.split_ratio(cx), 0.8);
        assert_eq!(state_runtime::state(cx).main_window.split_ratios[0], 0.8);
        assert!(main.window_drag_start.is_none());
    });
}

#[gpui::test]
fn split_resize_clamps_and_layout_changes_finish_the_original_orientation(
    cx: &mut gpui::TestAppContext,
) {
    let fixture = Fixture::new(cx);
    fixture.main.update(cx, |main, cx| {
        main.split_content_bounds = Some(gpui::Bounds::new(
            gpui::point(px(0.0), px(0.0)),
            gpui::size(px(1006.0), px(506.0)),
        ));
        let start = gpui::point(px(503.0), px(253.0));
        main.begin_split_resize(start, px(6.0), cx);
        main.update_split_resize(gpui::point(px(-500.0), start.y), cx);
        assert_eq!(main.split_ratio(cx), 0.2);
        main.update_split_resize(gpui::point(px(2000.0), start.y), cx);
        assert_eq!(main.split_ratio(cx), 0.8);
        main.activate_split_pane(TabId::Favorites, cx);
        assert_eq!(main.active_tab, TabId::Clipboard);
    });
    fixture.change_layout(
        ClipboardListLayout::Stacked,
        ClipboardWindowPosition::Remember,
        cx,
    );
    fixture.main.read_with(cx, |main, cx| {
        assert!(main.split_resize.is_none());
        assert_eq!(main.split_ratio(cx), 0.5);
        assert_eq!(state_runtime::state(cx).main_window.split_ratios[0], 0.8);
    });
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
fn batch_tags_cover_unloaded_items_and_preserve_selection_in_both_panes(
    cx: &mut gpui::TestAppContext,
) {
    let fixture = Fixture::new(cx);
    let config_path = fixture.directory.path().join("config.json");
    let mut config: crate::config::AppConfig =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config.general.global_notification_enabled = false;
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    cx.update(|cx| crate::config_runtime::ConfigRuntime::reload(config_path, cx));
    let panes = fixture.panes(cx);

    for source in 0..2 {
        let tag = fixture
            .tags
            .create(&format!("批量标签{source}"), "#1677FF")
            .unwrap();
        let selected_ids = panes[source].read_with(cx, |pane, _| pane.list_order_ids.clone());
        assert!(selected_ids.len() > 1);
        let source_tab = if source == 0 {
            TabId::Clipboard
        } else {
            TabId::Favorites
        };
        let item_ids = resolve_multi_selection_item_ids(
            source_tab,
            &selected_ids,
            &fixture.history,
            &fixture.favorites,
        )
        .unwrap();
        let generation = panes[source].update(cx, |pane, cx| {
            pane.items.truncate(1);
            let loaded_id = pane.items[0].history_id;
            pane.loaded_items.retain(|id, _| *id == loaded_id);
            pane.multi_selected_item_ids = selected_ids.clone();
            pane.multi_selection_anchor = selected_ids.first().copied();
            pane.selected_item_id = selected_ids.first().copied();
            pane.list_scroll_handle
                .as_ref()
                .set_offset(gpui::point(px(0.0), px(-48.0)));
            pane.set_tag_for_multi_selected_items(item_ids.clone(), tag.id, true, cx)
                .detach();
            pane.query_generation
        });
        cx.run_until_parked();

        for item_id in &item_ids {
            assert!(
                fixture
                    .tags
                    .item_tag_ids(*item_id)
                    .unwrap()
                    .contains(&tag.id)
            );
        }
        panes[source].read_with(cx, |pane, _| {
            assert_eq!(pane.multi_selected_item_ids, selected_ids);
            assert_eq!(pane.selected_item_id, selected_ids.first().copied());
            assert_eq!(pane.query_generation, generation);
            assert_eq!(pane.list_scroll_handle.offset().y, px(-48.0));
            assert!(pane.items.iter().all(|item| item.tag_ids.contains(&tag.id)));
            assert!(
                pane.loaded_items
                    .values()
                    .all(|item| item.tag_ids.contains(&tag.id))
            );
        });
        panes[1 - source].read_with(cx, |pane, _| {
            for item in &pane.items {
                if item_ids.contains(&item.item_id) {
                    assert!(item.tag_ids.contains(&tag.id));
                }
            }
        });
        panes[source].update(cx, |pane, cx| {
            pane.set_tag_for_multi_selected_items(item_ids.clone(), tag.id, false, cx)
                .detach();
        });
        cx.run_until_parked();
        for item_id in &item_ids {
            assert!(
                !fixture
                    .tags
                    .item_tag_ids(*item_id)
                    .unwrap()
                    .contains(&tag.id)
            );
        }
        panes[source].read_with(cx, |pane, _| {
            assert_eq!(pane.multi_selected_item_ids, selected_ids);
            assert_eq!(pane.list_scroll_handle.offset().y, px(-48.0));
            assert!(
                pane.items
                    .iter()
                    .all(|item| !item.tag_ids.contains(&tag.id))
            );
        });
        panes[1 - source].read_with(cx, |pane, _| {
            assert!(
                pane.items
                    .iter()
                    .all(|item| !item.tag_ids.contains(&tag.id))
            );
        });
    }
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
fn split_orientation_and_edge_positions_reuse_panes_until_single_is_selected(
    cx: &mut gpui::TestAppContext,
) {
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
    assert_eq!(fixture.panes(cx), panes);
    fixture.change_layout(
        ClipboardListLayout::Single,
        ClipboardWindowPosition::ScreenRightCenter,
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
fn split_ratios_are_independent_for_normal_and_both_edge_groups(cx: &mut gpui::TestAppContext) {
    let fixture = Fixture::new(cx);
    for (position, ratio) in [
        (ClipboardWindowPosition::Remember, 0.6),
        (ClipboardWindowPosition::ScreenLeftCenter, 0.3),
        (ClipboardWindowPosition::ScreenTopCenter, 0.7),
    ] {
        fixture.change_layout(ClipboardListLayout::SideBySide, position, cx);
        fixture.main.update(cx, |main, cx| {
            let start_ratio = main.split_ratio(cx);
            main.split_content_bounds = Some(gpui::Bounds::new(
                gpui::point(px(0.0), px(0.0)),
                gpui::size(px(1006.0), px(506.0)),
            ));
            main.begin_split_resize(gpui::point(px(500.0), px(250.0)), px(6.0), cx);
            main.update_split_resize(
                gpui::point(px(500.0 + 1000.0 * (ratio - start_ratio)), px(250.0)),
                cx,
            );
            main.finish_split_resize(cx);
            assert!((main.split_ratio(cx) - ratio).abs() < 0.001);
        });
    }
    let path = fixture.directory.path().join("state.json");
    cx.update(|cx| state_runtime::StateRuntime::install(path, cx));
    for (position, ratio) in [
        (ClipboardWindowPosition::Remember, 0.6),
        (ClipboardWindowPosition::ScreenRightCenter, 0.3),
        (ClipboardWindowPosition::ScreenBottomCenter, 0.7),
    ] {
        fixture.change_layout(ClipboardListLayout::SideBySide, position, cx);
        fixture.main.read_with(cx, |main, cx| {
            assert!((main.split_ratio(cx) - ratio).abs() < 0.001);
        });
    }
    fixture.main.update(cx, |main, cx| {
        main.begin_split_resize(gpui::point(px(500.0), px(250.0)), px(6.0), cx);
        main.update_split_resize(gpui::point(px(450.0), px(250.0)), cx);
    });
    fixture.change_layout(
        ClipboardListLayout::SideBySide,
        ClipboardWindowPosition::ScreenLeftCenter,
        cx,
    );
    fixture.main.read_with(cx, |main, cx| {
        assert!(main.split_resize.is_none());
        assert!((main.split_ratio(cx) - 0.3).abs() < 0.001);
        assert!(
            (state_runtime::state(cx).main_window.edge_split_ratios[1][0] - 0.65).abs() < 0.001
        );
    });
    let old: state_runtime::PersistedMainWindowState = serde_json::from_str("{}").unwrap();
    assert_eq!(
        old.restored_list_ratio(
            ClipboardListLayout::Stacked,
            ClipboardWindowPosition::ScreenLeftCenter
        ),
        0.5
    );
}

#[gpui::test]
fn all_screen_edges_render_split_panes_with_the_correct_scroll_axis_and_shared_filters(
    cx: &mut gpui::TestAppContext,
) {
    let fixture = Fixture::new(cx);
    let window = fixture.open_window(cx);
    let service = fixture
        .main
        .read_with(cx, |main, _| main.custom_text_service.clone());
    for index in 0..30 {
        service
            .create_manual_text_and_favorite(&format!("贴边内容 {index}"))
            .unwrap();
    }
    for position in [
        ClipboardWindowPosition::ScreenLeftCenter,
        ClipboardWindowPosition::ScreenRightCenter,
        ClipboardWindowPosition::ScreenTopCenter,
        ClipboardWindowPosition::ScreenBottomCenter,
    ] {
        let horizontal = position.uses_horizontal_list();
        let layout = if horizontal {
            ClipboardListLayout::SideBySide
        } else {
            ClipboardListLayout::Stacked
        };
        fixture.change_layout(layout, position, cx);
        let mut visual = gpui::VisualTestContext::from_window(window, cx);
        visual.simulate_resize(if horizontal {
            gpui::size(px(1200.0), px(300.0))
        } else {
            gpui::size(px(380.0), px(900.0))
        });
        fixture.main.update(cx, |main, cx| {
            main.search_query = "贴边内容".to_owned();
            main.refresh_history(cx);
        });
        cx.run_until_parked();
        draw_window(window, cx);
        let panes = fixture.panes(cx);
        let bounds = panes.each_ref().map(|pane| {
            pane.read_with(cx, |pane, _| {
                assert_eq!(pane.total_item_count, 30);
                assert_eq!(
                    pane.list_layout_cache.as_ref().unwrap().key.horizontal,
                    horizontal
                );
                assert_eq!(pane.history_query().search.as_deref(), Some("贴边内容"));
                pane.list_scroll_handle.bounds()
            })
        });
        if horizontal {
            assert_eq!(bounds[0].origin.y, bounds[1].origin.y);
            assert!(bounds[1].origin.x > bounds[0].origin.x);
        } else {
            assert_eq!(bounds[0].origin.x, bounds[1].origin.x);
            assert!(bounds[1].origin.y > bounds[0].origin.y);
        }
        let before = panes[1].read_with(cx, |pane, _| pane.list_scroll_handle.offset());
        visual.simulate_mouse_move(bounds[0].center(), None, Modifiers::default());
        panes[0].update(cx, |pane, _| {
            pane.list_scroll_handle
                .as_ref()
                .set_offset(gpui::point(px(0.0), px(0.0)));
        });
        visual.simulate_event(ScrollWheelEvent {
            position: bounds[0].center(),
            delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(-80.0))),
            modifiers: Modifiers::default(),
            touch_phase: gpui::TouchPhase::Started,
        });
        panes[0].read_with(cx, |pane, _| {
            let offset = pane.list_scroll_handle.offset();
            assert!(if horizontal { offset.x } else { offset.y } < px(0.0));
            assert_eq!(if horizontal { offset.y } else { offset.x }, px(0.0));
        });
        assert_eq!(
            panes[1].read_with(cx, |pane, _| pane.list_scroll_handle.offset()),
            before
        );
        fixture.main.update(cx, |main, cx| {
            main.activate_split_pane(TabId::Clipboard, cx)
        });
        visual.simulate_mouse_move(bounds[1].center(), None, Modifiers::default());
        fixture.main.update(cx, |main, cx| {
            assert_eq!(main.active_tab, TabId::Favorites);
            main.move_tab(-1, cx);
            assert_eq!(main.active_tab, TabId::Clipboard);
            assert_eq!(main.active_list_entity(cx), Some(panes[0].clone()));
        });
        let start = fixture
            .main
            .read_with(cx, |main, _| main.split_content_bounds.unwrap().center());
        visual.simulate_mouse_down(start, gpui::MouseButton::Left, Modifiers::default());
        fixture.main.read_with(cx, |main, cx| {
            assert!(main.list_pointer_interaction_active(cx))
        });
        let mut end = start;
        if horizontal {
            end.x += px(45.0);
        } else {
            end.y += px(45.0);
        }
        visual.simulate_mouse_move(end, Some(gpui::MouseButton::Left), Modifiers::default());
        visual.simulate_mouse_up(end, gpui::MouseButton::Left, Modifiers::default());
        fixture.main.read_with(cx, |main, cx| {
            assert!(!main.list_pointer_interaction_active(cx))
        });
        panes[1].update(cx, |pane, _| {
            pane.heat_scrollbar.drag_origin = Some((end, 0.5))
        });
        fixture.main.read_with(cx, |main, cx| {
            assert!(main.list_pointer_interaction_active(cx))
        });
        panes[1].update(cx, |pane, _| pane.heat_scrollbar.drag_origin = None);
        fixture.main.update(cx, |main, cx| {
            main.search_query.clear();
            main.refresh_history(cx);
        });
        cx.run_until_parked();
        // 下一种定位从均分开始，测试分隔条命中位置。
        fixture
            .main
            .update(cx, |main, cx| main.reset_split_ratio(cx));
    }
    // 在释放临时数据库和文本文件前关闭窗口，避免收尾绘制继续请求文件。
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
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

#[gpui::test]
fn external_tags_panel_renders_and_drops_tags_into_both_panes(cx: &mut gpui::TestAppContext) {
    use crate::windows::main::{components::tags_panel, tags_window};
    use gpui::{IntoElement, Render};

    struct Panel(Entity<MainWindow>);
    impl Render for Panel {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.0.update(cx, |main, cx| {
                main.sync_tags_panel_host(true);
                main.ensure_tags_search_input(window, cx);
                tags_panel::render_external(
                    main,
                    window,
                    cx,
                    window.viewport_size().width > px(200.0),
                )
                .into_any_element()
            })
        }
    }

    let fixture = Fixture::new(cx);
    let tags = [
        "外置测试标签一：验证横向面板变窄后标签可以自动换行显示",
        "外置测试标签二：验证横向面板变窄后标签可以自动换行显示",
    ]
    .map(|name| fixture.tags.create(name, "#1677FF").unwrap());
    let target = fixture.open_window(cx);
    cx.update(tags_window::install);
    fixture.main.update(cx, |main, cx| {
        main.tags_panel_open = true;
        main.refresh_tags(cx);
    });
    cx.run_until_parked();
    let main = fixture.main.clone();
    let panel = cx.open_window(gpui::size(px(100.0), px(400.0)), |window, cx| {
        let panel = cx.new(|_| Panel(main));
        gpui_component::Root::new(panel, window, cx)
    });
    draw_window(panel.into(), cx);
    let panes = fixture.panes(cx);
    for (index, pane) in panes.into_iter().enumerate() {
        let tag = &tags[index];
        if index == 1 {
            cx.update_window(panel.into(), |_, window, cx| {
                window.resize(gpui::size(px(900.0), px(120.0)));
                window.bounds_changed(cx);
            })
            .unwrap();
        }
        let (item_id, position) = pane.read_with(cx, |pane, _| {
            (
                pane.items[0].item_id,
                pane.list_scroll_handle.bounds().origin + gpui::point(px(50.0), px(25.0)),
            )
        });
        draw_window(panel.into(), cx);
        let chip = fixture.main.read_with(cx, |main, _| {
            let positions = main.tag_chip_positions.borrow();
            let same_row = positions[&tags[0].id].y == positions[&tags[1].id].y;
            assert_eq!(
                same_row,
                index == 1,
                "横向面板应在一行显示标签，竖向面板应逐行显示"
            );
            if index == 0 {
                assert_eq!(
                    positions[&tag.id].x,
                    px(8.5),
                    "标签应与搜索框使用相同的 8px 左边距"
                );
            }
            positions[&tag.id]
        });
        let mut source = gpui::VisualTestContext::from_window(panel.into(), cx);
        let start = chip + gpui::point(px(12.0), px(12.0));
        source.simulate_mouse_down(start, gpui::MouseButton::Left, Modifiers::default());
        source.simulate_mouse_move(
            start + gpui::point(px(8.0), px(0.0)),
            Some(gpui::MouseButton::Left),
            Modifiers::default(),
        );
        assert!(cx.update(|cx| cx.has_active_drag()));
        cx.update_window(target, |_, window, cx| {
            tags_window::dispatch_tag_drag(position, Modifiers::default(), true, window, cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert!(
            fixture
                .tags
                .item_tag_ids(item_id)
                .unwrap()
                .contains(&tag.id)
        );
        assert!(
            !fixture
                .main
                .read_with(cx, |main, _| main.is_tag_drag_source(tag.id))
        );
    }

    cx.update_window(panel.into(), |_, window, cx| {
        window.resize(gpui::size(px(220.0), px(120.0)));
        window.bounds_changed(cx);
    })
    .unwrap();
    draw_window(panel.into(), cx);
    fixture.main.read_with(cx, |main, _| {
        let positions = main.tag_chip_positions.borrow();
        assert_ne!(
            positions[&tags[0].id].y, positions[&tags[1].id].y,
            "横向面板宽度不足时标签应换行"
        );
        assert_eq!(
            positions[&tags[0].id].x, positions[&tags[1].id].x,
            "换行后应从行首显示"
        );
    });

    cx.update(tags_window::hide);
    assert!(fixture.main.read_with(cx, |main, _| main.tags_panel_open));

    fixture.main.update(cx, |main, _| {
        main.tags_search_query = "保留搜索".to_owned();
        main.sync_tags_panel_host(false);
        assert!(main.tags_search_input.is_none());
        main.sync_tags_panel_host(true);
        assert_eq!(main.tags_search_query, "保留搜索");
    });
}

#[gpui::test]
fn tags_panel_visibility_changes_only_on_click_in_both_modes(cx: &mut gpui::TestAppContext) {
    let fixture = Fixture::new(cx);
    let window = fixture.open_window(cx);
    for external in [false, true] {
        cx.update(|cx| {
            let mut clipboard = crate::config_runtime::clipboard_config(cx);
            clipboard.tags_panel_external = external;
            clipboard.filter_hover_switch = true;
            crate::config_runtime::set_clipboard_config(clipboard, cx).unwrap();
        });
        draw_window(window, cx);
        let button = fixture
            .main
            .read_with(cx, |main, _| main.tags_button_bounds.unwrap().center());
        let mut visual = gpui::VisualTestContext::from_window(window, cx);
        visual.simulate_mouse_move(button, None, Modifiers::default());
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert!(
            !fixture.main.read_with(cx, |main, _| main.tags_panel_open),
            "悬停标签按钮不应展开面板"
        );

        visual.simulate_click(button, Modifiers::default());
        draw_window(window, cx);
        assert!(fixture.main.read_with(cx, |main, _| main.tags_panel_open));
        visual.simulate_mouse_move(
            gpui::point(px(-20.0), px(-20.0)),
            None,
            Modifiers::default(),
        );
        cx.update_window(window, |_, window, cx| window.blur(cx))
            .unwrap();
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert!(
            fixture.main.read_with(cx, |main, _| main.tags_panel_open),
            "鼠标离开及搜索框失焦不应收起面板"
        );

        let button = fixture
            .main
            .read_with(cx, |main, _| main.tags_button_bounds.unwrap().center());
        visual.simulate_click(button, Modifiers::default());
        draw_window(window, cx);
        assert!(
            !fixture.main.read_with(cx, |main, _| main.tags_panel_open),
            "再次点击应收起面板，即使筛选悬停切换已开启"
        );
    }
}
