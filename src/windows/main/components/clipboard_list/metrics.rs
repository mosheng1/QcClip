use super::*;

#[derive(Clone, Copy, PartialEq)]
pub(super) struct ListItemMetrics {
    pub(super) rem_size: Pixels,
    pub(super) row_height: Pixels,
    pub(super) content_height: Pixels,
    pub(super) card_vertical_inset: Pixels,
    pub(super) horizontal_card_inset: Pixels,
    pub(super) style: ListItemStyle,
    pub(super) vertical_padding: Pixels,
    pub(super) meta_row_height: Pixels,
    pub(super) meta_font_size: Pixels,
    pub(super) preview_font_size: Pixels,
    pub(super) preview_line_height: Pixels,
    pub(super) preview_height: Pixels,
    pub(super) height: ListItemHeight,
    pub(super) adaptive_max_lines: usize,
    pub(super) preview_line_count: usize,
    pub(super) show_meta: bool,
    pub(super) info_row_progress: f32,
    pub(super) only_show_notes: bool,
    pub(super) horizontal: bool,
    font_metrics: Option<crate::ui::fonts::AppFontMetrics>,
}

impl ListItemMetrics {
    pub(super) fn from_rem_size(
        rem_size: Pixels,
        style: ListItemStyle,
        card_item_gap: CardItemGap,
        height: ListItemHeight,
        adaptive_max_lines: u8,
    ) -> Self {
        let vertical_padding = rem_size * ROW_VERTICAL_PADDING_SCALE;
        let preview_font_size = rem_size * PREVIEW_FONT_SCALE;
        let preview_line_height = preview_font_size * PREVIEW_LINE_HEIGHT_SCALE;
        let meta_row_height = rem_size * META_ROW_HEIGHT_SCALE;
        let adaptive_max_lines =
            usize::from(adaptive_max_lines.clamp(ADAPTIVE_MAX_LINES_MIN, ADAPTIVE_MAX_LINES_MAX));
        let preview_line_count = height
            .fixed_preview_lines()
            .unwrap_or(DEFAULT_ADAPTIVE_PREVIEW_LINES.min(adaptive_max_lines));
        let card_vertical_inset = match style {
            ListItemStyle::Seamless => px(0.0),
            ListItemStyle::Card => px(card_item_gap.pixels() / 2.0),
        };
        let metrics = Self {
            rem_size,
            row_height: px(0.0),
            content_height: px(0.0),
            card_vertical_inset,
            horizontal_card_inset: if style == ListItemStyle::Card {
                px(card_item_gap.pixels() / 2.0)
            } else {
                px(0.0)
            },
            style,
            vertical_padding,
            meta_row_height,
            meta_font_size: meta_row_height * 0.6,
            preview_font_size,
            preview_line_height,
            preview_height: px(0.0),
            height,
            adaptive_max_lines,
            preview_line_count,
            show_meta: height.shows_meta(),
            info_row_progress: 1.0,
            only_show_notes: false,
            horizontal: false,
            font_metrics: None,
        };
        metrics.with_preview_line_count(preview_line_count)
    }

    pub(super) fn with_font_metrics(
        mut self,
        font_metrics: crate::ui::fonts::AppFontMetrics,
    ) -> Self {
        self.font_metrics = Some(font_metrics);
        self.preview_line_height =
            font_metrics.line_height(self.preview_font_size, self.preview_line_height);
        self.meta_row_height = font_metrics.line_height(self.meta_font_size, self.meta_row_height);
        let metrics = self.with_preview_line_count(self.preview_line_count);
        if self.height == ListItemHeight::Adaptive {
            return metrics;
        }
        // 固定行高需同时容纳文本和文件预览，保证虚拟列表占位与绘制高度一致。
        let file_height = file_list_preview_height(
            self.height,
            self.rem_size,
            self.adaptive_max_lines,
            None,
            self.font_metrics,
        );
        metrics.with_preview_height(
            metrics.preview_height.max(file_height),
            metrics.preview_line_count,
        )
    }

    pub(super) fn with_preview_line_count(self, preview_line_count: usize) -> Self {
        let max_preview_lines = self
            .height
            .fixed_preview_lines()
            .unwrap_or(self.adaptive_max_lines);
        self.with_preview_line_count_capped(preview_line_count, max_preview_lines)
    }

    pub(super) fn with_preview_line_count_capped(
        self,
        preview_line_count: usize,
        max_preview_lines: usize,
    ) -> Self {
        let max_preview_lines = max_preview_lines.max(1);
        let preview_line_count = preview_line_count.max(1).min(max_preview_lines);
        let preview_height = self.preview_line_height * preview_line_count as f32;
        self.with_preview_height(preview_height, preview_line_count)
    }

    pub(super) fn with_preview_height(
        mut self,
        preview_height: Pixels,
        preview_line_count: usize,
    ) -> Self {
        self.preview_line_count = preview_line_count;
        self.preview_height = preview_height;
        self.content_height = self.vertical_padding * 2.0
            + if self.show_meta {
                self.meta_row_height * self.info_row_progress
            } else {
                px(0.0)
            }
            + self.preview_height
            + px(MAX_ROW_BORDER_WIDTH);
        self.row_height = match self.style {
            ListItemStyle::Seamless => self.content_height,
            ListItemStyle::Card => self.content_height + self.card_vertical_inset * 2.0,
        };
        self
    }

    pub(super) fn resource_preview_height(self) -> Pixels {
        if self.height == ListItemHeight::Adaptive {
            // 自适应项可以比大行高更高，但图片解码缩略图只按大行高限制尺寸。
            let large_preview_lines = ListItemHeight::Large.fixed_preview_lines().unwrap_or(1);
            self.preview_line_height * large_preview_lines as f32
        } else {
            self.preview_height
        }
    }

    pub(super) fn with_adaptive_image_dimensions(
        self,
        dimensions: ImageDimensions,
        available_width: Pixels,
    ) -> Self {
        let max_preview_height = self.preview_line_height * ADAPTIVE_IMAGE_PREVIEW_LINES as f32;
        let natural_height = f32::from(available_width).max(1.0) * dimensions.height as f32
            / dimensions.width as f32;
        let preview_height = px(natural_height.clamp(
            f32::from(self.preview_line_height),
            f32::from(max_preview_height),
        ));
        let preview_line_count =
            (f32::from(preview_height) / f32::from(self.preview_line_height)).ceil() as usize;
        self.with_preview_height(preview_height, preview_line_count)
    }

    /// 按 HTML 中文本和图片的实际顺序累计自适应预览高度。
    ///
    /// 图片高度上限与列表渲染时传给 HTML 渲染器的上限一致，保证估算高度与绘制高度相同。
    fn with_adaptive_html_segments(
        self,
        segments: &[html_preview::PreviewSegment],
        available_width: Pixels,
        window: &Window,
    ) -> Self {
        let line_height = f32::from(self.preview_line_height).max(1.0);
        let max_image_height = line_height * ADAPTIVE_IMAGE_PREVIEW_LINES as f32;
        let max_lines = self.adaptive_max_lines.max(ADAPTIVE_IMAGE_PREVIEW_LINES);
        let max_height = line_height * max_lines as f32;
        let available_width = f32::from(available_width).max(1.0);
        let mut height = 0.0;
        for segment in segments {
            if height >= max_height {
                break;
            }
            match segment {
                html_preview::PreviewSegment::Text(text) => {
                    let remaining_lines = ((max_height - height) / line_height).ceil() as usize;
                    let lines = measure_text_lines(
                        text,
                        px(available_width),
                        self.preview_font_size,
                        remaining_lines.max(1),
                        window,
                    );
                    // 渲染器为每个文本块追加 2px 底部间距。
                    height += line_height * lines as f32 + 2.0;
                }
                html_preview::PreviewSegment::Image(size) => {
                    let width = f32::from(size.width).max(1.0);
                    let natural_height = f32::from(size.height).max(1.0);
                    let fitted_height = natural_height * (available_width / width).min(1.0);
                    height += fitted_height.min(max_image_height);
                }
            }
        }
        let height = height.clamp(line_height, max_height);
        let preview_line_count = (height / line_height).ceil() as usize;
        self.with_preview_height(px(height), preview_line_count)
    }

    pub(super) fn for_preview(
        self,
        preview: Option<&FormatPreview>,
        html_images: &HashMap<PathBuf, Arc<RenderImage>>,
        file_count: Option<usize>,
        available_width: Pixels,
        window: &Window,
    ) -> Self {
        if preview.is_some_and(|preview| preview.content_kind == ContentKind::FileList) {
            let preview_height = file_list_preview_height(
                self.height,
                self.rem_size,
                self.adaptive_max_lines,
                file_count,
                self.font_metrics,
            );
            let preview_line_count = self.height.fixed_preview_lines().unwrap_or_else(|| {
                (f32::from(preview_height) / f32::from(self.preview_line_height))
                    .ceil()
                    .max(1.0) as usize
            });
            let preview_height = if self.height == ListItemHeight::Adaptive {
                preview_height
            } else {
                preview_height.max(self.preview_height)
            };
            return self.with_preview_height(preview_height, preview_line_count);
        }

        if self.height != ListItemHeight::Adaptive {
            return self;
        }

        if preview.is_some_and(|preview| preview.content_kind == ContentKind::Image) {
            if let Some(dimensions) = preview.and_then(|preview| preview.image_dimensions) {
                return self.with_adaptive_image_dimensions(dimensions, available_width);
            }
            return self.with_preview_line_count_capped(
                ADAPTIVE_IMAGE_PREVIEW_LINES,
                ADAPTIVE_IMAGE_PREVIEW_LINES,
            );
        }

        if preview_color(preview).is_some() {
            return self.with_preview_line_count(1);
        }

        if let Some(html) = preview
            .filter(|preview| preview.role() == FormatRole::Html)
            .and_then(|preview| preview.html_content.as_deref())
        {
            let segments = html_preview::preview_segments(html, html_images);
            // 没有已加载图片时与纯文本预览一致，避免图片缺失时改变原有行高。
            if segments
                .iter()
                .any(|segment| matches!(segment, html_preview::PreviewSegment::Image(_)))
            {
                return self.with_adaptive_html_segments(&segments, available_width, window);
            }
        }

        let preview_line_count = preview
            .and_then(|preview| (!preview.text.is_empty()).then_some(preview.text.as_str()))
            .map(|text| {
                measure_text_lines(
                    text,
                    available_width,
                    self.preview_font_size,
                    self.adaptive_max_lines,
                    window,
                )
            })
            .unwrap_or(DEFAULT_ADAPTIVE_PREVIEW_LINES.min(self.adaptive_max_lines));
        self.with_preview_line_count(preview_line_count)
    }
}

pub(super) fn estimate_text_lines(
    text: &str,
    available_width: Pixels,
    font_size: Pixels,
    max_lines: usize,
) -> usize {
    let max_lines = max_lines.max(1);
    let width_in_em = (f32::from(available_width) / f32::from(font_size)).max(1.0);
    let mut line_count = 1;
    let mut line_width = 0.0;
    for character in text.chars() {
        if character == '\n' {
            line_count += 1;
            line_width = 0.0;
        } else {
            let character_width = if character == '\t' {
                4.0
            } else if character.is_ascii() {
                0.55
            } else {
                1.0
            };
            if line_width > 0.0 && line_width + character_width > width_in_em {
                line_count += 1;
                line_width = character_width;
            } else {
                line_width += character_width;
            }
        }
        if line_count >= max_lines {
            return max_lines;
        }
    }
    line_count.clamp(1, max_lines)
}

pub(super) fn measure_text_lines(
    text: &str,
    available_width: Pixels,
    font_size: Pixels,
    max_lines: usize,
    window: &Window,
) -> usize {
    let text_style = window.text_style();
    let runs = [text_style.to_run(text.len())];
    window
        .text_system()
        .shape_text(
            text.to_owned(),
            font_size,
            &runs,
            Some(available_width),
            Some(max_lines.max(1)),
        )
        .map(|lines| {
            shaped_line_count(
                lines.iter().map(|line| line.wrap_boundaries().len()),
                max_lines,
            )
        })
        .unwrap_or_else(|_| estimate_text_lines(text, available_width, font_size, max_lines))
}

pub(super) fn shaped_line_count(
    wrap_boundary_counts: impl IntoIterator<Item = usize>,
    max_lines: usize,
) -> usize {
    let line_count = wrap_boundary_counts
        .into_iter()
        .map(|wrap_count| wrap_count.saturating_add(1))
        .sum::<usize>();
    line_count.clamp(1, max_lines.max(1))
}

pub(super) fn available_preview_width(
    window_width: Pixels,
    style: ListItemStyle,
    rem_size: Pixels,
) -> Pixels {
    available_preview_width_with_card_inset(
        window_width,
        style,
        rem_size,
        px(CARD_HORIZONTAL_INSET),
    )
}

pub(super) fn available_preview_width_with_card_inset(
    window_width: Pixels,
    style: ListItemStyle,
    rem_size: Pixels,
    card_inset: Pixels,
) -> Pixels {
    let card_inset = if style == ListItemStyle::Card {
        f32::from(card_inset) * 2.0
    } else {
        0.0
    };
    px((f32::from(window_width)
        - ROW_HORIZONTAL_PADDING * 2.0 * f32::from(rem_size) / 16.0
        - card_inset)
        .max(1.0))
}

pub(super) fn item_preview(
    item: &ClipboardListItem,
    display_priority: ClipboardDisplayPriority,
) -> Option<FormatPreview> {
    select_user_visible_previews_with_priority(
        &item.preview_options,
        &display_priority_roles(display_priority),
    )
    .into_iter()
    .next()
}

pub(super) fn resolve_row_height(
    history_id: Option<HistoryItemId>,
    height: ListItemHeight,
    measured_height: Option<Pixels>,
    placeholder_height: Pixels,
    adaptive_item_heights: &mut HashMap<HistoryItemId, Pixels>,
) -> Pixels {
    if height != ListItemHeight::Adaptive {
        return placeholder_height;
    }
    if let Some(measured_height) = measured_height {
        if let Some(history_id) = history_id {
            adaptive_item_heights.insert(history_id, measured_height);
        }
        measured_height
    } else if let Some(history_id) = history_id {
        adaptive_item_heights
            .get(&history_id)
            .copied()
            .unwrap_or(placeholder_height)
    } else {
        placeholder_height
    }
}

impl ListItemMetrics {
    pub(super) fn with_horizontal_card(
        mut self,
        viewport_height: Pixels,
        show_header: bool,
    ) -> Self {
        self.horizontal = true;
        self.height = ListItemHeight::Large;
        self.card_vertical_inset = px(12.0);
        self.vertical_padding = self.rem_size * 0.75;
        let header_height = if show_header {
            self.rem_size * 2.25
        } else {
            px(0.0)
        };
        let chrome = self.vertical_padding * 2.0
            + px(MAX_ROW_BORDER_WIDTH)
            + if self.show_meta {
                (self.meta_row_height + self.rem_size * 0.25) * self.info_row_progress
            } else {
                px(0.0)
            }
            + header_height;
        let preview_height = (viewport_height - self.card_vertical_inset * 2.0 - chrome)
            .max(self.preview_line_height);
        let lines = (f32::from(preview_height) / f32::from(self.preview_line_height))
            .floor()
            .max(1.0) as usize;
        let mut metrics = self.with_preview_height(preview_height, lines);
        let info_gap = if self.show_meta {
            self.rem_size * 0.25 * self.info_row_progress
        } else {
            px(0.0)
        };
        metrics.content_height += header_height + info_gap;
        metrics.row_height += header_height + info_gap;
        if self.style == ListItemStyle::Seamless {
            metrics.row_height += self.card_vertical_inset * 2.0;
        }
        metrics
    }

    pub(super) fn horizontal_item_width(self) -> Pixels {
        self.rem_size * 18.0 + self.horizontal_card_inset * 2.0
    }

    pub(super) fn with_info_row(mut self, show_meta: bool) -> Self {
        self.show_meta = show_meta;
        self.with_preview_height(self.preview_height, self.preview_line_count)
    }

    pub(super) fn with_info_row_progress(mut self, progress: f32) -> Self {
        let progress = progress.clamp(0.0, 1.0);
        let delta = if self.show_meta {
            self.meta_row_height * (progress - self.info_row_progress)
        } else {
            px(0.0)
        };
        if self.horizontal && self.show_meta {
            self.preview_height = (self.preview_height
                - delta
                - self.rem_size * 0.25 * (progress - self.info_row_progress))
                .max(px(0.0));
            self.preview_line_count = (f32::from(self.preview_height)
                / f32::from(self.preview_line_height))
            .floor()
            .max(1.0) as usize;
        } else {
            self.content_height += delta;
            self.row_height += delta;
        }
        self.info_row_progress = progress;
        self
    }

    pub(super) fn with_note_display(mut self, only_show_notes: bool) -> Self {
        self.only_show_notes = only_show_notes;
        self
    }

    pub(super) fn shows_only_note(self, note: &str) -> bool {
        !note.is_empty() && (self.only_show_notes || self.height == ListItemHeight::ExtraSmall)
    }

    pub(super) fn for_item(
        self,
        item: &ClipboardListItem,
        display_priority: ClipboardDisplayPriority,
        html_images: &HashMap<PathBuf, Arc<RenderImage>>,
        file_count: Option<usize>,
        available_width: Pixels,
        window: &Window,
    ) -> Self {
        if self.horizontal {
            return self;
        }
        if self.shows_only_note(&item.note_text) {
            if self.height != ListItemHeight::Adaptive {
                return self;
            }
            return self.with_preview_line_count(measure_text_lines(
                &item.note_text,
                available_width,
                self.preview_font_size,
                self.adaptive_max_lines,
                window,
            ));
        }
        let preview = item_preview(item, display_priority);
        let metrics = self.for_preview(
            preview.as_ref(),
            html_images,
            file_count,
            available_width,
            window,
        );
        if !item.note_text.is_empty() && self.height == ListItemHeight::Adaptive {
            metrics.with_preview_height(
                metrics.preview_height + metrics.preview_line_height,
                metrics.preview_line_count + 1,
            )
        } else {
            metrics
        }
    }
}

#[cfg(test)]
mod item_display_tests {
    use super::*;

    #[test]
    fn horizontal_card_uses_the_viewport_height_instead_of_the_vertical_line_limit() {
        let metrics = ListItemMetrics::from_rem_size(
            px(16.0),
            ListItemStyle::Card,
            CardItemGap::Eight,
            ListItemHeight::ExtraSmall,
            3,
        )
        .with_horizontal_card(px(420.0), true);
        assert_eq!(metrics.row_height, px(420.0));
        assert!(metrics.preview_line_count > 3);
        assert!(metrics.preview_height < metrics.content_height);
    }

    #[test]
    fn horizontal_seamless_items_have_no_card_inset_or_width_gap() {
        let card = ListItemMetrics::from_rem_size(
            px(16.0),
            ListItemStyle::Card,
            CardItemGap::Eight,
            ListItemHeight::Large,
            3,
        )
        .with_horizontal_card(px(420.0), true);
        let seamless = ListItemMetrics::from_rem_size(
            px(16.0),
            ListItemStyle::Seamless,
            CardItemGap::Eight,
            ListItemHeight::Large,
            3,
        )
        .with_horizontal_card(px(420.0), true);
        let seamless_without_header = ListItemMetrics::from_rem_size(
            px(16.0),
            ListItemStyle::Seamless,
            CardItemGap::Eight,
            ListItemHeight::Large,
            3,
        )
        .with_horizontal_card(px(420.0), false);

        assert_eq!(seamless.style, ListItemStyle::Seamless);
        assert_eq!(seamless.card_vertical_inset, px(12.0));
        assert_eq!(seamless.row_height, px(420.0));
        assert_eq!(seamless_without_header.row_height, px(420.0));
        assert_eq!(
            seamless_without_header.preview_height - seamless.preview_height,
            px(36.0)
        );
        assert_eq!(
            card.horizontal_item_width() - seamless.horizontal_item_width(),
            px(CardItemGap::Eight.pixels())
        );
    }

    #[test]
    fn information_visibility_changes_height_independently_of_preview_lines() {
        for height in ListItemHeight::ALL {
            let metrics = ListItemMetrics::from_rem_size(
                px(16.0),
                ListItemStyle::Card,
                CardItemGap::Eight,
                height,
                8,
            );
            let shown = metrics.with_info_row(true);
            let hidden = metrics.with_info_row(false);
            assert_eq!(
                shown.row_height - hidden.row_height,
                metrics.meta_row_height
            );
            assert_eq!(shown.preview_height, hidden.preview_height);
            assert_eq!(shown.preview_line_count, hidden.preview_line_count);
        }
    }

    #[test]
    fn hover_information_height_tracks_progress_without_changing_vertical_preview() {
        for style in [ListItemStyle::Card, ListItemStyle::Seamless] {
            for height in ListItemHeight::ALL {
                let base =
                    ListItemMetrics::from_rem_size(px(16.0), style, CardItemGap::Eight, height, 8)
                        .with_info_row(true)
                        .with_info_row_progress(0.0);
                for progress in [0.0, 0.25, 0.5, 1.0] {
                    let expanded = base.with_info_row_progress(progress);
                    assert_eq!(
                        expanded.row_height - base.row_height,
                        base.meta_row_height * progress
                    );
                    assert_eq!(
                        expanded.content_height - base.content_height,
                        base.meta_row_height * progress
                    );
                    assert_eq!(expanded.preview_height, base.preview_height);
                    assert_eq!(
                        expanded.with_info_row_progress(0.0).row_height,
                        base.row_height
                    );
                }
            }
        }
    }

    #[test]
    fn horizontal_hover_information_uses_preview_space_within_fixed_card_height() {
        let base = ListItemMetrics::from_rem_size(
            px(16.0),
            ListItemStyle::Card,
            CardItemGap::Eight,
            ListItemHeight::Large,
            8,
        )
        .with_info_row(true)
        .with_info_row_progress(0.0)
        .with_horizontal_card(px(420.0), true);
        for progress in [0.0, 0.5, 1.0] {
            let expanded = base.with_info_row_progress(progress);
            assert_eq!(expanded.row_height, base.row_height);
            assert_eq!(expanded.content_height, base.content_height);
            assert_eq!(
                base.preview_height - expanded.preview_height,
                (base.meta_row_height + base.rem_size * 0.25) * progress
            );
        }
    }
}

#[cfg(test)]
mod note_tests {
    use super::*;

    #[test]
    fn tall_app_fonts_fit_preview_lines_and_file_slots_without_enlarging_text() {
        for style in [ListItemStyle::Card, ListItemStyle::Seamless] {
            for height in ListItemHeight::ALL {
                for scale_factor in [1.0, 1.25, 1.5, 2.0] {
                    let font_metrics =
                        crate::ui::fonts::AppFontMetrics::for_test(1.5, scale_factor);
                    let base = ListItemMetrics::from_rem_size(
                        px(16.0),
                        style,
                        CardItemGap::Eight,
                        height,
                        8,
                    );
                    let metrics = base.with_font_metrics(font_metrics);
                    assert_eq!(metrics.preview_font_size, base.preview_font_size);
                    assert_eq!(metrics.meta_font_size, base.meta_font_size);
                    assert_eq!(metrics.preview_line_count, base.preview_line_count);
                    assert!(metrics.preview_line_height >= metrics.preview_font_size * 1.5);
                    assert!(metrics.meta_row_height >= metrics.meta_font_size * 1.5);
                    assert!(
                        metrics.preview_height
                            >= metrics.preview_line_height * metrics.preview_line_count as f32
                    );
                    assert_eq!(
                        metrics.content_height,
                        metrics.vertical_padding * 2.0
                            + metrics.preview_height
                            + px(MAX_ROW_BORDER_WIDTH)
                            + if metrics.show_meta {
                                metrics.meta_row_height
                            } else {
                                px(0.0)
                            }
                    );
                    assert_eq!(
                        metrics.row_height,
                        metrics.content_height + metrics.card_vertical_inset * 2.0
                    );
                    if height != ListItemHeight::Adaptive {
                        assert!(
                            metrics.preview_height
                                >= file_list_preview_height(
                                    height,
                                    metrics.rem_size,
                                    metrics.adaptive_max_lines,
                                    Some(2),
                                    Some(font_metrics),
                                )
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn scaled_text_measurement_uses_the_rendered_horizontal_padding() {
        assert_eq!(
            available_preview_width(px(200.0), ListItemStyle::Card, px(16.0)),
            px(164.0)
        );
        assert_eq!(
            available_preview_width(px(200.0), ListItemStyle::Card, px(32.0)),
            px(144.0)
        );
        assert_eq!(
            available_preview_width(px(200.0), ListItemStyle::Seamless, px(32.0)),
            px(160.0)
        );
        assert_eq!(
            available_preview_width(px(20.0), ListItemStyle::Card, px(32.0)),
            px(1.0)
        );
    }

    #[test]
    fn notes_replace_content_only_for_extra_small_or_explicit_setting() {
        for height in ListItemHeight::ALL {
            let metrics = ListItemMetrics::from_rem_size(
                px(16.0),
                ListItemStyle::Card,
                CardItemGap::Eight,
                height,
                8,
            );
            assert!(!metrics.shows_only_note(""));
            assert_eq!(
                metrics.shows_only_note("备注"),
                height == ListItemHeight::ExtraSmall
            );
            let notes_only = metrics.with_note_display(true);
            assert!(notes_only.shows_only_note("备注"));
            assert!(!notes_only.shows_only_note(""));
            assert_eq!(notes_only.row_height, metrics.row_height);
        }
    }
}

pub(super) fn file_list_view_for_item<'a>(
    item: &ClipboardListItem,
    display_priority: ClipboardDisplayPriority,
    file_list_views: &'a HashMap<FileListKey, Entity<FileListView>>,
) -> Option<&'a Entity<FileListView>> {
    let preview = item_preview(item, display_priority)?;
    if preview.format_kind != FormatKind::FileList
        || preview.content_kind != ContentKind::FileList
        || preview.image_source.is_none()
    {
        return None;
    }
    file_list_views.get(&FileListKey::new(
        item.item_id,
        &preview.format_namespace,
        &preview.format_key,
    ))
}

pub(super) fn file_list_meta_label(translator: &Translator, file_count: usize) -> String {
    translator.translate_with_args(
        "main.preview.file_list",
        &[("count", file_count.to_string())],
    )
}

pub(super) fn image_meta_label(
    translator: &Translator,
    dimensions: Option<ImageDimensions>,
) -> String {
    let Some(dimensions) = dimensions else {
        return translator.translate("main.preview.image");
    };
    translator.translate_with_args(
        "main.preview.image_dimensions",
        &[
            ("width", dimensions.width.to_string()),
            ("height", dimensions.height.to_string()),
        ],
    )
}
