use crate::ui::scale::ui_rem;

use super::super::{
    SettingsPage, notify_settings_error, notify_settings_status, open_settings_directory,
};
use crate::windows::notification::NotificationLevel;
use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, FontWeight, Hsla, div, img, px, relative};
use gpui_component::ActiveTheme as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::NumberInput;
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::switch::Switch;
use gpui_component::{
    Colorize as _, Disableable as _, Selectable as _, Sizable as _, h_flex, tooltip::Tooltip,
    v_flex,
};

use super::super::{DataStatsScope, SettingsWindowView, components, parse_optional_limit};
use crate::{
    config::HistoryConfig,
    config_runtime,
    services::{
        data_management::{self, DataManagementService, PreparedDataLocation},
        database::HistoryRepository,
        image_preview::ImagePreviewService,
    },
    ui::icon::icon,
    windows::menu::{DropdownItem, DropdownItemId},
};

impl SettingsWindowView {
    pub(in crate::windows::settings) fn render_data_management(
        &self,
        cx: &mut Context<SettingsWindowView>,
    ) -> AnyElement {
        let theme = cx.theme().colors;
        let translator = crate::i18n::translator(cx);
        let config = config_runtime::history_config(cx);
        let view = cx.entity().clone();
        let (data_root, default_data_root, using_default_data_root, can_change_data_location) =
            cx.read_global::<DataManagementService, _>(|service, _| {
                (
                    service.data_root().to_path_buf(),
                    service.default_data_root().to_path_buf(),
                    service.is_default_data_root(),
                    service.can_change_data_location(),
                )
            });
        let retention_items = [
            (
                "none",
                translator.translate("settings.data_management.unlimited"),
                config.retention_days.is_none(),
            ),
            (
                "7",
                translator.translate_with_args(
                    "settings.data_management.recent_days",
                    &[("count", "7".to_owned())],
                ),
                config.retention_days == Some(7),
            ),
            (
                "30",
                translator.translate_with_args(
                    "settings.data_management.recent_days",
                    &[("count", "30".to_owned())],
                ),
                config.retention_days == Some(30),
            ),
            (
                "90",
                translator.translate_with_args(
                    "settings.data_management.recent_days",
                    &[("count", "90".to_owned())],
                ),
                config.retention_days == Some(90),
            ),
            (
                "365",
                translator.translate_with_args(
                    "settings.data_management.recent_days",
                    &[("count", "365".to_owned())],
                ),
                config.retention_days == Some(365),
            ),
        ]
        .into_iter()
        .map(|(id, label, selected)| {
            DropdownItem::option(DropdownItemId::new(id), label).selected(selected)
        })
        .collect();
        let retention_view = view.clone();
        let retention_preset = components::settings_preset_dropdown(
            "settings-history-retention-preset",
            retention_items,
            move |id, cx| {
                let mut next = config_runtime::history_config(cx);
                let Some(value) = parse_optional_limit(id.as_str()) else {
                    return;
                };
                next.retention_days = value;
                save_history_config(retention_view.clone(), next, cx);
            },
            cx,
        );
        let retention_control = h_flex()
            .h(ui_rem(34.0))
            .w(ui_rem(160.0))
            .overflow_hidden()
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.input)
            .bg(theme.background)
            .child(
                NumberInput::new(&self.history_retention_input)
                    .appearance(false)
                    .controls_right()
                    .disabled(self.data_busy)
                    .placeholder(translator.translate("settings.data_management.unlimited"))
                    .small()
                    .h_full()
                    .flex_1(),
            )
            .child(retention_preset);

        let max_items_items = [
            (
                "none",
                translator.translate("settings.data_management.unlimited"),
                config.max_items.is_none(),
            ),
            (
                "100",
                translator.translate_with_args(
                    "settings.data_management.item_count",
                    &[("count", "100".to_owned())],
                ),
                config.max_items == Some(100),
            ),
            (
                "500",
                translator.translate_with_args(
                    "settings.data_management.item_count",
                    &[("count", "500".to_owned())],
                ),
                config.max_items == Some(500),
            ),
            (
                "1000",
                translator.translate_with_args(
                    "settings.data_management.item_count",
                    &[("count", "1,000".to_owned())],
                ),
                config.max_items == Some(1000),
            ),
            (
                "5000",
                translator.translate_with_args(
                    "settings.data_management.item_count",
                    &[("count", "5,000".to_owned())],
                ),
                config.max_items == Some(5000),
            ),
        ]
        .into_iter()
        .map(|(id, label, selected)| {
            DropdownItem::option(DropdownItemId::new(id), label).selected(selected)
        })
        .collect();
        let max_items_view = view.clone();
        let max_items_preset = components::settings_preset_dropdown(
            "settings-history-max-items-preset",
            max_items_items,
            move |id, cx| {
                let mut next = config_runtime::history_config(cx);
                let Some(value) = parse_optional_limit(id.as_str()) else {
                    return;
                };
                next.max_items = value;
                save_history_config(max_items_view.clone(), next, cx);
            },
            cx,
        );
        let max_items_control = h_flex()
            .h(ui_rem(34.0))
            .w(ui_rem(160.0))
            .overflow_hidden()
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.input)
            .bg(theme.background)
            .child(
                NumberInput::new(&self.history_max_items_input)
                    .appearance(false)
                    .controls_right()
                    .disabled(self.data_busy)
                    .placeholder(translator.translate("settings.data_management.unlimited"))
                    .small()
                    .h_full()
                    .flex_1(),
            )
            .child(max_items_preset);
        let reset_view = view.clone();
        let reset_translator = translator.clone();
        let reset_button = Button::new("settings-data-reset")
            .warning()
            .small()
            .disabled(self.data_busy)
            .label(translator.translate("settings.data_management.reset"))
            .on_click(move |_, _, cx| {
                let result = config_runtime::reset_history_config(cx);
                if let Err(error) = &result {
                    tracing::error!("恢复历史设置失败：{error}");
                }
                reset_view.update(cx, |view, cx| {
                    view.data_error = notify_settings_error(
                        SettingsPage::DataManagement,
                        result.err().map(|_| {
                            reset_translator
                                .translate("settings.data_management.history_reset_error")
                        }),
                        cx,
                    );
                    view.data_message = None;
                    cx.notify();
                });
            });
        let cleanup_view = view.clone();
        let cleanup_button = Button::new("settings-data-cleanup")
            .primary()
            .small()
            .disabled(self.data_busy)
            .label(translator.translate("settings.data_management.cleanup"))
            .on_click(move |_, _, cx| start_cleanup(cleanup_view.clone(), cx));
        let export_view = view.clone();
        let export_button = Button::new("settings-data-export")
            .secondary()
            .small()
            .disabled(self.data_busy)
            .label(translator.translate("settings.data_management.export"))
            .on_click(move |_, _, cx| start_export(export_view.clone(), cx));
        let import_view = view.clone();
        let import_button = Button::new("settings-data-import")
            .secondary()
            .small()
            .disabled(self.data_busy)
            .label(translator.translate("settings.data_management.import"))
            .on_click(move |_, _, cx| start_import(import_view.clone(), cx));
        let clear_history_view = view.clone();
        let clear_history_button = Button::new("settings-data-clear-all-history")
            .danger()
            .small()
            .disabled(self.data_busy)
            .label(translator.translate("settings.data_management.clear_all_history"))
            .on_click(move |_, window, cx| {
                confirm_destructive_action(
                    clear_history_view.clone(),
                    DataDestructiveAction::ClearHistory,
                    window,
                    cx,
                );
            });
        let reset_application_view = view.clone();
        let reset_application_button = Button::new("settings-data-reset-application")
            .danger()
            .small()
            .disabled(self.data_busy)
            .label(translator.translate("settings.data_management.reset_application"))
            .on_click(move |_, window, cx| {
                confirm_destructive_action(
                    reset_application_view.clone(),
                    DataDestructiveAction::ResetApplication,
                    window,
                    cx,
                );
            });
        let open_data_button = Button::new("settings-data-open-directory")
            .secondary()
            .small()
            .label(translator.translate("settings.data_management.open_directory"))
            .on_click(|_, _, cx| {
                let data_root = cx.read_global::<DataManagementService, _>(|service, _| {
                    service.data_root().to_path_buf()
                });
                open_settings_directory(data_root, "settings.data_management.open_directory", cx);
            });
        let change_location_view = view.clone();
        let change_location_button = Button::new("settings-data-change-location")
            .secondary()
            .small()
            .disabled(self.data_busy || !can_change_data_location)
            .label(translator.translate("settings.data_management.change_location"))
            .on_click(move |_, _, cx| start_choose_data_location(change_location_view.clone(), cx));
        let restore_location_view = view.clone();
        let restore_location_button = Button::new("settings-data-restore-location")
            .secondary()
            .small()
            .disabled(self.data_busy || using_default_data_root || !can_change_data_location)
            .label(translator.translate("settings.data_management.restore_default_location"))
            .on_click(move |_, _, cx| {
                start_data_location_switch(
                    restore_location_view.clone(),
                    default_data_root.clone(),
                    cx,
                )
            });
        let data_root_text = data_root.to_string_lossy().into_owned();
        let data_location_tooltip = data_root_text.clone();
        let data_location_control = v_flex()
            .w(ui_rem(390.0))
            .max_w_full()
            .gap_2()
            .child(
                div()
                    .id("settings-data-location-path")
                    .w_full()
                    .h(ui_rem(34.0))
                    .min_w_0()
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(theme.input)
                    .bg(theme.background)
                    .px_3()
                    .text_sm()
                    .text_color(theme.foreground)
                    .tooltip(move |window, cx| {
                        Tooltip::new(data_location_tooltip.clone()).build(window, cx)
                    })
                    .child(div().min_w_0().w_full().truncate().child(data_root_text)),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(change_location_button.flex_1())
                    .child(restore_location_button.flex_1())
                    .child(open_data_button.flex_1()),
            );

        let stats_actions = self.highlight_element(
            theme,
            translator.translate("settings.data_management.actions_title"),
            h_flex()
                .w_full()
                .items_center()
                .gap_2()
                .border_b_1()
                .border_color(theme.border)
                .px_4()
                .py_3()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.foreground)
                                .child(
                                    translator.translate("settings.data_management.actions_title"),
                                ),
                        )
                        .child(div().text_xs().text_color(theme.muted_foreground).child(
                            translator.translate("settings.data_management.actions_description"),
                        )),
                )
                .child(
                    v_flex()
                        .w(ui_rem(390.0))
                        .max_w_full()
                        .flex_shrink_0()
                        .gap_2()
                        .child(
                            h_flex()
                                .w_full()
                                .gap_2()
                                .child(cleanup_button.flex_grow(2.0))
                                .child(export_button.flex_1())
                                .child(import_button.flex_1()),
                        )
                        .child(
                            h_flex()
                                .w_full()
                                .gap_2()
                                .child(clear_history_button.flex_1())
                                .child(reset_application_button.flex_1()),
                        ),
                ),
        );
        let stats_dashboard = self.highlight_element(
            theme,
            translator.translate("settings.data_management.stats_title"),
            render_statistics_dashboard(
                theme,
                &translator,
                self.data_stats.as_ref(),
                self.data_stats_loading,
                config.max_items,
                self.data_stats_scope,
                view.clone(),
                {
                    let refresh_view = view.clone();
                    let refresh_label =
                        translator.translate("settings.data_management.refresh_stats");
                    Button::new("settings-data-stats-refresh")
                        .ghost()
                        .small()
                        .icon(crate::ui::icon::icon(
                            "refresh",
                            14.0,
                            theme.muted_foreground,
                        ))
                        .accessibility_label(refresh_label.clone())
                        .tooltip(refresh_label)
                        .disabled(self.data_stats_loading)
                        .on_click(move |_, _, cx| {
                            let _ = refresh_view.update(cx, |view, cx| {
                                view.refresh_data_stats(cx);
                                cx.notify();
                            });
                        })
                },
            ),
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
            .child(components::settings_page_header(
                theme,
                translator.translate("settings.page.data_management"),
                translator.translate("settings.data_management.description"),
                reset_button,
            ))
            .child(self.render_legacy_migration(cx))
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.data_management.data_location"),
                    Some(
                        translator
                            .translate(if can_change_data_location {
                                "settings.data_management.data_location_description"
                            } else {
                                "settings.data_management.portable_data_location_description"
                            })
                            .into(),
                    ),
                    data_location_control,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.data_management.retention"),
                    Some(
                        translator
                            .translate("settings.data_management.retention_description")
                            .into(),
                    ),
                    retention_control,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.data_management.max_items"),
                    Some(
                        translator
                            .translate("settings.data_management.max_items_description")
                            .into(),
                    ),
                    max_items_control,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.data_management.cleanup_on_startup"),
                    Some(
                        translator
                            .translate("settings.data_management.cleanup_on_startup_description")
                            .into(),
                    ),
                    Switch::new("settings-data-cleanup-on-startup")
                        .large()
                        .checked(config.cleanup_on_startup)
                        .disabled(self.data_busy)
                        .accessibility_label(
                            translator.translate("settings.data_management.cleanup_on_startup"),
                        )
                        .on_click(move |checked, _, cx| {
                            let mut next = config_runtime::history_config(cx);
                            next.cleanup_on_startup = *checked;
                            save_history_config(view.clone(), next, cx);
                        }),
                ),
            )
            .child(stats_actions)
            .child(stats_dashboard);
        if self.data_busy {
            let busy_message = translator.translate("settings.data_management.busy");
            section = section.child(components::settings_error_row(theme, &busy_message));
        }
        if let Some(message) = &self.data_message {
            section = section.child(
                div()
                    .border_t_1()
                    .border_color(theme.border)
                    .px_4()
                    .py_2()
                    .text_sm()
                    .text_color(theme.success)
                    .child(message.clone()),
            );
        }
        if let Some(error) = &self.data_error {
            section = section.child(components::settings_error_row(theme, error));
        }
        section.into_any_element()
    }
}

fn render_statistics_dashboard(
    theme: gpui_component::ThemeColor,
    translator: &crate::i18n::Translator,
    stats: Option<&data_management::DataManagementStats>,
    loading: bool,
    max_items: Option<u32>,
    content_type_scope: DataStatsScope,
    view: Entity<SettingsWindowView>,
    refresh_button: impl IntoElement,
) -> AnyElement {
    let favorites = stats.map(|stats| stats.favorite_count);
    let tags = stats.map(|stats| stats.tag_count);
    let status = if loading {
        translator.translate("settings.data_management.stats_loading")
    } else if stats.is_some() {
        translator.translate("settings.data_management.stats_ready")
    } else {
        translator.translate("settings.data_management.stats_unavailable")
    };
    let status_color = if loading {
        theme.info
    } else if stats.is_some() {
        theme.success
    } else {
        theme.muted_foreground
    };

    let retention_panel = render_retention_panel(theme, translator, stats, loading, max_items);
    let storage_panel = render_storage_panel(theme, translator, stats, loading);
    let content_type_panel = render_content_type_distribution(
        theme,
        translator,
        stats,
        loading,
        content_type_scope,
        view,
    );
    let source_app_panel = render_source_distribution(theme, translator, stats, loading);

    v_flex()
        .w_full()
        .gap_3()
        .border_b_1()
        .border_color(theme.border)
        .px_4()
        .py_4()
        .child(
            h_flex()
                .w_full()
                .items_center()
                .gap_2()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(
                            div()
                                .text_lg()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.foreground)
                                .child(
                                    translator.translate("settings.data_management.stats_title"),
                                ),
                        )
                        .child(div().text_xs().text_color(theme.muted_foreground).child(
                            translator.translate("settings.data_management.stats_description"),
                        )),
                )
                .child(
                    div()
                        .rounded(px(999.0))
                        .bg(status_color.opacity(0.12))
                        .px_2()
                        .py(ui_rem(3.0))
                        .text_xs()
                        .text_color(status_color)
                        .child(status),
                )
                .child(refresh_button),
        )
        .child(
            div()
                .w_full()
                .grid()
                .grid_cols(3)
                .gap_3()
                .items_stretch()
                .child(render_history_summary_panel(
                    theme, translator, stats, loading,
                ))
                .child(render_relation_summary(
                    theme, translator, favorites, tags, loading,
                )),
        )
        .child(render_two_column_panels(retention_panel, storage_panel))
        .child(render_two_column_panels(
            content_type_panel,
            source_app_panel,
        ))
        .into_any_element()
}

fn render_two_column_panels(left: AnyElement, right: AnyElement) -> AnyElement {
    div()
        .w_full()
        .grid()
        .grid_cols(2)
        .gap_3()
        .items_stretch()
        .child(left)
        .child(right)
        .into_any_element()
}

fn render_history_summary_panel(
    theme: gpui_component::ThemeColor,
    translator: &crate::i18n::Translator,
    stats: Option<&data_management::DataManagementStats>,
    loading: bool,
) -> AnyElement {
    let value = |value: Option<u64>| {
        value.map(format_number).unwrap_or_else(|| {
            if loading {
                "…".to_owned()
            } else {
                "—".to_owned()
            }
        })
    };
    let total = stats.map(|stats| stats.history_count);
    let pinned = stats.map(|stats| stats.pinned_history_count);
    let ordinary = stats.map(|stats| stats.retention_limit_used_count);
    let pending = stats.map(|stats| stats.pending_cleanup_count);
    let ratio = |count: Option<u64>| match (count, total) {
        (Some(count), Some(total)) if total > 0 => count as f32 / total as f32,
        _ => 0.0,
    };
    let mut distribution_bar = h_flex()
        .h(ui_rem(8.0))
        .w_full()
        .overflow_hidden()
        .rounded(px(4.0))
        .bg(theme.muted);
    for (segment_ratio, color) in [
        (ratio(ordinary), theme.success),
        (ratio(pinned), theme.info),
        (ratio(pending), theme.warning),
    ] {
        if segment_ratio > 0.0 {
            distribution_bar = distribution_bar.child(
                div()
                    .flex_none()
                    .h_full()
                    .bg(color)
                    .w(relative(segment_ratio)),
            );
        }
    }

    v_flex()
        .id("settings-stats-history-summary")
        .w_full()
        .min_w_0()
        .col_span(2)
        .h(ui_rem(116.0))
        .gap_2()
        .rounded(px(10.0))
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
        .p_3()
        .child(
            h_flex()
                .items_start()
                .justify_between()
                .gap_2()
                .child(
                    h_flex()
                        .min_w_0()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_none()
                                .size(ui_rem(30.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(8.0))
                                .bg(theme.primary.opacity(0.14))
                                .child(icon("clipboard-list", 16.0, theme.primary)),
                        )
                        .child(
                            v_flex()
                                .min_w_0()
                                .gap_1()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme.foreground)
                                        .child(
                                            translator
                                                .translate("settings.data_management.stat_history"),
                                        ),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(translator.translate(
                                            "settings.data_management.stat_history_caption",
                                        )),
                                ),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child(value(total)),
                ),
        )
        .child(distribution_bar)
        .child(
            h_flex()
                .w_full()
                .gap_3()
                .items_start()
                .child(history_summary_item(
                    theme.success,
                    translator.translate("settings.data_management.stat_in_scope"),
                    value(ordinary),
                    theme,
                ))
                .child(history_summary_item(
                    theme.info,
                    translator.translate("settings.data_management.stat_pinned"),
                    value(pinned),
                    theme,
                ))
                .child(history_summary_item(
                    theme.warning,
                    translator.translate("settings.data_management.stat_pending"),
                    value(pending),
                    theme,
                )),
        )
        .into_any_element()
}

fn render_relation_summary(
    theme: gpui_component::ThemeColor,
    translator: &crate::i18n::Translator,
    favorites: Option<u64>,
    tags: Option<u64>,
    loading: bool,
) -> AnyElement {
    let value = |value: Option<u64>| {
        value.map(format_number).unwrap_or_else(|| {
            if loading {
                "…".to_owned()
            } else {
                "—".to_owned()
            }
        })
    };

    v_flex()
        .w_full()
        .h(ui_rem(116.0))
        .rounded(px(10.0))
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
        .overflow_hidden()
        .child(relation_stat_row(
            "settings-stats-favorites",
            "star-filled",
            theme.info,
            translator.translate("settings.data_management.stat_favorites"),
            value(favorites),
            theme,
            true,
        ))
        .child(relation_stat_row(
            "settings-stats-tags",
            "tags",
            theme.primary,
            translator.translate("settings.data_management.stat_tags"),
            value(tags),
            theme,
            false,
        ))
        .into_any_element()
}

fn relation_stat_row(
    id: &'static str,
    icon_name: &'static str,
    accent: Hsla,
    label: String,
    value: String,
    theme: gpui_component::ThemeColor,
    with_bottom_border: bool,
) -> AnyElement {
    let mut row = h_flex()
        .id(id)
        .flex_1()
        .w_full()
        .items_center()
        .gap_2()
        .px_3()
        .child(
            div()
                .flex_none()
                .size(ui_rem(28.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(7.0))
                .bg(accent.opacity(0.14))
                .child(icon(icon_name, 15.0, accent)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(label),
        )
        .child(
            div()
                .flex_none()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.foreground)
                .child(value),
        );
    if with_bottom_border {
        row = row.border_b_1().border_color(theme.border);
    }
    row.into_any_element()
}

fn history_summary_item(
    accent: Hsla,
    label: String,
    value: String,
    theme: gpui_component::ThemeColor,
) -> AnyElement {
    h_flex()
        .flex_1()
        .min_w_0()
        .items_center()
        .gap_2()
        .child(
            h_flex()
                .min_w_0()
                .items_center()
                .gap_1()
                .child(div().size(ui_rem(6.0)).rounded(px(3.0)).bg(accent))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(label),
                ),
        )
        .child(
            div()
                .flex_none()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(accent)
                .child(value),
        )
        .into_any_element()
}

fn render_retention_panel(
    theme: gpui_component::ThemeColor,
    translator: &crate::i18n::Translator,
    stats: Option<&data_management::DataManagementStats>,
    loading: bool,
    max_items: Option<u32>,
) -> AnyElement {
    let (limit_used, pending) = stats
        .map(|stats| {
            (
                stats.retention_limit_used_count,
                stats.pending_cleanup_count,
            )
        })
        .unwrap_or_default();
    let ratio = match (stats, max_items) {
        (Some(_), Some(limit)) if limit > 0 => (limit_used as f32 / limit as f32).clamp(0.0, 1.0),
        (Some(_), Some(_)) => 1.0,
        _ => 0.0,
    };
    let ratio_text = if stats.is_none() {
        if loading {
            "…".to_owned()
        } else {
            "—".to_owned()
        }
    } else if let Some(limit) = max_items {
        if limit_used >= u64::from(limit) {
            "100%".to_owned()
        } else {
            let percentage = (limit_used as f64 / f64::from(limit) * 1000.0).round() / 10.0;
            format!("{:.1}%", percentage.min(99.9))
        }
    } else {
        translator.translate("settings.data_management.retention_unlimited_short")
    };
    let used_text = if stats.is_some() {
        if let Some(limit) = max_items {
            translator.translate_with_args(
                "settings.data_management.retention_used",
                &[
                    ("used", format_number(limit_used)),
                    ("limit", format_number(u64::from(limit))),
                ],
            )
        } else {
            translator.translate_with_args(
                "settings.data_management.retention_used_unlimited",
                &[("used", format_number(limit_used))],
            )
        }
    } else {
        translator.translate("settings.data_management.retention_used_unavailable")
    };
    let pending_text = if stats.is_some() {
        translator.translate_with_args(
            "settings.data_management.retention_pending",
            &[("count", format_number(pending))],
        )
    } else {
        translator.translate("settings.data_management.retention_pending_empty")
    };
    let capacity_message = if stats.is_none() {
        translator.translate("settings.data_management.retention_summary_unavailable")
    } else if let Some(limit) = max_items {
        if limit_used >= u64::from(limit) {
            translator.translate("settings.data_management.retention_full")
        } else {
            translator.translate_with_args(
                "settings.data_management.retention_remaining",
                &[(
                    "count",
                    format_number(u64::from(limit).saturating_sub(limit_used)),
                )],
            )
        }
    } else {
        translator.translate("settings.data_management.retention_no_limit")
    };
    let summary_text = if let Some(stats) = stats {
        let (key, count) = if max_items.is_some() {
            (
                "settings.data_management.retention_summary_limit",
                stats.over_limit_history_count,
            )
        } else {
            (
                "settings.data_management.retention_summary_range",
                stats.pending_cleanup_count,
            )
        };
        translator.translate_with_args(
            key,
            &[
                ("total", format_number(stats.history_count)),
                ("count", format_number(count)),
            ],
        )
    } else {
        translator.translate("settings.data_management.retention_summary_unavailable")
    };

    v_flex()
        .w_full()
        .h_full()
        .min_w_0()
        .gap_2()
        .rounded(px(10.0))
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
        .p_3()
        .child(
            h_flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.foreground)
                        .child(translator.translate("settings.data_management.retention_title")),
                )
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.success)
                        .child(ratio_text),
                ),
        )
        .child(
            div()
                .h(ui_rem(8.0))
                .w_full()
                .rounded(px(4.0))
                .bg(theme.muted)
                .child(
                    div()
                        .h_full()
                        .rounded(px(4.0))
                        .bg(theme.success)
                        .w(relative(ratio)),
                ),
        )
        .child(
            h_flex()
                .items_center()
                .justify_between()
                .text_xs()
                .child(
                    div()
                        .min_w_0()
                        .text_color(theme.muted_foreground)
                        .child(used_text),
                )
                .child(
                    div()
                        .flex_none()
                        .text_color(if pending > 0 {
                            theme.warning
                        } else {
                            theme.success
                        })
                        .child(pending_text),
                ),
        )
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(
                    if max_items.is_some_and(|limit| limit_used >= u64::from(limit)) {
                        theme.success
                    } else {
                        theme.muted_foreground
                    },
                )
                .child(capacity_message),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(summary_text),
        )
        .into_any_element()
}

fn render_storage_panel(
    theme: gpui_component::ThemeColor,
    translator: &crate::i18n::Translator,
    stats: Option<&data_management::DataManagementStats>,
    loading: bool,
) -> AnyElement {
    let total = stats.map(|stats| stats.total_bytes).unwrap_or(0);
    let total_text = stats
        .map(|stats| format_bytes(stats.total_bytes))
        .unwrap_or_else(|| {
            if loading {
                "…".to_owned()
            } else {
                "—".to_owned()
            }
        });
    let categories = stats.map(|stats| {
        [
            (
                "settings.data_management.storage_database",
                stats.database_bytes,
                theme.primary,
            ),
            (
                "settings.data_management.storage_raw_data",
                stats.raw_data_bytes,
                theme.info,
            ),
            (
                "settings.data_management.storage_images",
                stats.image_library_bytes,
                theme.success,
            ),
            (
                "settings.data_management.storage_sound",
                stats.sound_bytes,
                theme.warning,
            ),
            (
                "settings.data_management.storage_other",
                stats.other_bytes,
                theme.muted_foreground,
            ),
        ]
    });

    let mut panel = v_flex()
        .w_full()
        .min_w_0()
        .gap_2()
        .rounded(px(10.0))
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
        .p_3()
        .child(
            h_flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.foreground)
                        .child(translator.translate("settings.data_management.storage_title")),
                )
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child(total_text),
                ),
        );
    if let Some(categories) = categories {
        for (label_key, bytes, color) in categories {
            panel = panel.child(storage_row(
                translator.translate(label_key),
                bytes,
                total,
                color,
                theme,
            ));
        }
    } else {
        panel = panel.child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(translator.translate("settings.data_management.storage_empty")),
        );
    }
    panel.into_any_element()
}

fn render_content_type_distribution(
    theme: gpui_component::ThemeColor,
    translator: &crate::i18n::Translator,
    stats: Option<&data_management::DataManagementStats>,
    loading: bool,
    scope: DataStatsScope,
    view: Entity<SettingsWindowView>,
) -> AnyElement {
    let counts = stats.map(|stats| match scope {
        DataStatsScope::History => stats.content_type_counts,
        DataStatsScope::Favorites => stats.favorite_content_type_counts,
    });
    let max_count = counts
        .map(|counts| counts.iter().copied().max().unwrap_or(0))
        .unwrap_or(0);
    let items = [
        ("settings.data_management.content_type_text", theme.primary),
        ("settings.data_management.content_type_image", theme.info),
        ("settings.data_management.content_type_file", theme.success),
        ("settings.data_management.content_type_link", theme.warning),
        (
            "settings.data_management.content_type_other",
            theme.muted_foreground,
        ),
    ];
    let mut rows = v_flex().w_full().gap_1();
    for (index, (label_key, color)) in items.into_iter().enumerate() {
        rows = rows.child(distribution_row(
            translator.translate(label_key),
            counts.map(|counts| counts[index]),
            max_count,
            loading,
            color,
            theme,
        ));
    }
    render_distribution_panel(
        theme,
        translator.translate("settings.data_management.content_type_distribution"),
        translator.translate("settings.data_management.content_type_distribution_description"),
        rows.into_any_element(),
        false,
        Some(render_content_type_scope_switcher(
            theme, translator, scope, view,
        )),
    )
}

fn render_content_type_scope_switcher(
    theme: gpui_component::ThemeColor,
    translator: &crate::i18n::Translator,
    scope: DataStatsScope,
    view: Entity<SettingsWindowView>,
) -> AnyElement {
    let history_label = translator.translate("settings.data_management.content_type_scope_history");
    let history_view = view.clone();
    let history_button = Button::new("settings-data-content-type-history")
        .ghost()
        .small()
        .icon(icon(
            "clipboard-text",
            14.0,
            if scope == DataStatsScope::History {
                theme.primary
            } else {
                theme.muted_foreground
            },
        ))
        .selected(scope == DataStatsScope::History)
        .accessibility_label(history_label.clone())
        .tooltip(history_label)
        .on_click(move |_, _, cx| {
            history_view.update(cx, |view, cx| {
                if view.data_stats_scope != DataStatsScope::History {
                    view.data_stats_scope = DataStatsScope::History;
                    cx.notify();
                }
            });
        });
    let favorites_label =
        translator.translate("settings.data_management.content_type_scope_favorites");
    let favorites_view = view;
    let favorites_button = Button::new("settings-data-content-type-favorites")
        .ghost()
        .small()
        .icon(icon(
            "star-filled",
            14.0,
            if scope == DataStatsScope::Favorites {
                theme.primary
            } else {
                theme.muted_foreground
            },
        ))
        .selected(scope == DataStatsScope::Favorites)
        .accessibility_label(favorites_label.clone())
        .tooltip(favorites_label)
        .on_click(move |_, _, cx| {
            favorites_view.update(cx, |view, cx| {
                if view.data_stats_scope != DataStatsScope::Favorites {
                    view.data_stats_scope = DataStatsScope::Favorites;
                    cx.notify();
                }
            });
        });
    h_flex()
        .flex_none()
        .items_center()
        .gap_1()
        .child(history_button)
        .child(favorites_button)
        .into_any_element()
}

fn render_source_distribution(
    theme: gpui_component::ThemeColor,
    translator: &crate::i18n::Translator,
    stats: Option<&data_management::DataManagementStats>,
    loading: bool,
) -> AnyElement {
    let source_counts = stats.map(|stats| stats.source_app_counts.clone());
    let max_count = source_counts
        .as_ref()
        .and_then(|counts| counts.first().map(|source| source.count))
        .unwrap_or(0);
    let mut rows = v_flex().w_full().gap_1();
    if let Some(source_counts) = source_counts {
        if source_counts.is_empty() {
            rows =
                rows.child(div().text_xs().text_color(theme.muted_foreground).child(
                    translator.translate("settings.data_management.source_distribution_empty"),
                ));
        } else {
            for (index, source) in source_counts.into_iter().enumerate() {
                let label = if source.name.is_empty() {
                    translator.translate("settings.data_management.source_distribution_unknown")
                } else {
                    source.name
                };
                rows = rows.child(source_distribution_row(
                    index,
                    label,
                    source.count,
                    source.icon_path,
                    max_count,
                    theme,
                ));
            }
        }
    } else if loading {
        rows = rows.child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("…"),
        );
    } else {
        rows = rows.child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(translator.translate("settings.data_management.source_distribution_empty")),
        );
    }
    let rows = rows
        .h(ui_rem(132.0))
        .min_h_0()
        .pr(ui_rem(16.0))
        .overflow_y_scrollbar();
    render_distribution_panel(
        theme,
        translator.translate("settings.data_management.source_distribution"),
        translator.translate("settings.data_management.source_distribution_description"),
        rows.into_any_element(),
        true,
        None,
    )
}

fn source_distribution_row(
    index: usize,
    label: String,
    value: u64,
    icon_path: Option<std::path::PathBuf>,
    max_count: u64,
    theme: gpui_component::ThemeColor,
) -> AnyElement {
    let tooltip = label.clone();
    let app_icon = icon_path
        .map(|path| {
            img(path)
                .size(ui_rem(16.0))
                .object_fit(gpui::ObjectFit::Contain)
                .into_any_element()
        })
        .unwrap_or_else(|| icon("app-window", 15.0, theme.muted_foreground).into_any_element());
    let ratio = if max_count == 0 {
        0.0
    } else {
        value as f32 / max_count as f32
    };
    h_flex()
        .h(ui_rem(22.0))
        .w_full()
        .items_center()
        .gap_2()
        .child(
            div()
                .id(("settings-source-icon", index))
                .size(ui_rem(22.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .child(app_icon),
        )
        .child(
            div()
                .h(ui_rem(6.0))
                .flex_1()
                .rounded(px(3.0))
                .bg(theme.muted)
                .child(
                    div()
                        .h_full()
                        .rounded(px(3.0))
                        .bg(theme.info)
                        .w(relative(ratio)),
                ),
        )
        .child(
            div()
                .w(ui_rem(52.0))
                .flex_none()
                .text_right()
                .text_xs()
                .text_color(theme.foreground)
                .child(format_number(value)),
        )
        .into_any_element()
}

fn render_distribution_panel(
    theme: gpui_component::ThemeColor,
    title: String,
    description: String,
    body: AnyElement,
    flush_right: bool,
    header_action: Option<AnyElement>,
) -> AnyElement {
    let mut header = h_flex()
        .w_full()
        .items_center()
        .gap_2()
        .child(
            div()
                .flex_none()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.foreground)
                .child(title),
        )
        .child(
            div()
                .min_w_0()
                .flex_1()
                .truncate()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(description),
        );
    if let Some(header_action) = header_action {
        header = header.child(header_action);
    }
    if flush_right {
        header = header.pr_3();
    }
    let mut panel = v_flex()
        .w_full()
        .h(ui_rem(184.0))
        .min_w_0()
        .min_h_0()
        .gap_2()
        .rounded(px(10.0))
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
        .p_3()
        .child(header)
        .child(body);
    if flush_right {
        panel = panel.pr_0();
    }
    panel.into_any_element()
}

fn distribution_row(
    label: String,
    value: Option<u64>,
    max_count: u64,
    loading: bool,
    color: Hsla,
    theme: gpui_component::ThemeColor,
) -> AnyElement {
    let value_text = value.map(format_number).unwrap_or_else(|| {
        if loading {
            "…".to_owned()
        } else {
            "—".to_owned()
        }
    });
    let ratio = match (value, max_count) {
        (Some(value), max_count) if max_count > 0 => value as f32 / max_count as f32,
        _ => 0.0,
    };
    h_flex()
        .h(ui_rem(22.0))
        .w_full()
        .items_center()
        .gap_2()
        .child(
            div()
                .w(ui_rem(40.0))
                .flex_none()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(label),
        )
        .child(
            div()
                .h(ui_rem(6.0))
                .flex_1()
                .rounded(px(3.0))
                .bg(theme.muted)
                .child(div().h_full().rounded(px(3.0)).bg(color).w(relative(ratio))),
        )
        .child(
            div()
                .w(ui_rem(52.0))
                .flex_none()
                .text_right()
                .text_xs()
                .text_color(theme.foreground)
                .child(value_text),
        )
        .into_any_element()
}

fn storage_row(
    label: String,
    bytes: u64,
    total: u64,
    color: Hsla,
    theme: gpui_component::ThemeColor,
) -> AnyElement {
    let ratio = if total == 0 {
        0.0
    } else {
        (bytes as f32 / total as f32).clamp(0.0, 1.0)
    };
    h_flex()
        .w_full()
        .items_center()
        .gap_2()
        .child(
            div()
                .w(ui_rem(58.0))
                .flex_none()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(label),
        )
        .child(
            div()
                .h(ui_rem(6.0))
                .flex_1()
                .rounded(px(3.0))
                .bg(theme.muted)
                .child(div().h_full().rounded(px(3.0)).bg(color).w(relative(ratio))),
        )
        .child(
            div()
                .w(ui_rem(62.0))
                .flex_none()
                .text_right()
                .text_xs()
                .text_color(theme.foreground)
                .child(format_bytes(bytes)),
        )
        .into_any_element()
}

fn format_number(value: u64) -> String {
    let value = value.to_string();
    let mut grouped = String::with_capacity(value.len() + value.len() / 3);
    for (index, character) in value.chars().enumerate() {
        if index > 0 && (value.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(character);
    }
    grouped
}

fn save_history_config(
    view: gpui::Entity<SettingsWindowView>,
    config: HistoryConfig,
    cx: &mut App,
) {
    if view.read(cx).data_busy {
        return;
    }
    let translator = crate::i18n::translator(cx);
    let result = config_runtime::set_history_config(config, cx);
    if let Err(error) = &result {
        tracing::error!("保存历史设置失败：{error}");
    }
    view.update(cx, |view, cx| {
        view.data_error = notify_settings_error(
            SettingsPage::DataManagement,
            result
                .err()
                .map(|_| translator.translate("settings.data_management.history_save_error")),
            cx,
        );
        view.data_message = None;
        cx.notify();
    });
}

fn start_choose_data_location(view: gpui::Entity<SettingsWindowView>, cx: &mut App) {
    if view.read(cx).data_busy
        || !cx.read_global::<DataManagementService, _>(|service, _| {
            service.can_change_data_location()
        })
    {
        return;
    }
    let translator = crate::i18n::translator(cx);
    let dialog = rfd::AsyncFileDialog::new()
        .set_title(translator.translate("settings.data_management.change_location_title"));
    cx.spawn(async move |cx| {
        let Some(folder) = dialog.pick_folder().await else {
            view.update(cx, |view, cx| {
                view.data_message = notify_settings_status(
                    SettingsPage::DataManagement,
                    Some(
                        crate::i18n::translator(cx).translate("settings.data_management.cancelled"),
                    ),
                    NotificationLevel::Info,
                    cx,
                );
                cx.notify();
            });
            return;
        };
        let target = folder.path().to_path_buf();
        cx.update(|cx| start_data_location_switch(view, target, cx));
    })
    .detach();
}

fn start_data_location_switch(
    view: gpui::Entity<SettingsWindowView>,
    target: std::path::PathBuf,
    cx: &mut App,
) {
    start_data_replacement(view, target, false, cx);
}

#[derive(Clone, Copy)]
enum DataDestructiveAction {
    ClearHistory,
    ResetApplication,
}

fn confirm_destructive_action(
    view: Entity<SettingsWindowView>,
    action: DataDestructiveAction,
    window: &mut gpui::Window,
    cx: &mut App,
) {
    if view.read(cx).data_busy {
        return;
    }
    let translator = crate::i18n::translator(cx);
    let (title_key, detail_key, confirm_key) = match action {
        DataDestructiveAction::ClearHistory => (
            "settings.data_management.clear_all_history",
            "settings.data_management.clear_all_history_confirm_detail",
            "settings.data_management.clear_all_history_confirm",
        ),
        DataDestructiveAction::ResetApplication => (
            "settings.data_management.reset_application",
            "settings.data_management.reset_application_confirm_detail",
            "settings.data_management.reset_application_confirm",
        ),
    };
    let answer = window.prompt(
        gpui::PromptLevel::Warning,
        &translator.translate(title_key),
        Some(&translator.translate(detail_key)),
        &[
            gpui::PromptButton::cancel(translator.translate("main.form.cancel")),
            gpui::PromptButton::ok(translator.translate(confirm_key)),
        ],
        cx,
    );
    view.update(cx, |view, cx| {
        view.data_busy = true;
        cx.notify();
    });
    cx.spawn(async move |cx| {
        let confirmed = matches!(answer.await, Ok(1));
        cx.update(|cx| {
            view.update(cx, |view, cx| {
                view.data_busy = false;
                cx.notify();
            });
            if !confirmed {
                return;
            }
            match action {
                DataDestructiveAction::ClearHistory => start_history_cleanup(view, true, cx),
                DataDestructiveAction::ResetApplication => {
                    let target = cx.read_global::<DataManagementService, _>(|service, _| {
                        service.default_data_root().to_path_buf()
                    });
                    start_data_replacement(view, target, true, cx);
                }
            }
        });
    })
    .detach();
}

fn start_data_replacement(
    view: Entity<SettingsWindowView>,
    target: std::path::PathBuf,
    reset_application: bool,
    cx: &mut App,
) {
    if view.read(cx).data_busy {
        return;
    }
    let service = cx.read_global::<DataManagementService, _>(|service, _| service.clone());
    if !reset_application && !service.can_change_data_location() {
        return;
    }
    if !crate::windows::main::MainWindowService::begin_blocking_operation(
        if reset_application {
            "settings.data_management.reset_application"
        } else {
            "settings.data_management.change_location"
        },
        cx,
    ) {
        return;
    }
    crate::services::quick_favorite::QuickFavoriteService::suspend(cx);
    let translator = crate::i18n::translator(cx);
    view.update(cx, |view, cx| {
        view.data_busy = true;
        view.data_error = None;
        view.data_message = None;
        cx.notify();
    });
    if reset_application {
        crate::windows::quick_paste::QuickPasteWindowService::cancel(cx);
    }
    crate::windows::main::MainWindowService::stop_clipboard_listener(cx);
    cx.spawn(async move |cx| {
        let result = cx
            .background_executor()
            .spawn(async move {
                if reset_application {
                    service.prepare_application_reset()
                } else {
                    service.prepare_data_location_switch(&target)
                }
            })
            .await;
        match result {
            Ok(PreparedDataLocation {
                database,
                blob_store,
                data_root,
                cleanup_error,
                switch_guard,
            }) => {
                cx.update(|cx| {
                    crate::windows::menu::MenuService::close_all(cx);
                    crate::windows::preview::PreviewWindowService::destroy(cx);
                    crate::windows::notification::NotificationService::dismiss_all(cx);
                    crate::state_runtime::StateRuntime::reload(data_root.join("state.json"), cx);
                    let config_reload = crate::config_runtime::ConfigRuntime::reload(
                        data_root.join("config.json"),
                        cx,
                    );
                    let history_repository =
                        HistoryRepository::new(database.clone(), blob_store.clone());
                    let image_preview_service = ImagePreviewService::new(blob_store.clone());
                    cx.update_global::<DataManagementService, _>(|service, _| {
                        service.replace_runtime_data(
                            database.clone(),
                            blob_store.clone(),
                            data_root.clone(),
                        );
                    });
                    crate::services::sound::set_data_directory(data_root.join("sound"));
                    crate::windows::preview::PreviewWindowService::replace_image_preview_service(
                        image_preview_service.clone(),
                        cx,
                    );
                    crate::windows::notification::NotificationService::replace_data_sources(
                        history_repository,
                        image_preview_service,
                        cx,
                    );
                    crate::services::quick_favorite::QuickFavoriteService::replace_data_sources(
                        database.clone(),
                        blob_store.clone(),
                        cx,
                    );
                    crate::config_runtime::ConfigRuntime::apply_imported_config(config_reload, cx);
                });
                drop(switch_guard);
                cx.update(|cx| {
                    crate::windows::main::MainWindowService::replace_data_sources(
                        database.clone(),
                        blob_store.clone(),
                        &data_root,
                        cx,
                    );
                    let clipboard_write_suppressor =
                        crate::windows::main::MainWindowService::clipboard_write_suppressor(cx)
                            .unwrap_or_default();
                    crate::windows::quick_paste::QuickPasteWindowService::replace_data_sources(
                        database,
                        blob_store,
                        clipboard_write_suppressor,
                        cx,
                    );
                });
                view.update(cx, |view, cx| {
                    view.data_busy = false;
                    let message = translator.translate(if reset_application {
                        "settings.data_management.reset_application_success"
                    } else {
                        "settings.data_management.location_success"
                    });
                    let error = cleanup_error.map(|_| {
                        translator.translate(if reset_application {
                            "settings.data_management.reset_application_cleanup_error"
                        } else {
                            "settings.data_management.location_cleanup_error"
                        })
                    });
                    let (notification, level) = match error.as_ref() {
                        Some(error) => (format!("{message}\n{error}"), NotificationLevel::Warning),
                        None => (message.clone(), NotificationLevel::Success),
                    };
                    let _ = notify_settings_status(
                        SettingsPage::DataManagement,
                        Some(notification),
                        level,
                        cx,
                    );
                    view.data_error = error;
                    view.data_message = Some(message);
                    view.data_stats_request_id = view.data_stats_request_id.wrapping_add(1);
                    view.data_stats_policy = None;
                    view.data_stats_loading = false;
                    view.data_stats = None;
                    view.refresh_data_stats(cx);
                    cx.notify();
                });
            }
            Err(error) => {
                if reset_application {
                    tracing::error!("重置应用程序失败：{error}");
                } else {
                    tracing::error!("更改数据位置失败：{error}");
                }
                cx.update(|cx| {
                    crate::windows::main::MainWindowService::restart_clipboard_listener(cx);
                });
                view.update(cx, |view, cx| {
                    view.data_busy = false;
                    view.data_error = notify_settings_error(
                        SettingsPage::DataManagement,
                        Some(translator.translate(if reset_application {
                            "settings.data_management.reset_application_error"
                        } else {
                            "settings.data_management.location_error"
                        })),
                        cx,
                    );
                    cx.notify();
                });
            }
        }
        cx.update(|cx| {
            crate::windows::main::MainWindowService::finish_blocking_operation(cx);
            crate::services::quick_favorite::QuickFavoriteService::resume(cx);
        });
    })
    .detach();
}

fn start_cleanup(view: gpui::Entity<SettingsWindowView>, cx: &mut App) {
    start_history_cleanup(view, false, cx);
}

fn start_history_cleanup(view: Entity<SettingsWindowView>, clear_all: bool, cx: &mut App) {
    if view.read(cx).data_busy {
        return;
    }
    let service = cx.read_global::<DataManagementService, _>(|service, _| service.clone());
    let policy = config_runtime::history_config(cx);
    if !crate::windows::main::MainWindowService::begin_blocking_operation(
        if clear_all {
            "settings.data_management.clear_all_history"
        } else {
            "settings.data_management.cleanup"
        },
        cx,
    ) {
        return;
    }
    let translator = crate::i18n::translator(cx);
    view.update(cx, |view, cx| {
        view.data_busy = true;
        view.data_error = None;
        view.data_message = None;
        cx.notify();
    });
    if clear_all {
        crate::windows::quick_paste::QuickPasteWindowService::cancel(cx);
        crate::windows::preview::PreviewWindowService::destroy(cx);
        crate::windows::main::MainWindowService::stop_clipboard_listener(cx);
    }
    cx.spawn(async move |cx| {
        let result = cx
            .background_executor()
            .spawn(async move {
                if clear_all {
                    service.clear_all_history()
                } else {
                    service.cleanup_history(&policy)
                }
            })
            .await;
        let cleaned = result.is_ok();
        view.update(cx, |view, cx| {
            view.data_busy = false;
            match result {
                Ok(report) => {
                    view.data_message = notify_settings_status(
                        SettingsPage::DataManagement,
                        Some(translator.translate_with_args(
                            if clear_all {
                                "settings.data_management.clear_all_history_success"
                            } else {
                                "settings.data_management.cleanup_success"
                            },
                            &[
                                ("history_count", report.history_items_removed.to_string()),
                                ("orphan_count", report.orphan_items_removed.to_string()),
                            ],
                        )),
                        NotificationLevel::Success,
                        cx,
                    )
                }
                Err(error) => {
                    if clear_all {
                        tracing::error!("清空所有剪贴板历史失败：{error}");
                    } else {
                        tracing::error!("清理待清理历史失败：{error}");
                    }
                    view.data_error = notify_settings_error(
                        SettingsPage::DataManagement,
                        Some(translator.translate(if clear_all {
                            "settings.data_management.clear_all_history_error"
                        } else {
                            "settings.data_management.cleanup_error"
                        })),
                        cx,
                    );
                }
            }
            view.data_stats = None;
            view.data_stats_policy = None;
            view.data_stats_request_id = view.data_stats_request_id.wrapping_add(1);
            view.data_stats_loading = false;
            view.refresh_data_stats(cx);
            cx.notify();
        });
        if cleaned {
            cx.update(crate::windows::main::MainWindowService::refresh_history);
        }
        if clear_all {
            cx.update(crate::windows::main::MainWindowService::restart_clipboard_listener);
        }
        cx.update(crate::windows::main::MainWindowService::finish_blocking_operation);
    })
    .detach();
}

fn start_export(view: gpui::Entity<SettingsWindowView>, cx: &mut App) {
    if view.read(cx).data_busy {
        return;
    }
    let service = cx.read_global::<DataManagementService, _>(|service, _| service.clone());
    let suggested = service.suggested_export_file_name();
    if !crate::windows::main::MainWindowService::begin_blocking_operation(
        "settings.data_management.export",
        cx,
    ) {
        return;
    }
    let translator = crate::i18n::translator(cx);
    let dialog = rfd::AsyncFileDialog::new()
        .set_title(translator.translate("settings.data_management.export_title"))
        .set_file_name(suggested)
        .add_filter(
            translator.translate("settings.data_management.file_filter"),
            &["qcclip-data"],
        );
    view.update(cx, |view, cx| {
        view.data_busy = true;
        view.data_error = None;
        view.data_message = None;
        cx.notify();
    });
    cx.spawn(async move |cx| {
        let (message, error, level) = match dialog.save_file().await {
            Some(file) => match cx
                .background_executor()
                .spawn(async move { service.export_data(file.path()) })
                .await
            {
                Ok(()) => (
                    Some(translator.translate("settings.data_management.export_success")),
                    None,
                    NotificationLevel::Success,
                ),
                Err(error) => {
                    tracing::error!("导出数据失败：{error}");
                    (
                        None,
                        Some(translator.translate("settings.data_management.export_error")),
                        NotificationLevel::Error,
                    )
                }
            },
            None => (
                Some(translator.translate("settings.data_management.cancelled")),
                None,
                NotificationLevel::Info,
            ),
        };
        view.update(cx, |view, cx| {
            view.data_busy = false;
            view.data_message =
                notify_settings_status(SettingsPage::DataManagement, message, level, cx);
            view.data_error = notify_settings_error(SettingsPage::DataManagement, error, cx);
            cx.notify();
        });
        cx.update(crate::windows::main::MainWindowService::finish_blocking_operation);
    })
    .detach();
}

fn start_import(view: gpui::Entity<SettingsWindowView>, cx: &mut App) {
    if view.read(cx).data_busy {
        return;
    }
    let service = cx.read_global::<DataManagementService, _>(|service, _| service.clone());
    if !crate::windows::main::MainWindowService::begin_blocking_operation(
        "settings.data_management.import",
        cx,
    ) {
        return;
    }
    let translator = crate::i18n::translator(cx);
    let dialog = rfd::AsyncFileDialog::new()
        .set_title(translator.translate("settings.data_management.import_title"))
        .add_filter(
            translator.translate("settings.data_management.file_filter"),
            &["qcclip-data"],
        );
    view.update(cx, |view, cx| {
        view.data_busy = true;
        view.data_error = None;
        view.data_message = None;
        cx.notify();
    });
    cx.spawn(async move |cx| {
        let data_root = service.data_root().to_path_buf();
        let mut data_services_suspended = false;
        let result = match dialog.pick_file().await {
            Some(file) => match cx
                .background_executor()
                .spawn(async move { service.stage_import(file.path()) })
                .await
            {
                Ok(()) => {
                    cx.update(|cx| {
                        crate::services::quick_favorite::QuickFavoriteService::suspend(cx);
                        crate::windows::main::MainWindowService::stop_clipboard_listener(cx);
                    });
                    data_services_suspended = true;
                    let import_root = data_root.clone();
                    cx.background_executor()
                        .spawn(async move {
                            data_management::apply_pending_import_for_reload(&import_root)
                                .map(|(applied, guard)| (applied, Some(guard)))
                        })
                        .await
                }
                Err(error) => Err(error),
            },
            None => Ok((false, None)),
        };
        match result {
            Ok((true, Some(switch_guard))) => {
                let config_reload =
                    cx.update(|cx| data_management::reload_runtime_after_import(&data_root, cx));
                drop(switch_guard);
                cx.update(|cx| data_management::refresh_after_import(config_reload, cx));
                view.update(cx, |view, cx| {
                    view.data_busy = false;
                    view.data_message = notify_settings_status(
                        SettingsPage::DataManagement,
                        Some(translator.translate("settings.data_management.import_success")),
                        NotificationLevel::Success,
                        cx,
                    );
                    view.data_stats_request_id = view.data_stats_request_id.wrapping_add(1);
                    view.data_stats_policy = None;
                    view.data_stats_loading = false;
                    view.data_stats = None;
                    view.refresh_data_stats(cx);
                    cx.notify();
                });
            }
            Ok((false, switch_guard)) => {
                drop(switch_guard);
                if data_services_suspended {
                    cx.update(|cx| {
                        crate::windows::main::MainWindowService::restart_clipboard_listener(cx)
                    });
                }
                view.update(cx, |view, cx| {
                    view.data_busy = false;
                    view.data_message = notify_settings_status(
                        SettingsPage::DataManagement,
                        Some(translator.translate("settings.data_management.cancelled")),
                        NotificationLevel::Info,
                        cx,
                    );
                    cx.notify();
                })
            }
            Ok((true, None)) => {
                tracing::error!("数据导入没有取得热重载锁");
                if data_services_suspended {
                    cx.update(|cx| {
                        crate::windows::main::MainWindowService::restart_clipboard_listener(cx)
                    });
                }
                view.update(cx, |view, cx| {
                    view.data_busy = false;
                    view.data_error = notify_settings_error(
                        SettingsPage::DataManagement,
                        Some(translator.translate("settings.data_management.import_error")),
                        cx,
                    );
                    cx.notify();
                });
            }
            Err(error) => {
                tracing::error!("导入数据失败：{error}");
                if data_services_suspended {
                    cx.update(|cx| {
                        crate::windows::main::MainWindowService::restart_clipboard_listener(cx)
                    });
                }
                view.update(cx, |view, cx| {
                    view.data_busy = false;
                    view.data_error = notify_settings_error(
                        SettingsPage::DataManagement,
                        Some(translator.translate("settings.data_management.import_error")),
                        cx,
                    );
                    cx.notify();
                });
            }
        }
        cx.update(|cx| {
            crate::windows::main::MainWindowService::finish_blocking_operation(cx);
            if data_services_suspended {
                crate::services::quick_favorite::QuickFavoriteService::resume(cx);
            }
        });
    })
    .detach();
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    if bytes < 1024 * 1024 {
        return format!("{:.1} KB", bytes as f64 / 1024.0);
    }
    if bytes < 1024 * 1024 * 1024 {
        return format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0));
    }
    format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
}
