use std::{collections::HashMap, sync::Arc};

use gpui::{App, AppContext, BorrowAppContext as _, Global};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Locale(String);

impl Locale {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into()).normalized()
    }

    pub fn zh_cn() -> Self {
        Self("zh-CN".to_owned())
    }

    pub fn en_us() -> Self {
        Self("en-US".to_owned())
    }

    pub fn normalized(&self) -> Self {
        let raw = self.0.replace('_', "-");
        match raw.to_ascii_lowercase().as_str() {
            "zh" | "zh-cn" => Self::zh_cn(),
            "en" | "en-us" => Self::en_us(),
            _ => Self(raw),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_supported(&self) -> bool {
        matches!(self.normalized().as_str(), "zh-CN" | "en-US")
    }

    pub fn supported() -> [Self; 2] {
        [Self::zh_cn(), Self::en_us()]
    }
}

impl Default for Locale {
    fn default() -> Self {
        Self::zh_cn()
    }
}

#[derive(Debug, Clone, Default)]
pub struct MessageBundle {
    locale: Locale,
    entries: Arc<HashMap<String, String>>,
}

impl MessageBundle {
    pub fn from_json_str(locale: Locale, json: &str) -> Result<Self, serde_json::Error> {
        let entries = serde_json::from_str(json)?;
        Ok(Self {
            locale,
            entries: Arc::new(entries),
        })
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries.get(key).map(String::as_str)
    }
}

#[derive(Debug, Clone, Default)]
pub struct BundleRegistry {
    bundles: HashMap<Locale, MessageBundle>,
}

impl BundleRegistry {
    pub fn from_bundles(bundles: impl IntoIterator<Item = MessageBundle>) -> Self {
        Self {
            bundles: bundles
                .into_iter()
                .map(|bundle| (bundle.locale.clone(), bundle))
                .collect(),
        }
    }

    pub fn get(&self, locale: &Locale) -> Option<&MessageBundle> {
        self.bundles.get(&locale.normalized())
    }
}

#[derive(Debug, Clone)]
pub struct Translator {
    locale: Locale,
    fallback_locale: Locale,
    active_bundle: MessageBundle,
    fallback_bundle: MessageBundle,
}

impl Translator {
    pub fn from_registry(
        registry: &BundleRegistry,
        locale: Locale,
        fallback_locale: Locale,
    ) -> Self {
        let locale = locale.normalized();
        let fallback_locale = fallback_locale.normalized();
        let active_bundle = registry
            .get(&locale)
            .cloned()
            .or_else(|| registry.get(&fallback_locale).cloned())
            .unwrap_or_default();
        let fallback_bundle = registry.get(&fallback_locale).cloned().unwrap_or_default();

        Self {
            locale,
            fallback_locale,
            active_bundle,
            fallback_bundle,
        }
    }

    pub fn builtin(locale: Locale) -> Self {
        Self::from_registry(&builtin_bundles(), locale, Locale::zh_cn())
    }

    pub fn locale(&self) -> &Locale {
        &self.locale
    }

    pub fn fallback_locale(&self) -> &Locale {
        &self.fallback_locale
    }

    pub fn translate(&self, key: &str) -> String {
        self.text(key)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("i18n:{key}"))
    }

    pub fn translate_with_args(&self, key: &str, args: &[(&str, String)]) -> String {
        let mut text = self.translate(key);
        for (name, value) in args {
            text = text.replace(&format!("{{{name}}}"), value);
        }
        text
    }

    fn text(&self, key: &str) -> Option<&str> {
        self.active_bundle
            .get(key)
            .or_else(|| self.fallback_bundle.get(key))
    }
}

#[derive(Debug, Clone)]
pub struct I18n {
    locale: Locale,
    translator: Translator,
}

impl I18n {
    fn new(locale: Locale) -> Self {
        let locale = locale.normalized();
        Self {
            translator: Translator::builtin(locale.clone()),
            locale,
        }
    }
}

impl Global for I18n {}

pub fn install(locale: Locale, cx: &mut App) {
    cx.set_global(I18n::new(locale));
}

pub fn set_locale(locale: Locale, cx: &mut App) {
    let locale = locale.normalized();
    let changed = cx.update_global::<I18n, _>(|state, _| {
        if state.locale == locale {
            return false;
        }
        state.locale = locale.clone();
        state.translator = Translator::builtin(locale.clone());
        true
    });
    if changed {
        cx.refresh_windows();
    }
}

pub fn set_locale_code(value: &str, cx: &mut App) -> bool {
    let locale = Locale::new(value);
    if !locale.is_supported() {
        return false;
    }
    set_locale(locale, cx);
    true
}

pub fn translator<C>(cx: &C) -> Translator
where
    C: AppContext,
{
    cx.read_global::<I18n, _>(|state, _| state.translator.clone())
}

pub fn builtin_bundles() -> BundleRegistry {
    BundleRegistry::from_bundles([
        load_builtin_bundle(Locale::zh_cn(), include_str!("i18n/zh-CN.json")),
        load_builtin_bundle(Locale::en_us(), include_str!("i18n/en-US.json")),
    ])
}

fn load_builtin_bundle(locale: Locale, json: &str) -> MessageBundle {
    MessageBundle::from_json_str(locale.clone(), json)
        .unwrap_or_else(|error| panic!("内建语言资源加载失败（{}）：{error}", locale.as_str()))
}

pub fn clipboard_source(
    translator: &Translator,
    app: Option<&str>,
    device: Option<&str>,
) -> String {
    let app = app.filter(|name| !name.trim().is_empty());
    if let Some(device) = device {
        let from =
            translator.translate_with_args("main.source.device", &[("device", device.to_owned())]);
        app.map(|app| format!("{app} · {from}")).unwrap_or(from)
    } else {
        app.map(str::to_owned)
            .unwrap_or_else(|| translator.translate("main.source.unknown"))
    }
}

pub fn list_item_time(
    translator: &Translator,
    timestamp_millis: i64,
    format: crate::config::ListTimeFormat,
) -> String {
    match format {
        crate::config::ListTimeFormat::Absolute => {
            use chrono::Datelike as _;

            chrono::DateTime::from_timestamp_millis(timestamp_millis)
                .map(|time| {
                    let time = time.with_timezone(&chrono::Local);
                    let pattern = if time.year() == chrono::Local::now().year() {
                        "%m-%d %H:%M"
                    } else {
                        "%Y-%m-%d %H:%M"
                    };
                    time.format(pattern).to_string()
                })
                .unwrap_or_else(|| "—".to_owned())
        }
        crate::config::ListTimeFormat::Relative => relative_time(translator, timestamp_millis),
    }
}

pub fn relative_time(translator: &Translator, timestamp_millis: i64) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let now_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(timestamp_millis);
    let elapsed = now_millis.saturating_sub(timestamp_millis);
    let minute = 60_000;
    let hour = 60 * minute;
    let day = 24 * hour;
    if elapsed < minute {
        translator.translate("main.relative.just_now")
    } else if elapsed < hour {
        translator.translate_with_args(
            "main.relative.minutes_ago",
            &[("count", (elapsed / minute).to_string())],
        )
    } else if elapsed < day {
        translator.translate_with_args(
            "main.relative.hours_ago",
            &[("count", (elapsed / hour).to_string())],
        )
    } else {
        translator.translate_with_args(
            "main.relative.days_ago",
            &[("count", (elapsed / day).to_string())],
        )
    }
}

pub fn file_size(translator: &Translator, size: Option<u64>, is_directory: bool) -> String {
    if is_directory {
        return translator.translate("main.file.folder");
    }
    let Some(size) = size else {
        return translator.translate("main.file.unknown_size");
    };
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = size as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", size, UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_locale_resources_include_supported_languages() {
        let registry = builtin_bundles();
        assert!(registry.get(&Locale::zh_cn()).is_some());
        assert!(registry.get(&Locale::en_us()).is_some());
    }

    #[test]
    fn translator_supports_argument_substitution_and_missing_key_fallback() {
        let translator = Translator::builtin(Locale::en_us());
        assert_eq!(
            translator.translate_with_args("main.preview.char_count", &[("count", "3".to_owned())]),
            "3 characters"
        );
        assert_eq!(translator.translate("missing.key"), "i18n:missing.key");
    }

    #[test]
    fn translator_snapshot_keeps_active_and_fallback_text_after_registry_is_dropped() {
        let snapshot = {
            let registry = BundleRegistry::from_bundles([
                MessageBundle::from_json_str(
                    Locale::zh_cn(),
                    r#"{"shared":"中文","fallback":"回退文本"}"#,
                )
                .expect("中文测试资源无效"),
                MessageBundle::from_json_str(Locale::en_us(), r#"{"shared":"English"}"#)
                    .expect("英文测试资源无效"),
            ]);
            let translator = Translator::from_registry(&registry, Locale::en_us(), Locale::zh_cn());
            translator.clone()
        };
        assert_eq!(snapshot.translate("shared"), "English");
        assert_eq!(snapshot.translate("fallback"), "回退文本");
        assert_eq!(snapshot.translate("missing"), "i18n:missing");
    }

    #[test]
    fn switching_translator_does_not_change_an_existing_snapshot() {
        let mut translator = Translator::builtin(Locale::zh_cn());
        let snapshot = translator.clone();
        translator = Translator::builtin(Locale::en_us());
        assert_eq!(snapshot.translate("main.preview.image"), "图片");
        assert_eq!(translator.translate("main.preview.image"), "Image");
    }

    #[test]
    fn locale_normalizes_codes_and_rejects_unsupported_values() {
        assert_eq!(Locale::new("zh_cn"), Locale::zh_cn());
        assert_eq!(Locale::new("en"), Locale::en_us());
        assert!(!Locale::new("ja-JP").is_supported());
    }

    #[test]
    fn builtin_locale_resources_have_matching_key_sets() {
        let zh = include_str!("i18n/zh-CN.json");
        let en = include_str!("i18n/en-US.json");
        let zh: HashMap<String, String> = serde_json::from_str(zh).expect("中文语言资源格式无效");
        let en: HashMap<String, String> = serde_json::from_str(en).expect("英文语言资源格式无效");
        let mut zh_keys = zh.keys().collect::<Vec<_>>();
        let mut en_keys = en.keys().collect::<Vec<_>>();
        zh_keys.sort();
        en_keys.sort();
        assert_eq!(zh_keys, en_keys);
    }

    #[test]
    fn list_item_time_uses_local_absolute_time_by_default_and_supports_relative_time() {
        use crate::config::ListTimeFormat;
        use chrono::{Datelike as _, TimeZone as _};

        let translator = Translator::builtin(Locale::zh_cn());
        let current_year = chrono::Local::now().year();
        for (year, expected) in [
            (current_year, "11-17 14:30".to_owned()),
            (
                current_year - 1,
                format!("{}-11-17 14:30", current_year - 1),
            ),
        ] {
            let timestamp = chrono::Local
                .with_ymd_and_hms(year, 11, 17, 14, 30, 37)
                .single()
                .expect("无法创建本地测试时间")
                .timestamp_millis();
            assert_eq!(
                list_item_time(&translator, timestamp, ListTimeFormat::default()),
                expected
            );
        }
        assert_eq!(
            list_item_time(
                &translator,
                chrono::Utc::now().timestamp_millis(),
                ListTimeFormat::Relative
            ),
            "刚刚"
        );
    }

    #[test]
    fn clipboard_source_labels_include_device_and_handle_missing_application() {
        let zh = Translator::builtin(Locale::zh_cn());
        let en = Translator::builtin(Locale::en_us());
        assert_eq!(
            clipboard_source(&zh, Some("Chrome"), Some("工作电脑")),
            "Chrome · 来自工作电脑"
        );
        assert_eq!(
            clipboard_source(&zh, None, Some("工作电脑")),
            "来自工作电脑"
        );
        assert_eq!(
            clipboard_source(&en, Some("Chrome"), Some("PC")),
            "Chrome · From PC"
        );
        assert_eq!(clipboard_source(&en, Some("Chrome"), None), "Chrome");
        assert_eq!(
            clipboard_source(&en, None, None),
            en.translate("main.source.unknown")
        );
    }
}
