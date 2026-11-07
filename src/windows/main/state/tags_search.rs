use super::*;

fn toggle_tag_selection(selected_tag_ids: &mut HashSet<i64>, tag_id: i64, multi_select: bool) {
    let was_selected = selected_tag_ids.remove(&tag_id);
    if !multi_select {
        selected_tag_ids.clear();
    }
    if !was_selected {
        selected_tag_ids.insert(tag_id);
    }
}

impl MainWindow {
    pub(crate) fn refresh_tags(&mut self, cx: &mut Context<Self>) {
        self.tags_query_generation = self.tags_query_generation.wrapping_add(1);
        self.tag_drag_snapshot = None;
        self.tag_drag_source_id = None;
        self.clear_tag_drop_target();
        let generation = self.tags_query_generation;
        self.tags_loading = true;
        self.tags_error = false;
        let repository = self.tag_repository.clone();
        let task = cx.background_spawn(async move { repository.list() });
        cx.spawn(async move |this, cx| match task.await {
            Ok(tags) => {
                this.update(cx, |this, cx| {
                    if generation != this.tags_query_generation {
                        return;
                    }
                    this.tags = tags;
                    this.tags_loading = false;
                    this.tags_error = false;
                    cx.notify();
                })
                .ok();
            }
            Err(error) => {
                this.update(cx, |this, cx| {
                    if generation != this.tags_query_generation {
                        return;
                    }
                    this.tags_loading = false;
                    tracing::error!("标签加载失败：{error}");
                    this.tags_error = true;
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn is_tag_drag_source(&self, tag_id: i64) -> bool {
        self.tag_drag_source_id == Some(tag_id)
    }

    pub(crate) fn finish_tag_drag_preview(&mut self, cx: &mut Context<Self>) {
        if self.cancel_tag_reorder_preview() {
            cx.notify();
        }
    }

    pub(crate) fn begin_tag_reorder_drag(&mut self, source_id: i64) {
        if !self.tags.iter().any(|tag| tag.id == source_id) {
            return;
        }
        self.tag_drag_source_id = Some(source_id);
        self.tag_hovered_id = None;
        self.tag_drop_target = None;
        if self.tag_drag_snapshot.is_none() {
            self.tag_drag_snapshot = Some(TagDragSnapshot {
                tags: self.tags.clone(),
            });
        }
    }

    pub(crate) fn clear_tag_drop_target(&mut self) {
        self.tag_drop_target = None;
        self.tag_hovered_id = None;
        self.tag_reorder_animation = None;
    }

    pub(crate) fn clear_tag_drag_hover(&mut self, tag_id: i64) {
        if self.tag_hovered_id == Some(tag_id) {
            self.tag_hovered_id = None;
        }
    }

    pub(crate) fn preview_reorder_tag(&mut self, target: TagDropTarget) -> bool {
        if target.source_id == target.target_id
            || self.tag_hovered_id == Some(target.target_id)
            || !self.tags.iter().any(|tag| tag.id == target.source_id)
            || !self.tags.iter().any(|tag| tag.id == target.target_id)
        {
            return false;
        }
        if self.tag_drag_snapshot.is_none() {
            self.begin_tag_reorder_drag(target.source_id);
        }

        let mut order_ids = self.tags.iter().map(|tag| tag.id).collect::<Vec<_>>();
        if !move_id_to_target(&mut order_ids, target.source_id, target.target_id) {
            return false;
        }
        reorder_tags_by_ids(&mut self.tags, &order_ids);
        self.tag_reorder_animation_generation =
            self.tag_reorder_animation_generation.wrapping_add(1);
        self.tag_reorder_animation = Some(TagReorderAnimation {
            generation: self.tag_reorder_animation_generation,
            start_positions: self.tag_chip_positions.borrow().clone(),
        });
        self.tag_hovered_id = Some(target.target_id);
        self.tag_drop_target = Some(target);
        true
    }

    pub(crate) fn cancel_tag_reorder_preview(&mut self) -> bool {
        let Some(snapshot) = self.tag_drag_snapshot.take() else {
            self.tag_drag_source_id = None;
            self.clear_tag_drop_target();
            return false;
        };
        self.tags = snapshot.tags;
        self.tag_drag_source_id = None;
        self.clear_tag_drop_target();
        true
    }

    pub(super) fn finish_tag_reorder_preview(&mut self) {
        self.tag_drag_snapshot = None;
        self.tag_drag_source_id = None;
        self.clear_tag_drop_target();
    }

    pub(crate) fn commit_tag_reorder_preview(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.tag_drop_target else {
            self.cancel_tag_reorder_preview();
            return;
        };
        let preview_changed = self.tag_drag_snapshot.as_ref().is_some_and(|snapshot| {
            snapshot
                .tags
                .iter()
                .map(|tag| tag.id)
                .ne(self.tags.iter().map(|tag| tag.id))
        });
        if preview_changed {
            let target_id = self.tag_reorder_target_for_preview(target.source_id);
            self.reorder_tag(target.source_id, target_id, cx);
        } else {
            self.cancel_tag_reorder_preview();
        }
    }

    pub(super) fn tag_reorder_target_for_preview(&self, source_id: i64) -> i64 {
        let Some(snapshot) = self.tag_drag_snapshot.as_ref() else {
            return source_id;
        };
        let original_position = snapshot
            .tags
            .iter()
            .map(|tag| tag.id)
            .position(|id| id == source_id)
            .unwrap_or(0);
        let current_order = self.tags.iter().map(|tag| tag.id).collect::<Vec<_>>();
        let preview_position = current_order
            .iter()
            .position(|id| *id == source_id)
            .unwrap_or(original_position);
        if preview_position > original_position {
            current_order
                .get(preview_position - 1)
                .copied()
                .unwrap_or(source_id)
        } else if preview_position < original_position {
            current_order
                .get(preview_position + 1)
                .copied()
                .unwrap_or(source_id)
        } else {
            source_id
        }
    }

    pub(super) fn reorder_tag(&mut self, source_id: i64, target_id: i64, cx: &mut Context<Self>) {
        self.clear_tag_drop_target();
        if source_id == target_id {
            self.cancel_tag_reorder_preview();
            return;
        }
        let repository = self.tag_repository.clone();
        let task = cx.background_spawn(async move {
            repository
                .reorder(source_id, target_id)
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| match task.await {
            Ok(true) => {
                if let Some(this) = this.upgrade() {
                    this.update(cx, |this, cx| {
                        this.finish_tag_reorder_preview();
                        cx.notify();
                    });
                }
            }
            Ok(false) => {
                if let Some(this) = this.upgrade() {
                    this.update(cx, |this, cx| {
                        if this.cancel_tag_reorder_preview() {
                            cx.notify();
                        }
                    });
                }
            }
            Err(error) => {
                tracing::error!("标签排序失败：{error}");
                if let Some(this) = this.upgrade() {
                    this.update(cx, |this, cx| {
                        if this.cancel_tag_reorder_preview() {
                            cx.notify();
                        }
                    });
                }
            }
        })
        .detach();
    }

    pub(crate) fn select_tag(&mut self, tag_id: i64, cx: &mut Context<Self>) {
        toggle_tag_selection(
            &mut self.selected_tag_ids,
            tag_id,
            crate::config_runtime::clipboard_config(cx).tags_multi_select,
        );
        self.refresh_history(cx);
    }

    pub(crate) fn select_tab(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        if self.active_tab == tab_id {
            return;
        }
        self.suppress_tab_bar_hover(tab_id.layout_button());
        if self.is_gallery_active() {
            self.clear_gallery_resources();
        }
        if self.split_panes.is_some() {
            if tab_id == TabId::Emoji {
                self.cancel_split_previews(cx);
                self.active_tab = tab_id;
                self.persist_state(cx);
                cx.notify();
            } else {
                self.activate_split_pane(tab_id, cx);
            }
            return;
        }
        self.cancel_split_previews(cx);
        self.cancel_hover_preview(cx);
        self.list_smooth_scroll.reset();
        self.emoji_smooth_scroll.reset();
        self.symbol_smooth_scroll.reset();
        if crate::config_runtime::clipboard_config(cx).clear_tags_on_tab_switch
            && matches!(tab_id, TabId::Clipboard | TabId::Favorites)
        {
            self.selected_tag_ids.clear();
        }
        self.active_tab = tab_id;
        self.filters.clear_hovered();
        self.persist_state(cx);
        self.refresh_history(cx);
    }

    pub(crate) fn select_symbol_category(
        &mut self,
        category: SymbolCategoryId,
        cx: &mut Context<Self>,
    ) {
        if self.symbol_category == category {
            return;
        }
        self.suppress_tab_bar_hover(category.layout_button());
        if self.is_gallery_active() && category != SymbolCategoryId::Gallery {
            self.clear_gallery_resources();
        }
        self.emoji_smooth_scroll.reset();
        self.symbol_smooth_scroll.reset();
        self.symbol_category = category;
        self.persist_state(cx);
        cx.notify();
    }

    pub(super) fn persist_state(&self, cx: &mut Context<Self>) {
        let mut state = state_runtime::state(cx);
        state.main_window.active_tab = self.active_tab;
        state.main_window.symbol_category = self.symbol_category;
        state.main_window.gallery_sort = self.gallery_sort;
        if let Err(error) = state_runtime::set_state(state, cx) {
            tracing::error!("保存界面状态失败：{error}");
        }
    }

    pub(super) fn persist_symbol_history(&self, cx: &mut Context<Self>) {
        let mut state = state_runtime::state(cx);
        state.main_window.recent_emojis = self.recent_emojis.clone();
        state.main_window.recent_symbols = self.recent_symbols.clone();
        if let Err(error) = state_runtime::set_state(state, cx) {
            tracing::error!("保存表情/符号历史失败：{error}");
        }
    }

    pub(crate) fn select_symbol_group(
        &mut self,
        group: SymbolGroupId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.symbol_search_query.is_empty() {
            self.symbol_search_query.clear();
            if let Some(input) = self.symbol_search_input.clone() {
                input.update(cx, |input, cx| input.set_value("", window, cx));
            }
        }
        self.symbol_group = group;
        self.symbol_scroll_target = Some(group);
        self.symbol_smooth_scroll.reset();
        cx.notify();
    }

    pub(crate) fn select_emoji_category(
        &mut self,
        category: EmojiCategoryId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.symbol_search_query.is_empty() {
            self.symbol_search_query.clear();
            if let Some(input) = self.symbol_search_input.clone() {
                input.update(cx, |input, cx| input.set_value("", window, cx));
            }
        }
        self.emoji_smooth_scroll.reset();
        self.symbol_smooth_scroll.reset();
        self.emoji_category = category;
        self.emoji_scroll_target = Some(category);
        cx.notify();
    }

    pub(crate) fn paste_emoji(&mut self, emoji: String, cx: &mut Context<Self>) {
        let paste_service = self.paste_service.clone();
        let paste_value = emoji.clone();
        let this = cx.weak_entity();
        cx.defer(move |cx| {
            crate::windows::main::prepare_paste_target("粘贴表情", cx);

            let paste_task =
                cx.background_spawn(async move { paste_service.paste_text(&paste_value) });
            cx.spawn(async move |cx| match paste_task.await {
                Ok(()) => {
                    if let Some(this) = this.upgrade() {
                        this.update(cx, |this, cx| {
                            this.recent_emojis.retain(|recent| recent != &emoji);
                            this.recent_emojis.insert(0, emoji);
                            this.recent_emojis.truncate(40);
                            this.persist_symbol_history(cx);
                            cx.notify();
                        });
                    }
                }
                Err(error) => tracing::error!("粘贴表情失败：{error}"),
            })
            .detach();
        });
    }

    pub(crate) fn paste_symbol(&mut self, symbol: String, cx: &mut Context<Self>) {
        let paste_service = self.paste_service.clone();
        let paste_value = symbol.clone();
        let this = cx.weak_entity();
        cx.defer(move |cx| {
            crate::windows::main::prepare_paste_target("粘贴符号", cx);

            let paste_task =
                cx.background_spawn(async move { paste_service.paste_text(&paste_value) });
            cx.spawn(async move |cx| match paste_task.await {
                Ok(()) => {
                    if let Some(this) = this.upgrade() {
                        this.update(cx, |this, cx| {
                            this.recent_symbols.retain(|recent| recent != &symbol);
                            this.recent_symbols.insert(0, symbol);
                            this.recent_symbols.truncate(40);
                            this.persist_symbol_history(cx);
                            cx.notify();
                        });
                    }
                }
                Err(error) => tracing::error!("粘贴符号失败：{error}"),
            })
            .detach();
        });
    }

    pub(crate) fn add_tag(&mut self, tag: Tag, cx: &mut Context<Self>) {
        self.tags_query_generation = self.tags_query_generation.wrapping_add(1);
        self.tags_loading = false;
        self.tags_error = false;
        self.tags.retain(|existing| existing.id != tag.id);
        self.tags.push(tag);
        sort_tags(&mut self.tags);
        cx.notify();
    }

    pub(crate) fn save_tag(&mut self, tag: Tag, cx: &mut Context<Self>) {
        if self.tags.iter().any(|existing| existing.id == tag.id) {
            self.update_tag(tag, cx);
        } else {
            self.add_tag(tag, cx);
        }
    }

    pub(crate) fn update_tag(&mut self, tag: Tag, cx: &mut Context<Self>) {
        self.tags_query_generation = self.tags_query_generation.wrapping_add(1);
        self.tags_loading = false;
        self.tags_error = false;
        self.tags.retain(|existing| existing.id != tag.id);
        self.tags.push(tag);
        sort_tags(&mut self.tags);
        cx.notify();
    }

    pub(crate) fn remove_tags(&mut self, tag_ids: &[i64], cx: &mut Context<Self>) {
        if tag_ids.is_empty() {
            return;
        }

        let removed_ids: HashSet<i64> = tag_ids.iter().copied().collect();
        self.tags_query_generation = self.tags_query_generation.wrapping_add(1);
        self.tags.retain(|tag| !removed_ids.contains(&tag.id));
        let selection_changed = self
            .selected_tag_ids
            .iter()
            .any(|tag_id| removed_ids.contains(tag_id));
        self.selected_tag_ids
            .retain(|tag_id| !removed_ids.contains(tag_id));
        if selection_changed {
            self.refresh_history(cx);
        } else {
            cx.notify();
        }
    }

    pub(crate) fn ensure_tags_search_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tags_search_input.is_some() {
            return;
        }

        let translator = i18n::translator(cx);
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(translator.translate("main.tags.search_placeholder"))
        });
        let subscription =
            cx.subscribe_in(
                &input,
                window,
                |this, state, event, window, cx| match event {
                    InputEvent::Change => {
                        this.tags_search_query = state.read(cx).value().to_string();
                        cx.notify();
                    }
                    InputEvent::Focus => {
                        this.cancel_tags_panel_close();
                        if let Err(error) = crate::services::focus::focus_input_window(window) {
                            tracing::error!("标签搜索框获得焦点时激活窗口失败：{error}");
                            window.blur(cx);
                            return;
                        }
                        let focus_handle = state.focus_handle(cx);
                        window.defer(cx, move |window, cx| {
                            if focus_handle.is_focused(window) {
                                focus_handle.dispatch_action(&input::SelectAll, window, cx);
                            }
                        });
                        cx.notify();
                    }
                    InputEvent::Blur => {
                        if let Err(error) =
                            crate::services::focus::restore_previous_focus_for(window)
                        {
                            tracing::error!("标签搜索框失去焦点时恢复窗口焦点失败：{error}");
                        }
                        this.update_tags_panel_auto_close(window, cx);
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } => {}
                },
            );
        self.tags_search_input = Some(input);
        self.tags_search_input_locale = Some(translator.locale().clone());
        self._tags_search_input_subscription = Some(subscription);
    }

    pub(crate) fn ensure_search_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let is_vertical = crate::config_runtime::appearance_config(cx)
            .titlebar_position
            .is_vertical();
        if self.search_input.is_some() && self.search_input_vertical == Some(is_vertical) {
            return;
        }

        let translator = i18n::translator(cx);
        let previous_input = self.search_input.clone();
        let previous_value = previous_input
            .as_ref()
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_else(|| self.search_query.clone());
        let input_was_focused = previous_input
            .as_ref()
            .is_some_and(|input| input.focus_handle(cx).is_focused(window));
        let placeholder = translator.translate("main.search.placeholder");
        let placeholder = if is_vertical {
            String::new()
        } else {
            placeholder
        };
        let input = cx.new(|cx| {
            let input = InputState::new(window, cx).placeholder(placeholder);
            if is_vertical { input } else { input }
        });
        if !previous_value.is_empty() {
            let previous_value = previous_value.clone();
            input.update(cx, |input, cx| input.set_value(previous_value, window, cx));
        }
        let subscription = cx.subscribe_in(
            &input,
            window,
            move |this, state, event, window, cx| match event {
                InputEvent::Change => {
                    this.search_query = state.read(cx).value().to_string();
                    this.schedule_search_refresh(cx);
                    cx.notify();
                }
                InputEvent::Focus => {
                    if let Err(error) = crate::services::focus::focus_input_window(window) {
                        tracing::error!("输入框获得焦点时激活窗口失败：{error}");
                        window.blur(cx);
                        return;
                    }
                    this.search_expanded = true;
                    let focus_handle = state.focus_handle(cx);
                    window.defer(cx, move |window, cx| {
                        if focus_handle.is_focused(window) {
                            focus_handle.dispatch_action(&input::SelectAll, window, cx);
                        }
                    });
                    cx.notify();
                }
                InputEvent::Blur => {
                    if let Err(error) = crate::services::focus::restore_previous_focus_for(window) {
                        tracing::error!("输入框失去焦点时恢复窗口焦点失败：{error}");
                    }
                    if this.search_expanded {
                        this.search_expanded = !state.read(cx).value().trim().is_empty();
                    }
                    cx.notify();
                }
                InputEvent::PressEnter { .. } if is_vertical => {
                    let value = state.read(cx).value().to_string();
                    if value.contains('\n') {
                        state.update(cx, |input, cx| {
                            input.set_value(value.replace('\n', ""), window, cx)
                        });
                    }
                }
                InputEvent::PressEnter { .. } => {}
            },
        );
        let activation_subscription = cx.observe_window_activation(window, |_, window, cx| {
            if window.is_window_active() {
                // A file restored while the window was already open is only observed
                // when a scan runs; refresh on focus so recovery reaches the list.
                // Activation can fire while MainWindow is still being updated, so
                // defer the scan until the entity is returned to the app.
                cx.defer(|cx| {
                    crate::windows::main::MainWindowService::request_missing_scan(cx);
                });
                return;
            }
            window.blur(cx);
            if let Err(error) = crate::services::focus::clear_previous_focus_for(window) {
                tracing::error!("窗口失焦时清理窗口焦点失败：{error}");
            }
        });
        self.search_input = Some(input);
        self.search_input_locale = Some(translator.locale().clone());
        self.search_input_vertical = Some(is_vertical);
        self.search_query = previous_value;
        self._search_input_subscription = Some(subscription);
        self._window_activation_subscription = Some(activation_subscription);
        if input_was_focused {
            let input = self.search_input.clone().expect("搜索输入框状态尚未初始化");
            window.defer(cx, move |window, cx| {
                input.update(cx, |input, cx| input.focus(window, cx));
            });
        }
    }

    pub(crate) fn focus_search_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_search_input(window, cx);
        let Some(search_input) = self.search_input.clone() else {
            return;
        };
        if let Err(error) = crate::services::focus::focus_input_window(window) {
            tracing::error!("聚焦搜索框时激活窗口失败：{error}");
            return;
        }
        self.search_expanded = true;
        window.defer(cx, move |window, cx| {
            search_input.update(cx, |input, cx| input.focus(window, cx));
        });
        cx.notify();
    }

    pub(crate) fn clear_search_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_search_input(window, cx);
        let Some(search_input) = self.search_input.clone() else {
            return;
        };
        let has_query = !search_input.read(cx).value().is_empty();
        if has_query {
            search_input.update(cx, |input, cx| input.set_value("", window, cx));
            self.search_query.clear();
            self.schedule_search_refresh(cx);
        }
    }

    pub(crate) fn prepare_window_shown(&mut self, cx: &mut Context<Self>) {
        if let Some(panes) = self.split_panes.clone() {
            for pane in panes {
                pane.update(cx, |pane, cx| {
                    pane.prepare_window_shown(cx);
                    cx.notify();
                });
            }
        }
        let clipboard = crate::config_runtime::clipboard_config(cx);
        if clipboard.auto_scroll_to_top && self.active_tab != TabId::Emoji {
            // 显示动画首帧时窗口仍是折叠高度，延迟的 scroll_to_item 会按过小的视口对齐首项底部，
            // 动画结束后再回到顶部时产生可见滚动；直接设置偏移不依赖视口尺寸。
            self.list_scroll_handle
                .set_offset(gpui::point(gpui::px(0.0), gpui::px(0.0)));
            self.list_smooth_scroll.reset();
            self.selected_item_id = self.list_order_ids.first().copied();
            self.multi_selected_item_ids.clear();
            self.multi_selection_anchor = None;
            self.reconcile_selection();
        }
    }

    pub(crate) fn handle_window_shown(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prepare_window_shown(cx);
        let clipboard = crate::config_runtime::clipboard_config(cx);
        if clipboard.auto_clear_search {
            self.clear_search_input(window, cx);
        }
        if clipboard.auto_focus_search {
            self.focus_search_input(window, cx);
        }
        cx.notify();
    }

    pub(crate) fn toggle_search_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input_focused = self
            .search_input
            .as_ref()
            .is_some_and(|input| input.focus_handle(cx).is_focused(window));
        let has_query = !self.search_query.trim().is_empty();

        if self.search_expanded && !has_query {
            self.search_expanded = false;
            window.blur(cx);
        } else if self.search_expanded && input_focused {
            window.blur(cx);
        } else {
            self.focus_search_input(window, cx);
            return;
        }
        cx.notify();
    }

    pub(crate) fn sync_search_placeholder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let translator = i18n::translator(cx);
        if self.search_input_locale.as_ref() == Some(translator.locale()) {
            return;
        }
        if let Some(input) = self.search_input.clone() {
            let placeholder = translator.translate("main.search.placeholder");
            let placeholder = if self.search_input_vertical == Some(true) {
                String::new()
            } else {
                placeholder
            };
            input.update(cx, |input, cx| {
                input.set_placeholder(placeholder, window, cx)
            });
        }
        self.search_input_locale = Some(translator.locale().clone());
    }

    pub(crate) fn ensure_symbol_search_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.symbol_search_input.is_some() {
            return;
        }
        let translator = i18n::translator(cx);
        let placeholder_key = if self.symbol_category == SymbolCategoryId::Gallery {
            "main.gallery.search_placeholder"
        } else {
            "main.symbol.search_placeholder"
        };
        let placeholder = translator.translate(placeholder_key);
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder.clone()));
        let subscription =
            cx.subscribe_in(
                &input,
                window,
                |this, state, event, window, cx| match event {
                    InputEvent::Change => {
                        this.symbol_search_query = state.read(cx).value().to_string();
                        cx.notify();
                    }
                    InputEvent::Focus => {
                        if let Err(error) = crate::services::focus::focus_input_window(window) {
                            tracing::error!("符号搜索框获得焦点时激活窗口失败：{error}");
                            window.blur(cx);
                            return;
                        }
                        let focus_handle = state.focus_handle(cx);
                        window.defer(cx, move |window, cx| {
                            if focus_handle.is_focused(window) {
                                focus_handle.dispatch_action(&input::SelectAll, window, cx);
                            }
                        });
                        cx.notify();
                    }
                    InputEvent::Blur => {
                        if let Err(error) =
                            crate::services::focus::restore_previous_focus_for(window)
                        {
                            tracing::error!("符号搜索框失去焦点时恢复窗口焦点失败：{error}");
                        }
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } => {}
                },
            );
        self.symbol_search_input = Some(input);
        self.symbol_search_input_placeholder = Some(placeholder);
        self._symbol_search_input_subscription = Some(subscription);
    }

    pub(crate) fn sync_symbol_search_placeholder(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let translator = i18n::translator(cx);
        let placeholder_key = if self.symbol_category == SymbolCategoryId::Gallery {
            "main.gallery.search_placeholder"
        } else {
            "main.symbol.search_placeholder"
        };
        let placeholder = translator.translate(placeholder_key);
        if self.symbol_search_input_placeholder.as_ref() == Some(&placeholder) {
            return;
        }
        if let Some(input) = self.symbol_search_input.clone() {
            input.update(cx, |input, cx| {
                input.set_placeholder(placeholder.clone(), window, cx)
            });
        }
        self.symbol_search_input_placeholder = Some(placeholder);
    }

    pub(crate) fn sync_tags_search_placeholder(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let translator = i18n::translator(cx);
        if self.tags_search_input_locale.as_ref() == Some(translator.locale()) {
            return;
        }
        if let Some(input) = self.tags_search_input.clone() {
            let placeholder = translator.translate("main.tags.search_placeholder");
            input.update(cx, |input, cx| {
                input.set_placeholder(placeholder, window, cx)
            });
        }
        self.tags_search_input_locale = Some(translator.locale().clone());
    }

    pub(super) fn schedule_search_refresh(&mut self, cx: &mut Context<Self>) {
        self.query_generation = self.query_generation.wrapping_add(1);
        let generation = self.query_generation;
        self._search_debounce_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE_DELAY).await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| {
                    if this.query_generation == generation {
                        this.refresh_history(cx);
                    }
                });
            }
        }));
    }

    pub(crate) fn prepare_adaptive_height_cache(
        &mut self,
        key: AdaptiveHeightCacheKey,
        enabled: bool,
    ) {
        if !enabled {
            self.adaptive_item_heights.clear();
            self.adaptive_height_cache_key = None;
            return;
        }

        if self.adaptive_height_cache_key != Some(key) {
            self.adaptive_item_heights.clear();
            self.adaptive_height_cache_key = Some(key);
        }
    }

    pub(crate) fn cancel_tags_panel_close(&mut self) {
        self.tags_panel_close_task = None;
    }

    fn can_auto_close_tags_panel(&self, window: &Window, cx: &Context<Self>) -> bool {
        let search_focused = self
            .tags_search_input
            .as_ref()
            .is_some_and(|input| input.focus_handle(cx).is_focused(window));
        if search_focused
            || self.tags_panel_pinned
            || !self.tags_panel_open
            || self.tags_panel_resize_start.is_some()
        {
            return false;
        }
        let pointer_inside = window.is_window_hovered()
            && [self.tags_panel_bounds, self.tags_button_bounds]
                .into_iter()
                .flatten()
                .any(|bounds| bounds.contains(&window.mouse_position()));
        !pointer_inside && !cx.has_active_drag()
    }

    pub(crate) fn update_tags_panel_auto_close(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.can_auto_close_tags_panel(window, cx) {
            self.schedule_tags_panel_close(window, cx);
        } else {
            self.cancel_tags_panel_close();
        }
    }

    pub(crate) fn toggle_tags_panel_pin(&mut self) {
        self.tags_panel_pinned = !self.tags_panel_pinned;
        self.cancel_tags_panel_close();
    }

    pub(crate) fn toggle_sidebar_collapsed(&mut self) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
    }

    pub(crate) fn toggle_tags_match_mode(&mut self, cx: &mut Context<Self>) {
        self.tags_match_all = !self.tags_match_all;
        if !self.selected_tag_ids.is_empty() {
            self.refresh_history(cx);
        } else {
            cx.notify();
        }
    }

    pub(crate) fn toggle_tags_multi_select(&mut self, cx: &mut Context<Self>) {
        let mut clipboard = crate::config_runtime::clipboard_config(cx);
        clipboard.tags_multi_select = !clipboard.tags_multi_select;
        let multi_select = clipboard.tags_multi_select;
        if let Err(error) = crate::config_runtime::set_clipboard_config(clipboard, cx) {
            tracing::error!("保存标签选择模式失败：{error}");
            return;
        }
        if !multi_select && self.selected_tag_ids.len() > 1 {
            let retained_tag = self
                .tags
                .iter()
                .find(|tag| self.selected_tag_ids.contains(&tag.id))
                .map(|tag| tag.id);
            self.selected_tag_ids.retain(|id| Some(*id) == retained_tag);
            self.refresh_history(cx);
        } else {
            cx.notify();
        }
    }

    pub(crate) fn reset_tag_selection(&mut self, cx: &mut Context<Self>) {
        if self.selected_tag_ids.is_empty() {
            return;
        }
        self.selected_tag_ids.clear();
        self.refresh_history(cx);
    }

    pub(crate) fn toggle_clear_tags_on_tab_switch(&mut self, cx: &mut Context<Self>) {
        let mut clipboard = crate::config_runtime::clipboard_config(cx);
        clipboard.clear_tags_on_tab_switch = !clipboard.clear_tags_on_tab_switch;
        if let Err(error) = crate::config_runtime::set_clipboard_config(clipboard, cx) {
            tracing::error!("保存切换页面时清空标签选择设置失败：{error}");
        }
    }

    pub(crate) fn schedule_tags_panel_close(&mut self, window: &Window, cx: &mut Context<Self>) {
        if !self.can_auto_close_tags_panel(window, cx) || self.tags_panel_close_task.is_some() {
            return;
        }
        self.tags_panel_close_task = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(TAGS_PANEL_CLOSE_DELAY).await;
            this.update_in(cx, |this, window, cx| {
                this.tags_panel_close_task = None;
                if this.can_auto_close_tags_panel(window, cx) {
                    // 面板收起后不会再收到拖拽释放事件，需要主动清理标签拖拽状态。
                    this.cancel_tag_reorder_preview();
                    this.tags_panel_open = false;
                    cx.notify();
                }
            })
            .ok();
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::toggle_tag_selection;
    use std::collections::HashSet;

    #[test]
    fn tags_multi_selection_adds_and_removes_only_the_clicked_tag() {
        let mut selected = HashSet::new();
        toggle_tag_selection(&mut selected, 1, true);
        toggle_tag_selection(&mut selected, 2, true);
        assert_eq!(selected, HashSet::from([1, 2]));
        toggle_tag_selection(&mut selected, 1, true);
        assert_eq!(selected, HashSet::from([2]));
    }

    #[test]
    fn tags_single_selection_replaces_the_previous_tag_and_can_be_cleared() {
        let mut selected = HashSet::new();
        toggle_tag_selection(&mut selected, 1, false);
        assert_eq!(selected, HashSet::from([1]));
        toggle_tag_selection(&mut selected, 2, false);
        assert_eq!(selected, HashSet::from([2]));
        toggle_tag_selection(&mut selected, 2, false);
        assert!(selected.is_empty());
    }

    #[test]
    fn tags_single_selection_clears_multiple_previous_selections() {
        let mut selected = HashSet::from([1, 2]);
        toggle_tag_selection(&mut selected, 3, false);
        assert_eq!(selected, HashSet::from([3]));

        selected.insert(2);
        toggle_tag_selection(&mut selected, 2, false);
        assert!(selected.is_empty());
    }
}
