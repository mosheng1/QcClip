use crate::ui::scale::ui_rem;

use super::super::{SettingsPage, notify_settings_error};
use gpui::prelude::*;
use gpui::{AnyElement, Context};

use super::super::SETTINGS_SOUND_SLIDER_WIDTH;
use super::super::{
    SettingsWindowView,
    components::{
        settings_dropdown, settings_error_row, settings_page_header, settings_section_title,
    },
};
use crate::{
    config::{SoundConfig, SoundPreset},
    config_runtime, i18n,
    services::sound,
    windows::menu::{DropdownItem, DropdownItemId},
};
use gpui::{App, Entity, SharedString, div, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::slider::{Slider, SliderState};
use gpui_component::switch::Switch;
use gpui_component::{ActiveTheme as _, Disableable as _, IconNamed, Sizable as _, h_flex, v_flex};

impl SettingsWindowView {
    pub(in crate::windows::settings) fn render_notifications(
        &self,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().colors;
        let translator = i18n::translator(cx);
        let sound_config = config_runtime::sound_config(cx);
        let view = cx.entity().clone();
        let reset_error = translator.translate("settings.notifications.reset_error");
        let reset_view = view.clone();
        let reset_button = Button::new("settings-notifications-reset")
            .warning()
            .small()
            .label(translator.translate("settings.action.reset_page"))
            .on_click(move |_, _, cx| {
                let result = config_runtime::reset_notification_config(cx)
                    .and_then(|_| config_runtime::reset_sound_config(cx));
                let error_message = result.as_ref().err().map(|error| {
                    tracing::error!("重置通知与音效设置失败：{error}");
                    reset_error.clone()
                });
                reset_view.update(cx, |view, cx| {
                    view.notification_error =
                        notify_settings_error(SettingsPage::Notifications, error_message, cx);
                    cx.notify();
                });
            });
        let save_error = translator.translate("settings.sound.save_error");
        let enabled_switch = Switch::new("settings-sound-enabled")
            .large()
            .checked(sound_config.enabled)
            .accessibility_label(translator.translate("settings.sound.enabled"))
            .on_click({
                let view = view.clone();
                let save_error = save_error.clone();
                move |checked, _, cx| {
                    let enabled = *checked;
                    persist_sound_change(
                        view.clone(),
                        move |sound| sound.enabled = enabled,
                        save_error.clone(),
                        cx,
                    );
                }
            });

        let external_presets = sound::external_presets();
        let paste_items = sound_preset_items(
            translator.clone(),
            sound_config.paste.preset.clone(),
            &external_presets,
            false,
        );
        let paste_selector = settings_dropdown(
            "settings-sound-paste-preset-select",
            sound_config.paste.preset.selection_id(),
            paste_items,
            {
                let view = view.clone();
                let save_error = save_error.clone();
                move |action_id, cx| {
                    let Some(preset) = SoundPreset::from_selection_id(action_id.as_str()) else {
                        return;
                    };
                    persist_sound_change(
                        view.clone(),
                        move |sound| sound.paste.preset = preset,
                        save_error.clone(),
                        cx,
                    );
                }
            },
            cx,
        );
        let copy_items = sound_preset_items(
            translator.clone(),
            sound_config.copy.preset.clone(),
            &external_presets,
            false,
        );
        let copy_selector = settings_dropdown(
            "settings-sound-copy-preset-select",
            sound_config.copy.preset.selection_id(),
            copy_items,
            {
                let view = view.clone();
                let save_error = save_error.clone();
                move |action_id, cx| {
                    let Some(preset) = SoundPreset::from_selection_id(action_id.as_str()) else {
                        return;
                    };
                    persist_sound_change(
                        view.clone(),
                        move |sound| sound.copy.preset = preset,
                        save_error.clone(),
                        cx,
                    );
                }
            },
            cx,
        );
        let quick_paste_scroll_items = sound_preset_items(
            translator.clone(),
            sound_config.quick_paste_scroll.preset.clone(),
            &external_presets,
            true,
        );
        let quick_paste_scroll_selector = settings_dropdown(
            "settings-sound-quick-paste-scroll-preset-select",
            sound_config.quick_paste_scroll.preset.selection_id(),
            quick_paste_scroll_items,
            {
                let view = view.clone();
                let save_error = save_error.clone();
                move |action_id, cx| {
                    let Some(preset) = SoundPreset::from_selection_id(action_id.as_str()) else {
                        return;
                    };
                    persist_sound_change(
                        view.clone(),
                        move |sound| sound.quick_paste_scroll.preset = preset,
                        save_error.clone(),
                        cx,
                    );
                }
            },
            cx,
        );
        let paste_preview = sound_preview_button(
            "settings-sound-paste-preview",
            sound_config.paste.preset.clone(),
            sound_config.paste.volume,
            sound_config.enabled,
            translator.translate("settings.sound.preview"),
        );
        let global_paste_checkbox = Checkbox::new("settings-sound-global-paste")
            .checked(sound_config.global_paste)
            .accessibility_label(translator.translate("settings.sound.global_paste"))
            .tooltip(translator.translate("settings.sound.global_paste_tooltip"))
            .on_click({
                let view = view.clone();
                let save_error = save_error.clone();
                move |checked, _, cx| {
                    let global_paste = *checked;
                    persist_sound_change(
                        view.clone(),
                        move |sound| sound.global_paste = global_paste,
                        save_error.clone(),
                        cx,
                    );
                }
            });
        let copy_preview = sound_preview_button(
            "settings-sound-copy-preview",
            sound_config.copy.preset.clone(),
            sound_config.copy.volume,
            sound_config.enabled,
            translator.translate("settings.sound.preview"),
        );
        let quick_paste_scroll_preview = sound_preview_button(
            "settings-sound-quick-paste-scroll-preview",
            sound_config.quick_paste_scroll.preset.clone(),
            sound_config.quick_paste_scroll.volume,
            sound_config.enabled,
            translator.translate("settings.sound.preview"),
        );
        let paste_volume =
            sound_volume_control(&self.paste_volume_slider, sound_config.paste.volume, theme);
        let copy_volume =
            sound_volume_control(&self.copy_volume_slider, sound_config.copy.volume, theme);
        let quick_paste_scroll_volume = sound_volume_control(
            &self.quick_paste_scroll_volume_slider,
            sound_config.quick_paste_scroll.volume,
            theme,
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
                translator.translate("settings.page.notifications"),
                translator.translate("settings.notifications.description"),
                reset_button,
            ))
            .child(settings_section_title(
                theme,
                translator.translate("settings.section.notifications"),
            ))
            .children(self.render_notification_rows(cx))
            .child(settings_section_title(
                theme,
                translator.translate("settings.section.sound"),
            ))
            .child(self.setting_row(
                theme,
                translator.translate("settings.sound.enabled"),
                None,
                enabled_switch,
            ))
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.sound.copy_effect"),
                    Some(
                        translator
                            .translate("settings.sound.effect_description")
                            .into(),
                    ),
                    h_flex().gap_2().child(copy_selector).child(copy_preview),
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.sound.copy_volume"),
                    Some(
                        translator
                            .translate("settings.sound.volume_description")
                            .into(),
                    ),
                    copy_volume,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.sound.paste_effect"),
                    Some(
                        translator
                            .translate("settings.sound.effect_description")
                            .into(),
                    ),
                    h_flex()
                        .gap_2()
                        .child(paste_selector)
                        .child(paste_preview)
                        .child(global_paste_checkbox),
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.sound.paste_volume"),
                    Some(
                        translator
                            .translate("settings.sound.volume_description")
                            .into(),
                    ),
                    paste_volume,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.sound.quick_paste_scroll_effect"),
                    Some(
                        translator
                            .translate("settings.sound.quick_paste_scroll_effect_description")
                            .into(),
                    ),
                    h_flex()
                        .gap_2()
                        .child(quick_paste_scroll_selector)
                        .child(quick_paste_scroll_preview),
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.sound.quick_paste_scroll_volume"),
                    Some(
                        translator
                            .translate("settings.sound.volume_description")
                            .into(),
                    ),
                    quick_paste_scroll_volume,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.sound.external.title"),
                    Some(
                        translator
                            .translate("settings.sound.external.description")
                            .into(),
                    ),
                    Button::new("settings-sound-open-folder")
                        .secondary()
                        .small()
                        .label(translator.translate("settings.sound.external.open_folder"))
                        .on_click(|_, _, _| sound::open_sound_directory()),
                ),
            );
        if let Some(error) = &self.notification_error {
            section = section.child(settings_error_row(theme, error));
        }
        section.into_any_element()
    }
}

#[derive(Clone, Copy)]
enum SoundPreviewIcon {
    Play,
}

impl IconNamed for SoundPreviewIcon {
    fn path(self) -> SharedString {
        match self {
            Self::Play => "assets/icons/player-play.svg",
        }
        .into()
    }
}

pub(in crate::windows::settings) fn sound_preset_items(
    translator: i18n::Translator,
    selected: SoundPreset,
    external_presets: &[SoundPreset],
    include_quick_paste_scroll: bool,
) -> Vec<DropdownItem> {
    SoundPreset::ALL
        .into_iter()
        .chain(include_quick_paste_scroll.then_some(SoundPreset::QuickPasteScroll))
        .chain(external_presets.iter().cloned())
        .map(|preset| {
            let label = match &preset {
                SoundPreset::External(_) => preset.display_name(),
                _ => translator.translate(sound_preset_label_key(&preset)),
            };
            DropdownItem::option(DropdownItemId::new(preset.selection_id()), label)
                .selected(preset == selected)
        })
        .chain(
            (selected.external_file_name().is_some() && !external_presets.contains(&selected))
                .then(|| {
                    DropdownItem::option(
                        DropdownItemId::new(selected.selection_id()),
                        selected.display_name(),
                    )
                    .selected(true)
                }),
        )
        .collect()
}

pub(in crate::windows::settings) fn sound_preview_button(
    id: &'static str,
    preset: SoundPreset,
    volume: u8,
    enabled: bool,
    tooltip: String,
) -> AnyElement {
    let preset_for_click = preset.clone();
    Button::new(id)
        .ghost()
        .small()
        .tab_stop(false)
        .icon(SoundPreviewIcon::Play)
        .tooltip(tooltip)
        .disabled(!enabled || preset == SoundPreset::Disabled || volume == 0)
        .on_click(move |_, _, _| sound::preview(preset_for_click.clone(), volume))
        .into_any_element()
}

pub(in crate::windows::settings) fn sound_volume_control(
    slider: &Entity<SliderState>,
    volume: u8,
    theme: gpui_component::ThemeColor,
) -> AnyElement {
    h_flex()
        .w(ui_rem(SETTINGS_SOUND_SLIDER_WIDTH))
        .gap_3()
        .child(Slider::new(slider).flex_1())
        .child(
            div()
                .w(ui_rem(38.0))
                .text_sm()
                .text_center()
                .text_color(theme.foreground)
                .child(format!("{volume}%")),
        )
        .into_any_element()
}

pub(in crate::windows::settings) fn persist_sound_change<F>(
    view: Entity<SettingsWindowView>,
    update: F,
    save_error: String,
    cx: &mut App,
) where
    F: FnOnce(&mut SoundConfig),
{
    let mut sound_config = config_runtime::sound_config(cx);
    update(&mut sound_config);
    let error_message = match config_runtime::set_sound_config(sound_config, cx) {
        Ok(_) => None,
        Err(error) => {
            tracing::error!("保存音效设置失败：{error}");
            Some(save_error)
        }
    };
    view.update(cx, |view, cx| {
        view.notification_error =
            notify_settings_error(SettingsPage::Notifications, error_message, cx);
        cx.notify();
    });
}

pub(in crate::windows::settings) fn persist_sound_change_in_view<F>(
    view: &mut SettingsWindowView,
    field_name: &'static str,
    update: F,
    save_error: String,
    cx: &mut Context<SettingsWindowView>,
) where
    F: FnOnce(&mut SoundConfig),
{
    let mut sound_config = config_runtime::sound_config(cx);
    update(&mut sound_config);
    view.notification_error = notify_settings_error(
        SettingsPage::Notifications,
        match config_runtime::set_sound_config(sound_config, cx) {
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

fn sound_preset_label_key(preset: &SoundPreset) -> &'static str {
    match preset {
        SoundPreset::Disabled => "settings.sound.preset.disabled",
        SoundPreset::CrispChime => "settings.sound.preset.preset_2",
        SoundPreset::BrightPing => "settings.sound.preset.preset_1",
        SoundPreset::QuickPasteScroll => "settings.sound.preset.preset_3",
        SoundPreset::External(_) => "settings.sound.preset.external",
    }
}
