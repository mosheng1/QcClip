use super::*;

pub enum ClipboardListenerToggleResult {
    Enabled,
    Disabled,
    Failed,
}

impl MainWindow {
    pub fn replace_data_sources(
        &mut self,
        database: crate::services::database::Database,
        blob_store: crate::services::storage::RawBlobStore,
        data_root: &std::path::Path,
        cx: &mut Context<Self>,
    ) {
        self.history_repository = HistoryRepository::new(database.clone(), blob_store.clone());
        self.favorite_repository =
            FavoriteRepository::with_blob_store(database.clone(), blob_store.clone());
        self.tag_repository = TagRepository::new(database.clone());
        self.capture_service = CaptureService::new(database.clone(), blob_store.clone());
        self.custom_text_service = CustomTextService::new(database.clone(), blob_store.clone());
        self.paste_service = PasteService::with_clipboard_write_suppressor(
            database,
            blob_store.clone(),
            self.clipboard_write_suppressor.clone(),
        );
        self.image_preview_service = ImagePreviewService::new(blob_store);
        self.image_library = crate::services::image_library::ImageLibraryService::new(
            data_root.join("image_library"),
        );
        self.reload_after_data_import(cx);
    }

    pub fn start_clipboard_listener(
        &mut self,
        receiver: ClipboardEventReceiver,
        cx: &mut Context<Self>,
    ) {
        self.clipboard_listener_task.take();
        let receiver = Arc::new(Mutex::new(receiver));
        let capture_service = self.capture_service.clone();
        let task = cx.spawn(async move |this, cx| {
            loop {
                let receiver = receiver.clone();
                let event = cx
                    .background_executor()
                    .spawn(async move {
                        match receiver.lock() {
                            Ok(receiver) => match receiver.recv() {
                                Ok(event) => Some(event),
                                Err(error) => {
                                    tracing::info!(%error, "剪贴板事件通道已关闭，当前监听任务结束");
                                    None
                                }
                            },
                            Err(_) => {
                                tracing::error!("剪贴板监听接收器锁状态异常，停止监听");
                                None
                            }
                        }
                    })
                    .await;
                let Some(event) = event else {
                    tracing::debug!("剪贴板监听后台任务已退出");
                    break;
                };
                let captured = match event {
                    Ok(captured) => captured,
                    Err(error) => {
                        tracing::error!("剪贴板捕获失败：{error}");
                        continue;
                    }
                };
                let should_skip = cx.update(|cx| {
                    crate::services::application_filter::should_skip_clipboard_source(
                        &captured.source_application,
                        cx,
                    )
                });
                if should_skip {
                    tracing::debug!(
                        source = ?captured.source_application,
                        "应用过滤已跳过剪贴板内容入库"
                    );
                    continue;
                }
                let service = capture_service.clone();
                let store_result = cx
                    .background_executor()
                    .spawn(async move { service.store_captured_clipboard_with_status(&captured) })
                    .await;
                match store_result {
                    Ok(store_result) => {
                        let history_id = store_result.history_id;
                        crate::services::sound::play(crate::services::sound::SoundEvent::Copy);
                        cx.update(|cx| {
                            crate::windows::notification::NotificationService::show_clipboard(
                                history_id,
                                cx,
                            );
                        });
                        if let Some(this) = this.upgrade() {
                            this.update(cx, |this, cx| {
                                if this.active_tab == TabId::Clipboard {
                                    this.refresh_new_list_item(history_id, cx);
                                } else if this.active_tab == TabId::Favorites
                                    && store_result.formats_changed
                                {
                                    this.refresh_history(cx);
                                }
                            });
                        } else {
                            tracing::debug!("主窗口已关闭，停止处理剪贴板事件");
                            break;
                        }
                    }
                    Err(error) => tracing::error!("剪贴板写入数据库失败：{error}"),
                }
            }
        });
        self.clipboard_listener_task = Some(task);
    }

    pub fn stop_clipboard_listener(&mut self) {
        self._clipboard_monitor.take();
        self.clipboard_listener_task.take();
    }

    pub fn restart_clipboard_listener(&mut self, cx: &mut Context<Self>) {
        self.stop_clipboard_listener();
        if !self.clipboard_listener_enabled {
            return;
        }
        match ClipboardMonitor::start(self.clipboard_write_suppressor.clone()) {
            Ok((monitor, receiver)) => {
                self._clipboard_monitor = Some(monitor);
                self.start_clipboard_listener(receiver, cx);
                tracing::info!("剪贴板监听已重新加载");
            }
            Err(error) => {
                tracing::error!("重新启动剪贴板监听失败：{error}");
            }
        }
    }

    pub fn toggle_clipboard_listener(
        &mut self,
        cx: &mut Context<Self>,
    ) -> ClipboardListenerToggleResult {
        if self._clipboard_monitor.is_some() {
            self.clipboard_listener_enabled = false;
            self.stop_clipboard_listener();
            tracing::info!("剪贴板监听已关闭");
            ClipboardListenerToggleResult::Disabled
        } else {
            self.clipboard_listener_enabled = true;
            self.restart_clipboard_listener(cx);
            if self._clipboard_monitor.is_some() {
                ClipboardListenerToggleResult::Enabled
            } else {
                tracing::error!("剪贴板监听启动失败，无法开启监听");
                ClipboardListenerToggleResult::Failed
            }
        }
    }

    pub fn reload_after_data_import(&mut self, cx: &mut Context<Self>) {
        self.stop_clipboard_listener();
        let persisted_state = state_runtime::state(cx).main_window;
        self.active_tab = persisted_state.active_tab;
        self.symbol_category = persisted_state.symbol_category;
        self.recent_emojis = persisted_state.recent_emojis;
        self.recent_symbols = persisted_state.recent_symbols;
        self.gallery_sort = persisted_state.gallery_sort;
        self.selected_tag_ids.clear();
        self.tags_match_all = false;
        self.refresh_tags(cx);
        self.refresh_history(cx);
        self.gallery_groups_generation = self.gallery_groups_generation.wrapping_add(1);
        self.gallery_images_generation = self.gallery_images_generation.wrapping_add(1);
        self.gallery_groups_loaded = false;
        self.gallery_groups_loading = false;
        self.gallery_groups.clear();
        self.gallery_images.clear();
        self.selected_gallery_group_id = None;
        self.clear_gallery_image_cache();
        self.loaded_gallery_group_icons.clear();
        self.gallery_group_icon_loading.clear();
        self.gallery_group_icon_failures.clear();
        if self.is_gallery_active() {
            self.ensure_image_library_loaded(cx);
        }
        self.restart_clipboard_listener(cx);
        cx.notify();
    }

    pub fn refresh_history(&mut self, cx: &mut Context<Self>) {
        self.query_generation = self.query_generation.wrapping_add(1);
        // 丢弃上一次刷新任务。任务拥有后台查询句柄，丢弃后会取消仍在排队的查询，
        // 防止连续切换列表或筛选时过期查询挤占后台执行器。
        self.list_query_task = None;
        self.clear_list_drop_target();
        self.list_drag_source_id = None;
        self.list_drag_origin_position = None;
        self.list_reorder_commit_pending = false;
        self.adaptive_item_heights.clear();
        self.adaptive_height_cache_key = None;
        self.invalidate_list_layout_cache();
        self.list_scroll_handle = VirtualListScrollHandle::new();
        self.list_heatmap.replace(Vec::new());
        self.heat_scrollbar = HeatScrollbarState::default();
        self.list_smooth_scroll.reset();
        self.emoji_smooth_scroll.reset();
        self.symbol_smooth_scroll.reset();
        let generation = self.query_generation;
        if self.active_tab == TabId::Emoji {
            self.items.clear();
            self.loaded_items.clear();
            self.list_order_ids.clear();
            self.list_order_positions.clear();
            self.multi_selected_item_ids.clear();
            self.multi_selection_anchor = None;
            self.list_item_load_animations.clear();
            self.clear_image_cache();
            self.clear_file_list_cache();
            self.total_item_count = 0;
            self.selected_item_id = None;
            self.has_more_items = false;
            self.is_loading_more = false;
            self.pending_selection_index = None;
            self.loading_page_range = None;
            self.list_status = ListStatus::Empty;
            cx.notify();
            return;
        }

        self.list_status = ListStatus::Loading;
        self.items.clear();
        self.loaded_items.clear();
        self.list_order_ids.clear();
        self.list_order_positions.clear();
        self.multi_selected_item_ids.clear();
        self.multi_selection_anchor = None;
        self.list_item_load_animations.clear();
        self.clear_image_cache();
        self.clear_file_list_cache();
        self.total_item_count = 0;
        self.selected_item_id = None;
        self.has_more_items = false;
        self.is_loading_more = false;
        self.pending_selection_index = None;
        self.loading_page_range = None;
        let policy = crate::config_runtime::history_config(cx);
        let query = self.page_query(0, INITIAL_PAGE_SIZE);
        let active_tab = self.active_tab;
        let history_repository = self.history_repository.clone();
        let favorite_repository = self.favorite_repository.clone();
        let query_task = cx.background_spawn(async move {
            let snapshot = if active_tab == TabId::Favorites {
                favorite_repository.ordered_snapshot(&query)?
            } else {
                history_repository.ordered_snapshot_scoped(&query, &policy)?
            };
            let initial_ids = snapshot
                .ids
                .iter()
                .take(INITIAL_PAGE_SIZE.saturating_add(1))
                .copied()
                .collect::<Vec<_>>();
            let records = if active_tab == TabId::Favorites {
                favorite_repository.query_by_ids(&query, &initial_ids)?
            } else {
                history_repository.query_by_ids_scoped(&query, &initial_ids, &policy)?
            };
            Ok((snapshot, records))
        });
        self.list_query_task = Some(cx.spawn(async move |this, cx| {
            let result = query_task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| {
                    if this.accept_query_result(generation, result) {
                        cx.notify();
                    }
                });
            }
        }));
        cx.notify();
    }

    pub(crate) fn handle_list_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 滚动可能导致工具栏隐藏，残留拖拽状态需清除。
        self.floating_toolbar_drag = None;
        if self.list_smooth_scroll.handle_wheel(
            &self.list_scroll_handle,
            event,
            window,
            crate::config_runtime::animation_enabled(
                cx,
                crate::config::AnimationKind::SmoothScroll,
            ),
        ) {
            cx.stop_propagation();
            cx.notify();
        }
    }

    pub(crate) fn handle_emoji_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.emoji_smooth_scroll.handle_wheel(
            &self.emoji_scroll_handle,
            event,
            window,
            crate::config_runtime::animation_enabled(
                cx,
                crate::config::AnimationKind::SmoothScroll,
            ),
        ) {
            cx.stop_propagation();
            cx.notify();
        }
    }

    pub(crate) fn handle_symbol_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.symbol_smooth_scroll.handle_wheel(
            &self.symbol_scroll_handle,
            event,
            window,
            crate::config_runtime::animation_enabled(
                cx,
                crate::config::AnimationKind::SmoothScroll,
            ),
        ) {
            cx.stop_propagation();
            cx.notify();
        }
    }

    pub(crate) fn handle_gallery_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.symbol_smooth_scroll.handle_wheel(
            &self.gallery_scroll_handle,
            event,
            window,
            crate::config_runtime::animation_enabled(
                cx,
                crate::config::AnimationKind::SmoothScroll,
            ),
        ) {
            cx.stop_propagation();
            cx.notify();
        }
    }

    pub(crate) fn advance_scroll_animation(
        &mut self,
        window: &mut Window,
        animations_enabled: bool,
    ) {
        if !animations_enabled {
            self.list_smooth_scroll.reset();
            self.emoji_smooth_scroll.reset();
            self.symbol_smooth_scroll.reset();
            return;
        }
        self.list_smooth_scroll
            .advance(&self.list_scroll_handle, window);
        self.emoji_smooth_scroll
            .advance(&self.emoji_scroll_handle, window);
        if self.symbol_category == SymbolCategoryId::Gallery {
            self.symbol_smooth_scroll
                .advance(&self.gallery_scroll_handle, window);
        } else {
            self.symbol_smooth_scroll
                .advance(&self.symbol_scroll_handle, window);
        }
    }
}
