use crate::ui::scale::ui_rem;

use std::{ops::Range, rc::Rc};

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, FontWeight, IntoElement, Pixels, ScrollWheelEvent, Window, div, px, size,
};
use gpui_component::{
    ActiveTheme as _, VirtualListScrollHandle, scroll::Scrollbar, v_virtual_list,
};

use crate::windows::main::state::MainWindow;

pub(crate) const CELL_MIN_WIDTH: f32 = 42.0;
pub(crate) const ROW_HEIGHT: f32 = 42.0;
pub(crate) const HEADER_HEIGHT: f32 = 28.0;
pub(crate) const GRID_GAP: f32 = 2.0;
pub(crate) const GRID_PADDING: f32 = 4.0;
const LIST_BOTTOM_BAR_HEIGHT: f32 = 6.0;

#[derive(Clone, Debug)]
pub(crate) enum ListRow<C> {
    Header {
        category: C,
        label: String,
    },
    Items {
        category: C,
        item_indices: Vec<usize>,
    },
    Empty {
        label: String,
    },
}

pub(crate) fn append_section<C: Copy>(
    rows: &mut Vec<ListRow<C>>,
    category: C,
    label: String,
    indices: Vec<usize>,
    columns: u16,
) {
    rows.push(ListRow::Header { category, label });
    for chunk in indices.chunks(columns as usize) {
        rows.push(ListRow::Items {
            category,
            item_indices: chunk.to_vec(),
        });
    }
}

pub(crate) fn category_at_scroll<C: Copy>(rows: &[ListRow<C>], offset_y: Pixels) -> Option<C> {
    let scroll_position = (-f32::from(offset_y)).max(0.0);
    let mut row_top = 0.0;
    let mut category = None;
    for row in rows {
        if row_top > scroll_position {
            break;
        }
        if let ListRow::Header {
            category: row_category,
            ..
        } = row
        {
            category = Some(*row_category);
        }
        row_top += row_height(row);
    }
    category
}

pub(crate) fn row_top<C>(rows: &[ListRow<C>], index: usize) -> f32 {
    rows.iter().take(index).map(row_height).sum()
}

pub(crate) fn row_height<C>(row: &ListRow<C>) -> f32 {
    match row {
        ListRow::Header { .. } => HEADER_HEIGHT,
        ListRow::Items { .. } | ListRow::Empty { .. } => ROW_HEIGHT,
    }
}

pub(crate) fn render_virtual_list<C, F>(
    list_id: &'static str,
    rows: Rc<Vec<ListRow<C>>>,
    scroll_handle: VirtualListScrollHandle,
    on_scroll_wheel: fn(&mut MainWindow, &ScrollWheelEvent, &mut Window, &mut Context<MainWindow>),
    render_items: F,
    cx: &mut Context<MainWindow>,
) -> AnyElement
where
    C: Copy + 'static,
    F: Fn(usize, C, &[usize], &mut Context<MainWindow>) -> AnyElement + 'static,
{
    let item_sizes = Rc::new(
        rows.iter()
            .map(|row| {
                size(
                    px(0.0),
                    px(row_height(row) * crate::ui::scale::theme_scale(cx)),
                )
            })
            .collect::<Vec<_>>(),
    );
    render_sized_virtual_list(
        list_id,
        item_sizes,
        scroll_handle,
        on_scroll_wheel,
        move |_, range, _, cx| {
            range
                .filter_map(|index| {
                    rows.get(index).map(|row| match row {
                        ListRow::Header { label, .. } => render_header(label, cx),
                        ListRow::Items {
                            category,
                            item_indices,
                        } => render_items(index, *category, item_indices, cx),
                        ListRow::Empty { label } => render_empty(label, cx),
                    })
                })
                .collect()
        },
        cx,
    )
}

pub(crate) fn render_sized_virtual_list<F>(
    list_id: &'static str,
    item_sizes: Rc<Vec<gpui::Size<Pixels>>>,
    scroll_handle: VirtualListScrollHandle,
    on_scroll_wheel: fn(&mut MainWindow, &ScrollWheelEvent, &mut Window, &mut Context<MainWindow>),
    render_range: F,
    cx: &mut Context<MainWindow>,
) -> AnyElement
where
    F: Fn(&mut MainWindow, Range<usize>, &mut Window, &mut Context<MainWindow>) -> Vec<AnyElement>
        + 'static,
{
    let scroll_height = item_sizes
        .iter()
        .fold(px(0.0), |height, item_size| height + item_size.height);
    let mut list = v_virtual_list(cx.entity(), list_id, item_sizes, render_range);
    list = list
        .track_scroll(&scroll_handle)
        .overflow_hidden()
        .flex_1()
        .min_h_0()
        .w_full();

    div()
        .relative()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .w_full()
        .overflow_hidden()
        .on_scroll_wheel(cx.listener(move |this, event, window, cx| {
            on_scroll_wheel(this, event, window, cx);
        }))
        .child(list)
        .child(div().h(px(LIST_BOTTOM_BAR_HEIGHT)).w_full().flex_shrink_0())
        .child(Scrollbar::vertical(&scroll_handle).scroll_size(size(px(0.0), scroll_height)))
        .into_any_element()
}

pub(crate) fn render_grid_row<T, F>(
    row_index: usize,
    item_indices: &[usize],
    items: &[T],
    columns: u16,
    render_item: F,
    cx: &mut Context<MainWindow>,
) -> AnyElement
where
    F: Fn(usize, usize, &T, &mut Context<MainWindow>) -> AnyElement,
{
    div()
        .h(ui_rem(ROW_HEIGHT))
        .w_full()
        .grid()
        .grid_cols(columns)
        .gap(ui_rem(GRID_GAP))
        .px(ui_rem(GRID_PADDING))
        .children(item_indices.iter().filter_map(|item_index| {
            items
                .get(*item_index)
                .map(|item| render_item(row_index, *item_index, item, cx))
        }))
        .into_any_element()
}

pub(super) fn render_header(label: &str, cx: &mut Context<MainWindow>) -> AnyElement {
    div()
        .h(ui_rem(HEADER_HEIGHT))
        .w_full()
        .flex()
        .items_center()
        .px_2()
        .bg(cx.theme().colors.tab_bar)
        .text_size(ui_rem(12.0))
        .font_weight(FontWeight::MEDIUM)
        .text_color(cx.theme().colors.muted_foreground)
        .child(label.to_owned())
        .into_any_element()
}

pub(super) fn render_empty(label: &str, cx: &mut Context<MainWindow>) -> AnyElement {
    div()
        .h(ui_rem(ROW_HEIGHT))
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(cx.theme().colors.muted_foreground)
        .child(label.to_owned())
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{HEADER_HEIGHT, ListRow, ROW_HEIGHT, category_at_scroll, row_height, row_top};
    use gpui::px;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Category {
        First,
        Second,
    }

    fn rows() -> Vec<ListRow<Category>> {
        vec![
            ListRow::Header {
                category: Category::First,
                label: "第一组".to_owned(),
            },
            ListRow::Items {
                category: Category::First,
                item_indices: vec![0, 1],
            },
            ListRow::Header {
                category: Category::Second,
                label: "第二组".to_owned(),
            },
        ]
    }

    #[test]
    fn calculates_shared_list_positions() {
        let rows = rows();

        assert_eq!(row_height(&rows[0]), HEADER_HEIGHT);
        assert_eq!(row_height(&rows[1]), ROW_HEIGHT);
        assert_eq!(row_top(&rows, 2), HEADER_HEIGHT + ROW_HEIGHT);
        assert_eq!(category_at_scroll(&rows, px(0.0)), Some(Category::First));
        assert_eq!(
            category_at_scroll(&rows, px(-(HEADER_HEIGHT + ROW_HEIGHT))),
            Some(Category::Second)
        );
    }
}
