use crate::ui::scale::ui_rem;
use gpui_component::tooltip::ManagedTooltipExt as _;

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, Context, IntoElement, Window, div, px};
use gpui_component::{ActiveTheme as _, tooltip::Tooltip};

use crate::{
    config_runtime, i18n,
    symbols::{self, SymbolGroupId, SymbolItem},
    windows::main::state::MainWindow,
};

use super::{
    symbol_layout, symbol_list, symbol_search,
    symbol_sidebar::{self, SymbolSidebarItem, SymbolSidebarSelectHandler},
};

type SymbolListRow = symbol_list::ListRow<SymbolGroupId>;

pub(crate) fn render(
    main: &mut MainWindow,
    window: &Window,
    cx: &mut Context<MainWindow>,
) -> AnyElement {
    let data = symbols::data();
    let translator = i18n::translator(cx);
    let horizontal_categories = config_runtime::clipboard_config(cx)
        .window_position
        .uses_horizontal_list();
    let columns = symbol_layout::grid_columns(
        main,
        window,
        symbol_list::CELL_MIN_WIDTH,
        horizontal_categories,
    );
    let rows = Rc::new(build_rows(main, data, columns, &translator));

    main.symbol_category_row_indices.clear();
    for (index, row) in rows.iter().enumerate() {
        if let SymbolListRow::Header { category, .. } = row {
            main.symbol_category_row_indices.insert(*category, index);
        }
    }

    let scroll_target = main.symbol_scroll_target.take();
    if let Some(category) = scroll_target {
        if let Some(index) = main.symbol_category_row_indices.get(&category).copied() {
            let mut offset = main.symbol_scroll_handle.offset();
            offset.y =
                px(-symbol_list::row_top(&rows, index) * crate::ui::scale::window_scale(window));
            main.symbol_scroll_handle.set_offset(offset);
        }
    } else if main.symbol_search_query.trim().is_empty()
        && let Some(category) = symbol_list::category_at_scroll(
            &rows,
            main.symbol_scroll_handle.offset().y / crate::ui::scale::window_scale(window),
        )
    {
        main.symbol_group = category;
    }

    let sidebar_items = symbols::SYMBOL_GROUPS
        .iter()
        .map(|definition| {
            SymbolSidebarItem::new(
                definition.id as u32,
                definition.icon,
                translator.translate(definition.label_key),
            )
        })
        .collect::<Vec<_>>();
    let sidebar_handler: SymbolSidebarSelectHandler = Rc::new(|this, item_id, window, cx| {
        let Some(category) = symbols::SYMBOL_GROUPS
            .iter()
            .find(|definition| definition.id as u32 == item_id)
            .map(|definition| definition.id)
        else {
            return;
        };
        this.select_symbol_group(category, window, cx);
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
        main.symbol_group as u32,
        sidebar_handler,
        horizontal_categories,
        show_category_labels,
        cx,
    );
    let search_box = symbol_search::render(main, window, cx, None);
    let content = render_virtual_list(main, data, rows, columns, cx);

    symbol_layout::render(sidebar, search_box, content, horizontal_categories, cx)
}

fn build_rows(
    main: &MainWindow,
    data: &symbols::SymbolData,
    columns: u16,
    translator: &i18n::Translator,
) -> Vec<SymbolListRow> {
    let query = main.symbol_search_query.trim().to_lowercase();
    let mut rows = Vec::new();

    if query.is_empty() {
        let recent_indices = main
            .recent_symbols
            .iter()
            .filter_map(|glyph| data.items.iter().position(|item| item.glyph == *glyph))
            .collect::<Vec<_>>();
        let recent_is_empty = recent_indices.is_empty();
        symbol_list::append_section(
            &mut rows,
            SymbolGroupId::Recent,
            translator.translate("symbol.recent"),
            recent_indices,
            columns,
        );
        if recent_is_empty {
            rows.push(SymbolListRow::Empty {
                label: translator.translate("symbol.empty"),
            });
        }

        for category in symbols::SYMBOL_GROUPS
            .iter()
            .map(|definition| definition.id)
            .filter(|category| *category != SymbolGroupId::Recent)
        {
            let indices = data
                .items
                .iter()
                .enumerate()
                .filter_map(|(index, item)| (item.category == category).then_some(index))
                .collect::<Vec<_>>();
            let label = symbols::SYMBOL_GROUPS
                .iter()
                .find(|definition| definition.id == category)
                .map(|definition| translator.translate(definition.label_key))
                .unwrap_or_default();
            if !indices.is_empty() {
                symbol_list::append_section(&mut rows, category, label, indices, columns);
            }
        }
    } else {
        for category in symbols::SYMBOL_GROUPS
            .iter()
            .map(|definition| definition.id)
            .filter(|category| *category != SymbolGroupId::Recent)
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
            let label = symbols::SYMBOL_GROUPS
                .iter()
                .find(|definition| definition.id == category)
                .map(|definition| translator.translate(definition.label_key))
                .unwrap_or_default();
            if !indices.is_empty() {
                symbol_list::append_section(&mut rows, category, label, indices, columns);
            }
        }
    }
    if rows.is_empty() {
        rows.push(SymbolListRow::Empty {
            label: translator.translate("symbol.search_empty"),
        });
    }

    rows
}

fn render_virtual_list(
    main: &MainWindow,
    data: &'static symbols::SymbolData,
    rows: Rc<Vec<SymbolListRow>>,
    columns: u16,
    cx: &mut Context<MainWindow>,
) -> AnyElement {
    symbol_list::render_virtual_list(
        "symbol-list",
        rows,
        main.symbol_scroll_handle.clone(),
        MainWindow::handle_symbol_scroll_wheel,
        move |row_index, _category, item_indices, cx| {
            render_symbol_row(row_index, item_indices, data, columns, cx)
        },
        cx,
    )
}

fn render_symbol_row(
    row_index: usize,
    item_indices: &[usize],
    data: &'static symbols::SymbolData,
    columns: u16,
    cx: &mut Context<MainWindow>,
) -> AnyElement {
    symbol_list::render_grid_row(
        row_index,
        item_indices,
        &data.items,
        columns,
        render_symbol_item,
        cx,
    )
}

fn render_symbol_item(
    row_index: usize,
    item_index: usize,
    item: &SymbolItem,
    cx: &mut Context<MainWindow>,
) -> AnyElement {
    let glyph = item.glyph.clone();
    let tooltip = item.name.clone();
    let item_id = ((row_index as u32) << 16) | item_index as u32;
    div()
        .id(("symbol-item", item_id))
        .h(ui_rem(symbol_list::ROW_HEIGHT))
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .rounded_lg()
        .cursor_pointer()
        .managed_tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .hover(|this| this.bg(cx.theme().colors.secondary_hover))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.paste_symbol(glyph.clone(), cx);
        }))
        .child(
            div()
                .size(ui_rem(34.0))
                .flex()
                .items_center()
                .justify_center()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_size(ui_rem(24.0))
                .line_height(ui_rem(30.0))
                .font_family("Segoe UI Symbol")
                .child(item.glyph.clone()),
        )
        .into_any_element()
}
