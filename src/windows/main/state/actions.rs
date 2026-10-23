use super::*;
use crate::services::paste::PasteOrderTarget;

fn paste_order_target(source_tab: TabId, cx: &gpui::App) -> Option<PasteOrderTarget> {
    let clipboard = crate::config_runtime::clipboard_config(cx);
    match source_tab {
        TabId::Clipboard if clipboard.move_pasted_to_first_in_clipboard => {
            Some(PasteOrderTarget::Clipboard)
        }
        TabId::Favorites if clipboard.move_pasted_to_first_in_favorites => {
            Some(PasteOrderTarget::Favorites)
        }
        _ => None,
    }
}

impl MainWindow {
    fn refresh_after_paste(
        &mut self,
        source_tab: TabId,
        history_id: HistoryItemId,
        reordered: bool,
        cx: &mut Context<Self>,
    ) {
        if !reordered
            && self.active_tab == source_tab
            && matches!(self.list_status, ListStatus::Ready)
            && self.history_query().paste_filter.is_none()
        {
            self.refresh_loaded_list_item(source_tab, history_id, cx);
        } else {
            self.refresh_list_item_for_tab(source_tab, history_id, cx);
        }
    }

    pub(crate) fn paste_history_index(&mut self, index: usize, cx: &mut Context<Self>) {
        let order_ids = self.list_order_ids.clone();
        let active_tab = self.active_tab;
        let include_pinned = crate::config_runtime::shortcuts_config(cx)
            .quick_paste_1_to_9
            .include_pinned;
        let history_query = self.history_query();
        let history_repository = self.history_repository.clone();
        let paste_service = self.paste_service.clone();
        cx.defer(move |cx| {
            crate::windows::main::hide_main_window(cx);
            if let Err(error) = crate::services::focus::restore_previous_focus() {
                tracing::error!("快捷粘贴前恢复窗口焦点失败：{error}");
            }
            let history_config = crate::config_runtime::history_config(cx);
            let move_to_first = paste_order_target(TabId::Clipboard, cx);
            let paste_task = cx.background_spawn(async move {
                let mut query = if active_tab == TabId::Clipboard && !order_ids.is_empty() {
                    history_query
                } else {
                    crate::services::database::query::HistoryQuery::default()
                };
                query.include_pinned = include_pinned;
                let order_ids =
                    if active_tab == TabId::Clipboard && !order_ids.is_empty() && include_pinned {
                        order_ids
                    } else {
                        history_repository
                            .ordered_ids_scoped(&query, &history_config)
                            .map_err(|error| error.to_string())?
                    };
                let Some(history_id) = order_ids.get(index).copied() else {
                    return Ok::<(), String>(());
                };
                let item_id = history_repository
                    .item_ids_for_history_ids(&[history_id])
                    .map_err(|error| error.to_string())?
                    .get(&history_id)
                    .copied()
                    .ok_or_else(|| "快捷粘贴项目已不存在".to_owned())?;
                paste_service
                    .paste_item(item_id, move_to_first)
                    .map_err(|error| error.to_string())
            });
            cx.spawn(async move |cx| match paste_task.await {
                Ok(()) => {
                    cx.update(|cx| crate::windows::main::MainWindowService::refresh_history(cx))
                }
                Err(error) => tracing::error!("快捷粘贴第 {} 项失败：{error}", index + 1),
            })
            .detach();
        });
    }

    pub fn paste_item(&mut self, item_id: ClipboardItemId, cx: &mut Context<Self>) {
        let Some(history_id) = self
            .items
            .iter()
            .find(|item| item.item_id == item_id)
            .map(|item| item.history_id)
        else {
            return;
        };
        self.select_item(history_id);
        let source_tab = self.active_tab;
        let move_to_first = paste_order_target(source_tab, cx);

        let paste_service = self.paste_service.clone();
        let this = cx.weak_entity();
        cx.defer(move |cx| {
            crate::windows::main::hide_main_window(cx);
            if let Err(error) = crate::services::focus::restore_previous_focus() {
                tracing::error!("列表粘贴前恢复窗口焦点失败：{error}");
            }

            let paste_task = cx
                .background_spawn(async move { paste_service.paste_item(item_id, move_to_first) });
            cx.spawn(async move |cx| match paste_task.await {
                Ok(()) => {
                    if let Some(this) = this.upgrade() {
                        this.update(cx, |this, cx| {
                            this.refresh_after_paste(
                                source_tab,
                                history_id,
                                move_to_first.is_some(),
                                cx,
                            );
                        });
                    }
                }
                Err(error) => tracing::error!("剪贴板粘贴失败：{error}"),
            })
            .detach();
        });
        cx.notify();
    }

    pub fn paste_item_format(
        &mut self,
        item_id: ClipboardItemId,
        format_namespace: String,
        format_key: String,
        cx: &mut Context<Self>,
    ) {
        let Some(history_id) = self
            .items
            .iter()
            .find(|item| item.item_id == item_id)
            .map(|item| item.history_id)
        else {
            return;
        };
        self.select_item(history_id);
        let source_tab = self.active_tab;
        let move_to_first = paste_order_target(source_tab, cx);
        if let Err(error) = crate::services::focus::restore_previous_focus() {
            tracing::error!("指定格式粘贴前恢复窗口焦点失败：{error}");
        }

        let paste_service = self.paste_service.clone();
        let paste_task = cx.background_spawn(async move {
            paste_service.paste_item_format(item_id, &format_namespace, &format_key, move_to_first)
        });
        cx.spawn(async move |this, cx| match paste_task.await {
            Ok(()) => {
                if let Some(this) = this.upgrade() {
                    this.update(cx, |this, cx| {
                        this.refresh_after_paste(
                            source_tab,
                            history_id,
                            move_to_first.is_some(),
                            cx,
                        );
                    });
                }
            }
            Err(error) => tracing::error!("指定格式粘贴失败：{error}"),
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn paste_selected_item_as_plain_text(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(item) = self.selected_list_item() else {
            return false;
        };
        self.paste_plain_text_item(item.item_id, Some((self.active_tab, item.history_id)), cx);
        true
    }

    pub(crate) fn paste_latest_item_as_plain_text(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = crate::services::focus::restore_previous_focus() {
            tracing::error!("纯文本粘贴前恢复窗口焦点失败：{error}");
        }
        let active_tab = self.active_tab;
        let move_to_first = paste_order_target(TabId::Clipboard, cx);
        let paste_service = self.paste_service.clone();
        let paste_task = cx
            .background_spawn(async move { paste_service.paste_latest_plain_text(move_to_first) });
        cx.spawn(async move |this, cx| match paste_task.await {
            Ok(item_id) => {
                if let Some(this) = this.upgrade() {
                    this.update(cx, |this, cx| {
                        if let Some(list_item_id) = this
                            .items
                            .iter()
                            .find(|item| item.item_id == item_id)
                            .map(|item| item.history_id)
                        {
                            this.refresh_after_paste(
                                active_tab,
                                list_item_id,
                                move_to_first.is_some(),
                                cx,
                            );
                        }
                    });
                }
            }
            Err(error) => {
                tracing::error!("纯文本粘贴失败：{error}");
                if matches!(
                    error,
                    crate::services::paste::PasteError::NoRestorableFormat
                ) {
                    cx.update(|cx| {
                        crate::windows::main::MainWindowService::show_plain_text_paste_unavailable(
                            cx,
                        );
                    });
                }
            }
        })
        .detach();
    }

    fn paste_plain_text_item(
        &mut self,
        item_id: ClipboardItemId,
        refresh: Option<(TabId, HistoryItemId)>,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) = crate::services::focus::restore_previous_focus() {
            tracing::error!("纯文本粘贴前恢复窗口焦点失败：{error}");
        }
        let move_to_first = refresh.and_then(|(source_tab, _)| paste_order_target(source_tab, cx));
        let paste_service = self.paste_service.clone();
        let paste_task = cx.background_spawn(async move {
            paste_service.paste_item_plain_text(item_id, move_to_first)
        });
        cx.spawn(async move |this, cx| match paste_task.await {
            Ok(()) => {
                if let Some((source_tab, history_id)) = refresh
                    && let Some(this) = this.upgrade()
                {
                    this.update(cx, |this, cx| {
                        this.refresh_after_paste(
                            source_tab,
                            history_id,
                            move_to_first.is_some(),
                            cx,
                        );
                    });
                }
            }
            Err(error) => {
                tracing::error!("纯文本粘贴失败：{error}");
                if matches!(
                    error,
                    crate::services::paste::PasteError::NoRestorableFormat
                ) {
                    cx.update(|cx| {
                        crate::windows::main::MainWindowService::show_plain_text_paste_unavailable(
                            cx,
                        );
                    });
                }
            }
        })
        .detach();
        cx.notify();
    }

    pub fn copy_item(&mut self, item_id: ClipboardItemId, cx: &mut Context<Self>) {
        let Some(history_id) = self
            .items
            .iter()
            .find(|item| item.item_id == item_id)
            .map(|item| item.history_id)
        else {
            return;
        };
        self.select_item(history_id);

        let paste_service = self.paste_service.clone();
        let copy_task = cx.background_spawn(async move { paste_service.copy_item(item_id) });
        cx.spawn(async move |_this, cx| match copy_task.await {
            Ok(()) => {
                cx.update(|cx| {
                    crate::windows::notification::NotificationService::show_clipboard_item(
                        item_id, cx,
                    );
                });
            }
            Err(error) => tracing::error!("剪贴板复制失败：{error}"),
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn copy_multi_selected_items(&mut self, cx: &mut Context<Self>) {
        let selected_ids = self.multi_selected_item_ids.clone();
        let active_tab = self.active_tab;
        let history_repository = self.history_repository.clone();
        let favorite_repository = self.favorite_repository.clone();
        let paste_service = self.paste_service.clone();
        let title = i18n::translator(cx).translate("main.multi_selection.copy");
        let success_message = i18n::translator(cx).translate("main.multi_selection.copy_success");
        let failure_message = i18n::translator(cx).translate("main.multi_selection.copy_failed");
        let copy_task = cx.background_spawn(async move {
            let item_ids = resolve_multi_selection_item_ids(
                active_tab,
                &selected_ids,
                &history_repository,
                &favorite_repository,
            )?;
            paste_service
                .copy_merged_items(&item_ids)
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |_this, cx| {
            let result = copy_task.await;
            cx.update(|cx| {
                show_multi_selection_feedback(result, title, success_message, failure_message, cx);
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn paste_multi_selected_items(&mut self, cx: &mut Context<Self>) {
        let selected_ids = self.multi_selected_item_ids.clone();
        let refresh_list_ids = selected_ids.clone();
        let active_tab = self.active_tab;
        let move_to_first = paste_order_target(active_tab, cx);
        let history_repository = self.history_repository.clone();
        let favorite_repository = self.favorite_repository.clone();
        let prepare_service = self.paste_service.clone();
        let paste_service = self.paste_service.clone();
        let title = i18n::translator(cx).translate("main.multi_selection.paste");
        let success_message = i18n::translator(cx).translate("main.multi_selection.paste_success");
        let failure_message = i18n::translator(cx).translate("main.multi_selection.paste_failed");
        let preparation_task = cx.background_spawn(async move {
            let item_ids = resolve_multi_selection_item_ids(
                active_tab,
                &selected_ids,
                &history_repository,
                &favorite_repository,
            )?;
            prepare_service
                .prepare_merged_paste(&item_ids)
                .map(|prepared| (prepared, item_ids))
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| match preparation_task.await {
            Ok((prepared, _item_ids)) => {
                cx.update(|cx| {
                    crate::windows::main::hide_main_window(cx);
                    if let Err(error) = crate::services::focus::restore_previous_focus() {
                        tracing::error!("合并粘贴前恢复窗口焦点失败：{error}");
                    }
                });
                let paste_task = cx.background_spawn(async move {
                    paste_service
                        .paste_prepared_merged_items(prepared, move_to_first)
                        .map_err(|error| error.to_string())
                });
                cx.spawn(async move |cx| {
                    let result = paste_task.await;
                    if result.is_ok() {
                        if let Some(this) = this.upgrade() {
                            this.update(cx, |this, cx| {
                                if this.active_tab != active_tab {
                                    this.refresh_history(cx);
                                } else {
                                    for list_item_id in &refresh_list_ids {
                                        this.refresh_after_paste(
                                            active_tab,
                                            *list_item_id,
                                            move_to_first.is_some(),
                                            cx,
                                        );
                                    }
                                }
                            });
                        }
                    }
                    cx.update(|cx| {
                        show_multi_selection_feedback(
                            result,
                            title,
                            success_message,
                            failure_message,
                            cx,
                        );
                    });
                })
                .detach();
            }
            Err(error) => {
                cx.update(|cx| {
                    show_multi_selection_feedback(
                        Err(error),
                        title,
                        success_message,
                        failure_message,
                        cx,
                    );
                });
            }
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn delete_multi_selected_items(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selected_ids = self.multi_selected_item_ids.clone();
        if selected_ids.is_empty() {
            return;
        }

        match self.active_tab {
            TabId::Clipboard => {
                self.remove_multi_selected_items(selected_ids, TabId::Clipboard, cx);
            }
            TabId::Favorites => {
                if !crate::config_runtime::clipboard_config(cx).confirm_favorite_removal {
                    self.remove_multi_selected_items(selected_ids, TabId::Favorites, cx);
                    return;
                }
                let translator = i18n::translator(cx);
                let title = translator.translate_with_args(
                    "main.confirm.remove_favorites.title",
                    &[("count", selected_ids.len().to_string())],
                );
                let detail = translator.translate("main.confirm.remove_favorite.detail");
                let cancel = gpui::PromptButton::cancel(translator.translate("main.form.cancel"));
                let confirm = gpui::PromptButton::ok(translator.translate("main.form.confirm"));
                let answer = window.prompt(
                    gpui::PromptLevel::Warning,
                    &title,
                    Some(&detail),
                    &[cancel, confirm],
                    cx,
                );
                cx.spawn(async move |this, cx| {
                    if answer.await.unwrap_or(0) != 1 {
                        return;
                    }
                    this.update(cx, |this, cx| {
                        this.remove_multi_selected_items(selected_ids, TabId::Favorites, cx);
                    })
                    .ok();
                })
                .detach();
            }
            TabId::Emoji => {}
        }
    }

    pub(super) fn remove_multi_selected_items(
        &mut self,
        selected_ids: Vec<HistoryItemId>,
        source_tab: TabId,
        cx: &mut Context<Self>,
    ) {
        if selected_ids.is_empty()
            || !crate::windows::main::MainWindowService::begin_blocking_operation(
                if source_tab == TabId::Favorites {
                    "main.multi_selection.remove_favorites"
                } else {
                    "main.multi_selection.delete"
                },
                cx,
            )
        {
            return;
        }
        let history_repository = self.history_repository.clone();
        let favorite_repository = self.favorite_repository.clone();
        let expected_count = selected_ids.len();
        let selected_ids_for_ui = selected_ids.clone();
        let action = if source_tab == TabId::Favorites {
            "取消收藏"
        } else {
            "删除剪贴板历史"
        };
        let task = cx.background_spawn(async move {
            match source_tab {
                TabId::Clipboard => history_repository
                    .delete_many(&selected_ids)
                    .map_err(|error| error.to_string()),
                TabId::Favorites => {
                    let removed = favorite_repository
                        .remove_many_from_favorites(&selected_ids)
                        .map_err(|error| error.to_string())?;
                    Ok(removed)
                }
                TabId::Emoji => Ok(0),
            }
        });
        cx.spawn(async move |this, cx| {
            match task.await {
                Ok(removed_count) => {
                    if removed_count < expected_count {
                        tracing::warn!(
                            "{action}时部分所选内容已不存在：选择 {expected_count} 项，处理 {removed_count} 项"
                        );
                    }
                    this.update(cx, |this, cx| {
                        if this.active_tab == source_tab {
                            if !this.apply_removed_list_items(&selected_ids_for_ui) {
                                this.refresh_history(cx);
                            }
                        }
                    })
                    .ok();
                }
                Err(error) => tracing::error!("批量{action}失败：{error}"),
            }
            cx.update(crate::windows::main::MainWindowService::finish_blocking_operation);
        })
        .detach();
        cx.notify();
    }

    pub fn edit_note(&mut self, item_id: ClipboardItemId, cx: &mut Context<Self>) {
        let Some(item) = self.items.iter().find(|item| item.item_id == item_id) else {
            return;
        };
        let history_id = item.history_id;
        let source_tab = self.active_tab;
        self.select_item(history_id);
        let repository = self.history_repository.note_repository();
        let load_repository = repository.clone();
        let task = cx.background_spawn(async move { load_repository.load(item_id) });
        let main_window = cx.entity();
        cx.spawn(async move |_, cx| match task.await {
            Ok(text) => {
                let result = cx.update(|cx| {
                    crate::windows::preview::PreviewWindowService::hide(cx);
                    crate::windows::main::MainWindowService::cancel_hover_preview(cx);
                    crate::windows::note_editor::open(
                        item_id,
                        text,
                        repository,
                        move |cx| {
                            main_window.update(cx, |this, cx| {
                                this.refresh_list_item_for_tab(source_tab, history_id, cx);
                            })
                        },
                        cx,
                    )
                });
                if let Err(error) = result {
                    tracing::error!("打开备注编辑窗口失败：{error}");
                }
            }
            Err(error) => tracing::error!("读取条目备注失败：{error}"),
        })
        .detach();
        cx.notify();
    }

    /// 打开已有内容的自定义文本编辑窗口。
    pub fn edit_item(&mut self, item_id: ClipboardItemId, cx: &mut Context<Self>) {
        let Some(history_id) = self
            .items
            .iter()
            .find(|item| item.item_id == item_id)
            .map(|item| item.history_id)
        else {
            return;
        };
        self.select_item(history_id);
        let source_tab = self.active_tab;

        let paste_service = self.paste_service.clone();
        let custom_text_service = self.custom_text_service.clone();
        let tag_repository = self.tag_repository.clone();
        let selected_tag_ids = self
            .items
            .iter()
            .find(|item| item.item_id == item_id)
            .map(|item| item.tag_ids.clone())
            .unwrap_or_default();
        let main_window = cx.entity();
        let custom_text_service_for_state = custom_text_service.clone();
        let edit_task = cx.background_spawn(async move {
            let text = paste_service
                .item_plain_text(item_id)
                .map_err(|error| error.to_string())?;
            let can_restore = custom_text_service_for_state
                .can_restore_original(item_id)
                .map_err(|error| error.to_string())?;
            let tags = tag_repository.list().map_err(|error| error.to_string())?;
            Ok::<_, String>((text, can_restore, tags))
        });
        cx.spawn(async move |_this, cx| match edit_task.await {
            Ok((Some(text), can_restore, tags)) => {
                let save_window = main_window.clone();
                let save_service = custom_text_service.clone();
                let on_confirm = Box::new(
                    move |edited_text: String, tag_ids: Vec<i64>, cx: &mut gpui::App| {
                        let save_task = cx.background_executor().spawn(async move {
                            save_service.save_for_item_with_tags(item_id, &edited_text, &tag_ids)
                        });
                        cx.spawn(async move |cx| match save_task.await {
                            Ok(()) => {
                                save_window.update(cx, |this, cx| {
                                    this.refresh_list_item_for_tab(source_tab, history_id, cx);
                                });
                            }
                            Err(error) => tracing::error!("保存自定义文本失败：{error}"),
                        })
                        .detach();
                    },
                );
                let on_restore: Option<Box<dyn FnOnce(&mut gpui::App)>> = if can_restore {
                    let restore_service = custom_text_service.clone();
                    let restore_window = main_window.clone();
                    Some(Box::new(move |cx: &mut gpui::App| {
                        let restore_task = cx
                            .background_executor()
                            .spawn(async move { restore_service.remove_custom_text(item_id) });
                        cx.spawn(async move |cx| match restore_task.await {
                            Ok(()) => {
                                restore_window.update(cx, |this, cx| {
                                    this.refresh_list_item_for_tab(source_tab, history_id, cx);
                                });
                            }
                            Err(error) => tracing::error!("恢复原始内容失败：{error}"),
                        })
                        .detach();
                    }))
                } else {
                    None
                };
                let result = cx.update(|cx| {
                    crate::windows::custom_text_editor::open(
                        text,
                        tags,
                        selected_tag_ids,
                        on_confirm,
                        on_restore,
                        cx,
                    )
                });
                if let Err(error) = result {
                    tracing::error!("打开内容编辑窗口失败：{error}");
                }
            }
            Ok((None, _, _)) => tracing::error!("当前内容没有可编辑的纯文本格式"),
            Err(error) => tracing::error!("读取待编辑文本失败：{error}"),
        })
        .detach();
        cx.notify();
    }

    /// 打开新增文本窗口；确认后创建新的内容实体和历史关系。
    pub fn new_text_item(&mut self, cx: &mut Context<Self>) {
        let tag_repository = self.tag_repository.clone();
        let task = cx.background_spawn(async move { tag_repository.list() });
        cx.spawn(async move |this, cx| match task.await {
            Ok(tags) => {
                if let Some(this) = this.upgrade() {
                    this.update(cx, |this, cx| {
                        this.open_new_text_item_with_tags(tags, cx);
                    });
                }
            }
            Err(error) => tracing::error!("读取新增文本标签失败：{error}"),
        })
        .detach();
    }

    pub(super) fn open_new_text_item_with_tags(&mut self, tags: Vec<Tag>, cx: &mut Context<Self>) {
        let custom_text_service = self.custom_text_service.clone();
        let main_window = cx.entity();
        let on_confirm = Box::new(move |text: String, tag_ids: Vec<i64>, cx: &mut gpui::App| {
            let save_task = cx.background_executor().spawn(async move {
                custom_text_service.create_manual_text_with_tags(&text, &tag_ids)
            });
            cx.spawn(async move |cx| match save_task.await {
                Ok(history_id) => {
                    main_window.update(cx, |this, cx| {
                        if this.active_tab == TabId::Clipboard {
                            this.refresh_new_list_item(history_id, cx);
                        } else {
                            this.refresh_history(cx);
                        }
                    });
                }
                Err(error) => tracing::error!("新增自定义文本失败：{error}"),
            })
            .detach();
        });
        if let Err(error) =
            crate::windows::custom_text_editor::open_new(String::new(), tags, on_confirm, cx)
        {
            tracing::error!("打开新增文本窗口失败：{error}");
        }
    }

    pub(crate) fn observe_window_bounds(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let subscription = cx.observe_window_bounds(window, |this, window, cx| {
            // Windows 只在原生手动缩放结束后保存，窗口动画不参与尺寸持久化。
            if cfg!(target_os = "windows") {
                return;
            }
            let (should_record, generation) =
                cx.read_global::<MainWindowService, _>(|service, _| {
                    (
                        service.window_state.can_record_window_geometry(),
                        service.window_state.visibility_transition_generation(),
                    )
                });
            if !should_record {
                return;
            }

            let size = window.bounds().size;
            let width = f32::from(size.width);
            let height = f32::from(size.height);
            if !(state_runtime::PersistedWindowSize { width, height }).is_valid() {
                return;
            }
            this._window_size_persist_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(WINDOW_SIZE_PERSIST_DELAY)
                    .await;
                if let Some(this) = this.upgrade() {
                    this.update(cx, |this, cx| {
                        this._window_size_persist_task = None;
                        let should_record = cx.read_global::<MainWindowService, _>(|service, _| {
                            service.window_state.can_record_window_geometry()
                                && service.window_state.visibility_transition_generation()
                                    == generation
                        });
                        if should_record {
                            state_runtime::set_main_window_size(
                                state_runtime::PersistedWindowSize { width, height },
                                cx,
                            );
                        }
                    });
                }
            }));
        });
        self._window_bounds_subscription = Some(subscription);
    }

    /// 打开新增收藏窗口；已有编辑器时只聚焦，否则打开手动条目编辑器。
    pub fn new_favorite_item(&mut self, cx: &mut Context<Self>) {
        crate::services::quick_favorite::QuickFavoriteService::open(false, cx);
    }

    /// 使用系统默认程序打开条目：文件类型直接打开路径，图片类型生成临时文件后打开。
    pub fn open_item_with_default(&mut self, item_id: ClipboardItemId, cx: &mut Context<Self>) {
        let Some(history_id) = self
            .items
            .iter()
            .find(|item| item.item_id == item_id)
            .map(|item| item.history_id)
        else {
            return;
        };
        self.select_item(history_id);

        let paste_service = self.paste_service.clone();
        let open_task =
            cx.background_spawn(async move { paste_service.open_item_with_default(item_id) });
        cx.spawn(async move |_this, _cx| {
            if let Err(error) = open_task.await {
                tracing::error!("使用默认程序打开失败：{error}");
            }
        })
        .detach();
        cx.notify();
    }

    /// 在指定搜索引擎中搜索条目的纯文本内容。
    pub fn search_item(
        &mut self,
        item_id: ClipboardItemId,
        engine_index: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(item) = self.items.iter().find(|item| item.item_id == item_id) else {
            return;
        };
        // 多行文本合并为单行，避免 URL 里出现裸换行。
        let query = item
            .preview_text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if query.is_empty() {
            return;
        }
        let engines = crate::config_runtime::clipboard_config(cx).search_engines_or_default();
        let Some(engine) = engines.get(engine_index) else {
            return;
        };
        if !engine.url_template.contains("{query}") {
            tracing::error!("搜索引擎 {} 的地址缺少 {{query}} 占位符", engine.name);
            return;
        }
        let url = engine
            .url_template
            .replace("{query}", &url_encode_query(&query));
        // 走 GPUI 平台层 open_url：后台线程执行 ShellExecute，不在持有实体借用时
        // 阻塞/泵 UI 消息，避免嵌套更新导致 RefCell already borrowed。
        cx.open_url(&url);
        crate::state_runtime::set_last_search_engine_template(
            Some(engine.url_template.clone()),
            cx,
        );
        cx.notify();
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn load_drag_payloads(
        &self,
        item_id: ClipboardItemId,
    ) -> Result<Vec<crate::platform::clipboard::ClipboardPayload>, crate::services::paste::PasteError>
    {
        self.paste_service.load_payloads_for_item(item_id)
    }

    /// 读取条目首个纯文本格式的完整内容，用于右键菜单“在浏览器中打开”解析链接。
    pub(crate) fn item_plain_text(
        &self,
        item_id: ClipboardItemId,
    ) -> Result<Option<String>, crate::services::paste::PasteError> {
        self.paste_service.item_plain_text(item_id)
    }

    pub fn add_to_favorites(&mut self, item_id: ClipboardItemId, cx: &mut Context<Self>) {
        let Some(list_item_id) = self
            .items
            .iter()
            .find(|item| item.item_id == item_id)
            .map(|item| item.history_id)
        else {
            return;
        };
        let source_tab = self.active_tab;
        let favorite_repository = self.favorite_repository.clone();
        let favorite_task =
            cx.background_spawn(async move { favorite_repository.add_to_favorites(item_id) });
        cx.spawn(async move |this, cx| match favorite_task.await {
            Ok(true) => {
                if let Some(this) = this.upgrade() {
                    this.update(cx, |this, cx| {
                        if source_tab == TabId::Clipboard {
                            this.refresh_loaded_list_item(source_tab, list_item_id, cx);
                        } else {
                            this.refresh_list_item_for_tab(source_tab, list_item_id, cx);
                        }
                    });
                }
            }
            Ok(false) => {}
            Err(error) => tracing::error!("添加到收藏失败：{error}"),
        })
        .detach();
    }

    pub fn set_item_pinned(
        &mut self,
        list_item_id: HistoryItemId,
        favorite_id: Option<i64>,
        is_pinned: bool,
        cx: &mut Context<Self>,
    ) {
        let active_tab = self.active_tab;
        let history_repository = self.history_repository.clone();
        let favorite_repository = self.favorite_repository.clone();
        let target_id = if active_tab == TabId::Favorites {
            favorite_id
        } else {
            Some(list_item_id)
        };
        let Some(target_id) = target_id else {
            return;
        };
        let pin_task = cx.background_spawn(async move {
            if active_tab == TabId::Favorites {
                favorite_repository
                    .set_pinned(target_id, is_pinned)
                    .map_err(|error| error.to_string())
            } else {
                history_repository
                    .set_pinned(target_id, is_pinned)
                    .map_err(|error| error.to_string())
            }
        });
        cx.spawn(async move |this, cx| match pin_task.await {
            Ok(true) => {
                if let Some(this) = this.upgrade() {
                    this.update(cx, |this, cx| {
                        this.refresh_pinned_list_item(active_tab, list_item_id, cx);
                    });
                }
            }
            Ok(false) => {}
            Err(error) => tracing::error!("切换置顶状态失败：{error}"),
        })
        .detach();
    }

    pub fn add_to_tag(&mut self, item_id: ClipboardItemId, tag_id: i64, cx: &mut Context<Self>) {
        let list_item_id = self
            .loaded_items
            .values()
            .find(|item| item.item_id == item_id)
            .map(|item| item.history_id);
        let source_tab = self.active_tab;
        let tag_repository = self.tag_repository.clone();
        let tag_task =
            cx.background_spawn(async move { tag_repository.add_to_item(item_id, tag_id) });
        cx.spawn(async move |this, cx| match tag_task.await {
            Ok(true) => {
                if let Some(this) = this.upgrade() {
                    this.update(cx, |this, cx| {
                        if let Some(list_item_id) = list_item_id {
                            if source_tab == TabId::Clipboard {
                                this.refresh_loaded_list_item(source_tab, list_item_id, cx);
                            } else {
                                this.refresh_list_item_for_tab(source_tab, list_item_id, cx);
                            }
                        } else {
                            this.refresh_history(cx);
                        }
                    });
                }
            }
            Ok(false) => {}
            Err(error) => tracing::error!("添加标签失败：{error}"),
        })
        .detach();
    }

    pub fn remove_from_favorites(
        &mut self,
        favorite_id: i64,
        list_item_id: HistoryItemId,
        cx: &mut Context<Self>,
    ) {
        let source_tab = self.active_tab;
        let favorite_repository = self.favorite_repository.clone();
        let remove_task =
            cx.background_spawn(
                async move { favorite_repository.remove_from_favorites(favorite_id) },
            );
        cx.spawn(async move |this, cx| match remove_task.await {
            Ok(true) => {
                if let Some(this) = this.upgrade() {
                    this.update(cx, |this, cx| {
                        if source_tab == TabId::Clipboard {
                            this.refresh_loaded_list_item(source_tab, list_item_id, cx);
                        } else {
                            if !this.apply_removed_list_items(&[list_item_id]) {
                                this.refresh_history(cx);
                            } else {
                                cx.notify();
                            }
                        }
                    });
                }
            }
            Ok(false) => tracing::error!("未找到要取消收藏的收藏：{favorite_id}"),
            Err(error) => tracing::error!("取消收藏失败：{error}"),
        })
        .detach();
    }

    pub fn remove_from_tag(
        &mut self,
        item_id: ClipboardItemId,
        tag_id: i64,
        list_item_id: HistoryItemId,
        cx: &mut Context<Self>,
    ) {
        let source_tab = self.active_tab;
        let tag_repository = self.tag_repository.clone();
        let remove_task =
            cx.background_spawn(async move { tag_repository.remove_from_item(item_id, tag_id) });
        cx.spawn(async move |this, cx| match remove_task.await {
            Ok(true) => {
                if let Some(this) = this.upgrade() {
                    this.update(cx, |this, cx| {
                        if source_tab == TabId::Clipboard {
                            this.refresh_loaded_list_item(source_tab, list_item_id, cx);
                        } else {
                            this.refresh_list_item_for_tab(source_tab, list_item_id, cx);
                        }
                    });
                }
            }
            Ok(false) => {
                tracing::error!("未找到要移除的内容标签关系：内容 {item_id}，标签 {tag_id}")
            }
            Err(error) => tracing::error!("移除内容标签失败：{error}"),
        })
        .detach();
    }

    pub fn delete_item(&mut self, history_id: HistoryItemId, cx: &mut Context<Self>) {
        let source_tab = self.active_tab;
        let history_repository = self.history_repository.clone();
        let delete_task = cx.background_spawn(async move { history_repository.delete(history_id) });
        cx.spawn(async move |this, cx| match delete_task.await {
            Ok(true) => {
                if let Some(this) = this.upgrade() {
                    this.update(cx, |this, cx| {
                        if this.active_tab == source_tab {
                            if !this.apply_removed_list_items(&[history_id]) {
                                this.refresh_history(cx);
                            } else {
                                cx.notify();
                            }
                        } else {
                            this.refresh_history(cx);
                        }
                    });
                }
            }
            Ok(false) => tracing::error!("未找到要删除的剪贴板历史：{history_id}"),
            Err(error) => tracing::error!("剪贴板历史删除失败：{error}"),
        })
        .detach();
    }
}
