use super::super::{
    SettingsPage, notify_settings_error, notify_settings_status, open_settings_directory,
};
use crate::windows::notification::{NotificationLevel, NotificationService};
use gpui_component::tooltip::ManagedTooltipExt as _;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, FontWeight, div};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};

use super::super::SettingsWindowView;
use crate::services::{data_management::DataManagementService, legacy_migration};

impl SettingsWindowView {
    pub(in crate::windows::settings) fn render_legacy_migration(
        &self,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().colors;
        let translator = crate::i18n::translator(cx);
        let view = cx.entity();
        let detect_view = view.clone();
        let folder_view = view.clone();
        let zip_view = view.clone();
        let history_view = view.clone();
        let favorite_view = view.clone();
        let source_disabled =
            self.data_busy || !self.legacy_options.history && !self.legacy_options.favorites;
        let mut section = v_flex()
            .w_full()
            .min_w_0()
            .gap_3()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(translator.translate("migration.title")),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_4()
                    .flex_wrap()
                    .child(
                        Checkbox::new("legacy-history")
                            .label(translator.translate("migration.scope_history"))
                            .checked(self.legacy_options.history)
                            .disabled(self.data_busy)
                            .on_click(move |checked, _, cx| {
                                change_scope(history_view.clone(), true, *checked, cx);
                            }),
                    )
                    .child(
                        Checkbox::new("legacy-favorites")
                            .label(translator.translate("migration.scope_favorites"))
                            .checked(self.legacy_options.favorites)
                            .disabled(self.data_busy)
                            .on_click(move |checked, _, cx| {
                                change_scope(favorite_view.clone(), false, *checked, cx);
                            }),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Button::new("legacy-detect")
                            .secondary()
                            .small()
                            .disabled(source_disabled)
                            .label(translator.translate("migration.detect"))
                            .on_click(move |_, _, cx| {
                                choose_source(detect_view.clone(), SourceChoice::Detect, cx)
                            }),
                    )
                    .child(
                        Button::new("legacy-folder")
                            .secondary()
                            .small()
                            .disabled(source_disabled)
                            .label(translator.translate("migration.folder"))
                            .on_click(move |_, _, cx| {
                                choose_source(folder_view.clone(), SourceChoice::Folder, cx)
                            }),
                    )
                    .child(
                        Button::new("legacy-zip")
                            .secondary()
                            .small()
                            .disabled(source_disabled)
                            .label(translator.translate("migration.zip"))
                            .on_click(move |_, _, cx| {
                                choose_source(zip_view.clone(), SourceChoice::Zip, cx)
                            }),
                    ),
            );
        if let Some(prepared) = &self.legacy_migration {
            let preview = &prepared.preview;
            section = section
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(prepared.source.display().to_string()),
                )
                .child(
                    h_flex()
                        .w_full()
                        .gap_4()
                        .flex_wrap()
                        .child(translator.translate_with_args(
                            "migration.history",
                            &[("count", preview.history.to_string())],
                        ))
                        .child(translator.translate_with_args(
                            "migration.favorites",
                            &[("count", preview.favorites.to_string())],
                        )),
                )
                .child(div().text_sm().child(translator.translate_with_args(
                    "migration.summary",
                    &[
                        ("groups", preview.groups.to_string()),
                        ("duplicates", preview.duplicate_contents.to_string()),
                        ("imported", preview.already_imported.to_string()),
                        ("skipped", preview.skipped.to_string()),
                    ],
                )));
            if preview.warning_count > 0 {
                let mut warnings = v_flex()
                    .id("legacy-warnings")
                    .w_full()
                    .max_h(gpui::px(180.0))
                    .gap_1()
                    .text_xs()
                    .text_color(theme.warning)
                    .overflow_y_scrollbar()
                    .child(translator.translate_with_args(
                        "migration.warnings",
                        &[("count", preview.warning_count.to_string())],
                    ));
                for warning in &preview.warnings {
                    warnings = warnings.child(warning.clone());
                }
                section = section.child(warnings);
            }
            let import_view = view.clone();
            let cancel_view = view.clone();
            section = section.child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        Button::new("legacy-confirm")
                            .primary()
                            .small()
                            .disabled(
                                self.data_busy
                                    || !self.legacy_options.history
                                        && !self.legacy_options.favorites,
                            )
                            .label(translator.translate("migration.confirm"))
                            .on_click(move |_, _, cx| start_migration(import_view.clone(), cx)),
                    )
                    .child(
                        Button::new("legacy-dismiss")
                            .secondary()
                            .small()
                            .disabled(self.data_busy)
                            .label(translator.translate("migration.cancel"))
                            .on_click(move |_, _, cx| {
                                cancel_view.update(cx, |view, cx| {
                                    view.legacy_migration = None;
                                    view.legacy_source = None;
                                    cx.notify();
                                });
                            }),
                    ),
            );
            if let Some(progress) = &self.legacy_progress {
                section = section.child(div().text_sm().child(translator.translate_with_args(
                    "migration.progress",
                    &[
                        ("done", progress.load(Ordering::Relaxed).to_string()),
                        (
                            "total",
                            prepared.selected_count(self.legacy_options).to_string(),
                        ),
                    ],
                )));
            }
        }
        if let Some(report) = &self.legacy_report {
            let backup = report.backup.display().to_string();
            let tooltip = backup.clone();
            let folder = report.backup.parent().map(std::path::Path::to_path_buf);
            let open_label = translator.translate("migration.open_backup");
            section = section
                .child(
                    div()
                        .w_full()
                        .text_sm()
                        .text_color(theme.success)
                        .child(migration_result_message(report, &translator)),
                )
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .flex_none()
                                .text_xs()
                                .child(translator.translate("migration.backup")),
                        )
                        .child(
                            div()
                                .id("legacy-backup-path")
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .child(backup)
                                .managed_tooltip(move |window, cx| {
                                    Tooltip::new(tooltip.clone()).build(window, cx)
                                }),
                        )
                        .child(
                            Button::new("legacy-open-backup")
                                .ghost()
                                .small()
                                .icon(crate::ui::icon::icon(
                                    "folder-open",
                                    16.0,
                                    theme.muted_foreground,
                                ))
                                .accessibility_label(open_label.clone())
                                .tooltip(open_label)
                                .on_click(move |_, _, cx| {
                                    if let Some(folder) = folder.clone() {
                                        open_settings_directory(
                                            folder,
                                            "migration.open_backup",
                                            cx,
                                        );
                                    }
                                }),
                        ),
                );
        }
        if let Some(error) = &self.legacy_error {
            section = section.child(
                div()
                    .w_full()
                    .text_sm()
                    .text_color(theme.danger)
                    .child(error.clone()),
            );
        }
        section.into_any_element()
    }
}

fn change_scope(view: Entity<SettingsWindowView>, history: bool, checked: bool, cx: &mut App) {
    if view.read(cx).data_busy {
        return;
    }
    let source = view.update(cx, |view, cx| {
        if history {
            view.legacy_options.history = checked;
        } else {
            view.legacy_options.favorites = checked;
        }
        view.legacy_migration = None;
        view.legacy_report = None;
        view.legacy_error = None;
        cx.notify();
        view.legacy_source.clone()
    });
    if let Some(source) = source {
        start_source_analysis(view, source, cx);
    }
}

fn migration_result_message(
    report: &legacy_migration::MigrationReport,
    translator: &crate::i18n::Translator,
) -> String {
    translator.translate_with_args(
        "migration.result",
        &[
            ("contents", report.added_contents.to_string()),
            ("merged", report.merged_contents.to_string()),
            ("history", report.added_history.to_string()),
            ("favorites", report.added_favorites.to_string()),
            ("tags", report.added_tags.to_string()),
            ("skipped", report.skipped.to_string()),
            ("icons", report.imported_icons.to_string()),
        ],
    )
}

enum SourceChoice {
    Detect,
    Folder,
    Zip,
}

fn choose_source(view: Entity<SettingsWindowView>, choice: SourceChoice, cx: &mut App) {
    if view.read(cx).data_busy {
        return;
    }
    let translator = crate::i18n::translator(cx);
    let dialog = rfd::AsyncFileDialog::new().set_title(translator.translate("migration.title"));
    view.update(cx, |view, cx| {
        view.data_busy = true;
        view.legacy_error = None;
        cx.notify();
    });
    cx.spawn(async move |cx| {
        let source = match choice {
            SourceChoice::Detect => {
                cx.background_executor()
                    .spawn(async { legacy_migration::detect_source() })
                    .await
            }
            SourceChoice::Folder => dialog
                .pick_folder()
                .await
                .map(|file| file.path().to_path_buf()),
            SourceChoice::Zip => dialog
                .add_filter("ZIP", &["zip"])
                .pick_file()
                .await
                .map(|file| file.path().to_path_buf()),
        };
        view.update(cx, |view, cx| {
            view.data_busy = false;
            cx.notify();
        });
        if let Some(source) = source {
            cx.update(|cx| start_source_analysis(view.clone(), source, cx));
        } else {
            view.update(cx, |view, cx| {
                if matches!(choice, SourceChoice::Detect) {
                    view.legacy_error = notify_settings_status(
                        SettingsPage::DataManagement,
                        Some(translator.translate("migration.not_found")),
                        NotificationLevel::Warning,
                        cx,
                    );
                }
                cx.notify();
            });
        }
    })
    .detach();
}

pub(in crate::windows::settings) fn start_source_analysis(
    view: Entity<SettingsWindowView>,
    source: PathBuf,
    cx: &mut App,
) {
    if view.read(cx).data_busy {
        return;
    }
    let options = view.read(cx).legacy_options;
    if !options.history && !options.favorites {
        return;
    }
    let root =
        cx.read_global::<DataManagementService, _>(|service, _| service.data_root().to_path_buf());
    view.update(cx, |view, cx| {
        view.data_busy = true;
        view.legacy_source = Some(source.clone());
        view.legacy_migration = None;
        view.legacy_report = None;
        view.legacy_error = None;
        cx.notify();
    });
    cx.spawn(async move |cx| {
        let result = cx
            .background_executor()
            .spawn(async move { legacy_migration::prepare(&source, &root, options) })
            .await;
        view.update(cx, |view, cx| {
            view.data_busy = false;
            match result {
                Ok(prepared) => {
                    let translator = crate::i18n::translator(cx);
                    let preview = &prepared.preview;
                    let mut message = translator.translate_with_args(
                        "migration.summary",
                        &[
                            ("groups", preview.groups.to_string()),
                            ("duplicates", preview.duplicate_contents.to_string()),
                            ("imported", preview.already_imported.to_string()),
                            ("skipped", preview.skipped.to_string()),
                        ],
                    );
                    let level = if preview.warning_count > 0 {
                        message.push('\n');
                        message.push_str(&translator.translate_with_args(
                            "migration.warnings",
                            &[("count", preview.warning_count.to_string())],
                        ));
                        NotificationLevel::Warning
                    } else {
                        NotificationLevel::Info
                    };
                    NotificationService::show_message(
                        translator.translate("migration.title"),
                        message,
                        level,
                        cx,
                    );
                    view.legacy_migration = Some(Arc::new(prepared));
                }
                Err(error) => {
                    tracing::error!("分析旧版数据失败：{error}");
                    view.legacy_error = notify_settings_error(
                        SettingsPage::DataManagement,
                        Some(error.to_string()),
                        cx,
                    );
                }
            }
            cx.notify();
        });
    })
    .detach();
}

fn start_migration(view: Entity<SettingsWindowView>, cx: &mut App) {
    if view.read(cx).data_busy {
        return;
    }
    let Some(prepared) = view.read(cx).legacy_migration.clone() else {
        return;
    };
    let options = view.read(cx).legacy_options;
    let service = cx.read_global::<DataManagementService, _>(|service, _| service.clone());
    if !crate::windows::main::MainWindowService::begin_blocking_operation("migration.title", cx) {
        return;
    }
    let translator = crate::i18n::translator(cx);
    let dialog = rfd::AsyncMessageDialog::new()
        .set_title(translator.translate("migration.title"))
        .set_description(translator.translate_with_args(
            "migration.confirmation",
            &[("count", prepared.selected_count(options).to_string())],
        ))
        .set_level(rfd::MessageLevel::Warning)
        .set_buttons(rfd::MessageButtons::OkCancel);
    view.update(cx, |view, cx| {
        view.data_busy = true;
        view.legacy_error = None;
        cx.notify();
    });
    cx.spawn(async move |cx| {
        if dialog.show().await != rfd::MessageDialogResult::Ok {
            view.update(cx, |view, cx| {
                view.data_busy = false;
                cx.notify();
            });
            cx.update(crate::windows::main::MainWindowService::finish_blocking_operation);
            return;
        }
        let progress = Arc::new(AtomicUsize::new(0));
        view.update(cx, |view, cx| {
            view.legacy_progress = Some(progress.clone());
            cx.notify();
        });
        cx.update(|cx| {
            crate::windows::main::MainWindowService::stop_clipboard_listener(cx);
            let tick_view = view.downgrade();
            cx.spawn(async move |cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(150))
                        .await;
                    let active = tick_view
                        .update(cx, |view, cx| {
                            cx.notify();
                            view.legacy_progress.is_some()
                        })
                        .unwrap_or(false);
                    if !active {
                        break;
                    }
                }
            })
            .detach();
        });
        let result = cx
            .background_executor()
            .spawn(
                async move { legacy_migration::migrate(&prepared, options, &service, &progress) },
            )
            .await;
        cx.update(|cx| {
            if result.is_ok() {
                crate::windows::main::MainWindowService::reload_after_data_import(cx);
            } else {
                crate::windows::main::MainWindowService::restart_clipboard_listener(cx);
            }
        });
        view.update(cx, |view, cx| {
            view.data_busy = false;
            view.legacy_progress = None;
            match result {
                Ok(report) => {
                    let level = if report.skipped > 0 {
                        NotificationLevel::Warning
                    } else {
                        NotificationLevel::Success
                    };
                    NotificationService::show_message(
                        translator.translate("migration.title"),
                        migration_result_message(&report, &translator),
                        level,
                        cx,
                    );
                    view.legacy_report = Some(report);
                    view.legacy_migration = None;
                    view.data_stats_request_id = view.data_stats_request_id.wrapping_add(1);
                    view.data_stats_policy = None;
                    view.data_stats_loading = false;
                    view.data_stats = None;
                    view.refresh_data_stats(cx);
                }
                Err(error) => {
                    tracing::error!("迁移旧版数据失败：{error}");
                    view.legacy_error = notify_settings_error(
                        SettingsPage::DataManagement,
                        Some(error.to_string()),
                        cx,
                    );
                }
            }
            cx.notify();
        });
        cx.update(crate::windows::main::MainWindowService::finish_blocking_operation);
    })
    .detach();
}
