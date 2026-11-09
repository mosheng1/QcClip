use crate::ui::scale::ui_rem;

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div,
};
use gpui_component::{ActiveTheme as _, tooltip::Tooltip};

use crate::{
    i18n,
    platform::window::logical_to_physical_desktop,
    ui::hover_icon,
    windows::main::{state::MainWindow, tabs::TabId},
    windows::menu::MenuPosition,
};

pub fn render(
    selected_count: usize,
    active_tab: TabId,
    cx: &mut Context<MainWindow>,
) -> AnyElement {
    if selected_count == 0 || active_tab == TabId::Emoji {
        return div().into_any_element();
    }

    let theme = cx.theme().colors;
    let translator = i18n::translator(cx);
    let selected_label = translator.translate_with_args(
        "main.multi_selection.selected_count",
        &[("count", selected_count.to_string())],
    );
    let shortcut_hint = translator.translate("main.multi_selection.shortcut_hint");
    let (delete_icon, delete_label) = if active_tab == TabId::Favorites {
        (
            "star-off",
            translator.translate("main.multi_selection.remove_favorites"),
        )
    } else {
        ("trash", translator.translate("main.multi_selection.delete"))
    };
    let button = |id: &'static str, icon_name: &'static str, label: String| {
        let button = div()
            .id(id)
            .group(id)
            .size(ui_rem(28.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded_lg()
            .cursor_pointer()
            .hover(|this| this.bg(theme.secondary_hover))
            .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
            .child(hover_icon(id, icon_name, 16.0, theme.foreground));
        match id {
            "multi-selection-paste" => button.on_click(cx.listener(|this, _, _, cx| {
                this.paste_multi_selected_items(cx);
            })),
            "multi-selection-copy" => button.on_click(cx.listener(|this, _, _, cx| {
                this.copy_multi_selected_items(cx);
            })),
            "multi-selection-tags" => {
                button.on_click(cx.listener(|this, event: &gpui::ClickEvent, window, cx| {
                    let Some(anchor) = logical_to_physical_desktop(window, event.position()) else {
                        tracing::error!("无法确定菜单位置，未打开批量标签菜单");
                        return;
                    };
                    let position = MenuPosition::new(anchor.x, anchor.y, window.scale_factor());
                    this.open_multi_selection_tag_menu(position, cx);
                }))
            }
            "multi-selection-delete" => button.on_click(cx.listener(|this, _, window, cx| {
                this.delete_multi_selected_items(window, cx);
            })),
            "multi-selection-clear" => button.on_click(cx.listener(|this, _, _, cx| {
                this.clear_multi_selection();
                cx.notify();
            })),
            _ => button,
        }
    };

    div()
        .h(ui_rem(36.0))
        .w_full()
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .border_t_1()
        .border_color(theme.title_bar_border)
        .bg(theme.title_bar)
        .px_2()
        .child(
            div()
                .flex()
                .flex_col()
                .justify_center()
                .min_w_0()
                .child(
                    div()
                        .text_sm()
                        .line_height(ui_rem(17.0))
                        .text_color(theme.foreground)
                        .child(selected_label),
                )
                .child(
                    div()
                        .text_xs()
                        .line_height(ui_rem(14.0))
                        .text_color(theme.muted_foreground)
                        .child(shortcut_hint),
                ),
        )
        .child(div().flex().items_center().gap_1().children([
            button(
                "multi-selection-paste",
                "clipboard-list",
                translator.translate("main.multi_selection.paste"),
            ),
            button(
                "multi-selection-copy",
                "copy-plus",
                translator.translate("main.multi_selection.copy"),
            ),
            button(
                "multi-selection-tags",
                "tags",
                translator.translate("main.multi_selection.add_tag"),
            ),
            button("multi-selection-delete", delete_icon, delete_label),
            button(
                "multi-selection-clear",
                "cancel",
                translator.translate("main.multi_selection.deselect_all"),
            ),
        ]))
        .into_any_element()
}
