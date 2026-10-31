use crate::ui::scale::ui_rem;

use std::{collections::HashMap, ops::Range, path::PathBuf, rc::Rc, sync::Arc, time::Duration};

use gpui::ColorExt as _;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, ClickEvent, Context, Entity, FocusHandle, FontWeight, ListAlignment,
    ListState, MouseButton, MouseDownEvent, MouseMoveEvent, ObjectFit, Pixels, Point, Render,
    RenderImage, ScrollWheelEvent, SharedString, Stateful, Subscription, Task,
    UniformListScrollHandle, Window, div, img, list, px, size, uniform_list,
};
use gpui_component::{
    ActiveTheme as _,
    input::{InputEvent, Textarea, TextareaState},
    scroll::ScrollableElement,
    tooltip::Tooltip,
    v_flex,
};

use super::{
    CAPSULE_HEIGHT, CAPSULE_WIDTH, HEADER_HEIGHT, MIN_WIDTH, StickyData, StickyMode, StickyService,
    image_view::ImageView,
    palette::{Paper, hsla_to_rgb},
    selection::TokenSelection,
};
use crate::{
    domain::clipboard::{ClipboardItemId, FormatPreview},
    i18n,
    services::word_split::{self, JoinSeparator, SplitGranularity, SplitResult, TokenKind},
    state_runtime::PersistedStickyNote,
    ui::{
        html::{self as html_preview, HtmlDocument},
        icon,
    },
    windows::menu::{MenuActionId, MenuIcon, MenuItem, MenuModel, MenuPosition, MenuService},
};

const PERSIST_DELAY: Duration = Duration::from_millis(400);
const TOAST_DURATION: Duration = Duration::from_millis(1400);
const FLASH_DURATION: Duration = Duration::from_millis(700);
const WINDOW_DRAG_THRESHOLD: f32 = 4.0;
/// 拆词区域的内边距、词块间距和词块自身的水平留白（含边框）。
const TOKEN_AREA_PADDING: f32 = 10.0;
const TOKEN_GAP: f32 = 5.0;
const TOKEN_CHIP_EXTRA: f32 = 7.0 * 2.0 + 2.0;
/// 给滚动条预留的宽度，避免最右侧词块被遮挡。
const TOKEN_SCROLLBAR_GUTTER: f32 = 12.0;
const IMAGE_MAX_SIZE: (u32, u32) = (4096, 4096);
const FILE_ICON_SIZE: u16 = 32;
const OPACITY_LEVELS: [u8; 5] = [100, 90, 80, 65, 50];

enum Resource<T> {
    Idle,
    Loading,
    Ready(T),
    Failed,
}

struct LoadedImage {
    image: Arc<RenderImage>,
    dimensions: Option<(u32, u32)>,
}

struct FileEntry {
    path: PathBuf,
    name: String,
    detail: Option<String>,
    is_directory: bool,
    exists: bool,
    icon: Option<Arc<RenderImage>>,
}

struct SplitView {
    result: SplitResult,
    selection: TokenSelection,
    /// 按当前宽度排好的词块行，宽度或缩放变化后重新计算。
    layout: Option<TokenLayout>,
}

struct TokenLayout {
    width: f32,
    rem_size: f32,
    rows: Rc<Vec<Range<usize>>>,
}

/// 词块文字宽度按字符缓存，排版超长拆词结果时不必逐个整词排版。
#[derive(Default)]
struct CharWidths {
    rem_size: f32,
    widths: HashMap<char, f32>,
}

pub(super) struct StickyNote {
    item_id: ClipboardItemId,
    modes: Vec<(StickyMode, FormatPreview)>,
    mode_index: usize,
    text: String,
    /// 用户选择的便签颜色；为空时跟随主题。
    color: Option<u32>,
    topmost: bool,
    collapsed: bool,
    opacity: f32,
    /// 展开状态下的窗口尺寸，折叠成胶囊后用于恢复。
    expanded_size: gpui::Size<f32>,
    /// 按创建时请求的尺寸换算后的窗口位置；无边框窗口实际外框比请求值多出缩放边框。
    bounds: Bounds<Pixels>,
    frame_offset: Point<Pixels>,
    frame_extra: gpui::Size<Pixels>,
    image: Resource<LoadedImage>,
    image_view: ImageView,
    /// 图片区域在窗口内的位置和尺寸，滚轮缩放时用来计算锚点。
    image_viewport: Rc<std::cell::Cell<Bounds<Pixels>>>,
    /// 拖动平移时上一次的鼠标位置。
    image_drag: Option<Point<Pixels>>,
    /// 鼠标停在角落提示上时隐藏提示，移开后恢复。
    image_hint_hovered: bool,
    files: Resource<Vec<FileEntry>>,
    html_document: Option<Rc<HtmlDocument>>,
    html_images: Option<Rc<HashMap<PathBuf, Arc<RenderImage>>>>,
    html_list: ListState,
    /// 文本始终可以直接编辑，内容与已保存文本不同时才显示保存栏。
    text_state: Entity<TextareaState>,
    /// 根元素的焦点句柄，点击空白处时接管焦点使输入框失焦。
    focus_handle: FocusHandle,
    dirty: bool,
    /// 有未保存修改时第一次关闭只提示，再次关闭才放弃修改。
    close_armed: bool,
    saving: bool,
    save_error: bool,
    split: Option<SplitView>,
    token_scroll: UniformListScrollHandle,
    char_widths: CharWidths,
    granularity: SplitGranularity,
    include_symbols: bool,
    separator: JoinSeparator,
    header_drag_start: Option<Point<Pixels>>,
    toast: Option<SharedString>,
    flash: bool,
    _toast_task: Option<Task<()>>,
    _flash_task: Option<Task<()>>,
    _persist_task: Option<Task<()>>,
    _bounds_subscription: Subscription,
    _text_subscription: Subscription,
}

impl StickyNote {
    pub(super) fn new(
        data: StickyData,
        note: PersistedStickyNote,
        requested: Bounds<Pixels>,
        expanded_size: gpui::Size<f32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let actual = window.bounds();
        let frame_offset = actual.origin - requested.origin;
        let frame_extra = gpui::Size {
            width: actual.size.width - requested.size.width,
            height: actual.size.height - requested.size.height,
        };
        let bounds_subscription = cx.observe_window_bounds(window, |this, window, cx| {
            this.bounds = this.requested_bounds(window);
            if !this.collapsed {
                this.expanded_size = size(
                    f32::from(this.bounds.size.width),
                    f32::from(this.bounds.size.height),
                );
            }
            this.schedule_persist(cx);
        });
        let text = data.text.unwrap_or_default();
        let text_state = cx.new(|cx| {
            TextareaState::new(window, cx)
                .default_value(text.clone())
                .soft_wrap(true)
                .context_menu(false)
        });
        let text_subscription =
            cx.subscribe_in(&text_state, window, |this, state, event, window, cx| {
                if matches!(event, InputEvent::Blur) {
                    // 输入框失焦表示不再键盘操作，把前台还给之前的窗口并恢复不抢焦点。
                    // 焦点已切到其他便签的输入框时会话已属于新窗口，这里不能把前台抢回来。
                    if let Err(error) = crate::services::focus::restore_previous_focus_for(window) {
                        tracing::debug!(%error, "便签输入框失焦时恢复焦点失败");
                    }
                    return;
                }
                if !matches!(event, InputEvent::Change) {
                    return;
                }
                let dirty = {
                    let current = state.read(cx).text();
                    current.len() != this.text.len() || *current != this.text.as_str()
                };
                this.close_armed = false;
                if dirty != this.dirty {
                    this.dirty = dirty;
                    this.save_error = false;
                    cx.notify();
                }
            });
        let mut this = Self {
            item_id: data.item_id,
            modes: data.modes,
            mode_index: 0,
            text,
            color: note.color,
            topmost: note.topmost,
            collapsed: note.collapsed,
            opacity: note.opacity.clamp(0.3, 1.0),
            expanded_size,
            bounds: requested,
            frame_offset,
            frame_extra,
            image: Resource::Idle,
            image_view: ImageView::default(),
            image_viewport: Rc::default(),
            image_drag: None,
            image_hint_hovered: false,
            files: Resource::Idle,
            html_document: None,
            html_images: None,
            html_list: ListState::new(0, ListAlignment::Top, px(1024.0)),
            text_state,
            focus_handle: cx.focus_handle(),
            dirty: false,
            close_armed: false,
            saving: false,
            save_error: false,
            split: None,
            token_scroll: UniformListScrollHandle::new(),
            char_widths: CharWidths::default(),
            granularity: SplitGranularity::default(),
            include_symbols: false,
            separator: JoinSeparator::default(),
            header_drag_start: None,
            toast: None,
            flash: false,
            _toast_task: None,
            _flash_task: None,
            _persist_task: None,
            _bounds_subscription: bounds_subscription,
            _text_subscription: text_subscription,
        };
        // 拆词只作用于文本；条目优先显示其他格式时也切到文本再拆词。
        if note.split
            && let Some(index) = this
                .modes
                .iter()
                .position(|(mode, _)| *mode == StickyMode::Text)
        {
            this.mode_index = index;
            this.enter_split();
        }
        this.load_resources(cx);
        if this.collapsed {
            lock_resize(window, true);
        }
        this
    }

    pub(super) fn persisted(&self) -> PersistedStickyNote {
        PersistedStickyNote {
            item_id: self.item_id,
            x: f32::from(self.bounds.origin.x),
            y: f32::from(self.bounds.origin.y),
            width: self.expanded_size.width,
            height: self.expanded_size.height,
            color: self.color,
            topmost: self.topmost,
            collapsed: self.collapsed,
            opacity: self.opacity,
            split: self.split.is_some(),
        }
    }

    pub(super) fn flash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.collapsed {
            self.toggle_collapsed(window, cx);
        }
        self.flash = true;
        self._flash_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FLASH_DURATION).await;
            let _ = this.update(cx, |this, cx| {
                this.flash = false;
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// 把实际窗口外框换回创建窗口时使用的坐标体系，保存后再次打开不会逐渐变大或偏移。
    fn requested_bounds(&self, window: &Window) -> Bounds<Pixels> {
        let actual = window.bounds();
        Bounds {
            origin: actual.origin - self.frame_offset,
            size: gpui::Size {
                width: actual.size.width - self.frame_extra.width,
                height: actual.size.height - self.frame_extra.height,
            },
        }
    }

    fn mode(&self) -> Option<StickyMode> {
        self.modes.get(self.mode_index).map(|(mode, _)| *mode)
    }

    fn preview(&self) -> Option<&FormatPreview> {
        self.modes.get(self.mode_index).map(|(_, preview)| preview)
    }

    fn schedule_persist(&mut self, cx: &mut Context<Self>) {
        self._persist_task = Some(cx.spawn(async move |_, cx| {
            cx.background_executor().timer(PERSIST_DELAY).await;
            cx.update(StickyService::persist);
        }));
    }

    fn show_toast(&mut self, key: &str, cx: &mut Context<Self>) {
        self.toast = Some(i18n::translator(cx).translate(key).into());
        self._toast_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TOAST_DURATION).await;
            let _ = this.update(cx, |this, cx| {
                this.toast = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn set_mode(&mut self, index: usize, cx: &mut Context<Self>) {
        if index == self.mode_index || index >= self.modes.len() || self.block_if_dirty(cx) {
            return;
        }
        self.mode_index = index;
        if self.mode() != Some(StickyMode::Text) {
            self.split = None;
        }
        self.load_resources(cx);
        self.schedule_persist(cx);
        cx.notify();
    }

    /// 按当前模式懒加载图片、文件列表和 HTML 本地图片。
    fn load_resources(&mut self, cx: &mut Context<Self>) {
        let Some(preview) = self.preview().cloned() else {
            return;
        };
        let services = StickyService::services(cx);
        match self.mode() {
            Some(StickyMode::Image) if matches!(self.image, Resource::Idle) => {
                let Some(source) = preview.image_source.clone() else {
                    self.image = Resource::Failed;
                    return;
                };
                self.image = Resource::Loading;
                let service = services.image_preview_service;
                let task = cx.background_spawn(async move {
                    service.load_with_dimensions_hint(
                        &source,
                        preview.format_kind,
                        &preview.format_key,
                        IMAGE_MAX_SIZE,
                        preview.image_dimensions,
                    )
                });
                cx.spawn(async move |this, cx| {
                    let result = task.await;
                    let _ = this.update(cx, |this, cx| {
                        this.image = match result {
                            Ok(loaded) => Resource::Ready(LoadedImage {
                                image: loaded.image,
                                dimensions: loaded
                                    .dimensions
                                    .map(|dimensions| (dimensions.width, dimensions.height)),
                            }),
                            Err(error) => {
                                tracing::warn!(%error, "加载便签图片失败");
                                Resource::Failed
                            }
                        };
                        cx.notify();
                    });
                })
                .detach();
            }
            Some(StickyMode::File) if matches!(self.files, Resource::Idle) => {
                let Some(source) = preview.image_source.clone() else {
                    self.files = Resource::Failed;
                    return;
                };
                self.files = Resource::Loading;
                let service = services.image_preview_service;
                let task = cx.background_spawn(async move {
                    service
                        .load_file_list_paths(&source)
                        .map(|paths| paths.into_iter().map(load_file_entry).collect::<Vec<_>>())
                });
                cx.spawn(async move |this, cx| {
                    let result = task.await;
                    let _ = this.update(cx, |this, cx| {
                        this.files = match result {
                            Ok(entries) => Resource::Ready(entries),
                            Err(error) => {
                                tracing::warn!(%error, "加载便签文件列表失败");
                                Resource::Failed
                            }
                        };
                        cx.notify();
                    });
                })
                .detach();
            }
            Some(StickyMode::Html) if self.html_document.is_none() => {
                let Some(html) = preview.html_content.clone() else {
                    return;
                };
                let document = HtmlDocument::parse(&html, px(14.0), px(22.0), &preview.text);
                self.html_list.reset(document.len());
                self.html_document = Some(Rc::new(document));
                self.html_images = Some(Rc::new(HashMap::new()));
                let paths = html_preview::local_image_paths(&html);
                if paths.is_empty() {
                    return;
                }
                let service = services.image_preview_service;
                let task = cx.background_spawn(async move {
                    paths
                        .into_iter()
                        .filter_map(|path| {
                            service
                                .load_local_path(&path, IMAGE_MAX_SIZE)
                                .ok()
                                .map(|image| (path, image))
                        })
                        .collect::<HashMap<_, _>>()
                });
                cx.spawn(async move |this, cx| {
                    let images = task.await;
                    let _ = this.update(cx, |this, cx| {
                        this.html_images = Some(Rc::new(images));
                        // 图片尺寸变化后需要重新测量条目高度。
                        let count = this.html_list.item_count();
                        this.html_list.reset(count);
                        cx.notify();
                    });
                })
                .detach();
            }
            _ => {}
        }
    }

    fn title(&self, translator: &i18n::Translator) -> String {
        match self.mode() {
            Some(StickyMode::Text) => {
                first_line(&self.text).unwrap_or_else(|| translator.translate("sticky.mode.text"))
            }
            Some(StickyMode::Html) => self
                .preview()
                .and_then(|preview| first_line(&preview.text))
                .unwrap_or_else(|| translator.translate("sticky.mode.html")),
            Some(StickyMode::Image) => {
                let dimensions = match &self.image {
                    Resource::Ready(image) => image.dimensions,
                    _ => self
                        .preview()
                        .and_then(|preview| preview.image_dimensions)
                        .map(|dimensions| (dimensions.width, dimensions.height)),
                };
                match dimensions {
                    Some((width, height)) => translator.translate_with_args(
                        "sticky.title.image",
                        &[("width", width.to_string()), ("height", height.to_string())],
                    ),
                    None => translator.translate("sticky.mode.image"),
                }
            }
            Some(StickyMode::File) => match &self.files {
                Resource::Ready(entries) if entries.len() == 1 => entries[0].name.clone(),
                Resource::Ready(entries) => translator.translate_with_args(
                    "sticky.title.files",
                    &[("count", entries.len().to_string())],
                ),
                _ => translator.translate("sticky.mode.file"),
            },
            None => translator.translate("sticky.empty"),
        }
    }

    // ---- 窗口行为 ----

    /// 折叠成只显示标题的胶囊；胶囊不能拖动边缘缩放，展开时恢复原尺寸。
    ///
    /// 胶囊沿用系统窗口圆角而不另行裁剪：窗口材质、边框与阴影都按系统圆角绘制，
    /// 自定义形状会与它们对不齐。
    fn toggle_collapsed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.collapsed {
            self.collapsed = false;
            lock_resize(window, false);
            window.resize(size(
                px(self.expanded_size.width.max(MIN_WIDTH)),
                px(self.expanded_size.height.max(HEADER_HEIGHT * 2.0)),
            ));
        } else {
            let current = self.requested_bounds(window).size;
            self.expanded_size = size(f32::from(current.width), f32::from(current.height));
            self.collapsed = true;
            window.resize(size(px(CAPSULE_WIDTH), px(CAPSULE_HEIGHT)));
            lock_resize(window, true);
        }
        self.schedule_persist(cx);
        cx.notify();
    }

    fn toggle_topmost(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.topmost = !self.topmost;
        if let Err(error) = crate::platform::window::set_window_topmost(window, self.topmost) {
            tracing::error!("切换便签置顶失败：{error}");
        }
        self.schedule_persist(cx);
        cx.notify();
    }

    fn set_color(&mut self, color: Option<u32>, cx: &mut Context<Self>) {
        self.color = color;
        self.schedule_persist(cx);
        cx.notify();
    }

    /// 打开颜色选择窗口；选中后便签整体配色按该颜色重新生成。
    fn pick_color(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let initial = self
            .color
            .unwrap_or_else(|| hsla_to_rgb(Paper::resolve(None, cx).accent));
        let view = cx.entity().downgrade();
        let result = crate::windows::color_picker::open_color_picker_window_with_reset(
            Some(window),
            initial,
            i18n::translator(cx).translate("sticky.color.theme"),
            move |color, cx| {
                if let Some(view) = view.upgrade() {
                    view.update(cx, |this, cx| this.set_color(color, cx));
                }
            },
            cx,
        );
        match result {
            // 便签通常置顶，弹窗也要提到置顶层，否则会被便签挡住。
            Ok(handle) => {
                let _ = handle.update(cx, |_, window, _| {
                    let _ = crate::platform::window::raise_window_to_topmost(window);
                });
            }
            Err(error) => tracing::error!("打开便签颜色窗口失败：{error}"),
        }
    }

    fn set_opacity(&mut self, opacity: f32, window: &mut Window, cx: &mut Context<Self>) {
        self.opacity = opacity.clamp(0.3, 1.0);
        if let Err(error) = crate::platform::window::set_window_opacity(window, self.opacity) {
            tracing::error!("设置便签透明度失败：{error}");
        }
        self.schedule_persist(cx);
        cx.notify();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dirty && !self.close_armed {
            self.close_armed = true;
            self.show_toast("sticky.unsaved_close", cx);
            return;
        }
        let _ = crate::services::focus::restore_previous_focus_for(window);
        window.remove_window();
        cx.notify();
    }

    fn image_size(&self) -> Option<gpui::Size<f32>> {
        let Resource::Ready(image) = &self.image else {
            return None;
        };
        let pixels = image.image.size(0);
        Some(size(pixels.width.0 as f32, pixels.height.0 as f32))
    }

    /// 滚轮缩放图片本身，鼠标所在位置保持不动。
    fn zoom_image_content(
        &mut self,
        delta_y: f32,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(image) = self.image_size() else {
            return;
        };
        let viewport = self.image_viewport.get();
        if self
            .image_view
            .zoom_at(delta_y, position - viewport.origin, viewport.size, image)
        {
            cx.notify();
        }
    }

    fn reset_image_view(&mut self, cx: &mut Context<Self>) {
        if self.image_view != ImageView::default() {
            self.image_view = ImageView::default();
            cx.notify();
        }
    }

    /// Ctrl+滚轮按比例缩放整个窗口。
    fn zoom_image(&mut self, delta_y: f32, window: &mut Window, cx: &mut Context<Self>) {
        if delta_y == 0.0 || self.collapsed {
            return;
        }
        let factor = if delta_y > 0.0 { 1.1 } else { 1.0 / 1.1 };
        let current = self.requested_bounds(window).size;
        let body_width = f32::from(current.width);
        let body_height = (f32::from(current.height) - HEADER_HEIGHT).max(1.0);
        let width = (body_width * factor).clamp(MIN_WIDTH, 3000.0);
        let applied = width / body_width;
        let height = (body_height * applied).clamp(48.0, 3000.0) + HEADER_HEIGHT;
        window.resize(size(px(width), px(height)));
        cx.notify();
    }

    // ---- 复制与粘贴 ----

    /// 文本模式下有选中内容时只处理选中部分，否则处理全文。
    fn text_for_clipboard(&self, cx: &Context<Self>) -> Option<String> {
        if self.mode() != Some(StickyMode::Text) {
            return None;
        }
        let state = self.text_state.read(cx);
        let selected = state.selected_value();
        Some(if selected.is_empty() {
            state.value().to_string()
        } else {
            selected.to_string()
        })
    }

    fn copy_content(&mut self, cx: &mut Context<Self>) {
        let services = StickyService::services(cx);
        let item_id = self.item_id;
        let text = self.text_for_clipboard(cx);
        let task = cx.background_spawn(async move {
            match text {
                Some(text) => services.paste_service.copy_text(&text),
                None => services.paste_service.copy_item(item_id),
            }
        });
        self.finish_clipboard_task(task, "sticky.copied", cx);
    }

    fn paste_content(&mut self, window: &Window, cx: &mut Context<Self>) {
        let services = StickyService::services(cx);
        let item_id = self.item_id;
        let text = self.text_for_clipboard(cx);
        // 只在本便签输入框持有焦点时交还给之前的窗口；其他便签或主窗口的输入框持有焦点时
        // 前台就是粘贴目标，不能恢复那个窗口保存的旧目标。
        let _ = crate::services::focus::restore_previous_focus_for(window);
        let task = cx.background_spawn(async move {
            match text {
                Some(text) => services.paste_service.paste_text(&text),
                None => services.paste_service.paste_item(item_id, None),
            }
        });
        self.finish_clipboard_task(task, "sticky.pasted", cx);
    }

    fn copy_selected_tokens(&mut self, paste: bool, window: &Window, cx: &mut Context<Self>) {
        let Some(split) = self.split.as_ref() else {
            return;
        };
        let tokens = split
            .selection
            .selected_indices()
            .filter_map(|index| split.result.tokens.get(index));
        let text = word_split::join_tokens(tokens, self.separator);
        if text.is_empty() {
            return;
        }
        let services = StickyService::services(cx);
        if paste {
            let _ = crate::services::focus::restore_previous_focus_for(window);
        }
        let task = cx.background_spawn(async move {
            if paste {
                services.paste_service.paste_text(&text)
            } else {
                services.paste_service.copy_text(&text)
            }
        });
        self.finish_clipboard_task(
            task,
            if paste {
                "sticky.pasted"
            } else {
                "sticky.copied"
            },
            cx,
        );
    }

    fn finish_clipboard_task(
        &mut self,
        task: Task<Result<(), crate::services::paste::PasteError>>,
        success_key: &'static str,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => this.show_toast(success_key, cx),
                Err(error) => {
                    tracing::error!("便签剪贴板操作失败：{error}");
                    this.show_toast("sticky.action_failed", cx);
                }
            });
        })
        .detach();
    }

    // ---- 拆词 ----

    fn enter_split(&mut self) {
        let result = word_split::split(&self.text, self.granularity, self.include_symbols);
        let selection = TokenSelection::new(result.tokens.len());
        self.split = Some(SplitView {
            result,
            selection,
            layout: None,
        });
    }

    /// 从主窗口发起拆词时切到拆词界面；文本有未保存修改或不是文本便签时保持原样。
    pub(super) fn show_split(&mut self, cx: &mut Context<Self>) {
        if self.split.is_some() || self.block_if_dirty(cx) {
            return;
        }
        let Some(index) = self
            .modes
            .iter()
            .position(|(mode, _)| *mode == StickyMode::Text)
        else {
            return;
        };
        if index != self.mode_index {
            self.mode_index = index;
            self.load_resources(cx);
        }
        self.enter_split();
        self.schedule_persist(cx);
        cx.notify();
    }

    fn toggle_split(&mut self, cx: &mut Context<Self>) {
        if self.block_if_dirty(cx) {
            return;
        }
        if self.split.is_some() {
            self.split = None;
        } else {
            self.enter_split();
        }
        self.schedule_persist(cx);
        cx.notify();
    }

    fn resplit(&mut self, cx: &mut Context<Self>) {
        if self.split.is_some() {
            self.enter_split();
        }
        cx.notify();
    }

    // ---- 编辑 ----

    /// 有未保存修改时拦下会替换正文显示的操作，避免修改被悄悄丢掉。
    fn block_if_dirty(&mut self, cx: &mut Context<Self>) -> bool {
        if self.dirty {
            self.show_toast("sticky.unsaved_blocked", cx);
        }
        self.dirty
    }

    /// 便签默认不抢焦点；需要键盘操作文本时临时激活窗口。
    fn focus_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(error) = crate::services::focus::focus_input_window(window) {
            tracing::error!("激活便签文本焦点失败：{error}");
        }
        self.text_state
            .update(cx, |state, cx| state.focus(window, cx));
    }

    fn cancel_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.dirty || self.saving {
            return;
        }
        let text = self.text.clone();
        self.text_state
            .update(cx, |state, cx| state.set_value(text, window, cx));
        self.finish_editing(cx);
        // 取消表示不再编辑，把焦点还给之前的窗口；保存后保留焦点方便继续输入。
        if let Err(error) = crate::services::focus::restore_previous_focus_for(window) {
            tracing::debug!(%error, "取消便签编辑时恢复焦点失败");
        }
    }

    fn finish_editing(&mut self, cx: &mut Context<Self>) {
        self.dirty = false;
        self.close_armed = false;
        self.saving = false;
        self.save_error = false;
        cx.notify();
    }

    fn save_editing(&mut self, cx: &mut Context<Self>) {
        if !self.dirty || self.saving {
            return;
        }
        let text = self.text_state.read(cx).value().to_string();
        if text == self.text {
            self.finish_editing(cx);
            return;
        }
        self.saving = true;
        self.save_error = false;
        let services = StickyService::services(cx);
        let item_id = self.item_id;
        let saved_text = text.clone();
        let task = cx.background_spawn(async move {
            services
                .custom_text_service
                .save_for_item(item_id, &saved_text)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.saving = false;
                match result {
                    Ok(()) => {
                        this.text = text;
                        this.finish_editing(cx);
                        this.show_toast("sticky.saved", cx);
                        crate::windows::main::MainWindowService::refresh_history(cx);
                    }
                    Err(error) => {
                        tracing::error!("保存便签文本失败：{error}");
                        this.save_error = true;
                        cx.notify();
                    }
                }
            });
        })
        .detach();
        cx.notify();
    }

    // ---- 菜单 ----

    fn open_menu(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(anchor) = crate::platform::window::logical_to_physical_desktop(window, position)
        else {
            return;
        };
        let translator = i18n::translator(cx);
        let mode = self.mode();
        let mut items = vec![
            MenuItem::action(
                MenuActionId::new("copy"),
                translator.translate("sticky.copy"),
            )
            .with_icon(MenuIcon::svg("copy.svg")),
            MenuItem::action(
                MenuActionId::new("paste"),
                translator.translate("sticky.paste"),
            )
            .with_icon(MenuIcon::svg("clipboard.svg")),
        ];
        if mode == Some(StickyMode::Text) {
            let split = MenuItem::action(
                MenuActionId::new("split"),
                translator.translate("sticky.split"),
            )
            .with_icon(MenuIcon::svg("blocks.svg"));
            items.push(if self.split.is_some() {
                split.checked()
            } else {
                split
            });
        }
        if self.modes.len() > 1 {
            let children = self
                .modes
                .iter()
                .enumerate()
                .map(|(index, (mode, _))| {
                    let item = MenuItem::action(
                        MenuActionId::new(format!("mode-{index}")),
                        translator.translate(mode.label_key()),
                    )
                    .with_icon(MenuIcon::svg(format!("{}.svg", mode.icon())));
                    if index == self.mode_index {
                        item.checked()
                    } else {
                        item
                    }
                })
                .collect();
            items.push(MenuItem::submenu(
                translator.translate("sticky.mode"),
                children,
            ));
        }
        items.push(MenuItem::separator());
        let current_opacity = (self.opacity * 100.0).round() as u8;
        let opacities = OPACITY_LEVELS
            .iter()
            .map(|level| {
                let item = MenuItem::action(
                    MenuActionId::new(format!("opacity-{level}")),
                    format!("{level}%"),
                );
                if *level == current_opacity {
                    item.checked()
                } else {
                    item
                }
            })
            .collect();
        items.push(
            MenuItem::submenu(translator.translate("sticky.opacity"), opacities)
                .with_icon(MenuIcon::svg("droplet-half-2.svg")),
        );
        let topmost = MenuItem::action(
            MenuActionId::new("topmost"),
            translator.translate("sticky.topmost"),
        )
        .with_icon(MenuIcon::svg("pin.svg"));
        items.push(if self.topmost {
            topmost.checked()
        } else {
            topmost
        });
        items.push(
            MenuItem::action(
                MenuActionId::new("collapse"),
                translator.translate(if self.collapsed {
                    "sticky.expand"
                } else {
                    "sticky.collapse"
                }),
            )
            .with_icon(MenuIcon::svg(if self.collapsed {
                "layout-navbar-expand.svg"
            } else {
                "layout-navbar-collapse.svg"
            })),
        );
        items.push(MenuItem::separator());
        items.push(
            MenuItem::action(
                MenuActionId::new("close"),
                translator.translate("sticky.close"),
            )
            .with_icon(MenuIcon::svg("x.svg")),
        );
        items.push(
            MenuItem::action(
                MenuActionId::new("close-all"),
                translator.translate("sticky.close_all"),
            )
            .with_icon(MenuIcon::svg("trash.svg")),
        );

        let view = cx.entity().downgrade();
        let window_handle = window.window_handle();
        MenuService::open_context_menu(
            MenuModel::new(items),
            MenuPosition::new(anchor.x, anchor.y, window.scale_factor()),
            move |action, cx| {
                if action.as_str() == "close-all" {
                    StickyService::close_all(cx);
                    return;
                }
                let Some(view) = view.upgrade() else {
                    return;
                };
                let _ = window_handle.update(cx, |_, window, cx| {
                    view.update(cx, |this, cx| this.perform_menu_action(&action, window, cx));
                });
            },
            cx,
        );
    }

    fn perform_menu_action(
        &mut self,
        action: &MenuActionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let action = action.as_str();
        match action {
            "copy" => self.copy_content(cx),
            "paste" => self.paste_content(window, cx),
            "split" => self.toggle_split(cx),
            "topmost" => self.toggle_topmost(window, cx),
            "collapse" => self.toggle_collapsed(window, cx),
            "close" => self.close(window, cx),
            _ => {
                if let Some(index) = parse_suffix(action, "mode-") {
                    self.set_mode(index, cx);
                } else if let Some(level) = parse_suffix(action, "opacity-") {
                    self.set_opacity(level as f32 / 100.0, window, cx);
                }
            }
        }
    }

    // ---- 渲染 ----

    fn render_header(&mut self, paper: Paper, cx: &mut Context<Self>) -> AnyElement {
        let translator = i18n::translator(cx);
        let mode = self.mode();
        if self.collapsed {
            return self.render_capsule(paper, &translator, cx);
        }
        let is_text = mode == Some(StickyMode::Text);
        let mut buttons: Vec<AnyElement> = Vec::new();
        if self.modes.len() > 1 {
            for (index, (mode, _)) in self.modes.iter().enumerate() {
                let active = index == self.mode_index;
                buttons.push(header_button(
                    SharedString::from(format!("sticky-mode-{index}")),
                    mode.icon(),
                    active,
                    paper,
                    translator.translate(mode.label_key()),
                    cx.listener(move |this, _, _, cx| this.set_mode(index, cx)),
                ));
            }
            buttons.push(
                div()
                    .w(px(1.0))
                    .h(ui_rem(14.0))
                    .mx(px(2.0))
                    .bg(paper.border)
                    .into_any_element(),
            );
        }
        if is_text {
            buttons.push(header_button(
                "sticky-split",
                "blocks",
                self.split.is_some(),
                paper,
                translator.translate(if self.split.is_some() {
                    "sticky.split_exit"
                } else {
                    "sticky.split"
                }),
                cx.listener(|this, _, _, cx| this.toggle_split(cx)),
            ));
        }
        buttons.push(header_button(
            "sticky-copy",
            "copy",
            false,
            paper,
            translator.translate("sticky.copy"),
            cx.listener(|this, _, _, cx| this.copy_content(cx)),
        ));
        buttons.push(header_button(
            "sticky-topmost",
            if self.topmost { "pinned-filled" } else { "pin" },
            self.topmost,
            paper,
            translator.translate("sticky.topmost"),
            cx.listener(|this, _, window, cx| this.toggle_topmost(window, cx)),
        ));
        buttons.push(header_button(
            "sticky-more",
            "dots",
            false,
            paper,
            translator.translate("sticky.more"),
            // 菜单服务会按当前鼠标位置定位，这里只需提供窗口内的兜底坐标。
            cx.listener(|this, _, window, cx| this.open_menu(Point::default(), window, cx)),
        ));
        buttons.push(header_button(
            "sticky-close",
            "x",
            false,
            paper,
            translator.translate("sticky.close"),
            cx.listener(|this, _, window, cx| this.close(window, cx)),
        ));

        div()
            .id("sticky-header")
            .h(ui_rem(HEADER_HEIGHT))
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(2.0))
            .pl(px(10.0))
            .pr(px(5.0))
            .bg(paper.header)
            .border_b_1()
            .border_color(paper.border)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, _| {
                    this.header_drag_start = Some(event.position);
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, _| {
                let Some(start) = this.header_drag_start else {
                    return;
                };
                if !event.dragging() {
                    this.header_drag_start = None;
                    return;
                }
                let dx = f32::from(event.position.x - start.x);
                let dy = f32::from(event.position.y - start.y);
                if dx * dx + dy * dy >= WINDOW_DRAG_THRESHOLD * WINDOW_DRAG_THRESHOLD {
                    this.header_drag_start = None;
                    window.start_window_move();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.header_drag_start = None),
            )
            .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                if is_collapse_double_click(event) {
                    this.toggle_collapsed(window, cx);
                }
            }))
            .child(color_button(
                mode.map(StickyMode::icon).unwrap_or("note"),
                paper,
                translator.translate("sticky.color"),
                cx.listener(|this, _, window, cx| this.pick_color(window, cx)),
            ))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .ml(px(2.0))
                    .truncate()
                    .text_size(ui_rem(12.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(paper.muted)
                    .child(self.title(&translator)),
            )
            .children(buttons)
            .into_any_element()
    }

    /// 折叠后的胶囊只保留类型图标和截断的标题；拖动可移动，双击展开。
    fn render_capsule(
        &self,
        paper: Paper,
        translator: &i18n::Translator,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id("sticky-capsule")
            .size_full()
            .flex()
            .items_center()
            .gap(px(6.0))
            .px(px(12.0))
            .bg(paper.header)
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, _| {
                    this.header_drag_start = Some(event.position);
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, _| {
                let Some(start) = this.header_drag_start else {
                    return;
                };
                if !event.dragging() {
                    this.header_drag_start = None;
                    return;
                }
                let dx = f32::from(event.position.x - start.x);
                let dy = f32::from(event.position.y - start.y);
                if dx * dx + dy * dy >= WINDOW_DRAG_THRESHOLD * WINDOW_DRAG_THRESHOLD {
                    this.header_drag_start = None;
                    window.start_window_move();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.header_drag_start = None),
            )
            .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                if is_collapse_double_click(event) {
                    this.toggle_collapsed(window, cx);
                }
            }))
            .tooltip({
                let hint = translator.translate("sticky.expand_hint");
                move |window, cx| Tooltip::new(hint.clone()).build(window, cx)
            })
            .child(
                div()
                    .flex_none()
                    .size(px(8.0))
                    .rounded_full()
                    .bg(paper.swatch.unwrap_or(paper.accent)),
            )
            .child(icon(
                self.mode().map(StickyMode::icon).unwrap_or("note"),
                12.0,
                paper.muted,
            ))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_size(ui_rem(12.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(paper.foreground)
                    .child(self.title(translator)),
            )
            .into_any_element()
    }

    fn render_body(
        &mut self,
        paper: Paper,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let translator = i18n::translator(cx);
        match self.mode() {
            Some(StickyMode::Text) if self.split.is_some() => self.render_tokens(paper, window, cx),
            Some(StickyMode::Text) => {
                // 输入框只渲染可见行，查看和编辑超长文本都不会一次排版全文。
                div()
                    .size_full()
                    .px(px(6.0))
                    .py(px(4.0))
                    .text_size(ui_rem(14.0))
                    .text_color(paper.foreground)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| this.focus_text(window, cx)),
                    )
                    .child(
                        Textarea::new(&self.text_state)
                            .appearance(false)
                            .bordered(false)
                            .size_full()
                            .disabled(self.saving),
                    )
                    .into_any_element()
            }
            Some(StickyMode::Html) => {
                let Some(document) = self.html_document.clone() else {
                    return placeholder(translator.translate("sticky.empty"), paper);
                };
                if document.is_empty() {
                    return placeholder(translator.translate("sticky.empty"), paper);
                }
                let count = document.len();
                div()
                    .id("sticky-html")
                    .size_full()
                    .text_color(paper.foreground)
                    .child(
                        list(
                            self.html_list.clone(),
                            cx.processor(move |this, index: usize, _, _| {
                                let images = this.html_images.clone().unwrap_or_default();
                                div()
                                    .w_full()
                                    .px(px(14.0))
                                    .when(index == 0, |this| this.pt(px(10.0)))
                                    .when(index + 1 == count, |this| this.pb(px(10.0)))
                                    .child(document.render_item(index, &images))
                                    .into_any_element()
                            }),
                        )
                        .size_full(),
                    )
                    .vertical_scrollbar(&self.html_list)
                    .into_any_element()
            }
            Some(StickyMode::Image) => match &self.image {
                Resource::Ready(image) => self.render_image(image.image.clone(), paper, cx),
                Resource::Failed => placeholder(translator.translate("sticky.load_error"), paper),
                _ => placeholder(translator.translate("sticky.loading"), paper),
            },
            Some(StickyMode::File) => match &self.files {
                Resource::Ready(entries) => self.render_files(entries, paper, &translator, cx),
                Resource::Failed => placeholder(translator.translate("sticky.load_error"), paper),
                _ => placeholder(translator.translate("sticky.loading"), paper),
            },
            None => placeholder(translator.translate("sticky.empty"), paper),
        }
    }

    /// 图片视图：滚轮围绕鼠标缩放图片、拖动平移、双击还原；Ctrl+滚轮缩放窗口。
    fn render_image(
        &self,
        image: Arc<RenderImage>,
        paper: Paper,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let translator = i18n::translator(cx);
        let viewport = self.image_viewport.clone();
        let image_size = self.image_size().unwrap_or_default();
        let bounds = self
            .image_view
            .image_bounds(viewport.get().size, image_size);
        let zoomed = self.image_view.is_zoomed();
        let dragging = self.image_drag.is_some();
        let hint = {
            let text = if zoomed {
                translator.translate_with_args(
                    "sticky.image_hint_zoomed",
                    &[("percent", format!("{:.0}", self.image_view.scale * 100.0))],
                )
            } else {
                translator.translate("sticky.image_hint")
            };
            // 提示常驻角落；鼠标碰到时变透明，不遮挡图片，移开后恢复。
            // 只改透明度而不移除元素，这样仍能收到鼠标移出事件；
            // 提示没有遮挡命中区域，滚轮和拖动依然交给下方图片处理。
            div()
                .id("sticky-image-hint")
                .absolute()
                .right(px(8.0))
                .bottom(px(8.0))
                .max_w(px(260.0))
                .px(px(10.0))
                .py(px(5.0))
                .rounded(px(8.0))
                .bg(paper.background.opacity(0.62))
                .backdrop_blur(px(18.0))
                .border_1()
                .border_color(paper.border.opacity(0.6))
                .shadow_sm()
                .text_size(ui_rem(11.0))
                .line_height(ui_rem(16.0))
                .text_color(paper.foreground)
                .when(self.image_hint_hovered, |this| this.opacity(0.0))
                .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                    if this.image_hint_hovered != *hovered {
                        this.image_hint_hovered = *hovered;
                        cx.notify();
                    }
                }))
                .child(text)
        };
        div()
            .id("sticky-image")
            .relative()
            .size_full()
            .overflow_hidden()
            .when(zoomed, |this| {
                this.cursor(if dragging {
                    gpui::CursorStyle::ClosedHand
                } else {
                    gpui::CursorStyle::OpenHand
                })
            })
            .on_prepaint(move |prepaint, window, _| {
                // 视口尺寸在布局后才知道；变化时补绘一帧，让放大后的图片按新尺寸定位。
                if viewport.get() != prepaint.bounds {
                    viewport.set(prepaint.bounds);
                    if zoomed {
                        window.refresh();
                    }
                }
            })
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, cx| {
                cx.stop_propagation();
                let delta = f32::from(event.delta.pixel_delta(window.line_height()).y);
                if event.modifiers.control {
                    this.zoom_image(delta, window, cx);
                } else {
                    this.zoom_image_content(delta, event.position, cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    if event.click_count >= 2 {
                        this.reset_image_view(cx);
                        return;
                    }
                    if this.image_view.is_zoomed() {
                        this.image_drag = Some(event.position);
                        cx.notify();
                    }
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                let Some(previous) = this.image_drag else {
                    return;
                };
                if !event.dragging() {
                    this.image_drag = None;
                    cx.notify();
                    return;
                }
                let Some(image) = this.image_size() else {
                    return;
                };
                this.image_drag = Some(event.position);
                this.image_view.pan(
                    event.position - previous,
                    this.image_viewport.get().size,
                    image,
                );
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.image_drag.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            // 未放大时直接按适配方式铺满，不依赖上一帧测得的视口，图片加载后立即显示。
            .child(if zoomed {
                img(image)
                    .absolute()
                    .left(bounds.origin.x)
                    .top(bounds.origin.y)
                    .w(bounds.size.width)
                    .h(bounds.size.height)
            } else {
                img(image).size_full().object_fit(ObjectFit::Contain)
            })
            .child(hint)
            .into_any_element()
    }

    fn render_files(
        &self,
        entries: &[FileEntry],
        paper: Paper,
        translator: &i18n::Translator,
        _cx: &Context<Self>,
    ) -> AnyElement {
        let rows = entries.iter().enumerate().map(|(index, entry)| {
            let path = entry.path.clone();
            let reveal_path = entry.path.clone();
            let open_path = entry.path.clone();
            let group = SharedString::from(format!("sticky-file-{index}"));
            let file_icon = match &entry.icon {
                Some(image) => img(image.clone())
                    .size(ui_rem(26.0))
                    .object_fit(ObjectFit::Contain)
                    .into_any_element(),
                None => icon(
                    if entry.is_directory {
                        "folder"
                    } else {
                        "file-description"
                    },
                    20.0,
                    paper.muted,
                )
                .into_any_element(),
            };
            let detail = if entry.exists {
                entry.detail.clone().unwrap_or_else(|| {
                    if entry.is_directory {
                        translator.translate("sticky.file.folder")
                    } else {
                        String::new()
                    }
                })
            } else {
                translator.translate("sticky.file.missing")
            };
            div()
                .id(("sticky-file", index as u64))
                .group(group.clone())
                .w_full()
                .flex()
                .items_center()
                .gap(px(10.0))
                .px(px(8.0))
                .py(px(6.0))
                .rounded(px(8.0))
                .hover(|this| this.bg(paper.hover()))
                .when(!entry.exists, |this| this.opacity(0.55))
                .on_click(move |event: &ClickEvent, _, cx| {
                    if event.click_count() >= 2 {
                        cx.open_with_system(&path);
                    }
                })
                .child(
                    div()
                        .size(ui_rem(30.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(file_icon),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .truncate()
                                .text_size(ui_rem(13.0))
                                .text_color(paper.foreground)
                                .child(entry.name.clone()),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(ui_rem(11.0))
                                .text_color(paper.muted)
                                .child(detail),
                        ),
                )
                .when(entry.exists, |this| {
                    this.child(
                        div()
                            .flex()
                            .gap(px(2.0))
                            .invisible()
                            .group_hover(group.clone(), |style| style.visible())
                            .child(header_button(
                                SharedString::from(format!("sticky-file-open-{index}")),
                                "external-link",
                                false,
                                paper,
                                translator.translate("sticky.file.open"),
                                move |_, _, cx| cx.open_with_system(&open_path),
                            ))
                            .child(header_button(
                                SharedString::from(format!("sticky-file-reveal-{index}")),
                                "folder-open",
                                false,
                                paper,
                                translator.translate("sticky.file.reveal"),
                                move |_, _, cx| cx.reveal_path(&reveal_path),
                            )),
                    )
                })
                .into_any_element()
        });
        v_flex()
            .id("sticky-files")
            .size_full()
            .overflow_y_scrollbar()
            .child(v_flex().w_full().p(px(6.0)).gap(px(2.0)).children(rows))
            .into_any_element()
    }

    /// 计算（并缓存）当前宽度下拆词结果的行划分。
    fn token_rows(&mut self, window: &mut Window, cx: &App) -> Rc<Vec<Range<usize>>> {
        let rem_size = f32::from(window.rem_size());
        let width = (f32::from(window.viewport_size().width)
            - 2.0
            - TOKEN_AREA_PADDING * 2.0
            - TOKEN_SCROLLBAR_GUTTER)
            .max(40.0);
        let Some(split) = self.split.as_mut() else {
            return Rc::default();
        };
        if let Some(layout) = &split.layout
            && layout.width == width
            && layout.rem_size == rem_size
        {
            return layout.rows.clone();
        }
        if self.char_widths.rem_size != rem_size {
            self.char_widths = CharWidths {
                rem_size,
                widths: HashMap::new(),
            };
        }
        let mut style = window.text_style();
        style.font_family = cx.theme().font_family.clone();
        let font_size = ui_rem(13.0).to_pixels(window.rem_size());
        let extra = TOKEN_CHIP_EXTRA * rem_size / 16.0;
        let char_widths = &mut self.char_widths.widths;
        let widths = split
            .result
            .tokens
            .iter()
            .map(|token| {
                let text_width: f32 = token
                    .text
                    .chars()
                    .map(|character| {
                        *char_widths.entry(character).or_insert_with(|| {
                            let text = character.to_string();
                            let run = style.to_run(text.len());
                            f32::from(
                                window
                                    .text_system()
                                    .shape_line(text.into(), font_size, &[run], None)
                                    .width,
                            )
                        })
                    })
                    .sum();
                text_width.ceil() + extra
            })
            .collect::<Vec<_>>();
        let lines = split
            .result
            .tokens
            .iter()
            .map(|token| token.line)
            .collect::<Vec<_>>();
        let rows = Rc::new(layout_token_rows(&widths, &lines, width, TOKEN_GAP));
        split.layout = Some(TokenLayout {
            width,
            rem_size,
            rows: rows.clone(),
        });
        rows
    }

    fn render_tokens(
        &mut self,
        paper: Paper,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let translator = i18n::translator(cx);
        let Some(split) = self.split.as_ref() else {
            return placeholder(translator.translate("sticky.split.empty"), paper);
        };
        if split.result.tokens.is_empty() {
            return placeholder(translator.translate("sticky.split.empty"), paper);
        }
        let truncated = split.result.truncated.then(|| {
            div()
                .flex_none()
                .px(px(TOKEN_AREA_PADDING))
                .py(px(4.0))
                .text_size(ui_rem(11.0))
                .text_color(paper.muted)
                .child(translator.translate_with_args(
                    "sticky.split.truncated",
                    &[("count", word_split::MAX_TOKENS.to_string())],
                ))
        });
        let rows = self.token_rows(window, cx);
        let row_count = rows.len();
        // 只渲染可见的词块行；每行高度一致，滚动时按行号直接定位。
        let token_list = uniform_list(
            "sticky-token-rows",
            row_count,
            cx.processor(move |this, range: Range<usize>, _, cx| {
                let paper = Paper::resolve(this.color, cx);
                range
                    .filter_map(|row| rows.get(row).cloned())
                    .map(|tokens| {
                        div()
                            .w_full()
                            .flex()
                            .gap(px(TOKEN_GAP))
                            .pb(px(TOKEN_GAP))
                            .overflow_hidden()
                            .children(tokens.map(|index| this.render_token(index, paper, cx)))
                            .into_any_element()
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .size_full()
        .px(px(TOKEN_AREA_PADDING))
        .pt(px(TOKEN_AREA_PADDING))
        .pb(px(TOKEN_AREA_PADDING - TOKEN_GAP))
        .track_scroll(&self.token_scroll);
        v_flex()
            .size_full()
            .child(
                div()
                    .id("sticky-tokens")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(token_list)
                    .vertical_scrollbar(&self.token_scroll),
            )
            .children(truncated)
            .into_any_element()
    }

    fn render_token(&self, index: usize, paper: Paper, cx: &mut Context<Self>) -> AnyElement {
        let Some(split) = self.split.as_ref() else {
            return div().into_any_element();
        };
        let Some(token) = split.result.tokens.get(index) else {
            return div().into_any_element();
        };
        let selected = split.selection.is_selected(index);
        let (background, foreground) = if selected {
            (paper.accent, paper.accent_foreground)
        } else if token.kind.is_entity() {
            (paper.accent.opacity(0.12), paper.accent)
        } else if token.kind == TokenKind::Symbol {
            (paper.token(), paper.muted)
        } else {
            (paper.token(), paper.foreground)
        };
        div()
            .id(("sticky-token", index as u64))
            .flex_none()
            .max_w_full()
            .truncate()
            .px(ui_rem(7.0))
            .py(px(2.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(if selected {
                paper.accent
            } else {
                gpui::transparent_black()
            })
            .bg(background)
            .text_color(foreground)
            .text_size(ui_rem(13.0))
            .line_height(ui_rem(20.0))
            .cursor_pointer()
            .hover(|this| this.border_color(paper.accent.opacity(0.65)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if let Some(split) = this.split.as_mut() {
                        split.selection.press(index, event.modifiers.shift);
                        cx.notify();
                    }
                }),
            )
            .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                if !event.dragging() {
                    return;
                }
                if let Some(split) = this.split.as_mut()
                    && split.selection.drag_to(index)
                {
                    cx.notify();
                }
            }))
            .child(token.text.clone())
            .into_any_element()
    }

    fn render_footer(&mut self, paper: Paper, cx: &mut Context<Self>) -> Option<AnyElement> {
        let translator = i18n::translator(cx);
        if self.dirty {
            let hint = if self.save_error {
                div()
                    .text_color(gpui::red())
                    .child(translator.translate("sticky.save_error"))
            } else {
                div().child(translator.translate("sticky.edit_hint"))
            };
            return Some(
                footer_row(paper)
                    // 保存栏点击不应让根元素抢走输入框焦点，保存后可以继续输入。
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .child(
                        hint.min_w_0()
                            .flex_1()
                            .truncate()
                            .text_size(ui_rem(11.0))
                            .text_color(paper.muted),
                    )
                    .child(
                        pill(
                            "sticky-edit-cancel",
                            translator.translate("sticky.cancel"),
                            None,
                            false,
                            paper,
                        )
                        .on_click(
                            cx.listener(|this, _, window, cx| this.cancel_editing(window, cx)),
                        ),
                    )
                    .child(
                        pill(
                            "sticky-edit-save",
                            translator.translate("sticky.save"),
                            Some("device-floppy"),
                            true,
                            paper,
                        )
                        .when(self.saving, |this| this.opacity(0.6))
                        .on_click(cx.listener(|this, _, _, cx| this.save_editing(cx))),
                    )
                    .into_any_element(),
            );
        }
        let split = self.split.as_ref()?;
        let selected_count = split.selection.selected_count();
        let total = split.selection.len();
        let has_selection = selected_count > 0;
        let separator_label = translator.translate(separator_label_key(self.separator));
        // 拆词工具栏默认一行：设置项在左，选择与复制粘贴靠右；窗口较窄时操作组整体换到第二行。
        let settings = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(2.0))
            .child(segmented(
                [
                    (
                        "sticky-split-word",
                        translator.translate("sticky.split.word"),
                        self.granularity == SplitGranularity::Word,
                    ),
                    (
                        "sticky-split-char",
                        translator.translate("sticky.split.char"),
                        self.granularity == SplitGranularity::Character,
                    ),
                ],
                paper,
                cx.listener(|this, index: &usize, _, cx| {
                    this.granularity = if *index == 0 {
                        SplitGranularity::Word
                    } else {
                        SplitGranularity::Character
                    };
                    this.resplit(cx);
                }),
            ))
            .child(header_button(
                "sticky-split-symbols",
                "quote",
                self.include_symbols,
                paper,
                translator.translate("sticky.split.symbols"),
                cx.listener(|this, _, _, cx| {
                    this.include_symbols = !this.include_symbols;
                    this.resplit(cx);
                }),
            ))
            .child(
                pill(
                    "sticky-split-separator",
                    separator_label.clone(),
                    Some("link"),
                    false,
                    paper,
                )
                .bg(gpui::transparent_black())
                .px(px(5.0))
                .text_color(paper.muted)
                .tooltip({
                    let tooltip = translator.translate_with_args(
                        "sticky.split.separator_tooltip",
                        &[("mode", separator_label)],
                    );
                    move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx)
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    this.separator = next_separator(this.separator);
                    cx.notify();
                })),
            );
        let actions = div()
            .flex_none()
            .ml_auto()
            .flex()
            .items_center()
            .gap(px(2.0))
            .child(
                div()
                    .flex_none()
                    .px(px(4.0))
                    .text_size(ui_rem(11.0))
                    .text_color(paper.muted)
                    .child(translator.translate_with_args(
                        "sticky.split.selected",
                        &[
                            ("count", selected_count.to_string()),
                            ("total", total.to_string()),
                        ],
                    )),
            )
            .child(header_button(
                "sticky-split-all",
                "select-all",
                false,
                paper,
                translator.translate("sticky.split.select_all"),
                cx.listener(|this, _, _, cx| {
                    if let Some(split) = this.split.as_mut() {
                        split.selection.select_all();
                        cx.notify();
                    }
                }),
            ))
            .child(header_button(
                "sticky-split-invert",
                "square-toggle",
                false,
                paper,
                translator.translate("sticky.split.invert"),
                cx.listener(|this, _, _, cx| {
                    if let Some(split) = this.split.as_mut() {
                        split.selection.invert();
                        cx.notify();
                    }
                }),
            ))
            .child(dimmed(
                !has_selection,
                header_button(
                    "sticky-split-clear",
                    "deselect",
                    false,
                    paper,
                    translator.translate("sticky.split.clear"),
                    cx.listener(|this, _, _, cx| {
                        if let Some(split) = this.split.as_mut() {
                            split.selection.clear();
                            cx.notify();
                        }
                    }),
                ),
            ))
            .child(
                div()
                    .w(px(1.0))
                    .h(ui_rem(14.0))
                    .mx(px(2.0))
                    .bg(paper.border),
            )
            .child(dimmed(
                !has_selection,
                header_button(
                    "sticky-split-copy",
                    "copy",
                    has_selection,
                    paper,
                    translator.translate("sticky.copy"),
                    cx.listener(|this, _, window, cx| this.copy_selected_tokens(false, window, cx)),
                ),
            ))
            .child(dimmed(
                !has_selection,
                header_button(
                    "sticky-split-paste",
                    "clipboard",
                    false,
                    paper,
                    translator.translate("sticky.paste"),
                    cx.listener(|this, _, window, cx| this.copy_selected_tokens(true, window, cx)),
                ),
            ));
        let row = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(2.0))
            .px(px(6.0))
            .py(px(3.0))
            .child(settings)
            .child(actions);
        Some(
            div()
                .w_full()
                .flex_none()
                .border_t_1()
                .border_color(paper.border)
                .bg(paper.header)
                .child(row)
                .into_any_element(),
        )
    }
}

impl Render for StickyNote {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let paper = Paper::resolve(self.color, cx);
        let background = crate::window_material::tinted_background_element(paper.tint, cx);
        let header = self.render_header(paper, cx);
        let body = (!self.collapsed).then(|| {
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .overflow_hidden()
                .child(self.render_body(paper, window, cx))
        });
        let footer = if self.collapsed {
            None
        } else {
            self.render_footer(paper, cx)
        };
        let toast = self.toast.clone().map(|message| {
            div()
                .absolute()
                .bottom(px(if footer.is_some() { 44.0 } else { 12.0 }))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .px(px(12.0))
                        .py(px(4.0))
                        .rounded_full()
                        .bg(paper.foreground.opacity(0.85))
                        .text_color(paper.background)
                        .text_size(ui_rem(12.0))
                        .shadow_md()
                        .child(message),
                )
        });
        let content = div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(paper.background)
            .border_1()
            .border_color(if self.flash {
                paper.accent
            } else {
                paper.border
            })
            .child(header)
            .children(body)
            .children(footer)
            .children(toast);
        div()
            .id("sticky-note")
            .track_focus(&self.focus_handle)
            .relative()
            .size_full()
            .overflow_hidden()
            .text_color(paper.foreground)
            .font_family(cx.theme().font_family.clone())
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    window.prevent_default();
                    cx.stop_propagation();
                    this.open_menu(event.position, window, cx);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.release_token_drag(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.release_token_drag(cx)),
            )
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if !this.dirty {
                    return;
                }
                let keystroke = &event.keystroke;
                if keystroke.modifiers.control && keystroke.key == "s" {
                    cx.stop_propagation();
                    this.save_editing(cx);
                } else if keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.cancel_editing(window, cx);
                }
            }))
            .child(background)
            .child(content)
    }
}

impl StickyNote {
    fn release_token_drag(&mut self, cx: &mut Context<Self>) {
        if let Some(split) = self.split.as_mut()
            && split.selection.is_dragging()
        {
            split.selection.release();
            cx.notify();
        }
    }
}

/// 按词块宽度排行：原文换行处另起一行，放不下时折到下一行。
fn layout_token_rows(
    widths: &[f32],
    lines: &[usize],
    available_width: f32,
    gap: f32,
) -> Vec<Range<usize>> {
    let mut rows = Vec::new();
    let mut start = 0;
    let mut used = 0.0;
    for (index, (&width, &line)) in widths.iter().zip(lines).enumerate() {
        let new_line = index > start && lines[index - 1] != line;
        let overflow = index > start && used + gap + width > available_width;
        if new_line || overflow {
            rows.push(start..index);
            start = index;
            used = 0.0;
        }
        used = if index == start {
            width
        } else {
            used + gap + width
        };
    }
    if start < widths.len() {
        rows.push(start..widths.len());
    }
    rows
}

fn header_button(
    id: impl Into<SharedString>,
    icon_name: &str,
    active: bool,
    paper: Paper,
    tooltip: String,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let id: SharedString = id.into();
    div()
        .id(gpui::ElementId::Name(id))
        .size(ui_rem(24.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.0))
        .cursor_pointer()
        .when(active, |this| this.bg(paper.accent.opacity(0.16)))
        .hover(|this| this.bg(paper.hover()))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_click(event, window, cx);
        })
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .child(icon(
            icon_name,
            14.0,
            if active { paper.accent } else { paper.muted },
        ))
        .into_any_element()
}

fn is_collapse_double_click(event: &ClickEvent) -> bool {
    let count = event.click_count();
    count >= 2 && count % 2 == 0
}

fn lock_resize(window: &Window, locked: bool) {
    if let Err(error) = crate::platform::window::set_window_resize_locked(window, locked) {
        tracing::debug!(%error, "切换便签缩放锁定失败");
    }
}

/// 标题栏最左侧的颜色按钮：色块与类型图标叠加，点击打开颜色选择。
fn color_button(
    icon_name: &str,
    paper: Paper,
    tooltip: String,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let swatch = div()
        .absolute()
        .right(px(1.0))
        .bottom(px(1.0))
        .size(px(9.0))
        .rounded_full()
        .border_1()
        .border_color(paper.header)
        .map(|this| match paper.swatch {
            Some(color) => this.bg(color),
            // 跟随主题时用半边主题色、半边背景色表示“自动”。
            None => this.bg(gpui::linear_gradient(
                135.0,
                gpui::linear_color_stop(paper.accent, 0.5),
                gpui::linear_color_stop(paper.background, 0.5),
            )),
        });
    div()
        .id("sticky-color")
        .relative()
        .size(ui_rem(24.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.0))
        .cursor_pointer()
        .hover(|this| this.bg(paper.hover()))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_click(event, window, cx);
        })
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .child(icon(icon_name, 14.0, paper.muted))
        .child(swatch)
        .into_any_element()
}

fn pill(
    id: &'static str,
    label: String,
    icon_name: Option<&str>,
    primary: bool,
    paper: Paper,
) -> Stateful<gpui::Div> {
    let (background, foreground, hover) = if primary {
        (
            paper.accent,
            paper.accent_foreground,
            paper.accent.opacity(0.85),
        )
    } else {
        (paper.token(), paper.foreground, paper.hover())
    };
    div()
        .id(id)
        .flex_none()
        .h(ui_rem(24.0))
        .flex()
        .items_center()
        .gap(px(4.0))
        .px(px(8.0))
        .rounded(px(6.0))
        .cursor_pointer()
        .bg(background)
        .text_color(foreground)
        .text_size(ui_rem(12.0))
        .hover(move |this| this.bg(hover))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .children(icon_name.map(|name| icon(name, 12.0, foreground)))
        .when(!label.is_empty(), |this| this.child(label))
}

fn segmented(
    options: [(&'static str, String, bool); 2],
    paper: Paper,
    on_select: impl Fn(&usize, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let on_select = std::rc::Rc::new(on_select);
    div()
        .flex_none()
        .flex()
        .p(px(2.0))
        .gap(px(2.0))
        .rounded(px(7.0))
        .bg(paper.token())
        .children(
            options
                .into_iter()
                .enumerate()
                .map(|(index, (id, label, active))| {
                    let on_select = on_select.clone();
                    div()
                        .id(id)
                        .h(ui_rem(20.0))
                        .px(px(8.0))
                        .flex()
                        .items_center()
                        .rounded(px(5.0))
                        .cursor_pointer()
                        .text_size(ui_rem(12.0))
                        .when(active, |this| {
                            this.bg(paper.background)
                                .text_color(paper.accent)
                                .font_weight(FontWeight::SEMIBOLD)
                                .shadow_sm()
                        })
                        .when(!active, |this| {
                            this.text_color(paper.muted)
                                .hover(|this| this.text_color(paper.foreground))
                        })
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(move |_, window, cx| on_select(&index, window, cx))
                        .child(label)
                }),
        )
        .into_any_element()
}

fn footer_row(_paper: Paper) -> gpui::Div {
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(4.0))
        .px(px(8.0))
        .py(px(3.0))
}

fn dimmed(dimmed: bool, element: AnyElement) -> AnyElement {
    div()
        .flex_none()
        .when(dimmed, |this| this.opacity(0.4))
        .child(element)
        .into_any_element()
}

fn placeholder(message: String, paper: Paper) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_size(ui_rem(12.0))
        .text_color(paper.muted)
        .child(message)
        .into_any_element()
}

fn separator_label_key(separator: JoinSeparator) -> &'static str {
    match separator {
        JoinSeparator::Smart => "sticky.split.separator.smart",
        JoinSeparator::None => "sticky.split.separator.none",
        JoinSeparator::Space => "sticky.split.separator.space",
        JoinSeparator::Newline => "sticky.split.separator.newline",
    }
}

fn next_separator(separator: JoinSeparator) -> JoinSeparator {
    match separator {
        JoinSeparator::Smart => JoinSeparator::None,
        JoinSeparator::None => JoinSeparator::Space,
        JoinSeparator::Space => JoinSeparator::Newline,
        JoinSeparator::Newline => JoinSeparator::Smart,
    }
}

fn parse_suffix(action: &str, prefix: &str) -> Option<usize> {
    action.strip_prefix(prefix)?.parse().ok()
}

fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(80).collect())
}

fn load_file_entry(path: PathBuf) -> FileEntry {
    let metadata = std::fs::metadata(&path).ok();
    let exists = metadata.is_some();
    let is_directory = metadata.as_ref().is_some_and(|metadata| metadata.is_dir());
    let detail = metadata
        .as_ref()
        .filter(|metadata| !metadata.is_dir())
        .map(|metadata| format_bytes(metadata.len()));
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    let icon = exists
        .then(|| file_icon_provider::get_file_icon(&path, FILE_ICON_SIZE).ok())
        .flatten()
        .and_then(crate::windows::preview::render_file_icon);
    FileEntry {
        path,
        name,
        detail,
        is_directory,
        exists,
        icon,
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::layout_token_rows;

    #[test]
    fn token_rows_break_on_source_lines_and_overflow() {
        let widths = [30.0, 30.0, 30.0, 30.0, 200.0, 10.0];
        let lines = [0, 0, 0, 1, 1, 1];
        // 宽度 70 只放得下两个 30 宽的词块；超宽词块独占一行。
        assert_eq!(
            layout_token_rows(&widths, &lines, 70.0, 5.0),
            vec![0..2, 2..3, 3..4, 4..5, 5..6]
        );
        assert_eq!(
            layout_token_rows(&widths, &lines, 1000.0, 5.0),
            vec![0..3, 3..6]
        );
        assert!(layout_token_rows(&[], &[], 100.0, 5.0).is_empty());
    }
}
