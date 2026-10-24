use crate::ui::scale::ui_rem;

use super::super::{SettingsPage, notify_settings_error};
use gpui::prelude::*;
use gpui::{AnyElement, Context};

use super::super::SETTINGS_LINE_HEIGHT_SELECTOR_WIDTH;
use super::super::{
    SettingsWindowView,
    components::{
        clickable_setting_row, compact_clickable_setting_row, compact_setting_row, setting_row,
        settings_dropdown, settings_dropdown_with_font, settings_error_row, settings_page_header,
    },
};
use crate::{
    config::{
        AnimationConfig, AppearanceConfig, AppearanceMode, CardItemGap, ListItemHeight,
        ListItemStyle, PreviewInformationField, TitlebarPosition, WindowMaterial,
    },
    config_runtime, i18n,
    ui::theme::{ThemeOption, available_theme_options},
    ui::{SingleSelectOption, single_select, single_select_grid},
    windows::menu::{DropdownItem, DropdownItemId},
};
use gpui::{App, Entity, SharedString, Window, div, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::slider::Slider;
use gpui_component::switch::Switch;
use gpui_component::{ActiveTheme as _, Sizable as _, ThemeMode, WindowExt as _, h_flex, v_flex};

impl SettingsWindowView {
    pub(in crate::windows::settings) fn render_personalization(
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
        let reset_button = Button::new("settings-personalization-reset")
            .warning()
            .small()
            .label(translator.translate("settings.action.reset_page"))
            .on_click(move |_, _, cx| {
                let result = config_runtime::reset_appearance_config(cx);
                let error_message = result.as_ref().err().map(|error| {
                    tracing::error!("重置个性化设置失败：{error}");
                    reset_error.clone()
                });
                reset_view.update(cx, |view, cx| {
                    view.appearance_error =
                        notify_settings_error(SettingsPage::Personalization, error_message, cx);
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

        let mode_items = AppearanceMode::ALL
            .into_iter()
            .map(|mode| {
                DropdownItem::option(
                    DropdownItemId::new(mode.code()),
                    translator.translate(appearance_mode_label_key(mode)),
                )
                .selected(mode == appearance.mode)
            })
            .collect::<Vec<_>>();
        let save_error = translator.translate("settings.personalization.save_error");
        let mode_selector = settings_dropdown(
            "settings-appearance-mode-select",
            appearance.mode.code(),
            mode_items,
            {
                let view = view.clone();
                let save_error = save_error.clone();
                move |action_id, cx| {
                    let Some(mode) = AppearanceMode::ALL
                        .into_iter()
                        .find(|mode| mode.code() == action_id.as_str())
                    else {
                        return;
                    };
                    persist_appearance_change(
                        view.clone(),
                        "外观模式",
                        move |appearance| appearance.mode = mode,
                        save_error.clone(),
                        cx,
                    );
                }
            },
            cx,
        );

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

        // ---- 收藏轮盘外观面板：跟随材质开关 + 三个颜色（色块按钮 + 输入框） ----
        let wheel_follow_switch = Switch::new("settings-wheel-follow-material")
            .large()
            .checked(appearance.wheel_appearance.follow_window_material)
            .accessibility_label(
                translator.translate("settings.personalization.wheel_appearance.follow"),
            )
            .on_click({
                let view = view.clone();
                let save_error = save_error.clone();
                move |checked, _, cx| {
                    persist_appearance_change(
                        view.clone(),
                        "轮盘跟随窗口材质",
                        move |appearance| {
                            appearance.wheel_appearance.follow_window_material = *checked
                        },
                        save_error.clone(),
                        cx,
                    );
                }
            });
        let wheel_follow_title =
            translator.translate("settings.personalization.wheel_appearance.follow");
        let _wheel_follow_description =
            translator.translate("settings.personalization.wheel_appearance.follow_description");
        // 色块按钮 + 十六进制输入；色块点击弹出取色器，选中即保存。
        let make_wheel_color_row = {
            let view = view.clone();
            let save_error = save_error.clone();
            let pick_color_tooltip: gpui::SharedString = translator
                .translate("settings.personalization.wheel_appearance.pick_color")
                .into();
            let theme_border = theme.border;
            move |id: &'static str,
                  title: String,
                  description: String,
                  input: gpui::Entity<gpui_component::input::InputState>,
                  current_hex: &str,
                  apply: fn(&mut crate::config::WheelAppearanceConfig) -> &mut String,
                  field_name: &'static str|
                  -> gpui::AnyElement {
                wheel_color_row(
                    id,
                    title,
                    description,
                    input,
                    current_hex,
                    apply,
                    field_name,
                    view.clone(),
                    save_error.clone(),
                    pick_color_tooltip.clone(),
                    theme_border,
                )
            }
        };
        let theme_options = available_theme_options(cx);
        let light_selector = settings_dropdown(
            "settings-light-theme-select",
            appearance.light_theme.clone(),
            theme_options_for_mode(&theme_options, ThemeMode::Light, &appearance.light_theme),
            {
                let view = view.clone();
                let save_error = save_error.clone();
                move |action_id, cx| {
                    let value = action_id.as_str().to_owned();
                    persist_appearance_change(
                        view.clone(),
                        "浅色主题",
                        move |appearance| appearance.light_theme = value,
                        save_error.clone(),
                        cx,
                    );
                }
            },
            cx,
        );
        let dark_selector = settings_dropdown(
            "settings-dark-theme-select",
            appearance.dark_theme.clone(),
            theme_options_for_mode(&theme_options, ThemeMode::Dark, &appearance.dark_theme),
            {
                let view = view.clone();
                let save_error = save_error.clone();
                move |action_id, cx| {
                    let value = action_id.as_str().to_owned();
                    persist_appearance_change(
                        view.clone(),
                        "深色主题",
                        move |appearance| appearance.dark_theme = value,
                        save_error.clone(),
                        cx,
                    );
                }
            },
            cx,
        );

        let list_style_selector = single_select(
            "settings-list-item-style-select",
            ListItemStyle::ALL
                .into_iter()
                .map(|style| {
                    SingleSelectOption::new(
                        style,
                        translator.translate(list_item_style_label_key(style)),
                    )
                })
                .collect::<Vec<_>>(),
            appearance.list_item_style,
            {
                let view = view.clone();
                let save_error = save_error.clone();
                move |style, cx| {
                    persist_appearance_change(
                        view.clone(),
                        "列表项风格",
                        move |appearance| appearance.list_item_style = style,
                        save_error.clone(),
                        cx,
                    );
                }
            },
            theme,
            single_select_animation,
        );
        let card_gap_selector = single_select_grid(
            "settings-card-item-gap-select",
            CardItemGap::ALL
                .into_iter()
                .map(|gap| SingleSelectOption::new(gap, gap.code()))
                .collect::<Vec<_>>(),
            appearance.card_item_gap,
            3,
            {
                let view = view.clone();
                let save_error = save_error.clone();
                move |gap, cx| {
                    persist_appearance_change(
                        view.clone(),
                        "卡片间距",
                        move |appearance| appearance.card_item_gap = gap,
                        save_error.clone(),
                        cx,
                    );
                }
            },
            theme,
            single_select_animation,
        );
        let list_item_height_selector = single_select(
            "settings-list-item-height-select",
            ListItemHeight::ALL.into_iter().map(|height| {
                SingleSelectOption::new(
                    height,
                    translator.translate(list_item_height_label_key(height)),
                )
            }),
            appearance.list_item_height,
            {
                let view = view.clone();
                let save_error = save_error.clone();
                move |height, cx| {
                    persist_appearance_change(
                        view.clone(),
                        "列表项行高",
                        move |appearance| appearance.list_item_height = height,
                        save_error.clone(),
                        cx,
                    );
                }
            },
            theme,
            single_select_animation,
        );
        let adaptive_max_lines_selector = h_flex()
            .w(ui_rem(SETTINGS_LINE_HEIGHT_SELECTOR_WIDTH))
            .gap_3()
            .child(Slider::new(&self.adaptive_max_lines_slider).flex_1())
            .child(
                div()
                    .w(ui_rem(36.0))
                    .text_sm()
                    .text_center()
                    .text_color(theme.foreground)
                    .child(appearance.adaptive_max_lines.to_string()),
            );
        let only_show_notes_switch = Switch::new("settings-only-show-notes")
            .checked(appearance.only_show_notes)
            .accessibility_label(translator.translate("settings.personalization.only_show_notes"))
            .on_click({
                let view = view.clone();
                let save_error = save_error.clone();
                move |checked, _, cx| {
                    persist_appearance_change(
                        view.clone(),
                        "仅显示备注",
                        move |appearance| appearance.only_show_notes = *checked,
                        save_error.clone(),
                        cx,
                    )
                }
            });

        let gallery_gif_autoplay_switch = Switch::new("settings-gallery-gif-autoplay")
            .checked(appearance.gallery_gif_autoplay)
            .accessibility_label(
                translator.translate("settings.personalization.gallery_gif_autoplay"),
            )
            .on_click({
                let view = view.clone();
                let save_error = save_error.clone();
                move |checked, _, cx| {
                    persist_appearance_change(
                        view.clone(),
                        "图库 GIF 自动播放",
                        move |appearance| appearance.gallery_gif_autoplay = *checked,
                        save_error.clone(),
                        cx,
                    )
                }
            });

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
                translator.translate("settings.page.personalization"),
                translator.translate("settings.personalization.description"),
                reset_button,
            ))
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.personalization.appearance_mode"),
                    Some(
                        translator
                            .translate("settings.personalization.appearance_mode_description")
                            .into(),
                    ),
                    mode_selector,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.personalization.light_theme"),
                    Some(
                        translator
                            .translate("settings.personalization.light_theme_description")
                            .into(),
                    ),
                    light_selector,
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
                    dark_selector,
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
            .child(self.highlight_element(
                theme,
                translator.translate("settings.item_display.title"),
                compact_clickable_setting_row(
                    "settings-item-display",
                    theme,
                    translator.translate("settings.item_display.title"),
                    {
                        let view = view.clone();
                        move |_, window, cx| {
                            super::item_display_editor::open_item_display_sheet(
                                view.clone(),
                                window,
                                cx,
                            )
                        }
                    },
                ),
            ))
            .child(self.highlight_element(
                theme,
                translator.translate("settings.personalization.preview_information"),
                compact_clickable_setting_row(
                    "settings-preview-information",
                    theme,
                    translator.translate("settings.personalization.preview_information"),
                    {
                        let view = view.clone();
                        move |_, window, cx| {
                            open_preview_information_sheet(view.clone(), window, cx)
                        }
                    },
                ),
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
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.personalization.gallery_gif_autoplay"),
                    Some(
                        translator
                            .translate("settings.personalization.gallery_gif_autoplay_description")
                            .into(),
                    ),
                    gallery_gif_autoplay_switch,
                ),
            )
            .child(
                div()
                    .w_full()
                    .border_b_1()
                    .border_color(theme.border)
                    .px_4()
                    .py_3()
                    .child(window_material_panel),
            )
            .child(
                div()
                    .w_full()
                    .border_b_1()
                    .border_color(theme.border)
                    .px_4()
                    .py_3()
                    .child(
                        v_flex()
                            .w_full()
                            .gap_3()
                            .child(
                                self.highlight_element(
                                    theme,
                                    wheel_follow_title.clone(),
                                    h_flex()
                                        .w_full()
                                        .items_center()
                                        .justify_between()
                                        .gap_3()
                                        .child(
                                            div().text_sm().text_color(theme.foreground).child(
                                                wheel_follow_title,
                                            ),
                                        )
                                        .child(wheel_follow_switch),
                                ),
                            )
                            // 颜色行始终显示：跟随材质时同样允许覆盖颜色，无需先关开关。
                            .child(make_wheel_color_row(
                                "wheel-sector-color",
                                translator.translate(
                                    "settings.personalization.wheel_appearance.sector_color",
                                ),
                                translator.translate(
                                    "settings.personalization.wheel_appearance.sector_color_description",
                                ),
                                self.wheel_sector_color_input.clone(),
                                appearance.wheel_appearance.sector_color.as_str(),
                                |wheel| &mut wheel.sector_color,
                                "轮盘扇区颜色",
                            ))
                            .child(make_wheel_color_row(
                                "wheel-hover-color",
                                translator.translate(
                                    "settings.personalization.wheel_appearance.hover_color",
                                ),
                                translator.translate(
                                    "settings.personalization.wheel_appearance.hover_color_description",
                                ),
                                self.wheel_hover_color_input.clone(),
                                appearance.wheel_appearance.hover_color.as_str(),
                                |wheel| &mut wheel.hover_color,
                                "轮盘悬停颜色",
                            ))
                            .child(make_wheel_color_row(
                                "wheel-center-color",
                                translator.translate(
                                    "settings.personalization.wheel_appearance.center_color",
                                ),
                                translator.translate(
                                    "settings.personalization.wheel_appearance.center_color_description",
                                ),
                                self.wheel_center_color_input.clone(),
                                appearance.wheel_appearance.center_color.as_str(),
                                |wheel| &mut wheel.center_color,
                                "轮盘中心颜色",
                            ))
                            .child(make_wheel_color_row(
                                "wheel-title-color",
                                translator.translate(
                                    "settings.personalization.wheel_appearance.title_color",
                                ),
                                translator.translate(
                                    "settings.personalization.wheel_appearance.title_color_description",
                                ),
                                self.wheel_title_color_input.clone(),
                                appearance.wheel_appearance.title_color.as_str(),
                                |wheel| &mut wheel.title_color,
                                "轮盘标题颜色",
                            ))
                            .child(make_wheel_color_row(
                                "wheel-preview-color",
                                translator.translate(
                                    "settings.personalization.wheel_appearance.preview_color",
                                ),
                                translator.translate(
                                    "settings.personalization.wheel_appearance.preview_color_description",
                                ),
                                self.wheel_preview_color_input.clone(),
                                appearance.wheel_appearance.preview_color.as_str(),
                                |wheel| &mut wheel.preview_color,
                                "轮盘预览颜色",
                            ))
                            .child(make_wheel_color_row(
                                "wheel-page-color",
                                translator.translate(
                                    "settings.personalization.wheel_appearance.page_color",
                                ),
                                translator.translate(
                                    "settings.personalization.wheel_appearance.page_color_description",
                                ),
                                self.wheel_page_color_input.clone(),
                                appearance.wheel_appearance.page_color.as_str(),
                                |wheel| &mut wheel.page_color,
                                "轮盘页码颜色",
                            ))
                            // 轮盘字体：套用「应用字体」同款下拉控件。
                            .child({
                                let current_wheel_font = appearance
                                    .wheel_appearance
                                    .font_family
                                    .clone()
                                    .unwrap_or_default();
                                let mut wheel_font_items = vec![DropdownItem::option(
                                    DropdownItemId::new(""),
                                    translator.translate(
                                        "settings.personalization.wheel_appearance.font_default",
                                    ),
                                )
                                .selected(current_wheel_font.is_empty())];
                                wheel_font_items.extend(
                                    crate::ui::fonts::system_font_names(cx).iter().map(|family| {
                                        DropdownItem::option(
                                            DropdownItemId::new(family.clone()),
                                            family.clone(),
                                        )
                                        .selected(*family == current_wheel_font)
                                    }),
                                );
                                let font_row = settings_dropdown_with_font(
                                    "settings-wheel-font-select",
                                    current_wheel_font,
                                    wheel_font_items,
                                    Some(".SystemUIFont".into()),
                                    {
                                        let view = view.clone();
                                        let save_error = save_error.clone();
                                        move |id, cx| {
                                            let family =
                                                (!id.as_str().is_empty()).then(|| id.as_str().to_owned());
                                            persist_appearance_change(
                                                view.clone(),
                                                "轮盘字体",
                                                move |appearance| {
                                                    appearance.wheel_appearance.font_family = family
                                                },
                                                save_error.clone(),
                                                cx,
                                            );
                                        }
                                    },
                                    cx,
                                );
                                h_flex().w_full().gap_3().items_center()
                                    .child(
                                        div()
                                            .w(ui_rem(96.0))
                                            .flex_shrink_0()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(
                                                translator.translate(
                                                    "settings.personalization.wheel_appearance.font_family",
                                                ),
                                            ),
                                    )
                                    .child(div().flex_1().min_w_0().child(font_row))
                            })
                            // 字号滑条：标题 / 预览。
                            .child(
                                h_flex().w_full().gap_3().items_center()
                                    .child(
                                        div()
                                            .w(ui_rem(96.0))
                                            .flex_shrink_0()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(translator.translate(
                                                "settings.personalization.wheel_appearance.title_font_size",
                                            )),
                                    )
                                    .child(Slider::new(&self.wheel_title_size_slider).flex_1())
                                    .child(
                                        div()
                                            .w(ui_rem(40.0))
                                            .flex_shrink_0()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(format!(
                                                "{}px",
                                                appearance.wheel_appearance.title_font_size.round() as i32
                                            )),
                                    ),
                            )
                            .child(
                                h_flex().w_full().gap_3().items_center()
                                    .child(
                                        div()
                                            .w(ui_rem(96.0))
                                            .flex_shrink_0()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(translator.translate(
                                                "settings.personalization.wheel_appearance.preview_font_size",
                                            )),
                                    )
                                    .child(Slider::new(&self.wheel_preview_size_slider).flex_1())
                                    .child(
                                        div()
                                            .w(ui_rem(40.0))
                                            .flex_shrink_0()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(format!(
                                                "{}px",
                                                appearance.wheel_appearance.preview_font_size.round() as i32
                                            )),
                                    ),
                            ),
                    ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.personalization.list_item_style"),
                    Some(
                        translator
                            .translate("settings.personalization.list_item_style_description")
                            .into(),
                    ),
                    list_style_selector,
                ),
            );
        section = section.child(
            self.setting_row(
                theme,
                translator.translate("settings.personalization.list_item_height"),
                Some(
                    translator
                        .translate("settings.personalization.list_item_height_description")
                        .into(),
                ),
                list_item_height_selector,
            ),
        );
        if appearance.list_item_height == ListItemHeight::Adaptive {
            section = section.child(
                self.setting_row(
                    theme,
                    translator.translate("settings.personalization.adaptive_max_lines"),
                    Some(
                        translator
                            .translate("settings.personalization.adaptive_max_lines_description")
                            .into(),
                    ),
                    adaptive_max_lines_selector,
                ),
            );
        }
        section = section.child(
            self.setting_row(
                theme,
                translator.translate("settings.personalization.only_show_notes"),
                Some(
                    translator
                        .translate("settings.personalization.only_show_notes_description")
                        .into(),
                ),
                only_show_notes_switch,
            ),
        );
        if appearance.list_item_style == ListItemStyle::Card {
            section = section.child(
                self.setting_row(
                    theme,
                    translator.translate("settings.personalization.card_item_gap"),
                    Some(
                        translator
                            .translate("settings.personalization.card_item_gap_description")
                            .into(),
                    ),
                    card_gap_selector,
                ),
            );
        }
        if let Some(error) = &self.appearance_error {
            section = section.child(settings_error_row(theme, error));
        }
        section.into_any_element()
    }
}

fn open_preview_information_sheet(
    view: Entity<SettingsWindowView>,
    window: &mut Window,
    cx: &mut App,
) {
    let save_error = i18n::translator(cx).translate("settings.personalization.save_error");
    window.open_sheet(cx, move |sheet, _, cx| {
        let theme = cx.theme().colors;
        let translator = i18n::translator(cx);
        let information = config_runtime::preview_information_config(cx);
        let appearance = config_runtime::appearance_config(cx);
        let background = gpui::ColorExt::opacity(
            &crate::window_material::excluded_window_theme(cx).background,
            0.9,
        );
        let mut controls = h_flex().w_full().flex_shrink_0().gap_2().pb_2();
        for (enabled, action) in [(true, "enable_all"), (false, "disable_all")] {
            controls = controls.child(
                Button::new(format!("settings-preview-information-{action}"))
                    .small()
                    .label(translator.translate(&format!(
                        "settings.personalization.preview_information.{action}"
                    )))
                    .on_click({
                        let view = view.clone();
                        let save_error = save_error.clone();
                        move |_, _, cx| {
                            persist_appearance_change(
                                view.clone(),
                                "预览信息展示",
                                move |appearance| {
                                    appearance.preview_information.fields = if enabled {
                                        PreviewInformationField::ALL.to_vec()
                                    } else {
                                        Vec::new()
                                    };
                                },
                                save_error.clone(),
                                cx,
                            );
                        }
                    }),
            );
        }
        let mut content = v_flex()
            .size_full()
            .min_h_0()
            .overflow_y_scrollbar()
            .child(controls);
        for field in PreviewInformationField::ALL {
            let label = translator.translate(field.label_key());
            let switch = Switch::new(format!("settings-preview-information-{}", field.key()))
                .checked(information.enabled(field))
                .accessibility_label(label.clone())
                .on_click({
                    let view = view.clone();
                    let save_error = save_error.clone();
                    move |checked, _, cx| {
                        persist_appearance_change(
                            view.clone(),
                            "预览信息展示",
                            move |appearance| {
                                appearance.preview_information.set_enabled(field, *checked)
                            },
                            save_error.clone(),
                            cx,
                        );
                    }
                });
            content = content.child(compact_setting_row(theme, label, switch));
        }
        sheet
            .title(translator.translate("settings.personalization.preview_information"))
            .size(ui_rem(420.0))
            .resizable(false)
            .shadow_none()
            .bg(background)
            .when(
                appearance.window_material != WindowMaterial::Disabled,
                |this| this.backdrop_blur(px(18.0)),
            )
            .child(content)
    });
}

fn open_animations_sheet(view: Entity<SettingsWindowView>, window: &mut Window, cx: &mut App) {
    let save_error = i18n::translator(cx).translate("settings.personalization.save_error");
    window.open_sheet(cx, move |sheet, _, cx| {
        let theme = cx.theme().colors;
        let sheet_background = gpui::ColorExt::opacity(
            &crate::window_material::excluded_window_theme(cx).background,
            0.9,
        );
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
            .bg(sheet_background)
            .when(
                appearance.window_material != WindowMaterial::Disabled,
                |this| this.backdrop_blur(px(18.0)),
            )
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
        view.appearance_error =
            notify_settings_error(SettingsPage::Personalization, error_message, cx);
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
        view.appearance_error =
            notify_settings_error(SettingsPage::Personalization, error_message, cx);
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
                view.appearance_error =
                    notify_settings_error(SettingsPage::Personalization, error_message, cx);
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
        SettingsPage::Personalization,
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

fn theme_options_for_mode(
    themes: &[ThemeOption],
    mode: ThemeMode,
    current: &str,
) -> Vec<DropdownItem> {
    let mut items = themes
        .iter()
        .filter(|theme| theme.mode == mode)
        .map(|theme| {
            DropdownItem::option(
                DropdownItemId::new(theme.value.clone()),
                theme.label.clone(),
            )
            .selected(theme.value.as_ref() == current)
        })
        .collect::<Vec<_>>();
    if !items.iter().any(|item| item.id.as_str() == current) {
        items.push(
            DropdownItem::option(DropdownItemId::new(current.to_owned()), current.to_owned())
                .selected(true),
        );
    }
    items
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

const fn list_item_style_label_key(style: ListItemStyle) -> &'static str {
    match style {
        ListItemStyle::Seamless => "settings.personalization.list_item_style.seamless",
        ListItemStyle::Card => "settings.personalization.list_item_style.card",
    }
}

const fn list_item_height_label_key(height: ListItemHeight) -> &'static str {
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

/// 读取颜色输入框当前值作为取色器初始色（非法/为空回退中性灰）。
fn current_color_u32(
    input: &gpui::Entity<gpui_component::input::InputState>,
    cx: &gpui::App,
) -> u32 {
    let binding = input.read(cx).value();
    let raw = binding.trim().trim_start_matches('#');
    let parsed = raw
        .get(0..6)
        .and_then(|hex| u32::from_str_radix(hex, 16).ok());
    parsed.unwrap_or(0x808088)
}

/// 轮盘颜色行：标签 + 十六进制输入框 + 色块按钮（弹出取色器，选中即保存）。
#[allow(clippy::too_many_arguments)]
fn wheel_color_row(
    id: &'static str,
    title: String,
    description: String,
    input: gpui::Entity<gpui_component::input::InputState>,
    current_hex: &str,
    apply: fn(&mut crate::config::WheelAppearanceConfig) -> &mut String,
    field_name: &'static str,
    view: Entity<SettingsWindowView>,
    save_error: String,
    pick_color_tooltip: gpui::SharedString,
    border_color: gpui::Hsla,
) -> gpui::AnyElement {
    let label = title.clone();
    let description: gpui::SharedString = description.into();
    let hex_digits: String = current_hex
        .trim()
        .trim_start_matches('#')
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .take(6)
        .collect();
    let sample_color = if hex_digits.len() == 6 {
        u32::from_str_radix(&hex_digits, 16)
            .map(|rgb| gpui::rgba((rgb << 8) | 0xFF))
            .unwrap_or_else(|_| gpui::rgba(0x808088FF))
    } else {
        gpui::rgba(0x808088FF)
    };
    let picker_view = view.clone();
    let picker_input = input.clone();
    let picker_save_error = save_error.clone();
    let picker = div()
        .id(SharedString::from(format!("{id}-picker")))
        .w(ui_rem(22.0))
        .h(ui_rem(22.0))
        .flex_shrink_0()
        .rounded(px(4.0))
        .border_1()
        .border_color(border_color)
        .bg(sample_color)
        .cursor_pointer()
        .tooltip(move |window, cx| {
            gpui_component::tooltip::Tooltip::new(pick_color_tooltip.clone()).build(window, cx)
        })
        .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
            let initial = current_color_u32(&picker_input, cx);
            let input_for_done = picker_input.clone();
            let view_done = picker_view.clone();
            let done_save_error = picker_save_error.clone();
            let result = crate::windows::color_picker::open_color_picker_window(
                Some(window),
                initial,
                move |color, cx| {
                    let hex = format!("{color:06x}");
                    let _ = input_for_done;
                    persist_appearance_change(
                        view_done.clone(),
                        field_name,
                        move |appearance| *apply(&mut appearance.wheel_appearance) = hex.clone(),
                        done_save_error.clone(),
                        cx,
                    );
                },
                cx,
            );
            if let Err(error) = result {
                tracing::error!("打开轮盘颜色选择器失败：{error}");
            }
        });
    h_flex()
        .w_full()
        .gap_3()
        .items_center()
        .child(
            div()
                .id(SharedString::from(format!("{id}-label")))
                .w(ui_rem(96.0))
                .flex_shrink_0()
                .text_xs()
                .child(label)
                .tooltip(move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(description.clone()).build(window, cx)
                }),
        )
        .child(
            div()
                .w(ui_rem(120.0))
                .flex_shrink_0()
                .child(gpui_component::input::Input::new(&input).small().w_full()),
        )
        .child(picker)
        .into_any_element()
}
