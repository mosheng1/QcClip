use std::{
    collections::{HashMap, HashSet},
    fs,
    ops::Range,
    path::PathBuf,
    sync::Arc,
};

use file_icon_provider::Icon;
use gpui::prelude::*;
use gpui::{
    AnyElement, Context, IntoElement, ObjectFit, Pixels, Render, RenderImage, Window, div, img, px,
};
use gpui_component::{ActiveTheme as _, ThemeColor};

use crate::{
    config::{ADAPTIVE_MAX_LINES_MAX, ADAPTIVE_MAX_LINES_MIN, ListItemHeight},
    i18n::{self, Translator},
    ui::fonts::AppFontMetrics,
};

const FILE_ICON_SIZE: u16 = 32;
const FILE_LIST_DETAIL_LINES: usize = 2;
const FILE_LIST_PREVIEW_LINE_HEIGHT_SCALE: f32 = 1.09375;

#[derive(Clone, Debug)]
pub struct FileListEntry {
    pub path: PathBuf,
    pub name: String,
    pub size: Option<u64>,
    pub is_directory: bool,
    pub exists: bool,
}

#[derive(Clone, Copy)]
struct FileListPresentation {
    visible_entries: usize,
    icon_size: Pixels,
    text_size: Pixels,
    line_height: Pixels,
    gap: Pixels,
    show_path: bool,
}

fn normalized_adaptive_max_lines(adaptive_max_lines: usize) -> usize {
    adaptive_max_lines.clamp(
        usize::from(ADAPTIVE_MAX_LINES_MIN),
        usize::from(ADAPTIVE_MAX_LINES_MAX),
    )
}

pub(crate) fn adaptive_file_list_entry_count(adaptive_max_lines: usize) -> usize {
    (normalized_adaptive_max_lines(adaptive_max_lines) / FILE_LIST_DETAIL_LINES).max(1)
}

pub(crate) fn file_list_preview_height(
    height: ListItemHeight,
    rem_size: Pixels,
    adaptive_max_lines: usize,
    file_count: Option<usize>,
    font_metrics: Option<AppFontMetrics>,
) -> Pixels {
    let mut presentation = file_list_presentation(height, rem_size, adaptive_max_lines);
    if let Some(font_metrics) = font_metrics {
        presentation = presentation.with_font_metrics(font_metrics);
    }
    let visible_count = if height.fixed_preview_lines().is_some() {
        presentation.visible_entries
    } else {
        file_count
            .unwrap_or(1)
            .min(presentation.visible_entries)
            .max(1)
    };
    let natural_height = presentation.entry_height() * visible_count as f32
        + presentation.entry_gap(rem_size, height, visible_count)
            * visible_count.saturating_sub(1) as f32;
    let minimum = rem_size
        * FILE_LIST_PREVIEW_LINE_HEIGHT_SCALE
        * height.fixed_preview_lines().unwrap_or(0) as f32;
    natural_height.max(minimum)
}

fn file_list_presentation(
    height: ListItemHeight,
    rem_size: Pixels,
    adaptive_max_lines: usize,
) -> FileListPresentation {
    match height {
        ListItemHeight::Large => FileListPresentation {
            visible_entries: 2,
            icon_size: rem_size * 1.375,
            text_size: rem_size * 0.6875,
            line_height: rem_size * 0.75,
            gap: rem_size * 0.375,
            show_path: true,
        },
        ListItemHeight::Medium => FileListPresentation {
            visible_entries: 2,
            icon_size: rem_size * 1.125,
            text_size: rem_size * 0.5625,
            line_height: rem_size * 0.625,
            gap: rem_size * 0.25,
            show_path: true,
        },
        ListItemHeight::Small => FileListPresentation {
            visible_entries: 1,
            icon_size: rem_size * 1.25,
            text_size: rem_size * 0.6875,
            line_height: rem_size * 0.75,
            gap: rem_size * 0.375,
            show_path: true,
        },
        ListItemHeight::ExtraSmall => FileListPresentation {
            visible_entries: 1,
            icon_size: rem_size * 0.875,
            text_size: rem_size * 0.5625,
            line_height: rem_size * 0.625,
            gap: rem_size * 0.1875,
            show_path: false,
        },
        // 自适应模式按最大预览行数计算可容纳的文件数，每个详细文件占两行。
        ListItemHeight::Adaptive => {
            if normalized_adaptive_max_lines(adaptive_max_lines) < FILE_LIST_DETAIL_LINES {
                FileListPresentation {
                    visible_entries: 1,
                    icon_size: rem_size * 0.875,
                    text_size: rem_size * 0.5625,
                    line_height: rem_size * 0.625,
                    gap: rem_size * 0.1875,
                    show_path: false,
                }
            } else {
                FileListPresentation {
                    visible_entries: adaptive_file_list_entry_count(adaptive_max_lines),
                    icon_size: rem_size * 1.375,
                    text_size: rem_size * 0.6875,
                    line_height: rem_size * 0.75,
                    gap: rem_size * 0.375,
                    show_path: true,
                }
            }
        }
    }
}

impl FileListPresentation {
    fn with_font_metrics(mut self, font_metrics: AppFontMetrics) -> Self {
        self.line_height = font_metrics.line_height(self.text_size, self.line_height);
        self
    }

    fn natural_entry_height(self) -> Pixels {
        let text_height = self.line_height * if self.show_path { 2.0 } else { 1.0 };
        let text_height = text_height + self.text_gap();
        px(f32::from(self.icon_size).max(f32::from(text_height)))
    }

    fn entry_height(self) -> Pixels {
        self.natural_entry_height()
    }

    fn text_gap(self) -> Pixels {
        if self.show_path { self.gap } else { px(0.0) }
    }

    fn entry_gap(self, rem_size: Pixels, height: ListItemHeight, visible_count: usize) -> Pixels {
        if visible_count < 2 {
            return px(0.0);
        }
        if height == ListItemHeight::Adaptive {
            return self.gap;
        }

        let preview_height = rem_size
            * FILE_LIST_PREVIEW_LINE_HEIGHT_SCALE
            * height.fixed_preview_lines().unwrap_or(1) as f32;
        let remaining_height =
            f32::from(preview_height) - f32::from(self.entry_height()) * visible_count as f32;
        px((remaining_height / visible_count.saturating_sub(1) as f32).max(f32::from(self.gap)))
    }
}

/// 一个文件列表对应一个独立实体，避免在主列表虚拟列表回调中递归更新 MainWindow。
pub struct FileListView {
    paths: Arc<Vec<PathBuf>>,
    loaded_entries: HashMap<usize, FileListEntry>,
    loaded_icons: HashMap<usize, Arc<RenderImage>>,
    loading: HashSet<usize>,
    failures: HashSet<usize>,
    cache_range: Range<usize>,
}

impl FileListView {
    pub fn new(paths: Arc<Vec<PathBuf>>) -> Self {
        Self {
            paths,
            loaded_entries: HashMap::new(),
            loaded_icons: HashMap::new(),
            loading: HashSet::new(),
            failures: HashSet::new(),
            cache_range: 0..0,
        }
    }

    pub(crate) fn path_count(&self) -> usize {
        self.paths.len()
    }

    pub(crate) fn preview_summary(&self) -> String {
        self.paths
            .iter()
            .map(|path| {
                path.file_name()
                    .unwrap_or(path.as_os_str())
                    .to_string_lossy()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 在窗口销毁或外层列表淘汰该实体前取出图像，以便由窗口释放 GPU 资源。
    pub fn take_images(&mut self) -> Vec<Arc<RenderImage>> {
        self.loaded_icons.drain().map(|(_, image)| image).collect()
    }

    fn trim_cache(&mut self, range: Range<usize>, window: &mut Window) {
        self.cache_range = range.clone();
        self.loaded_entries.retain(|index, _| range.contains(index));
        let stale_indices = self
            .loaded_icons
            .keys()
            .filter(|index| !range.contains(index))
            .copied()
            .collect::<Vec<_>>();
        for index in stale_indices {
            if let Some(image) = self.loaded_icons.remove(&index) {
                let _ = window.drop_image(image);
            }
        }
        self.failures.retain(|index| range.contains(index));
    }

    fn ensure_entries_for_range(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        for index in range {
            let Some(path) = self.paths.get(index).cloned() else {
                continue;
            };
            if self.loaded_entries.contains_key(&index)
                || self.failures.contains(&index)
                || !self.loading.insert(index)
            {
                continue;
            }
            let task = cx.background_spawn(async move {
                let metadata = fs::metadata(&path).ok();
                let exists = metadata.is_some();
                let (size, is_directory) = metadata
                    .as_ref()
                    .map(|metadata| {
                        (
                            (!metadata.is_dir()).then_some(metadata.len()),
                            metadata.is_dir(),
                        )
                    })
                    .unwrap_or((None, false));
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_owned)
                    .unwrap_or_else(|| path.to_string_lossy().into_owned());
                let icon = exists
                    .then(|| file_icon_provider::get_file_icon(&path, FILE_ICON_SIZE).ok())
                    .flatten();
                (
                    FileListEntry {
                        path,
                        name,
                        size,
                        is_directory,
                        exists,
                    },
                    icon,
                )
            });
            cx.spawn(async move |this, cx| {
                let (entry, icon) = task.await;
                if let Some(this) = this.upgrade() {
                    this.update(cx, |this, cx| {
                        this.loading.remove(&index);
                        if !this.cache_range.contains(&index) {
                            return;
                        }
                        this.loaded_entries.insert(index, entry);
                        if let Some(icon) = icon.and_then(render_icon) {
                            this.loaded_icons.insert(index, icon);
                        }
                        cx.notify();
                    });
                }
            })
            .detach();
        }
    }

    fn entry_icon(&self, index: usize) -> Option<Arc<RenderImage>> {
        self.loaded_icons.get(&index).cloned()
    }

    fn render_entry(
        &self,
        index: usize,
        presentation: FileListPresentation,
        entry_height: Pixels,
        text_gap: Pixels,
        theme: ThemeColor,
        translator: &Translator,
    ) -> AnyElement {
        let Some(entry) = self.loaded_entries.get(&index) else {
            return div()
                .id(("file-entry-loading", index as u64))
                .h(entry_height)
                .min_h_0()
                .w_full()
                .flex_none()
                .flex()
                .items_center()
                .text_size(presentation.text_size)
                .line_height(presentation.line_height)
                .text_color(theme.muted_foreground)
                .child(translator.translate("main.file.loading"))
                .into_any_element();
        };

        let icon_element = if !entry.exists {
            let icon_name = if entry.is_directory {
                "folder"
            } else if entry
                .path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    matches!(
                        extension.to_ascii_lowercase().as_str(),
                        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "svg"
                    )
                })
            {
                "photo"
            } else {
                "file-description"
            };
            gpui_component::Icon::empty()
                .path(format!("assets/icons/{icon_name}.svg"))
                .size(presentation.icon_size)
                .text_color(theme.danger)
                .into_any_element()
        } else if let Some(image) = self.entry_icon(index) {
            img(image)
                .size(presentation.icon_size)
                .object_fit(ObjectFit::Contain)
                .into_any_element()
        } else {
            let icon_name = if entry.is_directory {
                "folder"
            } else {
                "file-description"
            };
            gpui_component::Icon::empty()
                .path(format!("assets/icons/{icon_name}.svg"))
                .size(presentation.icon_size)
                .text_color(theme.foreground)
                .into_any_element()
        };
        let name_color = if entry.exists {
            theme.foreground
        } else {
            theme.danger
        };
        let text = if presentation.show_path {
            let name_row = div()
                .w_full()
                .min_w_0()
                .flex()
                .items_center()
                .gap(presentation.gap)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(presentation.text_size)
                        .line_height(presentation.line_height)
                        .text_color(name_color)
                        .child(entry.name.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .w(presentation.text_size * if entry.exists { 6.0 } else { 8.0 })
                        .flex()
                        .items_center()
                        .justify_end()
                        .truncate()
                        .text_size(presentation.text_size)
                        .line_height(presentation.line_height)
                        .text_color(if entry.exists {
                            theme.muted_foreground
                        } else {
                            theme.danger
                        })
                        .child(if entry.exists {
                            i18n::file_size(translator, entry.size, entry.is_directory)
                        } else {
                            translator.translate("main.file.missing")
                        }),
                );
            div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .gap(text_gap)
                .child(name_row)
                .child(
                    div()
                        .w_full()
                        .truncate()
                        .text_size(presentation.text_size)
                        .line_height(presentation.line_height)
                        .text_color(if entry.exists {
                            theme.muted_foreground
                        } else {
                            theme.danger
                        })
                        .child(entry.path.to_string_lossy().into_owned()),
                )
                .into_any_element()
        } else {
            let mut name_row = div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(presentation.gap)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(presentation.text_size)
                        .line_height(presentation.line_height)
                        .text_color(name_color)
                        .child(entry.name.clone()),
                );
            if !entry.exists {
                name_row = name_row.child(
                    div()
                        .flex_none()
                        .text_size(presentation.text_size)
                        .line_height(presentation.line_height)
                        .text_color(theme.danger)
                        .child(translator.translate("main.file.missing")),
                );
            }
            name_row.into_any_element()
        };
        let row = div()
            .id(("file-entry", index as u64))
            .h(entry_height)
            .min_h_0()
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .gap(presentation.gap)
            .child(
                div()
                    .size(presentation.icon_size)
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon_element),
            )
            .child(text);
        row.into_any_element()
    }
}

impl Render for FileListView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let translator = i18n::translator(cx);
        let appearance = crate::config_runtime::appearance_config(cx);
        let item_height = if crate::config_runtime::clipboard_config(cx)
            .window_position
            .uses_horizontal_list()
        {
            ListItemHeight::Large
        } else {
            appearance.list_item_height
        };
        let presentation = file_list_presentation(
            item_height,
            window.rem_size(),
            usize::from(appearance.adaptive_max_lines),
        )
        .with_font_metrics(AppFontMetrics::from_window(window, cx));
        let visible_count = self.paths.len().min(presentation.visible_entries);
        let entry_height = presentation.entry_height();
        let entry_gap = presentation.entry_gap(window.rem_size(), item_height, visible_count);
        let text_gap = presentation.text_gap();
        let cache_range = 0..visible_count;
        self.trim_cache(cache_range.clone(), window);
        self.ensure_entries_for_range(cache_range, cx);
        let rows = (0..visible_count)
            .map(|index| {
                self.render_entry(
                    index,
                    presentation,
                    entry_height,
                    text_gap,
                    cx.theme().colors,
                    &translator,
                )
            })
            .collect::<Vec<_>>();

        div()
            .size_full()
            .flex()
            .flex_col()
            .gap(entry_gap)
            .justify_start()
            .overflow_hidden()
            .children(rows)
    }
}

fn render_icon(icon: Icon) -> Option<Arc<RenderImage>> {
    let pixels = usize::try_from(icon.width)
        .ok()?
        .checked_mul(usize::try_from(icon.height).ok()?)?
        .checked_mul(4)?;
    if icon.pixels.len() != pixels {
        return None;
    }
    let mut bgra = icon.pixels;
    for pixel in bgra.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let image = image::RgbaImage::from_raw(icon.width, icon.height, bgra)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(image)])))
}

#[cfg(test)]
mod tests {
    use super::{adaptive_file_list_entry_count, file_list_presentation, file_list_preview_height};
    use crate::config::ListItemHeight;
    use crate::i18n::{Locale, Translator};
    use gpui::px;

    fn zh_translator() -> Translator {
        Translator::builtin(Locale::zh_cn())
    }

    #[test]
    fn tall_app_fonts_keep_file_names_and_paths_inside_preview() {
        for height in ListItemHeight::ALL {
            for scale_factor in [1.0, 1.25, 1.5, 2.0] {
                let font_metrics = crate::ui::fonts::AppFontMetrics::for_test(1.5, scale_factor);
                let presentation =
                    file_list_presentation(height, px(16.0), 8).with_font_metrics(font_metrics);
                let preview_height = file_list_preview_height(
                    height,
                    px(16.0),
                    8,
                    Some(presentation.visible_entries),
                    Some(font_metrics),
                );
                assert!(presentation.line_height >= presentation.text_size * 1.5);
                let text_height = presentation.line_height
                    * if presentation.show_path { 2.0 } else { 1.0 }
                    + presentation.text_gap();
                assert!(presentation.entry_height() >= text_height);
                let count = presentation.visible_entries;
                let content_height = presentation.entry_height() * count as f32
                    + presentation.entry_gap(px(16.0), height, count)
                        * count.saturating_sub(1) as f32;
                assert!(preview_height >= content_height);
                assert!(
                    preview_height
                        >= file_list_preview_height(height, px(16.0), 8, Some(count), None)
                );
            }
        }
    }

    #[test]
    fn file_size_uses_human_readable_units() {
        let translator = zh_translator();
        assert_eq!(
            crate::i18n::file_size(&translator, Some(512), false),
            "512 B"
        );
        assert_eq!(
            crate::i18n::file_size(&translator, Some(1024), false),
            "1.0 KB"
        );
        assert_eq!(
            crate::i18n::file_size(&translator, Some(1024 * 1024), false),
            "1.0 MB"
        );
    }

    #[test]
    fn directories_and_missing_files_show_explicit_status() {
        let translator = zh_translator();
        assert_eq!(crate::i18n::file_size(&translator, None, true), "文件夹");
        assert_eq!(crate::i18n::file_size(&translator, None, false), "大小未知");
    }

    #[test]
    fn large_and_medium_file_lists_show_two_entries_with_paths() {
        for height in [ListItemHeight::Large, ListItemHeight::Medium] {
            let presentation = file_list_presentation(height, px(16.0), 8);
            assert_eq!(presentation.visible_entries, 2);
            assert!(presentation.show_path);
        }
    }

    #[test]
    fn small_file_lists_show_one_entry_with_path() {
        let presentation = file_list_presentation(ListItemHeight::Small, px(16.0), 8);
        assert_eq!(presentation.visible_entries, 1);
        assert!(presentation.show_path);
    }

    #[test]
    fn extra_small_file_lists_show_one_entry_without_path() {
        let presentation = file_list_presentation(ListItemHeight::ExtraSmall, px(16.0), 8);
        assert_eq!(presentation.visible_entries, 1);
        assert!(!presentation.show_path);
    }

    #[test]
    fn adaptive_file_lists_scale_with_the_maximum_preview_lines() {
        for (max_lines, expected_entries, expected_preview_lines) in [
            (1, 1, 1),
            (2, 1, 2),
            (3, 1, 2),
            (4, 2, 4),
            (8, 4, 8),
            (15, 7, 14),
        ] {
            assert_eq!(adaptive_file_list_entry_count(max_lines), expected_entries);
            let expected_height = if expected_preview_lines == 1 {
                14.0
            } else {
                30.0 * expected_entries as f32 + 6.0 * (expected_entries - 1) as f32
            };
            assert_eq!(
                file_list_preview_height(
                    ListItemHeight::Adaptive,
                    px(16.0),
                    max_lines,
                    Some(expected_entries),
                    None,
                ),
                px(expected_height)
            );
        }

        assert!(!file_list_presentation(ListItemHeight::Adaptive, px(16.0), 1).show_path);
        assert!(file_list_presentation(ListItemHeight::Adaptive, px(16.0), 4).show_path);
    }

    #[test]
    fn adaptive_file_list_height_follows_actual_file_count() {
        assert_eq!(
            file_list_preview_height(ListItemHeight::Adaptive, px(16.0), 8, Some(1), None),
            px(30.0)
        );
        assert_eq!(
            file_list_preview_height(ListItemHeight::Adaptive, px(16.0), 8, Some(4), None),
            px(138.0)
        );
        assert_eq!(
            file_list_preview_height(ListItemHeight::Adaptive, px(16.0), 8, Some(10), None),
            px(138.0)
        );
        assert_eq!(
            file_list_preview_height(ListItemHeight::Adaptive, px(16.0), 8, None, None),
            px(30.0)
        );
    }

    #[test]
    fn fixed_file_list_heights_fill_their_preview_slots() {
        for (height, expected_height) in [
            (ListItemHeight::Large, 70.0),
            (ListItemHeight::Medium, 52.5),
            (ListItemHeight::Small, 35.0),
            (ListItemHeight::ExtraSmall, 17.5),
        ] {
            assert_eq!(
                file_list_preview_height(height, px(16.0), 8, Some(1), None),
                px(expected_height)
            );
        }
    }

    #[test]
    fn file_entries_keep_text_together_and_separate_adjacent_entries() {
        let presentation = file_list_presentation(ListItemHeight::Large, px(16.0), 8);

        assert_eq!(presentation.entry_height(), px(30.0));
        assert_eq!(presentation.text_gap(), px(6.0));
        assert_eq!(
            presentation.entry_gap(px(16.0), ListItemHeight::Adaptive, 4),
            px(6.0)
        );
        assert_eq!(
            presentation.entry_gap(px(16.0), ListItemHeight::Large, 2),
            px(10.0)
        );
    }
}
