mod about;
mod application_filter;
mod clipboard_formats;
mod data_management;
mod data_packages;
mod general;
mod item_display_editor;
mod legacy_migration;
mod list_preview;
mod main_window;
mod notifications;
mod personalization;
mod quick_paste;
mod shortcuts;
mod sound;
mod tab_bar_editor;

pub(in crate::windows::settings) use legacy_migration::start_source_analysis;
pub(in crate::windows::settings) use personalization::persist_appearance_change_in_view;
pub(crate) use personalization::{
    appearance_mode_label_key, selected_theme_preview, theme_thumbnail,
};
pub(in crate::windows::settings) use sound::persist_sound_change_in_view;

pub(in crate::windows::settings) mod sync;
