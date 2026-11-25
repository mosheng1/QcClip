use crate::ui::scale::ui_rem;

use std::{collections::HashSet, rc::Rc, sync::Arc};

use gpui::prelude::*;
use gpui::{AnyElement, Context, IntoElement, RenderImage, Window, div, img, px, size};
use gpui_component::{ActiveTheme as _, tooltip::Tooltip};

use crate::{
    config_runtime,
    emoji::{self, EmojiCategoryId, EmojiItem},
    i18n,
    windows::main::{state::MainWindow, tabs::SymbolCategoryId},
};

use super::{
    symbol_layout, symbol_list, symbol_search,
    symbol_sidebar::{self, SymbolSidebarItem, SymbolSidebarSelectHandler},
};

type EmojiListRow = symbol_list::ListRow<EmojiCategoryId>;

pub(crate) struct EmojiRowsCache {
    query: String,
    columns: u16,
    recent_emojis: Vec<String>,
    locale: i18n::Locale,
    rows: Rc<Vec<EmojiListRow>>,
}

pub fn render(main: &mut MainWindow, window: &Window, cx: &mut Context<MainWindow>) -> AnyElement {
    let translator = i18n::translator(cx);
    let horizontal_categories = config_runtime::clipboard_config(cx)
        .window_position
        .uses_horizontal_list();
    let is_emoji = main.symbol_category == SymbolCategoryId::Emoji;
    let content = if is_emoji {
        main.ensure_emoji_resources(cx);
        let data = emoji::data();
        let columns = symbol_layout::grid_columns(
            main,
            window,
            symbol_list::CELL_MIN_WIDTH,
            horizontal_categories,
        );
        let query = main.symbol_search_query.trim().to_lowercase();
        let locale = translator.locale().clone();
        let cache_matches = main.emoji_rows_cache.as_ref().is_some_and(|cache| {
            cache.query == query
                && cache.columns == columns
                && cache.recent_emojis == main.recent_emojis
                && cache.locale == locale
        });
        let rows = if cache_matches {
            main.emoji_rows_cache.as_ref().unwrap().rows.clone()
        } else {
            let rows = Rc::new(build_rows(main, data, columns, &translator));
            main.emoji_category_row_indices.clear();
            for (index, row) in rows.iter().enumerate() {
                if let EmojiListRow::Header { category, .. } = row {
                    main.emoji_category_row_indices.insert(*category, index);
                }
            }

            main.emoji_rows_cache = Some(EmojiRowsCache {
                query,
                columns,
                recent_emojis: main.recent_emojis.clone(),
                locale,
                rows: rows.clone(),
            });
            rows
        };
        let scroll_target = main.emoji_scroll_target.take();
        if let Some(category) = scroll_target {
            if let Some(index) = main.emoji_category_row_indices.get(&category).copied() {
                let mut offset = main.emoji_scroll_handle.offset();
                offset.y =
                    px(-symbol_list::row_top(&rows, index)
                        * crate::ui::scale::window_scale(window));
                main.emoji_scroll_handle.set_offset(offset);
            }
        } else if main.symbol_search_query.trim().is_empty()
            && let Some(category) = symbol_list::category_at_scroll(
                &rows,
                main.emoji_scroll_handle.offset().y / crate::ui::scale::window_scale(window),
            )
        {
            main.emoji_category = category;
        }

        render_virtual_list(main, data, rows, columns, cx)
    } else {
        let key = match main.symbol_category {
            SymbolCategoryId::Symbols => "main.symbol.placeholder.symbols",
            SymbolCategoryId::Gallery => "main.symbol.placeholder.gallery",
            SymbolCategoryId::Emoji => unreachable!(),
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(cx.theme().colors.muted_foreground)
            .child(translator.translate(key))
            .into_any_element()
    };

    let sidebar_items = if is_emoji {
        emoji::EMOJI_CATEGORIES
            .iter()
            .map(|definition| {
                SymbolSidebarItem::new(
                    definition.id as u32,
                    definition.icon,
                    translator.translate(definition.label_key),
                )
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let sidebar_handler: SymbolSidebarSelectHandler = Rc::new(|this, item_id, window, cx| {
        let Some(category) = emoji::EMOJI_CATEGORIES
            .iter()
            .find(|definition| definition.id as u32 == item_id)
            .map(|definition| definition.id)
        else {
            return;
        };
        this.select_emoji_category(category, window, cx);
    });
    let category_area_width =
        f32::from(window.viewport_size().width) / crate::ui::scale::window_scale(window) / 2.0;
    let show_category_labels = horizontal_categories
        && symbol_sidebar::horizontal_labels_fit(
            sidebar_items.iter().map(|item| item.label.as_str()),
            category_area_width,
            16.0,
        );
    let sidebar = symbol_sidebar::render(
        &sidebar_items,
        if is_emoji {
            main.emoji_category as u32
        } else {
            u32::MAX
        },
        sidebar_handler,
        horizontal_categories,
        show_category_labels,
        cx,
    );
    let search_box = symbol_search::render(main, window, cx, None);

    symbol_layout::render(sidebar, search_box, content, horizontal_categories, cx)
}
fn build_rows(
    main: &MainWindow,
    data: &emoji::EmojiData,
    columns: u16,
    translator: &i18n::Translator,
) -> Vec<EmojiListRow> {
    let query = main.symbol_search_query.trim().to_lowercase();
    let mut rows = Vec::new();

    if query.is_empty() {
        let recent_indices = main
            .recent_emojis
            .iter()
            .filter_map(|glyph| data.items.iter().position(|item| item.glyph == *glyph))
            .collect::<Vec<_>>();
        let recent_is_empty = recent_indices.is_empty();
        symbol_list::append_section(
            &mut rows,
            EmojiCategoryId::Recent,
            translator.translate("emoji.recent"),
            recent_indices,
            columns,
        );
        if recent_is_empty {
            rows.push(EmojiListRow::Empty {
                label: translator.translate("emoji.empty"),
            });
        }

        for category in emoji::EMOJI_CATEGORIES
            .iter()
            .map(|category| category.id)
            .filter(|category| *category != EmojiCategoryId::Recent)
        {
            let indices = data
                .items
                .iter()
                .enumerate()
                .filter_map(|(index, item)| (item.category == category).then_some(index))
                .collect::<Vec<_>>();
            let label = emoji::EMOJI_CATEGORIES
                .iter()
                .find(|definition| definition.id == category)
                .map(|definition| translator.translate(definition.label_key))
                .unwrap_or_default();
            if !indices.is_empty() {
                symbol_list::append_section(&mut rows, category, label, indices, columns);
            }
        }
    } else {
        for category in emoji::EMOJI_CATEGORIES
            .iter()
            .map(|category| category.id)
            .filter(|category| *category != EmojiCategoryId::Recent)
        {
            let indices = data
                .items
                .iter()
                .enumerate()
                .filter_map(|(index, item)| {
                    (item.category == category && item.search_text.contains(&query))
                        .then_some(index)
                })
                .collect::<Vec<_>>();
            let label = emoji::EMOJI_CATEGORIES
                .iter()
                .find(|definition| definition.id == category)
                .map(|definition| translator.translate(definition.label_key))
                .unwrap_or_default();
            if !indices.is_empty() {
                symbol_list::append_section(&mut rows, category, label, indices, columns);
            }
        }
        if rows.is_empty() {
            rows.push(EmojiListRow::Empty {
                label: translator.translate("emoji.search_empty"),
            });
        }
    }

    rows
}

fn render_virtual_list(
    main: &MainWindow,
    data: &'static emoji::EmojiData,
    rows: Rc<Vec<EmojiListRow>>,
    columns: u16,
    cx: &mut Context<MainWindow>,
) -> AnyElement {
    let item_sizes = Rc::new(
        rows.iter()
            .map(|row| {
                size(
                    px(0.0),
                    px(symbol_list::row_height(row) * crate::ui::scale::theme_scale(cx)),
                )
            })
            .collect(),
    );
    let loading_rows = rows.clone();
    let main_window = cx.weak_entity();
    let list = symbol_list::render_sized_virtual_list(
        "emoji-list",
        item_sizes,
        main.emoji_scroll_handle.clone(),
        MainWindow::handle_emoji_scroll_wheel,
        move |this, range, _, cx| {
            range
                .filter_map(|index| {
                    rows.get(index).map(|row| match row {
                        EmojiListRow::Header { label, .. } => symbol_list::render_header(label, cx),
                        EmojiListRow::Empty { label } => symbol_list::render_empty(label, cx),
                        EmojiListRow::Items { item_indices, .. } => {
                            render_emoji_row(index, item_indices, data, columns, this, cx)
                        }
                    })
                })
                .collect()
        },
        cx,
    );
    div()
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .flex_col()
        .child(list)
        .on_children_prepainted(move |_, window, cx| {
            let _ = main_window.update(cx, |this, cx| {
                // 首行测量也会调用行渲染，完成布局后再按实际视口调度和淘汰图片。
                let scale = crate::ui::scale::window_scale(window);
                let top = -f32::from(this.emoji_scroll_handle.offset().y) / scale;
                let height =
                    f32::from(this.emoji_scroll_handle.base_handle().bounds().size.height) / scale;
                let mut row_top = 0.0;
                let mut indices = HashSet::new();
                for row in loading_rows.iter() {
                    let row_bottom = row_top + symbol_list::row_height(row);
                    if row_bottom >= top - symbol_list::ROW_HEIGHT
                        && row_top <= top + height + symbol_list::ROW_HEIGHT
                        && let EmojiListRow::Items { item_indices, .. } = row
                    {
                        indices.extend(item_indices.iter().copied());
                    }
                    row_top = row_bottom;
                }
                this.ensure_emoji_images(indices, window, cx);
            });
        })
        .into_any_element()
}

fn render_emoji_row(
    row_index: usize,
    item_indices: &[usize],
    data: &'static emoji::EmojiData,
    columns: u16,
    main: &MainWindow,
    cx: &mut Context<MainWindow>,
) -> AnyElement {
    symbol_list::render_grid_row(
        row_index,
        item_indices,
        &data.items,
        columns,
        |row_index, item_index, item, cx| {
            render_emoji_item(
                row_index,
                item_index,
                item,
                main.emoji_image(item_index),
                cx,
            )
        },
        cx,
    )
}
fn render_emoji_item(
    row_index: usize,
    item_index: usize,
    item: &EmojiItem,
    image: Option<Arc<RenderImage>>,
    cx: &mut Context<MainWindow>,
) -> AnyElement {
    let glyph = item.glyph.clone();
    let tooltip = item.name.clone();
    let item_id = ((row_index as u32) << 16) | item_index as u32;
    div()
        .id(("emoji-item", item_id))
        .h(ui_rem(42.0))
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .rounded_lg()
        .cursor_pointer()
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .hover(|this| this.bg(cx.theme().colors.secondary_hover))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.paste_emoji(glyph.clone(), cx);
        }))
        .child(
            div()
                .size(ui_rem(34.0))
                .flex()
                .items_center()
                .justify_center()
                .overflow_hidden()
                .children(image.map(|image| {
                    // 隐藏时最后一帧可能仍被保留，元素只持弱引用，避免阻止图片释放。
                    let image = Arc::downgrade(&image);
                    img(move |_: &mut Window, _: &mut gpui::App| image.upgrade().map(Ok))
                        .size(ui_rem(28.0))
                })),
        )
        .into_any_element()
}
