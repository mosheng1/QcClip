use gpui_component::tooltip::ManagedTooltipExt as _;
use std::{cell::Cell, rc::Rc};

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Div, Entity, Pixels, Point, Render, Size, Window, div, px, relative,
};
#[cfg(target_os = "windows")]
use gpui::{Bounds, point};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme as _, Sizable as _, ThemeColor};

use super::super::SettingsWindowView;
use super::personalization::persist_appearance_change_in_view;
#[cfg(target_os = "windows")]
use crate::platform::drag_preview::DragPreviewService;
use crate::{
    config::{TabBarButton, TabBarButtonGroup, TabBarLayout, TabBarSection},
    config_runtime, i18n,
    ui::{SingleSelectOption, icon, scale::ui_rem, single_select},
};

#[derive(Clone)]
struct SectionDrag(TabBarSection);

#[derive(Clone)]
struct ButtonDrag(TabBarButton);

#[derive(Clone, Copy)]
enum PreviewItem {
    Section(TabBarSection),
    Button(TabBarButton),
}

#[derive(Clone)]
struct PreviewContent {
    item: PreviewItem,
    layout: TabBarLayout,
    symbols: bool,
}

struct DragPreview {
    content: PreviewContent,
    size: Size<Pixels>,
    render_preview: bool,
}

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.render_preview {
            return div().into_any_element();
        }
        let theme = cx.theme().colors;
        let content = &self.content;
        let visual = match content.item {
            PreviewItem::Button(button) => {
                button_visual(button, &content.layout, content.symbols, theme).into_any_element()
            }
            PreviewItem::Section(section) => {
                let title = section_title(section, theme, cx);
                let buttons = content
                    .layout
                    .buttons_in(section_group(section, content.symbols))
                    .map(|button| button_visual(button, &content.layout, content.symbols, theme));
                section_frame(section, &content.layout, theme, title, buttons_row(buttons))
                    .into_any_element()
            }
        };
        div()
            .w(self.size.width)
            .h(self.size.height)
            .flex()
            .bg(theme.tab_bar)
            .child(visual)
            .into_any_element()
    }
}

impl SettingsWindowView {
    pub(super) fn render_tab_bar_editor(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let translator = i18n::translator(cx);
        let layout = config_runtime::appearance_config(cx).tab_bar;
        let view = cx.entity();
        let mode_selector = single_select(
            "tab-bar-editor-mode",
            [
                SingleSelectOption::new(
                    false,
                    translator.translate("settings.personalization.tab_bar.filter_mode"),
                ),
                SingleSelectOption::new(
                    true,
                    translator.translate("settings.personalization.tab_bar.symbol_mode"),
                ),
            ],
            self.tab_bar_edit_symbols,
            move |symbols, cx| {
                view.update(cx, |view, cx| {
                    view.tab_bar_edit_symbols = symbols;
                    cx.notify();
                })
            },
            theme,
            config_runtime::animation_enabled(
                cx,
                crate::config::AnimationKind::SingleSelectIndicator,
            ),
        );
        let reset_tooltip = translator.translate("settings.personalization.tab_bar.reset");
        let reset = Button::new("tab-bar-editor-reset")
            .ghost()
            .small()
            .icon(icon("refresh", 14.0, theme.muted_foreground))
            .tooltip(reset_tooltip)
            .on_click(cx.listener(|view, _, _, cx| {
                save_layout(view, |layout| *layout = TabBarLayout::default(), cx);
            }));
        let mut preview = div()
            .flex()
            .items_stretch()
            .w_full()
            .min_w(ui_rem(500.0))
            .bg(theme.tab_bar)
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border);
        for section in layout.sections.iter().copied() {
            preview = preview.child(self.tab_bar_editor_section(section, &layout, cx));
        }
        div()
            .w_full()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .mb_3()
                    .child(
                        div()
                            .min_w_0()
                            .text_size(ui_rem(13.0))
                            .text_color(theme.foreground)
                            .child(translator.translate("settings.personalization.tab_bar")),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .flex_none()
                            .child(mode_selector)
                            .child(reset),
                    ),
            )
            .child(
                div()
                    .id("tab-bar-editor-scroll")
                    .w_full()
                    .overflow_x_scrollbar()
                    .child(preview),
            )
            .into_any_element()
    }

    fn tab_bar_editor_section(
        &self,
        section: TabBarSection,
        layout: &TabBarLayout,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().colors;
        let size = Rc::new(Cell::new(Size::default()));
        let measured_size = size.clone();
        let content = PreviewContent {
            item: PreviewItem::Section(section),
            layout: layout.clone(),
            symbols: self.tab_bar_edit_symbols,
        };
        let title = section_title(section, theme, cx)
            .id(("tab-bar-section-handle", section as u32))
            .cursor_grab()
            .hover(|style| style.bg(theme.secondary_hover))
            .on_drag(SectionDrag(section), move |_, offset, window, cx| {
                cx.stop_propagation();
                start_drag_preview(content.clone(), size.get(), offset, window, cx)
            });
        let buttons = layout
            .buttons_in(section_group(section, self.tab_bar_edit_symbols))
            .map(|button| self.tab_bar_editor_button(button, layout, cx));
        let dragging = cx.active_drag().is_some_and(|drag| {
            drag.value
                .downcast_ref::<SectionDrag>()
                .is_some_and(|drag| drag.0 == section)
        });
        // 原位置只隐藏内容并保留占位，预览使用相同渲染函数及实测尺寸。
        section_frame(section, layout, theme, title, buttons_row(buttons))
            .id(("tab-bar-editor-section", section as u32))
            .when(section == TabBarSection::Pages, |style| {
                style.flex_none().flex_basis(relative(0.35))
            })
            .when(section == TabBarSection::Filters, |style| style.flex_1())
            .when(section == TabBarSection::Tags, |style| {
                style.w(ui_rem(72.0)).flex_none()
            })
            .on_prepaint(move |prepaint, _, _| measured_size.set(prepaint.bounds.size))
            .when(dragging, |style| style.invisible())
            .on_drop(cx.listener(move |view, drag: &SectionDrag, _, cx| {
                cx.stop_propagation();
                hide_drag_preview(cx);
                save_layout(view, |layout| layout.move_section(drag.0, section), cx);
            }))
            .on_drag_over::<SectionDrag>(move |style, drag, _, _| {
                if drag.0 != section {
                    style.bg(theme.secondary_hover)
                } else {
                    style
                }
            })
            .into_any_element()
    }

    fn tab_bar_editor_button(
        &self,
        button: TabBarButton,
        layout: &TabBarLayout,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().colors;
        let translator = i18n::translator(cx);
        let status = translator.translate(if layout.is_visible(button) {
            "settings.personalization.tab_bar.visible"
        } else {
            "settings.personalization.tab_bar.hidden"
        });
        let tooltip = format!("{} · {status}", translator.translate(button.label_key()));
        let size = Rc::new(Cell::new(Size::default()));
        let measured_size = size.clone();
        let dragging = cx.active_drag().is_some_and(|drag| {
            drag.value
                .downcast_ref::<ButtonDrag>()
                .is_some_and(|drag| drag.0 == button)
        });
        let active = button_active(button, layout.is_visible(button), self.tab_bar_edit_symbols);
        let mut element = button_visual(button, layout, self.tab_bar_edit_symbols, theme)
            .id(("tab-bar-editor-button", button as u32))
            .cursor_pointer()
            .hover(move |style| {
                style.bg(if active {
                    theme.primary_hover
                } else {
                    theme.secondary_hover
                })
            })
            .on_prepaint(move |prepaint, _, _| measured_size.set(prepaint.bounds.size))
            .when(dragging, |style| style.invisible())
            .managed_tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .on_click(cx.listener(move |view, _, _, cx| {
                cx.stop_propagation();
                save_layout(view, |layout| layout.toggle(button), cx);
            }));
        if button != TabBarButton::Tags {
            let content = PreviewContent {
                item: PreviewItem::Button(button),
                layout: layout.clone(),
                symbols: self.tab_bar_edit_symbols,
            };
            element = element
                .on_drag(ButtonDrag(button), move |_, offset, window, cx| {
                    cx.stop_propagation();
                    start_drag_preview(content.clone(), size.get(), offset, window, cx)
                })
                .can_drop(move |drag, _, _| {
                    drag.downcast_ref::<ButtonDrag>()
                        .is_some_and(|drag| drag.0 != button && drag.0.group() == button.group())
                })
                .on_drop(cx.listener(move |view, drag: &ButtonDrag, _, cx| {
                    cx.stop_propagation();
                    hide_drag_preview(cx);
                    save_layout(view, |layout| layout.move_button(drag.0, button), cx);
                }))
                .on_drag_over::<ButtonDrag>(move |style, drag, _, _| {
                    if drag.0 != button && drag.0.group() == button.group() {
                        style.bg(theme.secondary_hover).border_color(theme.primary)
                    } else {
                        style
                    }
                });
        }
        element.into_any_element()
    }
}

fn section_group(section: TabBarSection, symbols: bool) -> TabBarButtonGroup {
    match section {
        TabBarSection::Pages => TabBarButtonGroup::Pages,
        TabBarSection::Filters if symbols => TabBarButtonGroup::Symbols,
        TabBarSection::Filters => TabBarButtonGroup::Filters,
        TabBarSection::Tags => TabBarButtonGroup::Tags,
    }
}

fn section_title(section: TabBarSection, theme: ThemeColor, cx: &App) -> Div {
    div()
        .h(ui_rem(28.0))
        .flex_none()
        .flex()
        .items_center()
        .gap_1()
        .px_2()
        .text_size(ui_rem(11.0))
        .text_color(theme.muted_foreground)
        .child(icon("grip-vertical", 12.0, theme.muted_foreground))
        .child(
            div()
                .min_w_0()
                .truncate()
                .child(i18n::translator(cx).translate(section.label_key())),
        )
}

fn buttons_row(buttons: impl IntoIterator<Item = impl IntoElement>) -> Div {
    div()
        .h(ui_rem(40.0))
        .flex_none()
        .flex()
        .items_center()
        .px_1()
        .gap_1()
        .children(buttons)
}

fn section_frame(
    section: TabBarSection,
    layout: &TabBarLayout,
    theme: ThemeColor,
    title: impl IntoElement,
    buttons: impl IntoElement,
) -> Div {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .when(
            section != *layout.sections.last().unwrap_or(&section),
            |style| style.border_r_1().border_color(theme.border),
        )
        .child(title)
        .child(buttons)
}

fn button_active(button: TabBarButton, visible: bool, symbols: bool) -> bool {
    let active_page = if symbols {
        TabBarButton::Emoji
    } else {
        TabBarButton::Clipboard
    };
    visible && (button == active_page || button == TabBarButton::SymbolEmoji)
}

fn button_visual(
    button: TabBarButton,
    layout: &TabBarLayout,
    symbols: bool,
    theme: ThemeColor,
) -> Div {
    let visible = layout.is_visible(button);
    let active = button_active(button, visible, symbols);
    let color = if active {
        theme.primary_foreground
    } else if visible {
        theme.foreground
    } else {
        theme.muted_foreground
    };
    let button_icon = if button == TabBarButton::SymbolEmoji {
        div()
            .size(ui_rem(16.0))
            .flex()
            .items_center()
            .justify_center()
            .text_size(ui_rem(14.0))
            .line_height(ui_rem(16.0))
            .font_family("Segoe UI Emoji")
            .child("😀")
            .into_any_element()
    } else {
        icon(button.icon(), 16.0, color).into_any_element()
    };
    div()
        .relative()
        .h(ui_rem(28.0))
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.0))
        .border_1()
        .border_color(if visible { theme.tab_bar } else { theme.border })
        .when(active, |style| {
            style.bg(theme.primary).border_color(theme.primary)
        })
        .when(!visible, |style| style.opacity(0.4))
        .child(button_icon)
        .child(
            div()
                .absolute()
                .right_0()
                .bottom_0()
                .size(ui_rem(10.0))
                .rounded(px(2.0))
                .border_1()
                .border_color(if visible {
                    theme.primary
                } else {
                    theme.muted_foreground
                })
                .bg(if visible {
                    theme.primary
                } else {
                    theme.tab_bar
                })
                .when(visible, |style| {
                    style.child(icon("check", 8.0, theme.primary_foreground))
                }),
        )
}

fn start_drag_preview(
    content: PreviewContent,
    size: Size<Pixels>,
    cursor_offset: Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<DragPreview> {
    let preview = cx.new(|_| DragPreview {
        content: content.clone(),
        size,
        render_preview: true,
    });
    #[cfg(target_os = "windows")]
    if let Some(origin) = crate::platform::window::logical_client_origin_on_desktop(window) {
        let mouse = window.mouse_position();
        let bounds = Bounds::new(
            point(
                px(origin.x) + mouse.x - cursor_offset.x,
                px(origin.y) + mouse.y - cursor_offset.y,
            ),
            size,
        );
        let scale = window.scale_factor().max(1.0);
        let offset_physical = point(
            f32::from(cursor_offset.x) * scale,
            f32::from(cursor_offset.y) * scale,
        );
        let popup_preview = cx.new(|_| DragPreview {
            content,
            size,
            render_preview: true,
        });
        match DragPreviewService::start(
            window.window_handle(),
            popup_preview,
            bounds,
            window.display(cx).map(|display| display.id()),
            offset_physical,
            window.rem_size(),
            |cx| cx.refresh_windows(),
            cx,
        ) {
            Ok(()) => preview.update(cx, |preview, _| preview.render_preview = false),
            Err(error) => tracing::error!("启动 Tab 栏拖拽预览失败：{error}"),
        }
    }
    #[cfg(not(target_os = "windows"))]
    let _ = (cursor_offset, window);
    preview
}

fn hide_drag_preview(cx: &mut App) {
    #[cfg(target_os = "windows")]
    DragPreviewService::hide(cx);
    #[cfg(not(target_os = "windows"))]
    let _ = cx;
}

fn save_layout(
    view: &mut SettingsWindowView,
    update: impl FnOnce(&mut TabBarLayout),
    cx: &mut Context<SettingsWindowView>,
) {
    let error = i18n::translator(cx).translate("settings.personalization.save_error");
    persist_appearance_change_in_view(
        view,
        "Tab 栏布局",
        |appearance| {
            update(&mut appearance.tab_bar);
        },
        error,
        cx,
    );
}
