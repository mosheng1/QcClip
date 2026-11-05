use crate::ui::scale::ui_rem;

use super::super::{SettingsPage, notify_settings_error};
use gpui::prelude::*;
use gpui::{AnyElement, Context};

use super::super::{
    SettingsWindowView,
    components::{
        clickable_setting_row, setting_row, settings_dropdown, settings_dropdown_with_font,
        settings_error_row, settings_page_header, settings_section_title,
    },
};
use crate::{
    config::{
        AnimationConfig, AppearanceConfig, AppearanceMode, ListItemHeight, ListItemStyle,
        TitlebarPosition, WindowMaterial,
    },
    config_runtime, i18n,
    ui::icon::icon,
    ui::theme::{ThemeOption, ThemePreviewColors, available_theme_options},
    ui::{SingleSelectOption, single_select},
    windows::menu::{DropdownItem, DropdownItemId},
};
use gpui::DurationWithEasing as _;
use gpui::{App, Entity, FontWeight, Hsla, SharedString, Window, div, px, relative};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::slider::Slider;
use gpui_component::switch::Switch;
use gpui_component::{ActiveTheme as _, Sizable as _, ThemeMode, WindowExt as _, h_flex, v_flex};
use gpui_component::{Colorize as _, ThemeColor, tooltip::Tooltip};
use std::{rc::Rc, time::Duration};

impl SettingsWindowView {
    pub(in crate::windows::settings) fn render_appearance(
        &self,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().colors;
        let translator = i18n::translator(cx);
        let appearance = config_runtime::appearance_config(cx);
        let single_select_animation = config_runtime::animation_enabled(
            cx,
            crate::config::AnimationKind::SingleSelectIndicator,
        );
        let view = cx.entity().clone();
        let reset_error = translator.translate("settings.personalization.reset_error");
        let reset_view = view.clone();
        let reset_button = Button::new("settings-appearance-reset")
            .warning()
            .small()
            .label(translator.translate("settings.action.reset_page"))
            .on_click(move |_, _, cx| {
                let mut appearance = config_runtime::appearance_config(cx);
                reset_appearance_page_fields(&mut appearance);
                let result = config_runtime::set_appearance_config(appearance, cx);
                let error_message = result.as_ref().err().map(|error| {
                    tracing::error!("重置外观设置失败：{error}");
                    reset_error.clone()
                });
                reset_view.update(cx, |view, cx| {
                    view.appearance_error =
                        notify_settings_error(SettingsPage::Appearance, error_message, cx);
                    cx.notify();
                });
            });

        let mut scale_presets = crate::config::UI_SCALE_PRESETS.to_vec();
        if !scale_presets.contains(&appearance.ui_scale_percent) {
            scale_presets.push(appearance.ui_scale_percent);
            scale_presets.sort_unstable();
        }
        let scale_items = scale_presets
            .into_iter()
            .map(|percent| {
                DropdownItem::option(
                    DropdownItemId::new(percent.to_string()),
                    format!("{percent}%"),
                )
                .selected(percent == appearance.ui_scale_percent)
            })
            .collect();
        let scale_selector = settings_dropdown(
            "settings-ui-scale-select",
            appearance.ui_scale_percent.to_string(),
            scale_items,
            {
                let view = view.clone();
                let save_error = translator.translate("settings.personalization.save_error");
                move |id, cx| {
                    let Ok(percent) = id.as_str().parse::<u16>() else {
                        return;
                    };
                    persist_appearance_change(
                        view.clone(),
                        "界面缩放",
                        move |appearance| appearance.ui_scale_percent = percent,
                        save_error.clone(),
                        cx,
                    );
                }
            },
            cx,
        );

        let current_font = appearance.app_font_family.as_deref().unwrap_or("");
        let mut font_items = vec![
            DropdownItem::option(
                DropdownItemId::new(""),
                translator.translate("settings.personalization.app_font_default"),
            )
            .selected(current_font.is_empty()),
        ];
        font_items.extend(
            crate::ui::fonts::system_font_names(cx)
                .iter()
                .map(|family| {
                    DropdownItem::option(DropdownItemId::new(family.clone()), family.clone())
                        .selected(family == current_font)
                }),
        );
        let font_selector = settings_dropdown_with_font(
            "settings-app-font-select",
            current_font.to_owned(),
            font_items,
            Some(".SystemUIFont".into()),
            {
                let view = view.clone();
                let save_error = translator.translate("settings.personalization.save_error");
                move |id, cx| {
                    let family = (!id.as_str().is_empty()).then(|| id.as_str().to_owned());
                    persist_appearance_change(
                        view.clone(),
                        "应用字体",
                        move |appearance| appearance.app_font_family = family,
                        save_error.clone(),
                        cx,
                    );
                }
            },
            cx,
        );

        let save_error = translator.translate("settings.personalization.save_error");
        let theme_options = available_theme_options(cx);
        let light_preview =
            selected_theme_preview(&theme_options, ThemeMode::Light, &appearance.light_theme);
        let dark_preview =
            selected_theme_preview(&theme_options, ThemeMode::Dark, &appearance.dark_theme);
        let mode_cards = h_flex()
            .w_full()
            .gap_3()
            .children(AppearanceMode::ALL.into_iter().map(|mode| {
                let view = view.clone();
                let save_error = save_error.clone();
                appearance_mode_card(
                    mode,
                    translator.translate(appearance_mode_label_key(mode)),
                    mode == appearance.mode,
                    light_preview,
                    dark_preview,
                    theme,
                    move |cx| {
                        persist_appearance_change(
                            view.clone(),
                            "外观模式",
                            move |appearance| appearance.mode = mode,
                            save_error.clone(),
                            cx,
                        );
                    },
                )
            }));

        let horizontal_list = config_runtime::clipboard_config(cx)
            .window_position
            .uses_horizontal_list();
        let titlebar_positions = TitlebarPosition::ALL
            .into_iter()
            .filter(|position| !horizontal_list || !position.is_vertical())
            .collect::<Vec<_>>();
        let titlebar_position = if horizontal_list && appearance.titlebar_position.is_vertical() {
            TitlebarPosition::Top
        } else {
            appearance.titlebar_position
        };
        let titlebar_position_selector = single_select(
            "settings-titlebar-position-select",
            titlebar_positions.into_iter().map(|position| {
                SingleSelectOption::new(
                    position,
                    translator.translate(titlebar_position_label_key(position)),
                )
            }),
            titlebar_position,
            {
                let view = view.clone();
                let save_error = save_error.clone();
                move |position, cx| {
                    persist_appearance_change(
                        view.clone(),
                        "标题栏位置",
                        move |appearance| appearance.titlebar_position = position,
                        save_error.clone(),
                        cx,
                    );
                }
            },
            theme,
            single_select_animation,
        );
        let current_material =
            crate::window_material::supported_material(appearance.window_material);
        let material_enabled = current_material != WindowMaterial::Disabled;
        let window_material_selector = single_select(
            "settings-window-material-select",
            crate::window_material::available_materials()
                .into_iter()
                .map(|material| {
                    SingleSelectOption::new(
                        material,
                        translator.translate(window_material_label_key(material)),
                    )
                })
                .collect::<Vec<_>>(),
            current_material,
            {
                let view = view.clone();
                let save_error = save_error.clone();
                move |material, cx| {
                    persist_appearance_change(
                        view.clone(),
                        "窗口材质",
                        move |appearance| appearance.window_material = material,
                        save_error.clone(),
                        cx,
                    );
                }
            },
            theme,
            single_select_animation,
        );
        let window_material_opacity_selector = h_flex()
            .w_full()
            .gap_3()
            .child(Slider::new(&self.window_material_opacity_slider).flex_1())
            .child(
                div()
                    .w(ui_rem(52.0))
                    .text_sm()
                    .text_center()
                    .text_color(theme.foreground)
                    .child(format!(
                        "{}%",
                        (appearance.window_material_opacity * 100.0).round() as i32
                    )),
            );
        let window_material_brightness_selector = h_flex()
            .w_full()
            .gap_3()
            .child(Slider::new(&self.window_material_brightness_slider).flex_1())
            .child(
                div()
                    .w(ui_rem(52.0))
                    .text_sm()
                    .text_center()
                    .text_color(theme.foreground)
                    .child(format!(
                        "{}%",
                        (crate::window_material::current_brightness(cx) * 100.0).round() as i32
                    )),
            );
        let background_image_blur_selector = h_flex()
            .w_full()
            .gap_3()
            .child(Slider::new(&self.background_image_blur_slider).flex_1())
            .child(
                div()
                    .w(ui_rem(52.0))
                    .text_sm()
                    .text_center()
                    .text_color(theme.foreground)
                    .child(format!(
                        "{}px",
                        appearance.background_image_blur.round() as i32
                    )),
            );
        let background_image_selector = Button::new("settings-background-image-choose")
            .small()
            .max_w(ui_rem(220.0))
            .label(
                appearance
                    .background_image_path
                    .as_deref()
                    .and_then(|path| {
                        std::path::Path::new(path)
                            .file_name()
                            .and_then(|name| name.to_str())
                    })
                    .map(str::to_owned)
                    .unwrap_or_else(|| {
                        translator.translate("settings.personalization.choose_background_image")
                    }),
            )
            .on_click({
                let view = view.clone();
                let save_error = save_error.clone();
                move |_, _, cx| {
                    choose_background_image(view.clone(), save_error.clone(), cx);
                }
            });
        // 材质面板中的所有设置共用紧凑的标签和控件布局。
        let slider_row = |label: String, control: AnyElement| {
            let title = label.clone();
            self.highlight_element(
                theme,
                title,
                h_flex()
                    .w_full()
                    .gap_3()
                    .items_center()
                    .child(
                        div()
                            .w(ui_rem(96.0))
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(label),
                    )
                    .child(div().flex_1().min_w_0().child(control)),
            )
        };
        let background_image_title =
            translator.translate("settings.personalization.background_image");
        let background_image_row = self.highlight_element(
            theme,
            background_image_title.clone(),
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .w(ui_rem(96.0))
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(background_image_title),
                )
                .child(background_image_selector),
        );

        let window_material_panel = v_flex()
            .w_full()
            .gap_3()
            .child(
                self.highlight_element(
                    theme,
                    translator.translate("settings.personalization.window_material"),
                    h_flex()
                        .w_full()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(div().text_sm().text_color(theme.foreground).child(
                            translator.translate("settings.personalization.window_material"),
                        ))
                        .child(window_material_selector),
                ),
            )
            .when(material_enabled, |this| {
                this.child(slider_row(
                    translator.translate("settings.personalization.window_material_opacity"),
                    window_material_opacity_selector.into_any_element(),
                ))
            })
            .when(
                matches!(
                    current_material,
                    WindowMaterial::Acrylic | WindowMaterial::Blur | WindowMaterial::CustomImage
                ),
                |this| {
                    this.child(slider_row(
                        translator.translate("settings.personalization.window_material_brightness"),
                        window_material_brightness_selector.into_any_element(),
                    ))
                },
            )
            .when(current_material == WindowMaterial::CustomImage, |this| {
                this.child(
                    div()
                        .w_full()
                        .border_t_1()
                        .border_color(theme.border)
                        .pt_3()
                        .child(
                            v_flex().w_full().gap_3().child(background_image_row).child(
                                slider_row(
                                    translator.translate(
                                        "settings.personalization.background_image_blur",
                                    ),
                                    background_image_blur_selector.into_any_element(),
                                ),
                            ),
                        ),
                )
            });

        let on_theme_select: Rc<dyn Fn(ThemeMode, SharedString, &mut App)> = {
            let view = view.clone();
            let save_error = save_error.clone();
            Rc::new(move |mode, value, cx| {
                let value = value.to_string();
                if mode.is_dark() {
                    persist_appearance_change(
                        view.clone(),
                        "深色主题",
                        move |appearance| appearance.dark_theme = value,
                        save_error.clone(),
                        cx,
                    );
                } else {
                    persist_appearance_change(
                        view.clone(),
                        "浅色主题",
                        move |appearance| appearance.light_theme = value,
                        save_error.clone(),
                        cx,
                    );
                }
            })
        };
        let external_badge = translator.translate("settings.personalization.theme_external_badge");
        let swatch_hover_animation =
            config_runtime::animation_enabled(cx, crate::config::AnimationKind::IconHoverScale);
        let light_swatches = theme_swatches(
            &theme_options,
            ThemeMode::Light,
            &appearance.light_theme,
            &external_badge,
            theme,
            swatch_hover_animation,
            &on_theme_select,
        );
        let dark_swatches = theme_swatches(
            &theme_options,
            ThemeMode::Dark,
            &appearance.dark_theme,
            &external_badge,
            theme,
            swatch_hover_animation,
            &on_theme_select,
        );

        let mut section = v_flex()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .overflow_hidden()
            .rounded(px(8.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.title_bar)
            .child(settings_page_header(
                theme,
                translator.translate("settings.page.appearance"),
                translator.translate("settings.personalization.description"),
                reset_button,
            ))
            .child(settings_section_title(
                theme,
                translator.translate("settings.section.theme"),
            ))
            .child(self.highlight_element(
                theme,
                translator.translate("settings.personalization.appearance_mode"),
                settings_block(
                    theme,
                    translator.translate("settings.personalization.appearance_mode"),
                    translator.translate("settings.personalization.appearance_mode_description"),
                    mode_cards,
                ),
            ))
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.personalization.light_theme"),
                    Some(
                        translator
                            .translate("settings.personalization.light_theme_description")
                            .into(),
                    ),
                    light_swatches,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.personalization.dark_theme"),
                    Some(
                        translator
                            .translate("settings.personalization.dark_theme_description")
                            .into(),
                    ),
                    dark_swatches,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.personalization.ui_scale"),
                    Some(
                        translator
                            .translate("settings.personalization.ui_scale_description")
                            .into(),
                    ),
                    scale_selector,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.personalization.app_font"),
                    Some(
                        translator
                            .translate("settings.personalization.app_font_description")
                            .into(),
                    ),
                    font_selector,
                ),
            )
            .child(settings_section_title(
                theme,
                translator.translate("settings.section.window_material"),
            ))
            .child(
                div()
                    .w_full()
                    .border_b_1()
                    .border_color(theme.border)
                    .px_4()
                    .py_3()
                    .child(window_material_panel),
            )
            .child(settings_section_title(
                theme,
                translator.translate("settings.section.layout"),
            ))
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.personalization.titlebar_position"),
                    Some(
                        translator
                            .translate("settings.personalization.titlebar_position_description")
                            .into(),
                    ),
                    titlebar_position_selector,
                ),
            )
            .child(self.highlight_element(
                theme,
                translator.translate("settings.personalization.tab_bar"),
                self.render_tab_bar_editor(cx),
            ))
            .child(
                self.highlight_element(
                    theme,
                    translator.translate("settings.personalization.animations"),
                    clickable_setting_row(
                        "settings-personalization-animations",
                        theme,
                        translator.translate("settings.personalization.animations"),
                        Some(
                            translator
                                .translate("settings.personalization.animations_description")
                                .into(),
                        ),
                        {
                            let view = view.clone();
                            move |_, window, cx| open_animations_sheet(view.clone(), window, cx)
                        },
                    ),
                ),
            );
        if let Some(error) = &self.appearance_error {
            section = section.child(settings_error_row(theme, error));
        }
        section.into_any_element()
    }
}

fn open_animations_sheet(view: Entity<SettingsWindowView>, window: &mut Window, cx: &mut App) {
    let save_error = i18n::translator(cx).translate("settings.personalization.save_error");
    window.open_sheet(cx, move |sheet, _, cx| {
        let theme = cx.theme().colors;
        let translator = i18n::translator(cx);
        let appearance = config_runtime::appearance_config(cx);
        let enable_all = Button::new("settings-animations-enable-all")
            .small()
            .label(translator.translate("settings.personalization.animations.enable_all"))
            .on_click({
                let view = view.clone();
                let save_error = save_error.clone();
                move |_, _, cx| {
                    set_all_animations(view.clone(), true, save_error.clone(), cx);
                }
            });
        let disable_all = Button::new("settings-animations-disable-all")
            .small()
            .label(translator.translate("settings.personalization.animations.disable_all"))
            .on_click({
                let view = view.clone();
                let save_error = save_error.clone();
                move |_, _, cx| {
                    set_all_animations(view.clone(), false, save_error.clone(), cx);
                }
            });

        let controls = h_flex()
            .w_full()
            .gap_2()
            .pb_2()
            .child(enable_all)
            .child(disable_all);
        let mut content = v_flex()
            .size_full()
            .gap_1()
            .overflow_y_scrollbar()
            .child(controls);
        for kind in AnimationConfig::ALL {
            let label = translator.translate(kind.label_key());
            let description = translator.translate(kind.description_key());
            let switch = Switch::new(format!("settings-animation-{}", kind.config_key()))
                .large()
                .checked(appearance.animations.enabled(kind))
                .accessibility_label(label.clone())
                .on_click({
                    let view = view.clone();
                    let save_error = save_error.clone();
                    move |checked, _, cx| {
                        persist_appearance_change(
                            view.clone(),
                            "动画与动效",
                            move |appearance| appearance.animations.set_enabled(kind, *checked),
                            save_error.clone(),
                            cx,
                        );
                    }
                });
            content = content.child(setting_row(theme, label, Some(description.into()), switch));
        }

        sheet
            .title(translator.translate("settings.personalization.animations"))
            .size(ui_rem(420.0))
            .resizable(false)
            .shadow_none()
            .map(|this| crate::window_material::sheet_surface(this, cx))
            .child(content)
    });
}

fn set_all_animations(
    view: Entity<SettingsWindowView>,
    enabled: bool,
    save_error: String,
    cx: &mut App,
) {
    let mut appearance = config_runtime::appearance_config(cx);
    for kind in AnimationConfig::ALL {
        appearance.animations.set_enabled(kind, enabled);
    }
    let error_message = match config_runtime::set_appearance_config(appearance, cx) {
        Ok(_) => None,
        Err(error) => {
            tracing::error!("保存动画与动效设置失败：{error}");
            Some(save_error)
        }
    };
    view.update(cx, |view, cx| {
        view.appearance_error = notify_settings_error(view.active_page, error_message, cx);
        cx.notify();
    });
}

pub(super) fn persist_appearance_change<F>(
    view: Entity<SettingsWindowView>,
    field_name: &'static str,
    update: F,
    save_error: String,
    cx: &mut App,
) where
    F: FnOnce(&mut AppearanceConfig),
{
    let mut appearance = config_runtime::appearance_config(cx);
    update(&mut appearance);
    let error_message = match config_runtime::set_appearance_config(appearance, cx) {
        Ok(_) => None,
        Err(error) => {
            tracing::error!("保存{field_name}失败：{error}");
            Some(save_error)
        }
    };
    view.update(cx, |view, cx| {
        view.appearance_error = notify_settings_error(view.active_page, error_message, cx);
        cx.notify();
    });
}

fn choose_background_image(view: Entity<SettingsWindowView>, save_error: String, cx: &mut App) {
    let dialog = rfd::AsyncFileDialog::new()
        .set_title("选择背景图片")
        .add_filter(
            "图片文件",
            &["png", "jpg", "jpeg", "webp", "bmp", "gif", "ico"],
        );
    cx.spawn(async move |cx| {
        let Some(file) = dialog.pick_file().await else {
            return;
        };
        let path = file.path().to_string_lossy().into_owned();
        cx.update(|cx| {
            let mut appearance = config_runtime::appearance_config(cx);
            appearance.background_image_path = Some(path);
            let error_message = match config_runtime::set_appearance_config(appearance, cx) {
                Ok(_) => None,
                Err(error) => {
                    tracing::error!("保存背景图设置失败：{error}");
                    Some(save_error)
                }
            };
            view.update(cx, |view, cx| {
                view.appearance_error = notify_settings_error(view.active_page, error_message, cx);
                cx.notify();
            });
        });
    })
    .detach();
}

pub(in crate::windows::settings) fn persist_appearance_change_in_view<F>(
    view: &mut SettingsWindowView,
    field_name: &'static str,
    update: F,
    save_error: String,
    cx: &mut Context<SettingsWindowView>,
) where
    F: FnOnce(&mut AppearanceConfig),
{
    let mut appearance = config_runtime::appearance_config(cx);
    update(&mut appearance);
    view.appearance_error = notify_settings_error(
        view.active_page,
        match config_runtime::set_appearance_config(appearance, cx) {
            Ok(_) => None,
            Err(error) => {
                tracing::error!("保存{field_name}失败：{error}");
                Some(save_error)
            }
        },
        cx,
    );
    cx.notify();
}

/// 把“外观”页展示的字段恢复为默认值，列表与预览相关字段保持不变。
fn reset_appearance_page_fields(appearance: &mut AppearanceConfig) {
    let default = AppearanceConfig::default();
    appearance.mode = default.mode;
    appearance.light_theme = default.light_theme;
    appearance.dark_theme = default.dark_theme;
    appearance.ui_scale_percent = default.ui_scale_percent;
    appearance.app_font_family = default.app_font_family;
    appearance.window_material = default.window_material;
    appearance.window_material_opacity = default.window_material_opacity;
    appearance.window_material_brightness = default.window_material_brightness;
    appearance.background_image_path = default.background_image_path;
    appearance.background_image_blur = default.background_image_blur;
    appearance.titlebar_position = default.titlebar_position;
    appearance.tab_bar = default.tab_bar;
    appearance.animations = default.animations;
}

/// 外观页中占满整行的设置块：标题与说明在上，自定义控件在下。
fn settings_block(
    theme: ThemeColor,
    title: String,
    description: String,
    content: impl IntoElement,
) -> AnyElement {
    v_flex()
        .w_full()
        .min_w_0()
        .flex_shrink_0()
        .gap_3()
        .border_b_1()
        .border_color(theme.border)
        .px_4()
        .py_3()
        .child(
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.foreground)
                        .child(title),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(description),
                ),
        )
        .child(content)
        .into_any_element()
}

/// 当前配置对应主题的预览色；配置的主题不存在时与运行时一样回退到默认主题。
fn selected_theme_preview(
    themes: &[ThemeOption],
    mode: ThemeMode,
    current: &str,
) -> ThemePreviewColors {
    themes
        .iter()
        .filter(|option| option.mode == mode)
        .find(|option| option.value.as_ref() == current)
        .or_else(|| {
            themes
                .iter()
                .find(|option| option.mode == mode && option.is_default)
        })
        .map(|option| option.preview)
        .unwrap_or_else(|| ThemePreviewColors::fallback(mode))
}

/// 用主题颜色拼出的剪贴板窗口缩略图：标题栏加三行列表，第一行为选中态。
fn theme_thumbnail(colors: ThemePreviewColors) -> gpui::Div {
    let line = |width: f32, color: Hsla| {
        div()
            .h(ui_rem(3.0))
            .w(relative(width))
            .rounded(px(2.0))
            .bg(color)
    };
    let row = |active: bool, width: f32| {
        h_flex()
            .w_full()
            .h(ui_rem(10.0))
            .flex_shrink_0()
            .px(ui_rem(4.0))
            .gap(ui_rem(3.0))
            .rounded(px(3.0))
            .when(active, |this| {
                this.bg(colors.list_active).child(
                    div()
                        .w(ui_rem(2.0))
                        .h(ui_rem(6.0))
                        .flex_shrink_0()
                        .rounded(px(1.0))
                        .bg(colors.primary),
                )
            })
            .child(line(
                width,
                if active {
                    colors.foreground
                } else {
                    colors.muted_foreground.opacity(0.6)
                },
            ))
    };
    v_flex()
        .size_full()
        .overflow_hidden()
        .rounded(px(5.0))
        .border_1()
        .border_color(colors.border)
        .bg(colors.background)
        .child(
            h_flex()
                .w_full()
                .h(ui_rem(10.0))
                .flex_shrink_0()
                .px(ui_rem(4.0))
                .gap(ui_rem(2.0))
                .bg(colors.title_bar)
                .border_b_1()
                .border_color(colors.border)
                .children((0..3).map(|_| {
                    div()
                        .size(ui_rem(3.0))
                        .rounded_full()
                        .bg(colors.muted_foreground.opacity(0.5))
                })),
        )
        .child(
            v_flex()
                .w_full()
                .p(ui_rem(3.0))
                .gap(ui_rem(2.0))
                .child(row(true, 0.6))
                .child(row(false, 0.75))
                .child(row(false, 0.45)),
        )
}

fn selection_mark(selected: bool, theme: ThemeColor) -> AnyElement {
    div()
        .size(ui_rem(14.0))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .when(selected, |this| {
            this.bg(theme.primary)
                .child(icon("check", 9.0, theme.primary_foreground))
        })
        .when(!selected, |this| this.border_1().border_color(theme.input))
        .into_any_element()
}

fn appearance_mode_card(
    mode: AppearanceMode,
    label: String,
    selected: bool,
    light: ThemePreviewColors,
    dark: ThemePreviewColors,
    theme: ThemeColor,
    on_click: impl Fn(&mut App) + 'static,
) -> AnyElement {
    let preview = match mode {
        AppearanceMode::Light => theme_thumbnail(light).into_any_element(),
        AppearanceMode::Dark => theme_thumbnail(dark).into_any_element(),
        // 跟随系统：浅色窗口的右半边替换为深色窗口的右半边，拼成一个完整窗口。
        AppearanceMode::System => div()
            .size_full()
            .relative()
            .child(theme_thumbnail(light))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .h_full()
                    .w(relative(0.5))
                    .overflow_hidden()
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .h_full()
                            .w(relative(2.0))
                            .child(theme_thumbnail(dark)),
                    ),
            )
            .into_any_element(),
    };
    v_flex()
        .id(SharedString::from(format!(
            "settings-appearance-mode-{}",
            mode.code()
        )))
        .flex_1()
        .min_w_0()
        .gap_2()
        .p_2()
        .rounded(px(8.0))
        .cursor_pointer()
        .border_1()
        .border_color(if selected {
            theme.primary
        } else {
            theme.border
        })
        .bg(if selected {
            theme.primary.opacity(0.06)
        } else {
            theme.background
        })
        .when(!selected, |this| {
            this.hover(|style| style.border_color(theme.muted_foreground.opacity(0.5)))
        })
        .on_click(move |_, _, cx| on_click(cx))
        .child(div().w_full().h(ui_rem(64.0)).child(preview))
        .child(
            h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .justify_center()
                .child(selection_mark(selected, theme))
                .child(
                    div()
                        .text_xs()
                        .text_color(if selected {
                            theme.foreground
                        } else {
                            theme.muted_foreground
                        })
                        .child(label),
                ),
        )
        .into_any_element()
}

/// 某一模式下的主题色板：圆点底色为主题背景色，中心为强调色，悬停显示主题名。
fn theme_swatches(
    themes: &[ThemeOption],
    mode: ThemeMode,
    current: &str,
    external_badge: &str,
    theme: ThemeColor,
    hover_animation: bool,
    on_select: &Rc<dyn Fn(ThemeMode, SharedString, &mut App)>,
) -> AnyElement {
    let options = themes
        .iter()
        .filter(|option| option.mode == mode)
        .collect::<Vec<_>>();
    let selected_label = options
        .iter()
        .find(|option| option.value.as_ref() == current)
        .map(|option| option.label.clone());
    let mode_key = if mode.is_dark() { "dark" } else { "light" };
    let hover_motion = Duration::from_millis(140).with_easing(gpui::ease_in_out);

    h_flex()
        .max_w_full()
        .gap_3()
        .items_center()
        .when_some(selected_label, |this, label| {
            this.child(
                div()
                    .max_w(ui_rem(96.0))
                    .truncate()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(label),
            )
        })
        .child(
            h_flex()
                .flex_wrap()
                .gap(ui_rem(6.0))
                .children(options.into_iter().enumerate().map(|(index, option)| {
                    let selected = option.value.as_ref() == current;
                    let colors = option.preview;
                    let tooltip: SharedString = if option.is_external {
                        format!("{} · {external_badge}", option.label).into()
                    } else {
                        option.label.clone()
                    };
                    let on_select = on_select.clone();
                    let value = option.value.clone();
                    // 色块用主题强调色填充：各主题背景色彼此接近，同色系下会和设置页背景融合，
                    // 强调色才是区分主题的主要特征。选中环取当前界面前景色，浅色界面下为深色环、
                    // 深色界面下为浅色环，与所在行是浅色主题还是深色主题无关。
                    //
                    // 色块撑满槽位、用固定像素的边框与内边距留出间隙，不用 flex 居中：居中会产生
                    // 半像素偏移，GPUI 像素对齐时向零取整，色块会整体偏向左上 1 个设备像素。
                    // 悬停时收窄内边距让色块放大，槽位尺寸不变，不会推挤相邻色块。
                    div()
                        .id(SharedString::from(format!(
                            "settings-theme-swatch-{mode_key}-{index}"
                        )))
                        .size(ui_rem(30.0))
                        .flex_shrink_0()
                        .p(px(3.0))
                        .rounded(ui_rem(9.0))
                        .border_2()
                        .border_color(if selected {
                            theme.foreground
                        } else {
                            gpui::transparent_black()
                        })
                        .cursor_pointer()
                        .when(hover_animation, |this| {
                            this.transitions(|transitions| transitions.p(hover_motion.clone()))
                        })
                        .when(!selected, |this| this.hover(|style| style.p(px(1.0))))
                        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                        .on_click(move |_, _, cx| on_select(mode, value.clone(), cx))
                        .child(div().size_full().rounded(ui_rem(6.0)).bg(colors.primary))
                })),
        )
        .into_any_element()
}

const fn appearance_mode_label_key(mode: AppearanceMode) -> &'static str {
    match mode {
        AppearanceMode::System => "settings.personalization.mode.system",
        AppearanceMode::Light => "settings.personalization.mode.light",
        AppearanceMode::Dark => "settings.personalization.mode.dark",
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

pub(super) const fn list_item_style_label_key(style: ListItemStyle) -> &'static str {
    match style {
        ListItemStyle::Seamless => "settings.personalization.list_item_style.seamless",
        ListItemStyle::Card => "settings.personalization.list_item_style.card",
    }
}

pub(super) const fn list_item_height_label_key(height: ListItemHeight) -> &'static str {
    match height {
        ListItemHeight::Adaptive => "settings.personalization.list_item_height.adaptive",
        ListItemHeight::Large => "settings.personalization.list_item_height.large",
        ListItemHeight::Medium => "settings.personalization.list_item_height.medium",
        ListItemHeight::Small => "settings.personalization.list_item_height.small",
        ListItemHeight::ExtraSmall => "settings.personalization.list_item_height.extra_small",
    }
}

const fn window_material_label_key(material: WindowMaterial) -> &'static str {
    match material {
        WindowMaterial::Disabled => "settings.personalization.window_material.disabled",
        WindowMaterial::Transparent => "settings.personalization.window_material.transparent",
        WindowMaterial::Acrylic => "settings.personalization.window_material.acrylic",
        WindowMaterial::Blur => "settings.personalization.window_material.blur",
        WindowMaterial::CustomImage => "settings.personalization.window_material.custom_image",
    }
}
