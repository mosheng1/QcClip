use super::*;

#[path = "list_refresh.rs"]
mod list_refresh;
use list_refresh::{
    HistoryItemRefresh, HistoryRefreshReason, apply_captured_list_order, load_history_item_refresh,
    reconcile_snapshot_selection, replace_list_order, retain_list_records_for_order,
};

impl MainWindow {
    pub(super) fn rebuild_list_order_positions(&mut self) {
        self.list_order_positions.clear();
        self.list_order_positions.reserve(self.list_order_ids.len());
        self.list_order_positions.extend(
            self.list_order_ids
                .iter()
                .enumerate()
                .map(|(index, history_id)| (*history_id, index)),
        );
    }

    pub(super) fn reindex_list_order_positions(&mut self, range: Range<usize>) {
        for index in
            range.start.min(self.list_order_ids.len())..range.end.min(self.list_order_ids.len())
        {
            self.list_order_positions
                .insert(self.list_order_ids[index], index);
        }
    }

    pub(super) fn move_list_order_item_to_position(
        &mut self,
        history_id: HistoryItemId,
        target_position: usize,
    ) -> bool {
        let Some(source_position) = self.list_order_positions.get(&history_id).copied() else {
            return false;
        };
        if source_position == target_position || target_position >= self.list_order_ids.len() {
            return false;
        }

        if source_position < target_position {
            self.list_order_ids[source_position..=target_position].rotate_left(1);
        } else {
            self.list_order_ids[target_position..=source_position].rotate_right(1);
        }
        self.list_heatmap
            .move_item(source_position, target_position);
        if let Some(cache) = self.list_layout_cache.as_ref() {
            cache.sizes.move_item(source_position, target_position);
        }

        self.reindex_list_order_positions(
            source_position.min(target_position)..source_position.max(target_position) + 1,
        );
        true
    }

    pub(crate) fn invalidate_list_layout_cache(&mut self) {
        if self.list_layout_cache.as_ref().is_some_and(|cache| {
            cache.key.list_item_height != crate::config::ListItemHeight::Adaptive
        }) {
            return;
        }
        self.list_layout_generation = self.list_layout_generation.wrapping_add(1);
        self.list_layout_cache = None;
    }

    pub fn replace_items(&mut self, mut items: Vec<ClipboardListItem>) {
        self.adaptive_item_heights.clear();
        self.adaptive_height_cache_key = None;
        self.invalidate_list_layout_cache();
        items.sort_by(compare_list_items);
        if self.total_item_count == 0 {
            self.total_item_count = items.len();
        }
        self.selected_item_id = self
            .selected_item_id
            .filter(|history_id| self.list_order_positions.contains_key(history_id));
        if self.selected_item_id.is_none() {
            self.selected_item_id = items.first().map(|item| item.history_id);
        }
        self.list_status = if items.is_empty() {
            ListStatus::Empty
        } else {
            ListStatus::Ready
        };
        for item in &mut items {
            item.is_selected = Some(item.history_id) == self.selected_item_id;
        }
        assign_page_shortcuts(&mut items, 0);
        self.items = items;
        let loaded_ids = self
            .items
            .iter()
            .map(|item| item.history_id)
            .collect::<Vec<_>>();
        self.loaded_items = self
            .items
            .iter()
            .cloned()
            .map(|item| (item.history_id, item))
            .collect();
        self.mark_list_items_loaded(loaded_ids);
    }

    pub(super) fn mark_list_items_loaded(
        &mut self,
        history_ids: impl IntoIterator<Item = HistoryItemId>,
    ) {
        let history_ids = history_ids.into_iter().collect::<Vec<_>>();
        if history_ids.is_empty() {
            return;
        }

        self.list_item_load_animation_generation =
            self.list_item_load_animation_generation.wrapping_add(1);
        let generation = self.list_item_load_animation_generation;
        for (order, history_id) in history_ids.into_iter().enumerate() {
            self.list_item_load_animations
                .insert(history_id, ListItemLoadAnimation { generation, order });
        }
    }

    pub fn load_page(&mut self, cx: &mut Context<Self>, offset: usize, page_size: usize) {
        self.load_page_with_animation(cx, offset, page_size, true);
    }

    pub(crate) fn load_page_without_animation(
        &mut self,
        cx: &mut Context<Self>,
        offset: usize,
        page_size: usize,
    ) {
        self.load_page_with_animation(cx, offset, page_size, false);
    }

    fn load_page_with_animation(
        &mut self,
        cx: &mut Context<Self>,
        offset: usize,
        page_size: usize,
        animate_loaded: bool,
    ) {
        if self.active_tab == TabId::Emoji
            || offset >= self.total_item_count
            || self.is_loading_more
            || !matches!(self.list_status, ListStatus::Ready)
        {
            return;
        }

        let page_size = page_size.clamp(1, MAX_QUERY_PAGE_SIZE);
        let page_end = offset.saturating_add(page_size).min(self.total_item_count);
        let requested_ids = self
            .list_order_ids
            .get(offset..page_end)
            .unwrap_or_default()
            .to_vec();
        self.loading_page_range = Some(offset..page_end);
        let generation = self.query_generation;
        let policy = crate::config_runtime::history_config(cx);
        let query = self.page_query(offset, page_size);
        let active_tab = self.active_tab;
        let history_repository = self.history_repository.clone();
        let favorite_repository = self.favorite_repository.clone();
        self.is_loading_more = true;
        let query_task = cx.background_spawn(async move {
            if active_tab == TabId::Favorites {
                favorite_repository.query_by_ids(&query, &requested_ids)
            } else {
                history_repository.query_by_ids_scoped(&query, &requested_ids, &policy)
            }
        });
        self.list_query_task = Some(cx.spawn(async move |this, cx| {
            let result = query_task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| {
                    if let Some((page_loaded, page_range)) =
                        this.accept_page_result(generation, page_size, animate_loaded, result)
                    {
                        if page_loaded {
                            this.load_pending_selection(page_range, cx);
                        } else {
                            this.pending_selection_index = None;
                        }
                        cx.notify();
                    }
                });
            }
        }));
        cx.notify();
    }

    pub(super) fn load_pending_selection(
        &mut self,
        loaded_range: Range<usize>,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.pending_selection_index else {
            return;
        };
        let Some(history_id) = self.list_order_ids.get(index).copied() else {
            self.pending_selection_index = None;
            return;
        };
        if self.selected_item_id != Some(history_id) || self.loaded_items.contains_key(&history_id)
        {
            self.pending_selection_index = None;
            return;
        }
        if loaded_range.contains(&index) {
            self.pending_selection_index = None;
            return;
        }
        self.pending_selection_index = None;
        self.load_page(cx, index, 1);
    }

    pub(crate) fn trim_loaded_items(&mut self, visible_range: Range<usize>) {
        let cache_start = visible_range.start.min(self.total_item_count);
        let cache_end = visible_range.end.min(self.total_item_count);
        let mut cached_ids = self
            .list_order_ids
            .get(cache_start..cache_end)
            .unwrap_or_default()
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        if let Some(source_id) = self.list_drag_source_id {
            cached_ids.insert(source_id);
        }
        self.loaded_items
            .retain(|history_id, _| cached_ids.contains(history_id));
        self.list_item_load_animations
            .retain(|history_id, _| cached_ids.contains(history_id));

        self.items
            .retain(|item| cached_ids.contains(&item.history_id));
        self.reconcile_selection();
    }

    pub(crate) fn trim_image_cache(
        &mut self,
        cache_range: Range<usize>,
        mut window: Option<&mut Window>,
    ) {
        // 列表增量重排后，非顶部缓存会先清空索引，等待新页面数据到达。
        // 此时不能把仍可复用的图片误判为过期，否则会导致视口图片反复解码。
        if self.loaded_items.is_empty() && !self.loaded_images.is_empty() {
            return;
        }
        let allowed_history_ids = self
            .list_order_ids
            .get(cache_range)
            .unwrap_or_default()
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        let allowed_item_ids: HashSet<_> = allowed_history_ids
            .iter()
            .filter_map(|history_id| self.loaded_items.get(history_id))
            .map(|item| item.item_id)
            .collect();
        let stale_keys = self
            .loaded_images
            .keys()
            .filter(|key| !allowed_item_ids.contains(&key.item_id))
            .cloned()
            .collect::<Vec<_>>();
        for key in stale_keys {
            if let Some(loaded) = self.loaded_images.remove(&key)
                && let Some(window) = window.as_deref_mut()
            {
                let _ = window.drop_image(loaded.image);
            }
        }
        self.image_load_failures
            .retain(|key| allowed_item_ids.contains(&key.item_id));
        self.image_cache_item_ids = allowed_item_ids;
    }

    pub(crate) fn trim_html_image_cache(
        &mut self,
        cache_range: Range<usize>,
        display_priority: ClipboardDisplayPriority,
        mut window: Option<&mut Window>,
    ) {
        // 列表增量重排期间先保留缓存，避免新页面到达前重复加载图片。
        if self.loaded_items.is_empty() && !self.loaded_html_images.is_empty() {
            return;
        }
        let allowed_paths = self.html_image_paths_for_range(cache_range, display_priority);
        let stale_paths = self
            .loaded_html_images
            .keys()
            .filter(|path| !allowed_paths.contains(*path))
            .cloned()
            .collect::<Vec<_>>();
        for path in stale_paths {
            if let Some(image) = self.loaded_html_images.remove(&path)
                && let Some(window) = window.as_deref_mut()
            {
                let _ = window.drop_image(image);
            }
        }
        self.html_image_load_failures
            .retain(|path| allowed_paths.contains(path));
        self.html_image_cache_paths = allowed_paths;
    }

    pub(super) fn html_image_paths_for_range(
        &self,
        range: Range<usize>,
        display_priority: ClipboardDisplayPriority,
    ) -> HashSet<PathBuf> {
        range
            .flat_map(|index| {
                self.list_order_ids
                    .get(index)
                    .and_then(|history_id| self.loaded_items.get(history_id))
                    .into_iter()
                    .filter_map(|item| {
                        select_user_visible_previews_with_priority(
                            &item.preview_options,
                            &display_priority_roles(display_priority),
                        )
                        .into_iter()
                        .next()
                    })
                    .filter_map(|preview| preview.html_content)
                    .flat_map(|html| html_preview::local_image_paths(&html))
            })
            .take(MAX_HTML_IMAGE_PATHS_PER_RANGE)
            .collect()
    }

    pub(crate) fn trim_application_icon_cache(
        &mut self,
        cache_range: Range<usize>,
        mut window: Option<&mut Window>,
    ) {
        if self.loaded_items.is_empty() && !self.loaded_application_icons.is_empty() {
            return;
        }
        let allowed_keys: HashSet<_> = self
            .list_order_ids
            .get(cache_range)
            .unwrap_or_default()
            .iter()
            .filter_map(|history_id| self.loaded_items.get(history_id))
            .filter_map(|item| item.source_app_icon.as_ref())
            .map(ApplicationIconKey::new)
            .collect();
        let stale_keys = self
            .loaded_application_icons
            .keys()
            .filter(|key| !allowed_keys.contains(*key))
            .cloned()
            .collect::<Vec<_>>();
        for key in stale_keys {
            if let Some(image) = self.loaded_application_icons.remove(&key)
                && let Some(window) = window.as_deref_mut()
            {
                let _ = window.drop_image(image);
            }
        }
        self.application_icon_load_failures
            .retain(|key| allowed_keys.contains(key));
    }

    pub(crate) fn ensure_images_for_range(
        &mut self,
        range: Range<usize>,
        max_size: (u32, u32),
        display_priority: ClipboardDisplayPriority,
        cx: &mut Context<Self>,
    ) {
        let requests = range
            .flat_map(|index| {
                self.list_order_ids
                    .get(index)
                    .and_then(|history_id| self.loaded_items.get(history_id))
                    .into_iter()
                    .flat_map(|item| {
                        select_user_visible_previews_with_priority(
                            &item.preview_options,
                            &display_priority_roles(display_priority),
                        )
                        .into_iter()
                        .filter_map(|preview| {
                            if preview.content_kind != ContentKind::Image {
                                return None;
                            }
                            preview.image_source.map(|source| {
                                (
                                    ImagePreviewKey::new(
                                        item.item_id,
                                        &preview.format_namespace,
                                        &preview.format_key,
                                    ),
                                    source,
                                    preview.format_kind,
                                    preview.format_key,
                                    preview.image_dimensions,
                                )
                            })
                        })
                    })
            })
            .collect::<Vec<_>>();
        for (key, source, format_kind, format_key, dimensions_hint) in requests {
            if self.loaded_images.contains_key(&key)
                || self.image_load_failures.contains(&key)
                || !self.image_loading.insert(key.clone())
            {
                continue;
            }
            let service = self.image_preview_service.clone();
            let task = cx.background_spawn(async move {
                service.load_with_dimensions_hint(
                    &source,
                    format_kind,
                    &format_key,
                    max_size,
                    dimensions_hint,
                )
            });
            cx.spawn(async move |this, cx| {
                let result = task.await;
                if let Some(this) = this.upgrade() {
                    this.update(cx, |this, cx| {
                        this.image_loading.remove(&key);
                        match result {
                            Ok(loaded) if this.image_cache_item_ids.contains(&key.item_id) => {
                                if let Some(dimensions) = loaded.dimensions {
                                    this.update_image_preview_dimensions(&key, dimensions);
                                }
                                this.loaded_images.insert(key, loaded);
                                cx.notify();
                            }
                            Ok(_) => {}
                            Err(error) => {
                                tracing::warn!(item_id = key.item_id, format_key = %key.format_key,
                                    %error, "加载列表图片预览失败");
                                this.image_load_failures.insert(key);
                            }
                        }
                    });
                }
            })
            .detach();
        }
    }

    pub(super) fn update_image_preview_dimensions(
        &mut self,
        key: &ImagePreviewKey,
        dimensions: ImageDimensions,
    ) {
        self.invalidate_list_layout_cache();
        for item in self
            .loaded_items
            .values_mut()
            .filter(|item| item.item_id == key.item_id)
        {
            for preview in item
                .preview_options
                .iter_mut()
                .chain(item.all_format_options.iter_mut())
            {
                if preview.format_namespace == key.format_namespace
                    && preview.format_key == key.format_key
                {
                    preview.image_dimensions = Some(dimensions);
                }
            }
        }
    }

    pub(crate) fn ensure_html_images_for_range(
        &mut self,
        range: Range<usize>,
        max_size: (u32, u32),
        display_priority: ClipboardDisplayPriority,
        cx: &mut Context<Self>,
    ) {
        for path in self.html_image_paths_for_range(range, display_priority) {
            if self.loaded_html_images.contains_key(&path)
                || self.html_image_load_failures.contains(&path)
                || !self.html_image_loading.insert(path.clone())
            {
                continue;
            }
            let service = self.image_preview_service.clone();
            let task_path = path.clone();
            let task =
                cx.background_spawn(async move { service.load_local_path(&task_path, max_size) });
            cx.spawn(async move |this, cx| {
                let result = task.await;
                if let Some(this) = this.upgrade() {
                    this.update(cx, |this, cx| {
                        this.html_image_loading.remove(&path);
                        match result {
                            Ok(image) if this.html_image_cache_paths.contains(&path) => {
                                this.loaded_html_images.insert(path, image);
                                cx.notify();
                            }
                            Ok(_) => {}
                            Err(error) if this.html_image_cache_paths.contains(&path) => {
                                this.html_image_load_failures.insert(path.clone());
                                tracing::warn!(%error, "加载 HTML 本地图片失败");
                            }
                            Err(_) => {}
                        }
                    });
                }
            })
            .detach();
        }
    }

    pub(crate) fn ensure_application_icons_for_range(
        &mut self,
        range: Range<usize>,
        max_size: (u32, u32),
        cx: &mut Context<Self>,
    ) {
        let requests = range
            .filter_map(|index| {
                self.list_order_ids
                    .get(index)
                    .and_then(|history_id| self.loaded_items.get(history_id))
            })
            .filter_map(|item| item.source_app_icon.as_ref())
            .map(|source| (ApplicationIconKey::new(source), source.clone()))
            .collect::<Vec<_>>();
        for (key, source) in requests {
            if self.loaded_application_icons.contains_key(&key)
                || self.application_icon_load_failures.contains(&key)
                || !self.application_icon_loading.insert(key.clone())
            {
                continue;
            }
            let service = self.image_preview_service.clone();
            let task = cx.background_spawn(async move {
                let source = ImagePreviewSource::External {
                    sha256: source.sha256,
                    byte_length: source.byte_length,
                    relative_path: source.relative_path,
                };
                service.load(&source, FormatKind::Image, "application-icon", max_size)
            });
            cx.spawn(async move |this, cx| {
                let result = task.await;
                if let Some(this) = this.upgrade() {
                    this.update(cx, |this, cx| {
                        this.application_icon_loading.remove(&key);
                        match result {
                            Ok(image) => {
                                this.loaded_application_icons.insert(key, image);
                                cx.notify();
                            }
                            Err(error) => {
                                this.application_icon_load_failures.insert(key);
                                tracing::warn!(%error, "加载来源应用图标失败");
                            }
                        }
                    });
                }
            })
            .detach();
        }
    }

    pub(crate) fn trim_file_list_cache(
        &mut self,
        cache_range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 列表增量重排期间索引会短暂清空；保留已有文件列表实体，避免非顶部视口反复重建。
        if self.loaded_items.is_empty() && !self.file_list_views.is_empty() {
            return;
        }
        let allowed_keys = self
            .list_order_ids
            .get(cache_range)
            .unwrap_or_default()
            .iter()
            .filter_map(|history_id| self.loaded_items.get(history_id))
            .flat_map(|item| {
                item.preview_options.iter().filter_map(|preview| {
                    if preview.format_kind != FormatKind::FileList
                        || preview.content_kind != ContentKind::FileList
                    {
                        return None;
                    }
                    preview.image_source.as_ref().map(|_| {
                        FileListKey::new(
                            item.item_id,
                            &preview.format_namespace,
                            &preview.format_key,
                        )
                    })
                })
            })
            .collect::<HashSet<_>>();
        self.file_list_cache_keys = allowed_keys.clone();
        let stale_keys = self
            .file_list_views
            .keys()
            .filter(|key| !allowed_keys.contains(*key))
            .cloned()
            .collect::<Vec<_>>();
        for key in stale_keys {
            self.drop_file_list_view(&key, window, cx);
        }
        self.file_list_failures
            .retain(|key| allowed_keys.contains(key));
    }

    pub(crate) fn ensure_file_lists_for_range(
        &mut self,
        range: Range<usize>,
        cx: &mut Context<Self>,
    ) {
        let requests = range
            .filter_map(|index| {
                self.list_order_ids
                    .get(index)
                    .and_then(|history_id| self.loaded_items.get(history_id))
            })
            .flat_map(|item| {
                item.preview_options.iter().filter_map(|preview| {
                    if preview.format_kind != FormatKind::FileList
                        || preview.content_kind != ContentKind::FileList
                    {
                        return None;
                    }
                    preview.image_source.as_ref().map(|source| {
                        (
                            FileListKey::new(
                                item.item_id,
                                &preview.format_namespace,
                                &preview.format_key,
                            ),
                            source.clone(),
                        )
                    })
                })
            })
            .collect::<Vec<_>>();
        for (key, source) in requests {
            if self.file_list_views.contains_key(&key)
                || self.file_list_failures.contains(&key)
                || !self.file_list_loading.insert(key.clone())
            {
                continue;
            }
            let service = self.image_preview_service.clone();
            let task = cx.background_spawn(async move { service.load_file_list_paths(&source) });
            cx.spawn(async move |this, cx| {
                let result = task.await;
                if let Some(this) = this.upgrade() {
                    this.update(cx, |this, cx| {
                        this.file_list_loading.remove(&key);
                        if !this.file_list_cache_keys.contains(&key) {
                            return;
                        }
                        match result {
                            Ok(paths) => {
                                let paths = Arc::new(paths);
                                let view = cx.new(|_| FileListView::new(paths));
                                this.file_list_views.insert(key, view);
                                this.invalidate_list_layout_cache();
                                cx.notify();
                            }
                            Err(error) => {
                                tracing::warn!(%error, "文件列表路径读取失败");
                                this.file_list_failures.insert(key);
                            }
                        }
                    });
                }
            })
            .detach();
        }
    }

    pub(crate) fn file_list_for(
        &self,
        item_id: ClipboardItemId,
        format_namespace: &str,
        format_key: &str,
    ) -> Option<Entity<FileListView>> {
        self.file_list_views
            .get(&FileListKey::new(item_id, format_namespace, format_key))
            .cloned()
    }

    pub(crate) fn cleanup_file_list_cache(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.file_list_cache_needs_cleanup {
            return;
        }
        let keys = self.file_list_views.keys().cloned().collect::<Vec<_>>();
        for key in keys {
            self.drop_file_list_view(&key, window, cx);
        }
        self.file_list_cache_keys.clear();
        self.file_list_failures.clear();
        self.file_list_cache_needs_cleanup = false;
    }

    pub(super) fn drop_file_list_view(
        &mut self,
        key: &FileListKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.file_list_views.remove(key) else {
            return;
        };
        let images = view.update(cx, |view, _| view.take_images());
        for image in images {
            let _ = window.drop_image(image);
        }
    }

    pub(crate) fn image_for(
        &self,
        item_id: ClipboardItemId,
        format_namespace: &str,
        format_key: &str,
    ) -> Option<LoadedImagePreview> {
        self.loaded_images
            .get(&ImagePreviewKey::new(item_id, format_namespace, format_key))
            .cloned()
    }

    pub(crate) fn application_icon_for(
        &self,
        source: &ApplicationIconSource,
    ) -> Option<Arc<RenderImage>> {
        self.loaded_application_icons
            .get(&ApplicationIconKey::new(source))
            .cloned()
    }

    pub(super) fn clear_image_cache(&mut self) {
        // 后台任务不能被简单地从集合中移除，否则刷新期间会为同一张图片重复启动解码。
        // 任务完成后会根据当前缓存范围决定是否丢弃结果。
        self.image_load_failures.clear();
        self.image_cache_item_ids.clear();
        self.image_cache_invalidated_item_ids.clear();
        self.html_image_load_failures.clear();
        self.html_image_cache_paths.clear();
        self.image_cache_needs_cleanup = true;
    }

    pub(super) fn clear_file_list_cache(&mut self) {
        // 资源需要在下一帧拿到 Window 后释放；后台路径任务完成后会检查当前缓存范围。
        self.file_list_cache_keys.clear();
        self.file_list_failures.clear();
        self.file_list_cache_needs_cleanup = true;
    }

    pub(super) fn invalidate_image_cache_item(&mut self, item_id: ClipboardItemId) {
        self.image_cache_invalidated_item_ids.insert(item_id);
        self.image_cache_needs_cleanup = true;
    }

    pub(crate) fn cleanup_image_cache(&mut self, window: &mut Window) {
        if !self.image_cache_needs_cleanup {
            return;
        }
        if self.image_cache_invalidated_item_ids.is_empty() {
            for (_, loaded) in self.loaded_images.drain() {
                let _ = window.drop_image(loaded.image);
            }
            for (_, image) in self.loaded_html_images.drain() {
                let _ = window.drop_image(image);
            }
        } else {
            let stale_keys = self
                .loaded_images
                .keys()
                .filter(|key| self.image_cache_invalidated_item_ids.contains(&key.item_id))
                .cloned()
                .collect::<Vec<_>>();
            for key in stale_keys {
                if let Some(loaded) = self.loaded_images.remove(&key) {
                    let _ = window.drop_image(loaded.image);
                }
            }
        }
        self.image_cache_invalidated_item_ids.clear();
        self.image_cache_needs_cleanup = false;
    }

    pub(super) fn refresh_list_item_for_tab(
        &mut self,
        source_tab: TabId,
        list_item_id: HistoryItemId,
        cx: &mut Context<Self>,
    ) {
        if self.active_tab != source_tab {
            self.refresh_history(cx);
            return;
        }
        self.refresh_list_item(list_item_id, cx);
    }

    /// 只更新当前已加载条目的元数据，不重新统计数量和读取完整排序 ID。
    /// 收藏关系在剪贴板历史页不会改变列表顺序，适合走这条快速路径。
    pub(super) fn refresh_loaded_list_item(
        &mut self,
        source_tab: TabId,
        list_item_id: HistoryItemId,
        cx: &mut Context<Self>,
    ) {
        if self.active_tab != source_tab {
            self.refresh_history(cx);
            return;
        }
        let generation = self.query_generation;
        let policy = crate::config_runtime::history_config(cx);
        let query = self.page_query(0, 1);
        let active_tab = self.active_tab;
        let history_repository = self.history_repository.clone();
        let favorite_repository = self.favorite_repository.clone();
        let query_task = cx.background_spawn(async move {
            if active_tab == TabId::Favorites {
                favorite_repository.find_by_id(list_item_id, &query)
            } else {
                history_repository.find_by_id_scoped(list_item_id, &query, &policy)
            }
        });
        cx.spawn(async move |this, cx| {
            let result = query_task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| {
                    if generation != this.query_generation || this.active_tab != active_tab {
                        return;
                    }
                    match result {
                        Ok(Some(record)) => this.apply_loaded_list_record(record),
                        Ok(None) => this.refresh_list_item(list_item_id, cx),
                        Err(error) => tracing::error!("列表元数据更新失败：{error}"),
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn apply_loaded_list_record(&mut self, record: HistoryListRecord) {
        if let Some(index) = self.list_order_positions.get(&record.history_id).copied() {
            self.list_heatmap.update(index, record.paste_count);
        }
        self.apply_loaded_list_item(record_to_list_item(record));
    }

    fn apply_loaded_list_item(&mut self, mut item: ClipboardListItem) {
        let history_id = item.history_id;
        item.is_selected = self.selected_item_id == Some(history_id);
        let old_shortcut = self
            .items
            .iter()
            .find(|existing| existing.history_id == history_id)
            .map(|existing| existing.shortcut.clone());
        if let Some(shortcut) = old_shortcut {
            item.shortcut = shortcut;
        }
        for existing in &mut self.items {
            if existing.history_id == history_id {
                *existing = item.clone();
            }
        }
        if self.loaded_items.contains_key(&history_id) {
            self.loaded_items.insert(history_id, item);
        }
    }

    /// 无保留策略时增量更新捕获顺序；有限范围使用数据库快照。
    pub(crate) fn refresh_new_list_item(
        &mut self,
        history_id: HistoryItemId,
        cx: &mut Context<Self>,
    ) {
        self.refresh_history_record(history_id, HistoryRefreshReason::Capture, cx);
    }

    pub(crate) fn refresh_updated_history_item(
        &mut self,
        history_id: HistoryItemId,
        cx: &mut Context<Self>,
    ) {
        self.refresh_history_record(history_id, HistoryRefreshReason::Update, cx);
    }

    fn refresh_history_record(
        &mut self,
        history_id: HistoryItemId,
        reason: HistoryRefreshReason,
        cx: &mut Context<Self>,
    ) {
        if self.active_tab != TabId::Clipboard
            || !matches!(self.list_status, ListStatus::Ready | ListStatus::Empty)
        {
            self.refresh_history(cx);
            return;
        }
        let generation = self.query_generation;
        let policy = crate::config_runtime::history_config(cx);
        let query = self.page_query(0, 1);
        let repository = self.history_repository.clone();
        let requested_order = if reason.requires_snapshot(&policy) {
            self.list_order_ids.clone()
        } else {
            Vec::new()
        };
        let query_task = cx.background_spawn(async move {
            load_history_item_refresh(&repository, history_id, &query, &policy, reason)
        });
        cx.spawn(async move |this, cx| {
            let result = query_task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| {
                    if generation != this.query_generation || this.active_tab != TabId::Clipboard {
                        return;
                    }
                    match result {
                        Ok(HistoryItemRefresh::Captured {
                            record,
                            pinned_count,
                        }) => {
                            let is_new_list_item =
                                !this.list_order_positions.contains_key(&history_id);
                            if this.apply_history_record_change(history_id, record, pinned_count) {
                                if is_new_list_item
                                    && crate::config_runtime::clipboard_config(cx)
                                        .auto_scroll_to_top
                                {
                                    this.list_scroll_handle
                                        .scroll_to_item(0, gpui::ScrollStrategy::Top);
                                    this.list_smooth_scroll.reset();
                                }
                                cx.notify();
                            }
                        }
                        Ok(HistoryItemRefresh::Snapshot { record, snapshot }) => {
                            let scroll_to_top = matches!(reason, HistoryRefreshReason::Capture)
                                && record.is_some()
                                && !this.list_order_positions.contains_key(&history_id)
                                && crate::config_runtime::clipboard_config(cx).auto_scroll_to_top;
                            if !this.apply_list_record(
                                history_id,
                                record,
                                &requested_order,
                                snapshot,
                            ) {
                                this.refresh_history_record(history_id, reason, cx);
                                return;
                            }
                            if scroll_to_top {
                                this.list_scroll_handle
                                    .scroll_to_item(0, gpui::ScrollStrategy::Top);
                                this.list_smooth_scroll.reset();
                            }
                            cx.notify();
                        }
                        Err(error) => tracing::error!("历史列表项查询失败：{error}"),
                    }
                });
            }
        })
        .detach();
    }

    pub(super) fn refresh_pinned_list_item(
        &mut self,
        source_tab: TabId,
        list_item_id: HistoryItemId,
        cx: &mut Context<Self>,
    ) {
        if self.active_tab != source_tab
            || !matches!(self.list_status, ListStatus::Ready | ListStatus::Empty)
        {
            self.refresh_history(cx);
            return;
        }
        let generation = self.query_generation;
        let policy = crate::config_runtime::history_config(cx);
        let query = self.page_query(0, 1);
        let active_tab = self.active_tab;
        let history_repository = self.history_repository.clone();
        let favorite_repository = self.favorite_repository.clone();
        let query_task = cx.background_spawn(async move {
            if active_tab == TabId::Favorites {
                let record = favorite_repository.find_by_id(list_item_id, &query)?;
                let pinned_count = favorite_repository.pinned_count(&query)?;
                Ok::<_, HistoryQueryError>((record, pinned_count))
            } else {
                let record = history_repository.find_by_id_scoped(list_item_id, &query, &policy)?;
                let pinned_count = history_repository.pinned_count(&query)?;
                Ok::<_, HistoryQueryError>((record, pinned_count))
            }
        });
        cx.spawn(async move |this, cx| {
            let result = query_task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| {
                    if generation != this.query_generation || this.active_tab != active_tab {
                        return;
                    }
                    match result {
                        Ok((Some(record), pinned_count)) => {
                            if this.apply_pinned_list_record(list_item_id, record, pinned_count) {
                                cx.notify();
                            } else {
                                this.refresh_list_item(list_item_id, cx);
                            }
                        }
                        Ok((None, _)) => this.refresh_history(cx),
                        Err(error) => tracing::error!("置顶列表项更新失败：{error}"),
                    }
                });
            }
        })
        .detach();
    }

    fn apply_pinned_list_record(
        &mut self,
        list_item_id: HistoryItemId,
        record: HistoryListRecord,
        pinned_count: usize,
    ) -> bool {
        let Some(source_position) = self.list_order_positions.get(&list_item_id).copied() else {
            return false;
        };
        let item = record_to_list_item(record.clone());
        let mut order_ids = std::mem::take(&mut self.list_order_ids);
        order_ids.remove(source_position);
        let target_position = if item.is_pinned {
            0
        } else {
            pinned_count.min(order_ids.len())
        };
        order_ids.insert(target_position, list_item_id);
        self.list_heatmap
            .move_item(source_position, target_position);
        self.list_order_ids = order_ids;
        self.reindex_list_order_positions(
            source_position.min(target_position)..source_position.max(target_position) + 1,
        );
        if source_position != target_position {
            self.invalidate_list_layout_cache();
        }
        self.apply_loaded_list_record(record);
        self.items.sort_by(compare_list_items);
        refresh_shortcuts_after_move_to(
            &mut self.items,
            list_item_id,
            source_position,
            target_position,
        );
        self.sync_loaded_shortcuts();
        true
    }

    pub(super) fn apply_removed_list_items(&mut self, history_ids: &[HistoryItemId]) -> bool {
        if !matches!(self.list_status, ListStatus::Ready | ListStatus::Empty) {
            return false;
        }
        if history_ids.is_empty() {
            return true;
        }
        let removed_ids: HashSet<_> = history_ids.iter().copied().collect();
        let mut removed_positions = history_ids
            .iter()
            .filter_map(|history_id| self.list_order_positions.get(history_id).copied())
            .collect::<Vec<_>>();
        removed_positions.sort_unstable();
        removed_positions.dedup();
        if removed_positions.is_empty() {
            return true;
        }
        self.list_heatmap.remove_positions(&removed_positions);
        let visible_removed_count = removed_positions.len();
        for history_id in history_ids {
            if let Some(item) = self.loaded_items.get(history_id) {
                self.invalidate_image_cache_item(item.item_id);
            }
        }
        if let [position] = removed_positions.as_slice() {
            let removed_id = self.list_order_ids.remove(*position);
            self.list_order_positions.remove(&removed_id);
            self.reindex_list_order_positions(*position..self.list_order_ids.len());
        } else {
            self.list_order_ids
                .retain(|history_id| !removed_ids.contains(history_id));
            self.rebuild_list_order_positions();
        }
        self.items
            .retain(|item| !removed_ids.contains(&item.history_id));
        self.loaded_items
            .retain(|history_id, _| !removed_ids.contains(history_id));
        self.adaptive_item_heights
            .retain(|history_id, _| !removed_ids.contains(history_id));
        self.list_item_load_animations
            .retain(|history_id, _| !removed_ids.contains(history_id));
        self.total_item_count = self.total_item_count.saturating_sub(visible_removed_count);
        self.invalidate_list_layout_cache();
        refresh_shortcuts_after_removal(&mut self.items, &removed_positions);
        self.sync_loaded_shortcuts();
        if self
            .selected_item_id
            .is_some_and(|id| removed_ids.contains(&id))
        {
            self.selected_item_id = self.items.first().map(|item| item.history_id);
        }
        self.multi_selected_item_ids
            .retain(|history_id| !removed_ids.contains(history_id));
        if self.multi_selected_item_ids.is_empty() {
            self.multi_selection_anchor = None;
        } else if self
            .multi_selection_anchor
            .is_some_and(|id| removed_ids.contains(&id))
        {
            self.multi_selection_anchor = self.multi_selected_item_ids.first().copied();
        }
        for item in &mut self.items {
            item.is_selected = self.selected_item_id == Some(item.history_id);
        }
        self.has_more_items = self.loaded_items.len() < self.total_item_count;
        self.list_status = if self.total_item_count == 0 {
            ListStatus::Empty
        } else {
            ListStatus::Ready
        };
        true
    }

    fn apply_history_record_change(
        &mut self,
        history_id: HistoryItemId,
        record: Option<HistoryListRecord>,
        pinned_count: usize,
    ) -> bool {
        let source_position = self.list_order_positions.get(&history_id).copied();
        let Some(record) = record else {
            if source_position.is_none() {
                return false;
            }
            self.apply_removed_list_items(&[history_id]);
            return true;
        };

        let item = record_to_list_item(record);
        let presentation_changed = self.loaded_items.get(&history_id).is_none_or(|previous| {
            previous.preview_text != item.preview_text
                || previous.preview_options != item.preview_options
                || previous.all_format_options != item.all_format_options
                || previous.content_kind != item.content_kind
                || previous.note_text != item.note_text
                || previous.tag_ids != item.tag_ids
                || previous.source_app_name != item.source_app_name
                || previous.source_app_icon != item.source_app_icon
        });
        let (_, destination_position) = apply_captured_list_order(
            &mut self.list_order_ids,
            &mut self.list_heatmap,
            &mut self.total_item_count,
            &item,
            pinned_count,
        );
        if let Some(source_position) = source_position {
            self.reindex_list_order_positions(
                source_position.min(destination_position)
                    ..source_position.max(destination_position) + 1,
            );
        } else {
            self.reindex_list_order_positions(destination_position..self.list_order_ids.len());
            if self.selected_item_id.is_none() && self.total_item_count == 1 {
                self.selected_item_id = Some(history_id);
            }
        }
        let mut item = item;
        if let Some(shortcut) = self
            .loaded_items
            .get(&history_id)
            .map(|existing| existing.shortcut.clone())
        {
            item.shortcut = shortcut;
        }
        item.is_selected = self.selected_item_id == Some(history_id);
        self.items
            .retain(|existing| existing.history_id != history_id);
        insert_sorted_item(&mut self.items, item.clone());
        self.loaded_items.insert(history_id, item);
        if presentation_changed || source_position != Some(destination_position) {
            self.adaptive_item_heights.remove(&history_id);
            self.invalidate_list_layout_cache();
        }
        if let Some(source_position) = source_position {
            refresh_shortcuts_after_move_to(
                &mut self.items,
                history_id,
                source_position,
                destination_position,
            );
        } else {
            refresh_shortcuts_after_insert(&mut self.items, history_id, destination_position);
            self.mark_list_items_loaded([history_id]);
        }
        self.sync_loaded_shortcuts();
        self.has_more_items = self.loaded_items.len() < self.total_item_count;
        self.list_status = ListStatus::Ready;
        true
    }

    pub(super) fn refresh_list_item(
        &mut self,
        list_item_id: HistoryItemId,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.list_status, ListStatus::Ready | ListStatus::Empty) {
            self.refresh_history(cx);
            return;
        }
        let generation = self.query_generation;
        let policy = crate::config_runtime::history_config(cx);
        let query = self.page_query(0, 1);
        let active_tab = self.active_tab;
        let repository = self.history_repository.clone();
        let favorite_repository = self.favorite_repository.clone();
        let requested_order = self.list_order_ids.clone();
        let query_task = cx.background_spawn(async move {
            let record = if active_tab == TabId::Favorites {
                favorite_repository.find_by_id(list_item_id, &query)?
            } else {
                repository.find_by_id_scoped(list_item_id, &query, &policy)?
            };
            let snapshot = if active_tab == TabId::Favorites {
                favorite_repository.ordered_snapshot(&query)?
            } else {
                repository.ordered_snapshot_scoped(&query, &policy)?
            };
            Result::<(Option<HistoryListRecord>, ListOrderSnapshot), HistoryQueryError>::Ok((
                record, snapshot,
            ))
        });
        cx.spawn(async move |this, cx| {
            let result = query_task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| {
                    if generation != this.query_generation || this.active_tab != active_tab {
                        return;
                    }
                    match result {
                        Ok((record, snapshot)) => {
                            if !this.apply_list_record(
                                list_item_id,
                                record,
                                &requested_order,
                                snapshot,
                            ) {
                                this.refresh_list_item(list_item_id, cx);
                                return;
                            }
                        }
                        Err(error) => tracing::error!("列表增量更新失败：{error}"),
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    pub(super) fn reconcile_selection(&mut self) {
        self.multi_selected_item_ids
            .retain(|history_id| self.list_order_positions.contains_key(history_id));
        if self.multi_selected_item_ids.is_empty() {
            self.multi_selection_anchor = None;
        }
        for item in &mut self.items {
            item.is_selected = Some(item.history_id) == self.selected_item_id;
        }
        self.selected_item_id = self
            .selected_item_id
            .filter(|history_id| self.list_order_positions.contains_key(history_id));
    }

    pub(super) fn sync_loaded_shortcuts(&mut self) {
        for item in &self.items {
            if let Some(loaded_item) = self.loaded_items.get_mut(&item.history_id) {
                loaded_item.shortcut.clone_from(&item.shortcut);
            }
        }
    }

    pub(super) fn history_query(&self) -> HistoryQuery {
        let mut query =
            HistoryQuery::from_filters(self.filters.active_filters().map(|filter| match filter {
                FilterId::Text => HistoryFilter::Text,
                FilterId::Image => HistoryFilter::Image,
                FilterId::File => HistoryFilter::File,
                FilterId::Link => HistoryFilter::Link,
                FilterId::Unpasted => HistoryFilter::Unpasted,
                FilterId::Pasted => HistoryFilter::Pasted,
            }));
        query.search = (!self.search_query.trim().is_empty()).then(|| self.search_query.clone());
        query.tag_ids = self.selected_tag_ids.iter().copied().collect::<Vec<_>>();
        query.tag_ids.sort_unstable();
        query.tag_match_all = self.tags_match_all;
        query
    }

    pub(super) fn page_query(&self, offset: usize, page_size: usize) -> HistoryQuery {
        let mut query = self.history_query();
        query.offset = u32::try_from(offset).unwrap_or(u32::MAX);
        query.limit = u32::try_from(page_size.saturating_add(1))
            .unwrap_or(u32::MAX)
            .min((MAX_QUERY_PAGE_SIZE + 1) as u32);
        query.preview_char_limit = Some(LIST_PREVIEW_CHAR_LIMIT);
        query.load_image_sources = true;
        query.load_html_content = true;
        query.html_content_char_limit = Some(LIST_PREVIEW_CHAR_LIMIT);
        query
    }

    pub(super) fn accept_query_result(
        &mut self,
        generation: u64,
        result: Result<(ListOrderSnapshot, Vec<HistoryListRecord>), HistoryQueryError>,
    ) -> bool {
        if generation != self.query_generation {
            return false;
        }
        match result {
            Ok((snapshot, records)) => {
                self.total_item_count = snapshot.ids.len();
                self.list_heatmap.replace(snapshot.paste_counts);
                self.list_order_ids = snapshot.ids;
                self.rebuild_list_order_positions();
                let (records, has_more) = split_page(records, INITIAL_PAGE_SIZE);
                let items = records.into_iter().map(record_to_list_item).collect();
                self.is_loading_more = false;
                self.replace_items(items);
                self.has_more_items = has_more || self.loaded_items.len() < self.total_item_count;
            }
            Err(error) => {
                self.items.clear();
                self.loaded_items.clear();
                self.list_order_ids.clear();
                self.list_order_positions.clear();
                self.list_heatmap.replace(Vec::new());
                self.clear_image_cache();
                self.clear_file_list_cache();
                self.total_item_count = 0;
                self.selected_item_id = None;
                self.has_more_items = false;
                self.is_loading_more = false;
                tracing::error!("剪贴板列表查询失败：{error}");
                self.list_status = ListStatus::Error;
            }
        }
        true
    }

    pub(super) fn accept_page_result(
        &mut self,
        generation: u64,
        page_size: usize,
        animate_loaded: bool,
        result: Result<Vec<HistoryListRecord>, HistoryQueryError>,
    ) -> Option<(bool, Range<usize>)> {
        if generation != self.query_generation {
            return None;
        }
        self.is_loading_more = false;
        let page_range = self.loading_page_range.take().unwrap_or_default();
        let page_loaded = match result {
            Ok(records) => {
                let page_loaded = !records.is_empty();
                let (records, _) = split_page(records, page_size);
                let mut newly_loaded = Vec::new();
                for (record_offset, record) in records.into_iter().enumerate() {
                    if let Some(index) = self.list_order_positions.get(&record.history_id).copied()
                    {
                        self.list_heatmap.update(index, record.paste_count);
                    }
                    let mut item = record_to_list_item(record);
                    item.shortcut = format!(
                        "Ctrl+{}",
                        page_range
                            .start
                            .saturating_add(record_offset)
                            .saturating_add(1)
                    );
                    let history_id = item.history_id;
                    let was_loaded = self.loaded_items.contains_key(&history_id);
                    self.loaded_items.insert(item.history_id, item.clone());
                    self.items
                        .retain(|existing| existing.history_id != item.history_id);
                    insert_sorted_item(&mut self.items, item);
                    if !was_loaded {
                        newly_loaded.push(history_id);
                    }
                }
                if animate_loaded {
                    self.mark_list_items_loaded(newly_loaded);
                } else {
                    for history_id in newly_loaded {
                        self.list_item_load_animations.remove(&history_id);
                    }
                }
                self.has_more_items = self.loaded_items.len() < self.total_item_count;
                self.sync_loaded_shortcuts();
                self.reconcile_selection();
                page_loaded
            }
            Err(error) => {
                tracing::error!("剪贴板列表分页加载失败：{error}");
                false
            }
        };
        Some((page_loaded, page_range))
    }

    pub(super) fn apply_list_record(
        &mut self,
        list_item_id: HistoryItemId,
        record: Option<HistoryListRecord>,
        requested_order: &[HistoryItemId],
        snapshot: ListOrderSnapshot,
    ) -> bool {
        if self.list_drag_source_id.is_some() {
            return true;
        }
        let was_loaded = self.loaded_items.contains_key(&list_item_id);
        if !replace_list_order(
            &mut self.list_order_ids,
            &mut self.list_heatmap,
            &mut self.total_item_count,
            requested_order,
            snapshot,
        ) {
            return false;
        }
        self.rebuild_list_order_positions();
        // 单条读取和快照之间可能又发生捕获，以快照范围为准。
        let updated_item = record
            .filter(|_| self.list_order_positions.contains_key(&list_item_id))
            .map(record_to_list_item);
        let has_record = updated_item.is_some();
        for item_id in retain_list_records_for_order(
            &mut self.items,
            &mut self.loaded_items,
            &self.list_order_positions,
        ) {
            self.invalidate_image_cache_item(item_id);
        }
        self.adaptive_item_heights
            .retain(|history_id, _| self.list_order_positions.contains_key(history_id));
        self.list_item_load_animations
            .retain(|history_id, _| self.list_order_positions.contains_key(history_id));
        self.invalidate_list_layout_cache();
        // 被更新或删除的内容可能改变预览高度，其他行的缓存可以继续复用。
        self.adaptive_item_heights.remove(&list_item_id);
        match updated_item {
            Some(item)
                if self
                    .items
                    .iter()
                    .find(|existing| existing.history_id == list_item_id)
                    .is_some_and(|existing| {
                        compare_list_items(existing, &item) == Ordering::Equal
                    }) =>
            {
                self.apply_loaded_list_item(item);
            }
            None if !self
                .items
                .iter()
                .any(|item| item.history_id == list_item_id) =>
            {
                // 目标未加载，快照的范围变化已在上方应用。
            }
            updated_item => {
                let previous_item_ids: HashSet<_> =
                    self.items.iter().map(|item| item.item_id).collect();
                let loaded_prefix = self.loaded_items.len() == self.items.len()
                    && self
                        .items
                        .iter()
                        .all(|item| self.loaded_items.contains_key(&item.history_id));
                let previous_len = self.items.len();
                self.items.retain(|item| item.history_id != list_item_id);
                if let Some(item) = updated_item {
                    insert_sorted_item(&mut self.items, item);
                    if self.has_more_items && previous_len > 0 && self.items.len() > previous_len {
                        self.items.pop();
                    }
                }
                let current_item_ids: HashSet<_> =
                    self.items.iter().map(|item| item.item_id).collect();
                for item_id in previous_item_ids.difference(&current_item_ids) {
                    self.invalidate_image_cache_item(*item_id);
                }
                refresh_shortcuts_for_order(&mut self.items, &self.list_order_ids);
                if loaded_prefix {
                    self.loaded_items = self
                        .items
                        .iter()
                        .cloned()
                        .map(|item| (item.history_id, item))
                        .collect();
                } else {
                    self.loaded_items.clear();
                }
                if has_record && !was_loaded {
                    self.mark_list_items_loaded([list_item_id]);
                }
            }
        }
        reconcile_snapshot_selection(
            &mut self.selected_item_id,
            &mut self.items,
            &self.list_order_positions,
        );
        self.has_more_items = self.loaded_items.len() < self.total_item_count;
        self.reconcile_selection();
        self.list_status = if self.total_item_count == 0 {
            ListStatus::Empty
        } else {
            ListStatus::Ready
        };
        true
    }
}
