#![allow(dead_code, unused_imports)]

#[path = "../src/config/mod.rs"]
mod config;

use config::{AppConfig, ClipboardFormatConfig};

#[test]
fn released_config_uses_native_common_format_defaults() {
    let loaded =
        config::load::from_str(r#"{"schemaVersion":1,"clipboard":{"autoFocusSearch":true}}"#)
            .expect("无法加载已发布的配置");
    let formats = &loaded.config.clipboard.capture_formats;
    assert!(loaded.config.clipboard.respect_history_exclusion);
    assert!(!formats.all);
    for name in [
        "CF_UNICODETEXT",
        "HTML Format",
        "Rich Text Format",
        "PNG",
        "CF_DIBV5",
        "CF_DIB",
        "CF_HDROP",
    ] {
        assert!(formats.includes(name), "默认缺少原生格式：{name}");
    }
    for name in ["Biff12", "Embed Source", "图片", "文本", "文件"] {
        assert!(!formats.includes(name));
    }
    assert!(loaded.config.clipboard.auto_focus_search);
    assert!(loaded.warnings.is_empty());
}

#[test]
fn native_names_are_trimmed_deduplicated_and_matched_in_full() {
    let loaded = config::load::from_str(r#"{"clipboard":{"captureFormats":{"names":[" Biff12 ","biff12","Embed Source","HTML Format"," ","bad\nname"]}}}"#)
        .expect("无法加载原生格式名单");
    let formats = &loaded.config.clipboard.capture_formats;
    assert_eq!(formats.names, ["Biff12", "Embed Source", "HTML Format"]);
    assert!(formats.includes("BIFF12"));
    assert!(!formats.includes("Biff"));
    assert!(!formats.includes("registered:Biff12"));
    assert!(!formats.includes("图片"));
}

#[test]
fn all_mode_preserves_custom_list_across_round_trip_and_switching_back() {
    let mut config = AppConfig::default();
    config.clipboard.capture_formats = ClipboardFormatConfig {
        all: true,
        names: vec!["Biff12".into()],
    };
    let json = serde_json::to_string(&config).expect("无法序列化格式配置");
    let mut loaded = config::load::from_str(&json)
        .expect("无法重新加载格式配置")
        .config;
    assert_eq!(loaded, config);
    assert!(loaded.clipboard.capture_formats.includes("Embed Source"));
    loaded.clipboard.capture_formats.all = false;
    assert!(loaded.clipboard.capture_formats.includes("Biff12"));
    assert!(!loaded.clipboard.capture_formats.includes("Embed Source"));
}

#[test]
fn empty_list_intentionally_disables_format_capture() {
    let loaded = config::load::from_str(r#"{"clipboard":{"captureFormats":{"names":[]}}}"#)
        .expect("无法加载空格式名单");
    assert!(loaded.config.clipboard.capture_formats.names.is_empty());
    assert!(
        !loaded
            .config
            .clipboard
            .capture_formats
            .includes("CF_UNICODETEXT")
    );
}

#[test]
fn invalid_fields_do_not_discard_other_format_preferences() {
    let loaded = config::load::from_str(
        r#"{"clipboard":{"captureFormats":{"all":"bad","names":["Biff12"]}}}"#,
    )
    .expect("无法加载部分无效的配置");
    assert!(!loaded.config.clipboard.capture_formats.all);
    assert_eq!(loaded.config.clipboard.capture_formats.names, ["Biff12"]);
    assert_eq!(loaded.warnings.len(), 1);
    let loaded =
        config::load::from_str(r#"{"clipboard":{"captureFormats":{"all":true,"names":12}}}"#)
            .expect("无法加载部分无效的配置");
    assert!(loaded.config.clipboard.capture_formats.all);
    assert_eq!(
        loaded.config.clipboard.capture_formats.names,
        ClipboardFormatConfig::default().names
    );
    assert_eq!(loaded.warnings.len(), 1);
}

#[test]
fn format_name_validation_uses_windows_utf16_length() {
    assert!(ClipboardFormatConfig::valid_name("Embed Source"));
    assert!(ClipboardFormatConfig::valid_name(&"A".repeat(255)));
    assert!(!ClipboardFormatConfig::valid_name(&"A".repeat(256)));
    assert!(!ClipboardFormatConfig::valid_name(&"😀".repeat(128)));
    assert!(!ClipboardFormatConfig::valid_name(" \t "));
    assert!(!ClipboardFormatConfig::valid_name("bad\0name"));
}
