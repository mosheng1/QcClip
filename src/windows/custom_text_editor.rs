use crate::ui::scale::ui_rem;

use gpui::prelude::*;
use std::collections::HashSet;

use gpui::{
    AnyElement, App, Bounds, Context, DisplayId, ElementId, Entity, FontWeight, IntoElement,
    PromptButton, PromptLevel, Render, Subscription, TitlebarOptions, Window,
    WindowBackgroundAppearance, WindowBounds, WindowControlArea, WindowDecorations, WindowHandle,
    WindowKind, WindowOptions, div, px, relative, rgb, size,
};
use gpui_component::{
    ActiveTheme as _, InteractiveElementExt as _, Root, Sizable as _,
    button::ButtonVariants as _,
    collapsible::Collapsible,
    h_flex,
    input::{Editor, EditorState, Input, InputEvent, InputState},
    scroll::ScrollableElement,
    v_flex,
};

use crate::{
    domain::clipboard::Tag,
    i18n,
    ui::{hover_icon, hover_icon_path},
    windows::dialog::{dialog_button, dialog_footer, dialog_primary_button},
};

const EDITOR_WINDOW_WIDTH: f32 = 900.0;
const EDITOR_WINDOW_HEIGHT: f32 = 640.0;
const EDITOR_WINDOW_MIN_WIDTH: f32 = 520.0;
const EDITOR_WINDOW_MIN_HEIGHT: f32 = 360.0;
const EDITOR_TITLE_BAR_HEIGHT: f32 = 36.0;
const EDITOR_WINDOW_CONTROL_WIDTH: f32 = 42.0;

/// 打开自定义文本编辑窗口。
///
pub(crate) fn open(
    text: String,
    tags: Vec<Tag>,
    selected_tag_ids: Vec<i64>,
    on_confirm: Box<dyn FnOnce(String, Vec<i64>, &mut App)>,
    on_restore: Option<Box<dyn FnOnce(&mut App)>>,
    cx: &mut App,
) -> Result<(), String> {
    open_with_title(
        text,
        "main.editor.title",
        tags,
        selected_tag_ids,
        false,
        None,
        on_confirm,
        on_restore,
        cx,
    )
    .map(|_| ())
}

pub(crate) fn open_new(
    text: String,
    tags: Vec<Tag>,
    on_confirm: Box<dyn FnOnce(String, Vec<i64>, &mut App)>,
    cx: &mut App,
) -> Result<(), String> {
    open_with_title(
        text,
        "main.editor.new_title",
        tags,
        Vec::new(),
        false,
        None,
        on_confirm,
        None,
        cx,
    )
    .map(|_| ())
}

pub(crate) fn open_new_favorite(
    text: String,
    tags: Vec<Tag>,
    display_id: Option<DisplayId>,
    on_confirm: Box<dyn FnOnce(String, Vec<i64>, &mut App)>,
    cx: &mut App,
) -> Result<WindowHandle<Root>, String> {
    open_with_title(
        text,
        "main.editor.new_favorite_title",
        tags,
        Vec::new(),
        true,
        display_id,
        on_confirm,
        None,
        cx,
    )
}

fn open_with_title(
    text: String,
    title_key: &'static str,
    tags: Vec<Tag>,
    selected_tag_ids: Vec<i64>,
    tags_expanded: bool,
    display_id: Option<DisplayId>,
    on_confirm: Box<dyn FnOnce(String, Vec<i64>, &mut App)>,
    on_restore: Option<Box<dyn FnOnce(&mut App)>>,
    cx: &mut App,
) -> Result<WindowHandle<Root>, String> {
    let title = i18n::translator(cx).translate(title_key);
    let window = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    display_id,
                    size(px(EDITOR_WINDOW_WIDTH), px(EDITOR_WINDOW_HEIGHT)),
                    cx,
                ))),
                display_id,
                titlebar: Some(TitlebarOptions {
                    title: Some(title.clone().into()),
                    appears_transparent: true,
                    ..Default::default()
                }),
                kind: WindowKind::Normal,
                focus: true,
                is_movable: true,
                is_resizable: true,
                is_minimizable: true,
                window_background: WindowBackgroundAppearance::Transparent,
                window_min_size: Some(size(
                    px(EDITOR_WINDOW_MIN_WIDTH),
                    px(EDITOR_WINDOW_MIN_HEIGHT),
                )),
                window_decorations: Some(WindowDecorations::Client),
                ..Default::default()
            },
            move |window, cx| {
                crate::platform::window::configure_frameless_window(window, true);
                window.bounds_changed(cx);
                crate::window_material::register_window(window, cx);
                let view = cx.new(|cx| {
                    CustomTextEditor::new(
                        window,
                        cx,
                        text,
                        title,
                        tags,
                        selected_tag_ids,
                        tags_expanded,
                        on_confirm,
                        on_restore,
                    )
                });
                cx.new(|cx| crate::window_material::new_root(view, window, cx))
            },
        )
        .map_err(|error| error.to_string())?;
    if let Err(error) = window.update(cx, |_, window, _| window.activate_window()) {
        tracing::error!("激活文本编辑窗口失败：{error}");
    }
    Ok(window)
}

pub(crate) struct CustomTextEditor {
    editor: Entity<EditorState>,
    tag_search_input: Entity<InputState>,
    _tag_search_subscription: Subscription,
    tags: Vec<Tag>,
    selected_tag_ids: HashSet<i64>,
    tags_expanded: bool,
    title: String,
    on_confirm: Option<Box<dyn FnOnce(String, Vec<i64>, &mut App)>>,
    on_restore: Option<Box<dyn FnOnce(&mut App)>>,
}

impl CustomTextEditor {
    fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        text: String,
        title: String,
        tags: Vec<Tag>,
        selected_tag_ids: Vec<i64>,
        tags_expanded: bool,
        on_confirm: Box<dyn FnOnce(String, Vec<i64>, &mut App)>,
        on_restore: Option<Box<dyn FnOnce(&mut App)>>,
    ) -> Self {
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .default_value(text)
                .soft_wrap(true)
                .cursor_surrounding_lines(Some(3))
        });
        let translator = i18n::translator(cx);
        let tag_search_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(translator.translate("main.editor.tags_search_placeholder"))
        });
        let tag_search_subscription =
            cx.subscribe_in(&tag_search_input, window, |_, _, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            });
        let editor_for_focus = editor.clone();
        window.defer(cx, move |window, cx| {
            editor_for_focus.update(cx, |editor, cx| editor.focus(window, cx));
        });
        Self {
            editor,
            tag_search_input,
            _tag_search_subscription: tag_search_subscription,
            tags,
            selected_tag_ids: selected_tag_ids.into_iter().collect(),
            tags_expanded,
            title,
            on_confirm: Some(on_confirm),
            on_restore,
        }
    }

    fn render_tags_section(
        &self,
        _window: &mut Window,
        editor_surface: gpui::Hsla,
        editor_border: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().colors;
        let material_enabled = theme.background.alpha < 0.999;
        let tag_chip_surface = material_surface(theme.background, material_enabled, 0.68);
        let translator = i18n::translator(cx);
        let selected_names = self
            .tags
            .iter()
            .filter(|tag| self.selected_tag_ids.contains(&tag.id))
            .map(|tag| tag.name.as_str())
            .collect::<Vec<_>>();
        let selected_summary = if selected_names.is_empty() {
            translator.translate("main.editor.tags_empty")
        } else {
            selected_names.join("、")
        };
        let search_query = self.tag_search_input.read(cx).value().trim().to_lowercase();
        let visible_tags = self
            .tags
            .iter()
            .filter(|tag| {
                search_query.is_empty() || tag.name.to_lowercase().contains(&search_query)
            })
            .collect::<Vec<_>>();
        let editor = cx.entity();
        let tag_chips = visible_tags.iter().map(|tag| {
            let tag_id = tag.id;
            let selected = self.selected_tag_ids.contains(&tag_id);
            let tag_name = tag.name.clone();
            let tag_color = parse_tag_color(&tag.color, theme.blue);
            let foreground = if selected {
                theme.primary_foreground
            } else {
                theme.foreground
            };
            let editor = editor.clone();
            div()
                .id(("custom-text-editor-tag", tag_id as u64))
                .flex_none()
                .max_w_full()
                .h(ui_rem(30.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .px(px(8.0))
                .rounded_lg()
                .border_1()
                .border_color(editor_border)
                .cursor_pointer()
                .text_size(ui_rem(11.0))
                .text_color(foreground)
                .bg(if selected {
                    material_surface(theme.primary, material_enabled, 0.88)
                } else {
                    tag_chip_surface
                })
                .hover(|this| {
                    if selected {
                        this.bg(theme.primary_hover)
                    } else {
                        this.bg(theme.secondary_hover)
                    }
                })
                .child(
                    div()
                        .size(ui_rem(8.0))
                        .flex_none()
                        .rounded_full()
                        .bg(tag_color),
                )
                .child(div().max_w(px(180.0)).truncate().child(tag_name))
                .on_click(move |_, _, cx| {
                    editor.update(cx, |this, cx| {
                        if !this.selected_tag_ids.insert(tag_id) {
                            this.selected_tag_ids.remove(&tag_id);
                        }
                        cx.notify();
                    });
                })
                .into_any_element()
        });
        let tag_list = if visible_tags.is_empty() {
            div()
                .h(ui_rem(42.0))
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(ui_rem(11.0))
                .text_color(theme.muted_foreground)
                .child(if self.tags.is_empty() {
                    translator.translate("main.editor.tags_empty_available")
                } else {
                    translator.translate("main.editor.tags_no_match")
                })
                .into_any_element()
        } else {
            v_flex()
                .w_full()
                .max_h(px(180.0))
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .child(
                    v_flex().size_full().overflow_y_scrollbar().child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .flex_wrap()
                            .items_start()
                            .content_start()
                            .gap(px(6.0))
                            .px(px(8.0))
                            .py(px(6.0))
                            .children(tag_chips),
                    ),
                )
                .into_any_element()
        };
        let header = div()
            .id("custom-text-editor-tags-toggle")
            .group("custom-text-editor-tags-toggle")
            .w_full()
            .h(ui_rem(34.0))
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .cursor_pointer()
            .hover(|this| this.bg(theme.secondary_hover))
            .child(hover_icon(
                "custom-text-editor-tags-toggle",
                if self.tags_expanded {
                    "chevron-up"
                } else {
                    "chevron-down"
                },
                14.0,
                theme.muted_foreground,
            ))
            .child(
                div()
                    .flex_none()
                    .text_size(ui_rem(12.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child(translator.translate("main.editor.tags")),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_size(ui_rem(11.0))
                    .text_color(theme.muted_foreground)
                    .child(selected_summary),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.tags_expanded = !this.tags_expanded;
                cx.notify();
            }));
        let search = Input::new(&self.tag_search_input)
            .small()
            .appearance(false)
            .bordered(false)
            .focus_bordered(false)
            .cleanable(false)
            .w_full();
        Collapsible::new()
            .open(self.tags_expanded)
            .w_full()
            .border_t_1()
            .border_color(editor_border)
            .child(header)
            .content(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .bg(editor_surface)
                    .child(
                        div()
                            .h(ui_rem(30.0))
                            .w_full()
                            .flex()
                            .items_center()
                            .px_2()
                            .child(search),
                    )
                    .child(tag_list),
            )
            .into_any_element()
    }
}

impl Render for CustomTextEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let material_enabled = theme.background.alpha < 0.999;
        let editor_surface = material_surface(theme.background, material_enabled, 0.88);
        let editor_border = material_surface(theme.input, material_enabled, 0.82);
        let translator = i18n::translator(cx);
        let mut footer_actions = Vec::with_capacity(3);
        if self.on_restore.is_some() {
            footer_actions.push(
                dialog_button(
                    "custom-text-editor-restore",
                    translator.translate("main.editor.restore"),
                )
                .danger()
                .on_click(cx.listener(|this, _, window, cx| {
                    let Some(on_restore) = this.on_restore.take() else {
                        return;
                    };
                    let translator = i18n::translator(cx);
                    let title = translator.translate("main.editor.restore_confirm_title");
                    let detail = translator.translate("main.editor.restore_confirm_detail");
                    let cancel = PromptButton::cancel(translator.translate("main.form.cancel"));
                    let confirm = PromptButton::ok(translator.translate("main.form.confirm"));
                    let answer = window.prompt(
                        PromptLevel::Warning,
                        &title,
                        Some(&detail),
                        &[cancel, confirm],
                        cx,
                    );
                    let editor = cx.entity();
                    let editor_window = window.window_handle();
                    cx.spawn(async move |_this, cx| {
                        let answer = match answer.await {
                            Ok(answer) => answer,
                            Err(error) => {
                                tracing::error!(error = ?error, "等待恢复确认结果失败");
                                0
                            }
                        };
                        if answer == 1 {
                            cx.update(|cx| on_restore(cx));
                            if let Err(error) = editor_window.update(cx, |_, window, _| {
                                window.remove_window();
                            }) {
                                tracing::debug!(%error, "关闭自定义文本编辑窗口失败");
                            }
                        } else {
                            editor.update(cx, |this, _| {
                                this.on_restore = Some(on_restore);
                            });
                        }
                    })
                    .detach();
                }))
                .into_any_element(),
            );
        }
        footer_actions.extend([
            dialog_button(
                "custom-text-editor-cancel",
                translator.translate("main.form.cancel"),
            )
            .on_click(|_, window: &mut Window, _| window.remove_window())
            .into_any_element(),
            dialog_primary_button(
                "custom-text-editor-confirm",
                translator.translate("main.form.confirm"),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                let Some(on_confirm) = this.on_confirm.take() else {
                    window.remove_window();
                    return;
                };
                let text = this.editor.read(cx).value().to_string();
                let mut tag_ids = this.selected_tag_ids.iter().copied().collect::<Vec<_>>();
                tag_ids.sort_unstable();
                on_confirm(text, tag_ids, cx);
                window.remove_window();
            }))
            .into_any_element(),
        ]);

        div()
            .id("custom-text-editor")
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(editor_title_bar(window, &self.title, cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .flex_col()
                    .p(px(crate::windows::dialog::DIALOG_PADDING))
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .flex()
                            .flex_col()
                            .overflow_hidden()
                            .rounded_md()
                            .border_1()
                            .border_color(editor_border)
                            .bg(editor_surface)
                            .child(
                                div().flex_1().min_h_0().w_full().overflow_hidden().child(
                                    Editor::new(&self.editor)
                                        .appearance(false)
                                        .bordered(false)
                                        .editor_background(editor_surface)
                                        .editor_gutter_background(gpui::transparent_black())
                                        .h(relative(1.)),
                                ),
                            )
                            .child(self.render_tags_section(
                                window,
                                editor_surface,
                                editor_border,
                                cx,
                            )),
                    )
                    .child(dialog_footer(footer_actions, theme)),
            )
    }
}

fn editor_title_bar(
    window: &mut Window,
    title: &str,
    cx: &mut Context<CustomTextEditor>,
) -> impl IntoElement {
    let theme = cx.theme().colors;

    div()
        .w_full()
        .h(ui_rem(EDITOR_TITLE_BAR_HEIGHT))
        .flex_shrink_0()
        .flex()
        .items_center()
        .border_b_1()
        .border_color(theme.title_bar_border)
        .bg(theme.title_bar)
        .child(
            div()
                .id("custom-text-editor-title")
                .h_full()
                .flex_shrink_0()
                .flex()
                .items_center()
                .px_3()
                .window_control_area(WindowControlArea::Drag)
                .on_double_click(|_, window, _| window.zoom_window())
                .child(
                    div()
                        .text_size(ui_rem(15.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child(title.to_owned()),
                ),
        )
        .child(
            div()
                .id("custom-text-editor-title-drag")
                .h_full()
                .flex_1()
                .window_control_area(WindowControlArea::Drag)
                .on_double_click(|_, window, _| window.zoom_window()),
        )
        .child(editor_window_controls(window, cx))
}

fn parse_tag_color(color: &str, fallback: gpui::Hsla) -> gpui::Hsla {
    let value = color.trim().strip_prefix('#').unwrap_or(color.trim());
    if value.len() == 6
        && value.chars().all(|character| character.is_ascii_hexdigit())
        && let Ok(value) = u32::from_str_radix(value, 16)
    {
        return gpui::rgb_to_hsla(rgb(value));
    }
    fallback
}

fn material_surface(color: gpui::Hsla, material_enabled: bool, opacity: f32) -> gpui::Hsla {
    if material_enabled {
        gpui::ColorExt::opacity(&color, opacity)
    } else {
        color
    }
}

fn editor_window_controls(window: &mut Window, cx: &mut Context<CustomTextEditor>) -> AnyElement {
    let maximize_icon = if window.is_maximized() {
        "assets/icons/window-restore.svg"
    } else {
        "assets/icons/window-maximize.svg"
    };

    div()
        .h_full()
        .flex_shrink_0()
        .flex()
        .child(editor_window_control_button(
            "custom-text-editor-minimize",
            "assets/icons/window-minimize.svg",
            WindowControlArea::Min,
            false,
            |window, _| window.minimize_window(),
            window,
            cx,
        ))
        .child(editor_window_control_button(
            "custom-text-editor-maximize",
            maximize_icon,
            WindowControlArea::Max,
            false,
            |window, _| window.zoom_window(),
            window,
            cx,
        ))
        .child(editor_window_control_button(
            "custom-text-editor-close",
            "assets/icons/window-close.svg",
            WindowControlArea::Close,
            true,
            |window, _| window.remove_window(),
            window,
            cx,
        ))
        .into_any_element()
}

fn editor_window_control_button(
    id: impl Into<ElementId>,
    icon_path: &'static str,
    control_area: WindowControlArea,
    is_danger: bool,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut Context<CustomTextEditor>,
) -> AnyElement {
    let id = id.into();
    let group = format!("{id:?}");
    let theme = cx.theme().colors;
    let (hover_bg, hover_fg, active_bg) = if is_danger {
        (theme.danger, theme.danger_foreground, theme.danger_active)
    } else {
        (
            theme.secondary_hover,
            theme.foreground,
            theme.secondary_active,
        )
    };
    let hover_state = window.use_keyed_state(id.clone(), cx, |_, _| false);
    let icon_color = if *hover_state.read(cx) {
        hover_fg
    } else {
        theme.foreground
    };
    let hover_state_for_listener = hover_state.clone();

    div()
        .id(id)
        .flex()
        .w(ui_rem(EDITOR_WINDOW_CONTROL_WIDTH))
        .h_full()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .text_color(theme.foreground)
        .window_control_area(control_area)
        .hover(move |style| style.bg(hover_bg).text_color(hover_fg))
        .active(move |style| style.bg(active_bg).text_color(hover_fg))
        .on_click(move |_, window, cx| on_click(window, cx))
        .on_hover(move |hovered, _, cx| {
            let _ = hover_state_for_listener.update(cx, |state, cx| {
                if *state != *hovered {
                    *state = *hovered;
                    cx.notify();
                }
            });
        })
        .group(group.clone())
        .child(hover_icon_path(group, icon_path, 16.0, icon_color))
        .into_any_element()
}
