use super::*;

#[derive(Default)]
pub(super) struct EmojiResources {
    svgs: HashMap<String, String>,
    images: HashMap<usize, Arc<RenderImage>>,
    visible: HashSet<usize>,
    failures: HashSet<usize>,
    load_task: Option<Task<()>>,
    image_task: Option<Task<()>>,
}

impl MainWindow {
    pub(crate) fn ensure_emoji_resources(&mut self, cx: &mut Context<Self>) {
        if self.emoji_resources.is_some() || !MainWindowService::is_logically_visible(cx) {
            return;
        }
        self.emoji_resources = Some(EmojiResources::default());
        let load = cx.background_spawn(async { crate::assets::Assets::load_emoji_images() });
        let task = cx.spawn(async move |this, cx| {
            let result = load.await;
            let _ = this.update(cx, |this, cx| {
                let Some(resources) = this.emoji_resources.as_mut() else {
                    return;
                };
                resources.load_task = None;
                match result {
                    Ok(svgs) => resources.svgs = svgs,
                    Err(error) => tracing::error!("加载表情图片资源失败：{error}"),
                }
                cx.notify();
            });
        });
        self.emoji_resources.as_mut().unwrap().load_task = Some(task);
    }

    pub(crate) fn emoji_image(&self, index: usize) -> Option<Arc<RenderImage>> {
        self.emoji_resources.as_ref()?.images.get(&index).cloned()
    }

    pub(crate) fn ensure_emoji_images(
        &mut self,
        indices: HashSet<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !MainWindowService::is_logically_visible(cx) {
            return;
        }
        let Some(resources) = self.emoji_resources.as_mut() else {
            return;
        };
        if resources.visible != indices {
            // 取消旧视口的回写任务；后台栅格化结束后，其结果随任务一起丢弃。
            resources.image_task = None;
            resources.visible = indices;
            resources.images.retain(|index, image| {
                if resources.visible.contains(index) {
                    true
                } else {
                    cx.drop_image(image.clone(), Some(window));
                    false
                }
            });
            resources
                .failures
                .retain(|index| resources.visible.contains(index));
        }
        if resources.image_task.is_some() {
            return;
        }
        let data = crate::emoji::data();
        let requests = resources
            .visible
            .iter()
            .filter_map(|index| {
                if resources.images.contains_key(index) || resources.failures.contains(index) {
                    return None;
                }
                let key = crate::emoji::annotation_key(&data.items[*index].glyph);
                resources.svgs.get(&key).map(|svg| (*index, svg.clone()))
            })
            .collect::<Vec<_>>();
        if requests.is_empty() {
            return;
        }
        let renderer = cx.svg_renderer();
        // 只把本次可见图片的 SVG 交给后台任务，避免任务持有整个解压包。
        let load = cx.background_spawn(async move {
            requests
                .into_iter()
                .map(|(index, svg)| (index, renderer.render_single_frame(svg.as_bytes(), 1.0)))
                .collect::<Vec<_>>()
        });
        let task = cx.spawn(async move |this, cx| {
            let images = load.await;
            let _ = this.update(cx, |this, cx| {
                let Some(resources) = this.emoji_resources.as_mut() else {
                    return;
                };
                resources.image_task = None;
                for (index, result) in images {
                    if !resources.visible.contains(&index) {
                        continue;
                    }
                    match result {
                        Ok(image) => {
                            resources.images.insert(index, image);
                        }
                        Err(error) => {
                            resources.failures.insert(index);
                            tracing::error!("渲染表情图片失败：{error}");
                        }
                    }
                }
                cx.notify();
            });
        });
        resources.image_task = Some(task);
    }

    pub(crate) fn clear_emoji_resources(&mut self, window: &mut Window, cx: &mut App) {
        if let Some(resources) = self.emoji_resources.take() {
            // 释放 SVG、加载任务及 CPU 图片，并移除窗口内对应的纹理。
            for image in resources.images.into_values() {
                cx.drop_image(image, Some(window));
            }
            self.emoji_rows_cache = None;
        }
    }
}
