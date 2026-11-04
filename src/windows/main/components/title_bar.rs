use crate::ui::scale::ui_rem;

use gpui::prelude::*;
use gpui::{
    ClickEvent, Context, Focusable, InteractiveElement, IntoElement, LineFragment, MouseButton,
    MouseDownEvent, Pixels, StatefulInteractiveElement, Window, div, img, px, relative,
};
use gpui_component::{ActiveTheme as _, Sizable, input::Input};

use crate::{
    config::{
        ClipboardDisplayPriority, ClipboardListLayout, ClipboardPreviewPriority,
        ClipboardWindowPosition, TitlebarPosition,
    },
    config_runtime, i18n,
    ui::{hover_icon, titlebar_button},
    windows::main::state::MainWindow,
    windows::menu::{MenuActionId, MenuIcon, MenuItem, MenuModel, MenuPosition, MenuService},
};

const TITLEBAR_HEIGHT: f32 = 36.0;
const TITLEBAR_SIDE_WIDTH: f32 = 36.0;
const NEW_TEXT_ACTION: &str = "main.more.new_text";
const SETTINGS_ACTION: &str = "main.more.settings";
const CLEAR_UNPINNED_HISTORY_ACTION: &str = "main.more.clear-unpinned-history";
const CLEAR_ALL_HISTORY_ACTION: &str = "main.more.clear-all-history";
const LIST_LAYOUT_ACTION_PREFIX: &str = "main.more.list-layout.";
const WINDOW_POSITION_ACTION_PREFIX: &str = "main.more.window-position.";
const TITLEBAR_POSITION_ACTION_PREFIX: &str = "main.more.titlebar-position.";
const DISPLAY_PRIORITY_ACTION_PREFIX: &str = "main.more.display-priority.";
const PREVIEW_PRIORITY_ACTION_PREFIX: &str = "main.more.preview-priority.";

pub fn render(
    main: &MainWindow,
    window: &Window,
    cx: &mut Context<MainWindow>,
    position: TitlebarPosition,
    combined_horizontal: bool,
) -> impl IntoElement {
    let theme = cx.theme().colors;
    let translator = i18n::translator(cx);
    let edge_hide_enabled = config_runtime::clipboard_config(cx).edge_hide;
    let edge_hide_locked = config_runtime::clipboard_config(cx)
        .window_position
        .is_screen_edge();
    let is_vertical = position.is_vertical() && !combined_horizontal;
    let mut actions = div()
        .flex()
        .flex_1()
        .min_w_0()
        .gap_1()
        .on_mouse_down_out(cx.listener(|this, _, window, cx| {
            if this.search_expanded {
                window.blur(cx);
                if this.search_query.trim().is_empty() {
                    this.search_expanded = false;
                }
                cx.notify();
            }
        }));
    if is_vertical {
        actions = actions
            .w_full()
            .min_h_0()
            .mt_2()
            .flex_col()
            .items_center()
            .justify_end();
    } else {
        actions = actions.ml_2().items_center().justify_end();
    }

    if combined_horizontal || main.search_expanded {
        let search_input = main
            .search_input
            .as_ref()
            .expect("搜索输入框状态尚未初始化");
        let input_focused = search_input.focus_handle(cx).is_focused(window);
        let input_border = if input_focused {
            theme.ring
        } else {
            theme.input
        };
        let search_input_for_mouse = search_input.clone();
        let has_search_text = !search_input.read(cx).value().is_empty();
        let clear_button = if has_search_text {
            let search_input_for_clear = search_input.clone();
            Some(
                div()
                    .id("titlebar-search-clear-button")
                    .group("titlebar-search-clear-button")
                    .size(ui_rem(20.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|this| this.bg(theme.secondary_hover))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        search_input_for_clear.update(cx, |input, cx| {
                            input.set_value("", window, cx);
                            input.focus(window, cx);
                        });
                        this.search_query.clear();
                        this.refresh_history(cx);
                    }))
                    .child(hover_icon(
                        "titlebar-search-clear-button",
                        "x",
                        14.0,
                        theme.danger,
                    )),
            )
        } else {
            None
        };
        let vertical_placeholder = if is_vertical && !has_search_text {
            let placeholder = crate::i18n::translator(cx).translate("main.search.placeholder");
            let placeholder = wrap_vertical_search_text(&placeholder, window);
            Some(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .bottom_0()
                    .left_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .line_height(relative(1.25))
                    .text_color(theme.muted_foreground)
                    .child(placeholder),
            )
        } else {
            None
        };
        let mut search_input_element = Input::new(search_input)
            .small()
            .appearance(false)
            .bordered(false)
            .focus_bordered(false)
            .cleanable(false)
            .w_full();
        let mut search_box = div()
            .flex()
            .items_center()
            .rounded_lg()
            .border_1()
            .border_color(input_border)
            .bg(theme.title_bar)
            .capture_any_mouse_down(
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.window_drag_start = None;
                    if event.button != MouseButton::Left || window.is_window_active() {
                        return;
                    }

                    if let Err(error) = crate::services::focus::focus_input_window(window) {
                        tracing::error!("点击搜索输入框时激活窗口失败：{error}");
                        return;
                    }

                    search_input_for_mouse.update(cx, |input, cx| input.focus(window, cx));
                }),
            )
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation());
        if is_vertical {
            search_input_element = search_input_element.h_full().pl(ui_rem(6.0)).pr_0();
            let mut search_input_area = div()
                .w_full()
                .flex_1()
                .min_h_0()
                .relative()
                .flex()
                .items_center();
            if let Some(placeholder) = vertical_placeholder {
                search_input_area = search_input_area.child(placeholder);
            }
            search_input_area = search_input_area.child(search_input_element);
            search_box = search_box
                .w_full()
                .h(relative(0.4))
                .flex_none()
                .min_h_0()
                .flex_col()
                .child(search_input_area);
            if let Some(clear_button) = clear_button {
                search_box = search_box.child(clear_button.mb_1());
            }
        } else {
            if let Some(clear_button) = clear_button {
                search_input_element = search_input_element.suffix(clear_button);
            }
            search_box = search_box
                .h(ui_rem(28.0))
                .flex_1()
                .min_w_0()
                .child(search_input_element);
        }
        actions = actions.child(search_box);
    }

    if !combined_horizontal {
        actions = actions.child(
            div()
                .id("titlebar-search-button")
                .group("titlebar-search-button")
                .size(ui_rem(28.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_lg()
                .cursor_pointer()
                .hover(|this| this.bg(theme.secondary_hover))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _, window, cx| {
                    this.toggle_search_input(window, cx);
                }))
                .child(hover_icon(
                    "titlebar-search-button",
                    "search",
                    16.0,
                    theme.foreground,
                )),
        );
    }

    if let Some(buttons) = super::sync_buttons::render(main, cx) {
        actions = actions.child(buttons);
    }

    let more_button = div()
        .id("titlebar-more-button")
        .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
        .on_click(|event: &ClickEvent, window, cx| {
            cx.stop_propagation();
            open_more_menu(event.position(), window, cx);
        })
        .child(titlebar_button(
            "dots",
            translator.translate("main.titlebar.more_actions"),
            false,
            theme,
        ));
    actions = actions
        .child(
            titlebar_button(
                "browser",
                translator.translate("main.titlebar.edge_hide"),
                edge_hide_enabled,
                theme,
            )
            .when(edge_hide_locked, |button| {
                button.opacity(0.35).cursor_default()
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_click(move |_, _, cx| {
                let mut clipboard = config_runtime::clipboard_config(cx);
                if clipboard.window_position.is_screen_edge() {
                    return;
                }
                clipboard.edge_hide = !clipboard.edge_hide;
                if let Err(error) = config_runtime::set_clipboard_config(clipboard, cx) {
                    tracing::error!("保存贴边隐藏设置失败：{error}");
                }
            }),
        )
        .child(
            titlebar_button(
                "pin",
                translator.translate("main.titlebar.pin_window"),
                main.titlebar_window_fixed,
                theme,
            )
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, _, cx| {
                this.titlebar_window_fixed = !this.titlebar_window_fixed;
                crate::windows::main::MainWindowService::set_window_fixed(
                    this.titlebar_window_fixed,
                    cx,
                );
                cx.notify();
            })),
        )
        .child(more_button);

    let mut titlebar = div()
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .relative()
        .bg(theme.title_bar)
        .on_mouse_down(MouseButton::Right, |event, window, cx| {
            cx.stop_propagation();
            open_more_menu(event.position, window, cx);
        })
        .child(
            div()
                .id("main-update-icon")
                .size(ui_rem(24.0))
                .flex_shrink_0()
                .relative()
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                .child(img("assets/icon64.png").size(ui_rem(20.0)))
                .when(
                    crate::services::updates::UpdateService::available(cx).is_some(),
                    |view| {
                        let tooltip = i18n::translator(cx).translate("updates.view_progress");
                        view.cursor_pointer()
                            .tooltip(move |_, cx| {
                                cx.new(|_| gpui_component::tooltip::Tooltip::new(tooltip.clone()))
                                    .into()
                            })
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(|_, _, cx| crate::windows::settings::open_about_window(cx))
                            .child(
                                div()
                                    .absolute()
                                    .right(ui_rem(-2.0))
                                    .bottom(ui_rem(-2.0))
                                    .size(ui_rem(13.0))
                                    .rounded_full()
                                    .bg(theme.success)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(crate::ui::icon(
                                        "arrow-bar-to-up",
                                        10.0,
                                        theme.background,
                                    )),
                            )
                    },
                ),
        )
        .child(actions);
    if is_vertical {
        titlebar = titlebar
            .w(ui_rem(TITLEBAR_SIDE_WIDTH))
            .h_full()
            .p_1()
            .flex_col();
    } else {
        titlebar = titlebar
            .when(combined_horizontal, |this| this.flex_1().min_w_0())
            .when(!combined_horizontal, |this| this.w_full())
            .h(ui_rem(TITLEBAR_HEIGHT))
            .px_2();
    }

    let border = match position {
        TitlebarPosition::Top => div()
            .absolute()
            .left_0()
            .right_0()
            .bottom_0()
            .h(px(0.5))
            .bg(theme.title_bar_border),
        TitlebarPosition::Bottom => div()
            .absolute()
            .left_0()
            .right_0()
            .top_0()
            .h(px(0.5))
            .bg(theme.title_bar_border),
        TitlebarPosition::Left => div()
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .w(px(0.5))
            .bg(theme.title_bar_border),
        TitlebarPosition::Right => div()
            .absolute()
            .top_0()
            .left_0()
            .bottom_0()
            .w(px(0.5))
            .bg(theme.title_bar_border),
    };

    titlebar.child(border)
}

const VERTICAL_SEARCH_TEXT_WIDTH: Pixels = px(12.0);

fn wrap_vertical_search_text(text: &str, window: &Window) -> String {
    let text_style = window.text_style();
    let font_size = text_style.font_size.to_pixels(window.rem_size());
    let mut line_wrapper = window
        .text_system()
        .line_wrapper(text_style.font(), font_size);
    let mut result = String::new();

    for (line_index, line) in text.split('\n').enumerate() {
        if line_index > 0 {
            result.push('\n');
        }

        let mut start = 0;
        let mut segments = Vec::new();
        for boundary in line_wrapper.wrap_line(
            &[LineFragment::text(line)],
            VERTICAL_SEARCH_TEXT_WIDTH * crate::ui::scale::window_scale(window),
        ) {
            if boundary.ix > start {
                segments.push(line[start..boundary.ix].to_owned());
                start = boundary.ix;
            }
        }
        segments.push(line[start..].to_owned());
        result.push_str(&segments.join("\n"));
    }

    result
}

fn open_more_menu(position: gpui::Point<Pixels>, window: &Window, cx: &mut gpui::App) {
    let Some(anchor) = crate::platform::window::logical_to_physical_desktop(window, position)
    else {
        tracing::error!("无法确定更多菜单位置，未打开菜单");
        return;
    };

    let position = MenuPosition::new(anchor.x, anchor.y, window.scale_factor());
    MenuService::open_context_menu(build_more_menu(cx), position, handle_more_menu_action, cx);
}

fn build_more_menu(cx: &gpui::App) -> MenuModel {
    let translator = i18n::translator(cx);
    let appearance = config_runtime::appearance_config(cx);
    let clipboard = config_runtime::clipboard_config(cx);
    let horizontal_list = clipboard.window_position.uses_horizontal_list();
    let list_layout_items = ClipboardListLayout::ALL
        .into_iter()
        .map(|layout| {
            let item = MenuItem::action(
                MenuActionId::new(format!("{LIST_LAYOUT_ACTION_PREFIX}{}", layout.code())),
                translator.translate(layout.label_key()),
            );
            if layout == clipboard.effective_list_layout() {
                item.checked()
            } else {
                item
            }
        })
        .collect();
    let window_position_items = ClipboardWindowPosition::ALL
        .into_iter()
        .map(|window_position| {
            let item = MenuItem::action(
                MenuActionId::new(format!(
                    "{WINDOW_POSITION_ACTION_PREFIX}{}",
                    window_position.code()
                )),
                translator.translate(clipboard_window_position_label_key(window_position)),
            );
            if window_position == clipboard.window_position {
                item.checked()
            } else {
                item
            }
        })
        .collect();
    let selected_position = if horizontal_list && appearance.titlebar_position.is_vertical() {
        TitlebarPosition::Top
    } else {
        appearance.titlebar_position
    };
    let titlebar_position_items = TitlebarPosition::ALL
        .into_iter()
        .filter(|position| !horizontal_list || !position.is_vertical())
        .map(|position| {
            let item = MenuItem::action(
                MenuActionId::new(format!(
                    "{TITLEBAR_POSITION_ACTION_PREFIX}{}",
                    position.code()
                )),
                translator.translate(titlebar_position_label_key(position)),
            );
            if position == selected_position {
                item.checked()
            } else {
                item
            }
        })
        .collect();
    let display_priority_items = ClipboardDisplayPriority::ALL
        .into_iter()
        .map(|priority| {
            let item = MenuItem::action(
                MenuActionId::new(format!(
                    "{DISPLAY_PRIORITY_ACTION_PREFIX}{}",
                    priority.code()
                )),
                translator.translate(display_priority_label_key(priority)),
            );
            if priority == clipboard.display_priority {
                item.checked()
            } else {
                item
            }
        })
        .collect();
    let preview_priority_items = ClipboardPreviewPriority::ALL
        .into_iter()
        .map(|priority| {
            let item = MenuItem::action(
                MenuActionId::new(format!(
                    "{PREVIEW_PRIORITY_ACTION_PREFIX}{}",
                    priority.code()
                )),
                translator.translate(preview_priority_label_key(priority)),
            );
            if priority == clipboard.preview_priority {
                item.checked()
            } else {
                item
            }
        })
        .collect();

    MenuModel::new(vec![
        MenuItem::submenu(
            translator.translate("settings.clipboard.list_layout"),
            list_layout_items,
        )
        .with_icon(MenuIcon::svg("app-window.svg")),
        MenuItem::submenu(
            translator.translate("settings.clipboard.window_position"),
            window_position_items,
        )
        .with_icon(MenuIcon::svg("app-window.svg")),
        MenuItem::submenu(
            translator.translate("settings.personalization.titlebar_position"),
            titlebar_position_items,
        )
        .with_icon(MenuIcon::svg("arrows-move.svg")),
        MenuItem::submenu(
            translator.translate("settings.clipboard.display_priority"),
            display_priority_items,
        )
        .with_icon(MenuIcon::svg("sort-ascending.svg")),
        MenuItem::submenu(
            translator.translate("settings.clipboard.preview_priority"),
            preview_priority_items,
        )
        .with_icon(MenuIcon::svg("sort-ascending.svg")),
        MenuItem::separator(),
        MenuItem::submenu(
            translator.translate("main.menu.clear_clipboard"),
            vec![
                MenuItem::action(
                    MenuActionId::new(CLEAR_UNPINNED_HISTORY_ACTION),
                    translator.translate("main.menu.clear_history_keep_pinned"),
                ),
                MenuItem::action(
                    MenuActionId::new(CLEAR_ALL_HISTORY_ACTION),
                    translator.translate("main.menu.clear_history_all"),
                ),
            ],
        )
        .with_icon(MenuIcon::svg("clipboard-x.svg")),
        MenuItem::separator(),
        MenuItem::action(
            MenuActionId::new(SETTINGS_ACTION),
            translator.translate("tray.menu.settings"),
        )
        .with_icon(MenuIcon::svg("settings.svg")),
    ])
}

fn handle_more_menu_action(action: MenuActionId, cx: &mut gpui::App) {
    if matches!(
        action.as_str(),
        CLEAR_UNPINNED_HISTORY_ACTION | CLEAR_ALL_HISTORY_ACTION
    ) {
        crate::windows::main::MainWindowService::confirm_clear_history(
            action.as_str() == CLEAR_UNPINNED_HISTORY_ACTION,
            cx,
        );
        return;
    }
    if action.as_str() == NEW_TEXT_ACTION {
        crate::windows::main::MainWindowService::open_new_text_editor(cx);
        return;
    }
    if action.as_str() == SETTINGS_ACTION {
        crate::windows::settings::open_settings_window(cx);
        return;
    }

    if let Some(code) = action.as_str().strip_prefix(LIST_LAYOUT_ACTION_PREFIX) {
        if let Some(layout) = ClipboardListLayout::ALL
            .into_iter()
            .find(|layout| layout.code() == code)
        {
            let mut clipboard = config_runtime::clipboard_config(cx);
            clipboard.set_list_layout(layout);
            if let Err(error) = config_runtime::set_clipboard_config(clipboard, cx) {
                tracing::error!("保存列表布局失败：{error}");
            }
        }
        return;
    }
    if let Some(code) = action.as_str().strip_prefix(PREVIEW_PRIORITY_ACTION_PREFIX) {
        let Some(priority) = ClipboardPreviewPriority::ALL
            .into_iter()
            .find(|priority| priority.code() == code)
        else {
            return;
        };

        let mut clipboard = config_runtime::clipboard_config(cx);
        if clipboard.preview_priority == priority {
            return;
        }
        clipboard.preview_priority = priority;
        if let Err(error) = config_runtime::set_clipboard_config(clipboard, cx) {
            tracing::error!("保存预览优先级失败：{error}");
        }
        return;
    }

    if let Some(code) = action.as_str().strip_prefix(DISPLAY_PRIORITY_ACTION_PREFIX) {
        let Some(priority) = ClipboardDisplayPriority::ALL
            .into_iter()
            .find(|priority| priority.code() == code)
        else {
            return;
        };

        let mut clipboard = config_runtime::clipboard_config(cx);
        if clipboard.display_priority == priority {
            return;
        }
        clipboard.display_priority = priority;
        if let Err(error) = config_runtime::set_clipboard_config(clipboard, cx) {
            tracing::error!("保存展示优先级失败：{error}");
        }
        return;
    }

    if let Some(code) = action.as_str().strip_prefix(WINDOW_POSITION_ACTION_PREFIX) {
        let Some(position) = ClipboardWindowPosition::ALL
            .into_iter()
            .find(|position| position.code() == code)
        else {
            return;
        };

        let mut clipboard = config_runtime::clipboard_config(cx);
        if clipboard.window_position == position {
            return;
        }
        clipboard.window_position = position;
        if let Err(error) = config_runtime::set_clipboard_config(clipboard, cx) {
            tracing::error!("保存窗口显示位置失败：{error}");
        }
        return;
    }

    let Some(code) = action
        .as_str()
        .strip_prefix(TITLEBAR_POSITION_ACTION_PREFIX)
    else {
        return;
    };
    let Some(position) = TitlebarPosition::ALL
        .into_iter()
        .filter(|position| {
            !config_runtime::clipboard_config(cx)
                .window_position
                .uses_horizontal_list()
                || !position.is_vertical()
        })
        .find(|position| position.code() == code)
    else {
        return;
    };

    let mut appearance = config_runtime::appearance_config(cx);
    if appearance.titlebar_position == position {
        return;
    }
    appearance.titlebar_position = position;
    if let Err(error) = config_runtime::set_appearance_config(appearance, cx) {
        tracing::error!("保存标题栏位置失败：{error}");
    }
}

const fn titlebar_position_label_key(position: TitlebarPosition) -> &'static str {
    match position {
        TitlebarPosition::Top => "settings.personalization.position.top",
        TitlebarPosition::Bottom => "settings.personalization.position.bottom",
        TitlebarPosition::Left => "settings.personalization.position.left",
        TitlebarPosition::Right => "settings.personalization.position.right",
    }
}

const fn clipboard_window_position_label_key(position: ClipboardWindowPosition) -> &'static str {
    match position {
        ClipboardWindowPosition::MouseCenter => "settings.clipboard.window_position.mouse_center",
        ClipboardWindowPosition::MouseNearby => "settings.clipboard.window_position.mouse_nearby",
        ClipboardWindowPosition::Remember => "settings.clipboard.window_position.remember",
        ClipboardWindowPosition::ScreenCenter => "settings.clipboard.window_position.screen_center",
        ClipboardWindowPosition::ScreenLeftCenter => {
            "settings.clipboard.window_position.screen_left_center"
        }
        ClipboardWindowPosition::ScreenRightCenter => {
            "settings.clipboard.window_position.screen_right_center"
        }
        ClipboardWindowPosition::ScreenTopCenter => {
            "settings.clipboard.window_position.screen_top_center"
        }
        ClipboardWindowPosition::ScreenBottomCenter => {
            "settings.clipboard.window_position.screen_bottom_center"
        }
    }
}

const fn display_priority_label_key(priority: ClipboardDisplayPriority) -> &'static str {
    match priority {
        ClipboardDisplayPriority::TextHtmlImage => "settings.clipboard.priority.text_html_image",
        ClipboardDisplayPriority::TextImageHtml => "settings.clipboard.priority.text_image_html",
        ClipboardDisplayPriority::HtmlTextImage => "settings.clipboard.priority.html_text_image",
        ClipboardDisplayPriority::HtmlImageText => "settings.clipboard.priority.html_image_text",
        ClipboardDisplayPriority::ImageTextHtml => "settings.clipboard.priority.image_text_html",
        ClipboardDisplayPriority::ImageHtmlText => "settings.clipboard.priority.image_html_text",
    }
}

const fn preview_priority_label_key(priority: ClipboardPreviewPriority) -> &'static str {
    match priority {
        ClipboardPreviewPriority::FollowDisplay => {
            "settings.clipboard.preview_priority.follow_display"
        }
        ClipboardPreviewPriority::TextHtmlImage => "settings.clipboard.priority.text_html_image",
        ClipboardPreviewPriority::TextImageHtml => "settings.clipboard.priority.text_image_html",
        ClipboardPreviewPriority::HtmlTextImage => "settings.clipboard.priority.html_text_image",
        ClipboardPreviewPriority::HtmlImageText => "settings.clipboard.priority.html_image_text",
        ClipboardPreviewPriority::ImageTextHtml => "settings.clipboard.priority.image_text_html",
        ClipboardPreviewPriority::ImageHtmlText => "settings.clipboard.priority.image_html_text",
    }
}
