use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    ops::Deref,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{
    App, AppContext as _, BorrowAppContext as _, Global, Hsla, Lerp as _, SharedString,
    Subscription, Window, WindowId, px,
};
use gpui_component::{
    Colorize as _, Root, Theme, ThemeColor, ThemeConfig, ThemeMode, ThemeRegistry, try_parse_color,
};
use rust_embed::RustEmbed;
use serde::Deserialize;

use crate::{config::AppearanceMode, config_runtime};

#[derive(RustEmbed)]
#[folder = "themes"]
#[include = "*.json"]
struct EmbeddedThemeAssets;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QcClipTheme {
    pub mode: ThemeMode,
    pub capture: CaptureTheme,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureTheme {
    pub mask: u32,
    pub selection_border: u32,
    pub selection_hover_border: u32,
    pub selection_active_handle: u32,
    pub size_label_background: u32,
    pub size_label_text: u32,
}

impl QcClipTheme {
    fn for_mode(mode: ThemeMode) -> Self {
        Self {
            mode,
            capture: CaptureTheme {
                mask: 0x00000066,
                selection_border: 0xf4f4f4ee,
                selection_hover_border: 0xffffffff,
                selection_active_handle: 0xffffffff,
                size_label_background: 0x111827dd,
                size_label_text: 0xffffffff,
            },
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct QcClipThemeRuntime {
    tokens: QcClipTheme,
}

impl Global for QcClipThemeRuntime {}

#[derive(Default)]
struct SystemAppearanceWatchService {
    watches: HashMap<WindowId, Subscription>,
    theme_transition_generation: u64,
}

impl Global for SystemAppearanceWatchService {}

#[derive(Debug, Clone)]
struct ThemePackage {
    name: String,
    display_names: BTreeMap<String, String>,
    themes: Vec<ThemeEntry>,
    is_external: bool,
}

#[derive(Debug, Clone)]
struct ThemeEntry {
    config: Rc<ThemeConfig>,
}

impl Deref for ThemeEntry {
    type Target = ThemeConfig;

    fn deref(&self) -> &Self::Target {
        &self.config
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ThemePackageFile {
    name: String,
    #[serde(rename = "displayNames")]
    display_names: BTreeMap<String, String>,
    themes: Vec<ThemeConfig>,
}

impl ThemePackage {
    fn localized_name(&self, locale: &str) -> String {
        self.display_names
            .get(locale)
            .cloned()
            .or_else(|| {
                let language = locale.split('-').next().unwrap_or(locale);
                self.display_names.iter().find_map(|(key, value)| {
                    (key.split('-').next() == Some(language)).then(|| value.clone())
                })
            })
            .or_else(|| self.display_names.get("en-US").cloned())
            .or_else(|| self.display_names.values().next().cloned())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| self.name.clone())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ThemeOption {
    pub(crate) value: SharedString,
    pub(crate) label: SharedString,
    /// 所属主题包，用于把同一包的浅色与深色主题归到一起展示。
    pub(crate) package: SharedString,
    pub(crate) mode: ThemeMode,
    pub(crate) is_default: bool,
    pub(crate) is_external: bool,
    pub(crate) preview: ThemePreviewColors,
}

/// 设置页缩略预览所需的少量主题颜色，直接从主题配置解析，不影响全局主题。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ThemePreviewColors {
    pub(crate) background: Hsla,
    pub(crate) title_bar: Hsla,
    pub(crate) foreground: Hsla,
    pub(crate) muted_foreground: Hsla,
    pub(crate) primary: Hsla,
    pub(crate) border: Hsla,
    pub(crate) list_active: Hsla,
}

impl ThemePreviewColors {
    pub(crate) fn fallback(mode: ThemeMode) -> Self {
        let defaults = if mode.is_dark() {
            ThemeColor::dark()
        } else {
            ThemeColor::light()
        };
        Self {
            background: defaults.background,
            title_bar: defaults.title_bar,
            foreground: defaults.foreground,
            muted_foreground: defaults.muted_foreground,
            primary: defaults.primary,
            border: defaults.border,
            list_active: defaults.list_active,
        }
    }

    fn from_config(config: &ThemeConfig) -> Self {
        let fallback = Self::fallback(config.mode);
        let colors = &config.colors;
        let parse = |value: &Option<SharedString>, fallback: Hsla| {
            value
                .as_deref()
                .and_then(|value| try_parse_color(value).ok())
                .unwrap_or(fallback)
        };
        let background = parse(&colors.background, fallback.background);
        let primary = parse(&colors.primary, fallback.primary);
        Self {
            background,
            title_bar: parse(&colors.title_bar, background),
            foreground: parse(&colors.foreground, fallback.foreground),
            muted_foreground: parse(&colors.muted_foreground, fallback.muted_foreground),
            primary,
            border: parse(&colors.border, fallback.border),
            list_active: parse(&colors.list_active, primary.opacity(0.16)),
        }
    }
}

pub(crate) fn available_theme_options(cx: &App) -> Vec<ThemeOption> {
    let locale = config_runtime::interface_language(cx).code();
    let mut options = load_all_theme_packages(resolve_themes_dir().as_ref())
        .into_iter()
        .flat_map(|package| {
            let label: SharedString = package.localized_name(locale).into();
            let is_external = package.is_external;
            let package_name: SharedString = package.name.into();
            package.themes.into_iter().map(move |theme| ThemeOption {
                value: theme.name.clone(),
                label: label.clone(),
                package: package_name.clone(),
                mode: theme.mode,
                is_default: theme.is_default,
                is_external,
                preview: ThemePreviewColors::from_config(&theme),
            })
        })
        .collect::<Vec<_>>();

    let mut deduped = BTreeMap::<String, ThemeOption>::new();
    for option in options.drain(..) {
        deduped.insert(option.value.to_string(), option);
    }

    let mut options = deduped.into_values().collect::<Vec<_>>();
    options.sort_by(|left, right| {
        left.is_external
            .cmp(&right.is_external)
            .then_with(|| right.is_default.cmp(&left.is_default))
            .then_with(|| left.label.to_lowercase().cmp(&right.label.to_lowercase()))
            .then_with(|| left.value.cmp(&right.value))
    });
    options
}

macro_rules! for_each_theme_color_field {
    ($callback:ident) => {
        $callback!(accent);
        $callback!(accent_foreground);
        $callback!(accordion);
        $callback!(background);
        $callback!(border);
        $callback!(button);
        $callback!(button_active);
        $callback!(button_foreground);
        $callback!(button_hover);
        $callback!(button_danger);
        $callback!(button_danger_active);
        $callback!(button_danger_foreground);
        $callback!(button_danger_hover);
        $callback!(button_info);
        $callback!(button_info_active);
        $callback!(button_info_foreground);
        $callback!(button_info_hover);
        $callback!(button_primary);
        $callback!(button_primary_active);
        $callback!(button_primary_foreground);
        $callback!(button_primary_hover);
        $callback!(button_secondary);
        $callback!(button_secondary_active);
        $callback!(button_secondary_foreground);
        $callback!(button_secondary_hover);
        $callback!(button_success);
        $callback!(button_success_active);
        $callback!(button_success_foreground);
        $callback!(button_success_hover);
        $callback!(button_warning);
        $callback!(button_warning_active);
        $callback!(button_warning_foreground);
        $callback!(button_warning_hover);
        $callback!(group_box);
        $callback!(group_box_foreground);
        $callback!(caret);
        $callback!(chart_1);
        $callback!(chart_2);
        $callback!(chart_3);
        $callback!(chart_4);
        $callback!(chart_5);
        $callback!(chart_bullish);
        $callback!(chart_bearish);
        $callback!(danger);
        $callback!(danger_active);
        $callback!(danger_foreground);
        $callback!(danger_hover);
        $callback!(description_list_label);
        $callback!(description_list_label_foreground);
        $callback!(drag_border);
        $callback!(drop_target);
        $callback!(foreground);
        $callback!(info);
        $callback!(info_active);
        $callback!(info_foreground);
        $callback!(info_hover);
        $callback!(input);
        $callback!(link);
        $callback!(link_active);
        $callback!(link_hover);
        $callback!(list);
        $callback!(list_active);
        $callback!(list_active_border);
        $callback!(list_even);
        $callback!(list_head);
        $callback!(list_hover);
        $callback!(muted);
        $callback!(muted_foreground);
        $callback!(popover);
        $callback!(popover_foreground);
        $callback!(primary);
        $callback!(primary_active);
        $callback!(primary_foreground);
        $callback!(primary_hover);
        $callback!(progress_bar);
        $callback!(ring);
        $callback!(scrollbar);
        $callback!(scrollbar_thumb);
        $callback!(scrollbar_thumb_hover);
        $callback!(secondary);
        $callback!(secondary_active);
        $callback!(secondary_foreground);
        $callback!(secondary_hover);
        $callback!(selection);
        $callback!(sidebar);
        $callback!(sidebar_accent);
        $callback!(sidebar_accent_foreground);
        $callback!(sidebar_border);
        $callback!(sidebar_foreground);
        $callback!(sidebar_primary);
        $callback!(sidebar_primary_foreground);
        $callback!(skeleton);
        $callback!(slider_bar);
        $callback!(slider_thumb);
        $callback!(success);
        $callback!(success_foreground);
        $callback!(success_hover);
        $callback!(success_active);
        $callback!(switch);
        $callback!(switch_thumb);
        $callback!(tab);
        $callback!(tab_active);
        $callback!(tab_active_foreground);
        $callback!(tab_bar);
        $callback!(tab_bar_segmented);
        $callback!(tab_foreground);
        $callback!(table);
        $callback!(table_active);
        $callback!(table_active_border);
        $callback!(table_even);
        $callback!(table_head);
        $callback!(table_head_foreground);
        $callback!(table_foot);
        $callback!(table_foot_foreground);
        $callback!(table_hover);
        $callback!(table_row_border);
        $callback!(title_bar);
        $callback!(title_bar_border);
        $callback!(status_bar);
        $callback!(status_bar_border);
        $callback!(tiles);
        $callback!(warning);
        $callback!(warning_active);
        $callback!(warning_hover);
        $callback!(warning_foreground);
        $callback!(overlay);
        $callback!(window_border);
        $callback!(red);
        $callback!(red_light);
        $callback!(green);
        $callback!(green_light);
        $callback!(blue);
        $callback!(blue_light);
        $callback!(yellow);
        $callback!(yellow_light);
        $callback!(magenta);
        $callback!(magenta_light);
        $callback!(cyan);
        $callback!(cyan_light);
    };
}

fn interpolate_theme_colors(from: ThemeColor, to: ThemeColor, progress: f32) -> ThemeColor {
    let mut colors = from;
    macro_rules! interpolate_field {
        ($field:ident) => {
            colors.$field = from.$field.lerp(&to.$field, progress);
        };
    }
    for_each_theme_color_field!(interpolate_field);
    colors
}

fn theme_colors_equal(left: ThemeColor, right: ThemeColor) -> bool {
    macro_rules! compare_field {
        ($field:ident) => {
            if left.$field != right.$field {
                return false;
            }
        };
    }
    for_each_theme_color_field!(compare_field);
    true
}

const THEME_TRANSITION_DURATION: Duration = Duration::from_millis(240);
const THEME_TRANSITION_FRAME: Duration = Duration::from_millis(16);
const QC_CLIP_TITLE_BAR_HEIGHT: gpui::Pixels = px(36.0);

fn apply_ui_scale(theme: &mut Theme, scale: f32) {
    let config = if theme.mode.is_dark() {
        &theme.dark_theme
    } else {
        &theme.light_theme
    };
    let default = Theme::default();
    // 未声明字号的主题不会覆盖旧值，必须从配置或默认字号重新计算。
    theme.font_size = config.font_size.map(px).unwrap_or(default.font_size) * scale;
    theme.mono_font_size = config
        .mono_font_size
        .map(px)
        .unwrap_or(default.mono_font_size)
        * scale;
    theme.sheet.margin_top = QC_CLIP_TITLE_BAR_HEIGHT * (f32::from(theme.font_size) / 16.0);
}

fn apply_app_font(theme: &mut Theme, family: Option<&str>) {
    let config = if theme.mode.is_dark() {
        &theme.dark_theme
    } else {
        &theme.light_theme
    };
    // 恢复默认或切换主题时重新取主题字体，避免沿用上一次的用户选择。
    theme.font_family = family
        .map(|family| family.to_owned().into())
        .or_else(|| config.font_family.clone())
        .unwrap_or_else(|| Theme::default().font_family);
}

pub fn install(cx: &mut App) {
    let mode = ThemeMode::from(cx.window_appearance());
    cx.set_global(QcClipThemeRuntime {
        tokens: QcClipTheme::for_mode(mode),
    });
    cx.set_global(SystemAppearanceWatchService::default());
    apply_system_theme(None, cx);
    install_theme_registry_guard(cx);
    install_system_appearance_watchers(cx);

    if let Some(themes_dir) = resolve_themes_dir() {
        if let Err(error) = ThemeRegistry::watch_dir(themes_dir, cx, |cx| {
            apply_system_theme(None, cx);
        }) {
            tracing::error!("监听主题目录失败：{error}");
        }
    }
}

pub(crate) fn apply_appearance_from_config(cx: &mut App) {
    apply_system_theme(None, cx);
}

fn install_theme_registry_guard(cx: &mut App) {
    cx.observe_global::<ThemeRegistry>(|cx| {
        apply_system_theme(None, cx);
    })
    .detach();
}

fn install_system_appearance_watchers(cx: &mut App) {
    cx.observe_new::<Root>(|_, window, cx| {
        if let Some(window) = window {
            attach_system_appearance_watch(window, cx);
        }
    })
    .detach();

    cx.on_window_closed(|cx, _| {
        let open_window_ids = cx
            .windows()
            .into_iter()
            .map(|window| window.window_id())
            .collect::<HashSet<_>>();
        cx.update_global::<SystemAppearanceWatchService, _>(|service, _| {
            service
                .watches
                .retain(|window_id, _| open_window_ids.contains(window_id));
        });
    })
    .detach();
}

pub fn attach_system_appearance_watch(window: &mut Window, cx: &mut App) {
    let window_id = window.window_handle().window_id();
    let already_watching = cx.read_global::<SystemAppearanceWatchService, _>(|service, _| {
        service.watches.contains_key(&window_id)
    });
    if already_watching {
        return;
    }

    sync_system_theme(window, cx);
    let subscription = window.observe_window_appearance(|window, cx| {
        sync_system_theme(window, cx);
    });
    cx.update_global::<SystemAppearanceWatchService, _>(|service, _| {
        service.watches.insert(window_id, subscription);
    });
}

fn sync_system_theme(window: &mut Window, cx: &mut App) {
    let configured_mode = config_runtime::appearance_config(cx).mode;
    let mode = effective_theme_mode(configured_mode, ThemeMode::from(window.appearance()));
    // 新窗口复用已初始化的主题，避免重复下发所有窗口的亚克力材质。
    if mode != Theme::global(cx).mode {
        apply_system_theme(Some(window), cx);
    }
}

fn apply_system_theme(window: Option<&mut Window>, cx: &mut App) {
    let previous_colors = cx.has_global::<Theme>().then(|| Theme::global(cx).colors);
    let system_mode = window
        .as_deref()
        .map(Window::appearance)
        .map(ThemeMode::from)
        .unwrap_or_else(|| ThemeMode::from(cx.window_appearance()));
    let appearance = config_runtime::appearance_config(cx);
    let mode = effective_theme_mode(appearance.mode, system_mode);
    let themes_dir = resolve_themes_dir();
    let packages = load_all_theme_packages(themes_dir.as_ref());
    let light = resolve_theme_for_mode(&packages, &appearance.light_theme, ThemeMode::Light, cx);
    let dark = resolve_theme_for_mode(&packages, &appearance.dark_theme, ThemeMode::Dark, cx);

    {
        let theme = Theme::global_mut(cx);
        theme.light_theme = light.config;
        theme.dark_theme = dark.config;
    }
    Theme::change(mode, window, cx);
    apply_ui_scale(Theme::global_mut(cx), appearance.ui_scale());
    apply_app_font(Theme::global_mut(cx), appearance.app_font_family.as_deref());
    Theme::sync_base(cx);
    crate::window_material::capture_base_colors(cx);
    crate::window_material::apply_from_config(cx);

    Theme::global_mut(cx).focus_ring = false;
    Theme::sync_scrollbar_appearance(cx);
    cx.update_global::<QcClipThemeRuntime, _>(|runtime, _| {
        runtime.tokens = QcClipTheme::for_mode(mode);
    });

    let target_colors = Theme::global(cx).colors;
    if let Some(previous_colors) = previous_colors
        .filter(|previous| !theme_colors_equal(*previous, target_colors))
        .filter(|_| !cx.reduce_motion())
        .filter(|_| {
            crate::config_runtime::animation_enabled(
                cx,
                crate::config::AnimationKind::ThemeTransition,
            )
        })
    {
        let theme = Theme::global_mut(cx);
        theme.colors = previous_colors;
        theme.tokens = previous_colors.into();
        Theme::sync_base(cx);
        start_theme_transition(previous_colors, target_colors, cx);
    }
    cx.refresh_windows();
}

fn start_theme_transition(from: ThemeColor, to: ThemeColor, cx: &mut App) {
    let generation = cx.update_global::<SystemAppearanceWatchService, _>(|service, _| {
        service.theme_transition_generation = service.theme_transition_generation.wrapping_add(1);
        service.theme_transition_generation
    });

    cx.spawn(async move |cx| {
        let started_at = Instant::now();
        loop {
            let progress = (started_at.elapsed().as_secs_f32()
                / THEME_TRANSITION_DURATION.as_secs_f32())
            .clamp(0.0, 1.0);
            let progress = gpui::ease_in_out(progress);
            let completed = progress >= 1.0;
            let applied = cx.update(|cx| {
                let is_current = cx.read_global::<SystemAppearanceWatchService, _>(|service, _| {
                    service.theme_transition_generation == generation
                });
                if !is_current {
                    return false;
                }
                if !crate::config_runtime::animation_enabled(
                    cx,
                    crate::config::AnimationKind::ThemeTransition,
                ) {
                    cx.update_global::<Theme, _>(|theme, _| {
                        theme.colors = to;
                        theme.tokens = to.into();
                    });
                    Theme::sync_base(cx);
                    cx.refresh_windows();
                    return false;
                }

                let colors = interpolate_theme_colors(from, to, progress);
                cx.update_global::<Theme, _>(|theme, _| {
                    theme.colors = colors;
                    theme.tokens = colors.into();
                });
                Theme::sync_base(cx);
                cx.refresh_windows();
                true
            });
            if !applied || completed {
                break;
            }

            cx.background_executor().timer(THEME_TRANSITION_FRAME).await;
        }
    })
    .detach();
}

fn effective_theme_mode(configured_mode: AppearanceMode, system_mode: ThemeMode) -> ThemeMode {
    match configured_mode {
        AppearanceMode::System => system_mode,
        AppearanceMode::Light => ThemeMode::Light,
        AppearanceMode::Dark => ThemeMode::Dark,
    }
}

fn resolve_themes_dir() -> Option<PathBuf> {
    let candidates = [
        PathBuf::from("themes"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("themes"),
    ];
    candidates.into_iter().find(|path| path.exists())
}

fn resolve_theme_for_mode(
    packages: &[ThemePackage],
    preferred_name: &str,
    mode: ThemeMode,
    cx: &App,
) -> ThemeEntry {
    packages
        .iter()
        .rev()
        .flat_map(|package| package.themes.iter())
        .find(|theme| theme.mode == mode && theme.name.as_ref() == preferred_name)
        .cloned()
        .or_else(|| {
            packages
                .iter()
                .rev()
                .flat_map(|package| package.themes.iter())
                .find(|theme| theme.mode == mode && theme.is_default)
                .cloned()
        })
        .or_else(|| {
            packages
                .iter()
                .rev()
                .flat_map(|package| package.themes.iter())
                .find(|theme| theme.mode == mode)
                .cloned()
        })
        .unwrap_or_else(|| {
            let config = if mode.is_dark() {
                ThemeRegistry::global(cx).default_dark_theme().clone()
            } else {
                ThemeRegistry::global(cx).default_light_theme().clone()
            };
            ThemeEntry { config }
        })
}

fn load_all_theme_packages(themes_dir: Option<&PathBuf>) -> Vec<ThemePackage> {
    let embedded = load_embedded_theme_packages();
    let external = themes_dir
        .map(load_external_theme_packages)
        .unwrap_or_default();
    merge_theme_packages(embedded, external)
}

fn load_embedded_theme_packages() -> Vec<ThemePackage> {
    let mut packages = Vec::new();
    for path in EmbeddedThemeAssets::iter()
        .filter(|path| !path.starts_with('.') && !path.ends_with("schema.json"))
    {
        let Some(file) = EmbeddedThemeAssets::get(path.as_ref()) else {
            tracing::warn!(theme_asset = %path, "读取内嵌主题资源失败，已跳过该主题");
            continue;
        };
        let content = match std::str::from_utf8(file.data.as_ref()) {
            Ok(content) => content,
            Err(error) => {
                tracing::warn!(theme_asset = %path, %error, "内嵌主题资源不是有效 UTF-8，已跳过该主题");
                continue;
            }
        };
        match parse_theme_package(content) {
            Some(package) => packages.push(package),
            None => tracing::warn!(theme_asset = %path, "内嵌主题文件格式无效，已跳过该主题"),
        }
    }
    packages
}

fn load_external_theme_packages(themes_dir: &PathBuf) -> Vec<ThemePackage> {
    let entries = match std::fs::read_dir(themes_dir) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(%error, "读取外部主题目录失败，已跳过外部主题");
            return Vec::new();
        }
    };
    let mut packages = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(%error, "读取外部主题目录项失败，已跳过该主题");
                continue;
            }
        };
        let path = entry.path();
        let theme_file = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("<未知文件>");
        if theme_file.starts_with('.')
            || theme_file.eq_ignore_ascii_case("theme-schema.json")
            || path.extension().and_then(|extension| extension.to_str()) != Some("json")
        {
            continue;
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) => {
                tracing::warn!(theme_file, %error, "读取外部主题文件失败，已跳过该主题");
                continue;
            }
        };
        match parse_theme_package(&content) {
            Some(mut package) => {
                package.is_external = true;
                packages.push(package);
            }
            None => tracing::warn!(theme_file, "外部主题文件格式无效，已跳过该主题"),
        }
    }
    packages
}

fn merge_theme_packages(
    mut embedded: Vec<ThemePackage>,
    external: Vec<ThemePackage>,
) -> Vec<ThemePackage> {
    let builtin_ids = embedded
        .iter()
        .flat_map(|package| package.themes.iter())
        .map(|theme| theme.name.to_string())
        .collect::<BTreeSet<_>>();
    for mut package in external {
        package
            .themes
            .retain(|theme| !builtin_ids.contains(theme.name.as_ref()));
        if !package.themes.is_empty() {
            embedded.push(package);
        }
    }
    embedded
}

fn parse_theme_package(content: &str) -> Option<ThemePackage> {
    let package = serde_json::from_str::<ThemePackageFile>(content).ok()?;
    if package.themes.is_empty() {
        return None;
    }
    let name = if package.name.trim().is_empty() {
        package
            .themes
            .first()
            .map(|theme| theme.name.to_string())
            .unwrap_or_else(|| "Theme".to_owned())
    } else {
        package.name
    };
    Some(ThemePackage {
        name,
        display_names: package.display_names,
        themes: package
            .themes
            .into_iter()
            .map(|theme| ThemeEntry {
                config: Rc::new(theme),
            })
            .collect(),
        is_external: false,
    })
}

#[cfg(test)]
mod tests {
    use gpui_component::{ThemeColor, ThemeMode};

    use crate::config::{AppearanceConfig, AppearanceMode};

    use super::{
        effective_theme_mode, interpolate_theme_colors, load_embedded_theme_packages,
        theme_colors_equal,
    };

    #[test]
    fn theme_color_interpolation_reaches_both_endpoints() {
        let from = *ThemeColor::light();
        let mut to = from;
        to.background = gpui::black();
        to.foreground = gpui::white();

        assert!(theme_colors_equal(
            interpolate_theme_colors(from, to, 0.0),
            from
        ));
        assert!(theme_colors_equal(
            interpolate_theme_colors(from, to, 1.0),
            to
        ));
    }

    #[test]
    fn ui_scale_does_not_accumulate_and_respects_custom_theme_fonts() {
        let mut theme = gpui_component::Theme::default();
        theme.light_theme = std::rc::Rc::new(gpui_component::ThemeConfig::default());
        theme.mode = ThemeMode::Light;
        let base_font_size = theme.font_size;
        let base_mono_size = theme.mono_font_size;
        for scale in [1.25, 1.5, 1.5, 0.8, 2.0, 1.0] {
            super::apply_ui_scale(&mut theme, scale);
            assert_eq!(theme.font_size, base_font_size * scale);
            assert_eq!(theme.mono_font_size, base_mono_size * scale);
        }

        theme.dark_theme = std::rc::Rc::new(gpui_component::ThemeConfig {
            font_size: Some(20.0),
            mono_font_size: Some(15.0),
            ..Default::default()
        });
        theme.mode = ThemeMode::Dark;
        super::apply_ui_scale(&mut theme, 1.5);
        assert_eq!(theme.font_size, gpui::px(30.0));
        assert_eq!(theme.mono_font_size, gpui::px(22.5));
        theme.mode = ThemeMode::Light;
        super::apply_ui_scale(&mut theme, 1.0);
        assert_eq!(theme.font_size, base_font_size);
    }

    #[test]
    fn app_font_override_and_reset_respect_the_active_theme() {
        let mut theme = gpui_component::Theme::default();
        theme.light_theme = std::rc::Rc::new(gpui_component::ThemeConfig {
            font_family: Some("主题字体".into()),
            ..Default::default()
        });
        theme.dark_theme = std::rc::Rc::new(gpui_component::ThemeConfig::default());
        theme.mode = ThemeMode::Light;
        super::apply_app_font(&mut theme, Some("用户字体"));
        assert_eq!(theme.font_family.as_ref(), "用户字体");
        super::apply_app_font(&mut theme, None);
        assert_eq!(theme.font_family.as_ref(), "主题字体");
        theme.mode = ThemeMode::Dark;
        super::apply_app_font(&mut theme, None);
        assert_eq!(
            theme.font_family,
            gpui_component::Theme::default().font_family
        );
    }

    #[test]
    fn theme_color_interpolation_changes_colors_between_endpoints() {
        let from = *ThemeColor::light();
        let mut to = from;
        to.background = gpui::black();

        let middle = interpolate_theme_colors(from, to, 0.5);
        assert_ne!(middle.background, from.background);
        assert_ne!(middle.background, to.background);
    }

    #[test]
    fn embedded_theme_packages_include_default_light_and_dark_themes() {
        let packages = load_embedded_theme_packages();
        let names = packages
            .iter()
            .flat_map(|package| package.themes.iter())
            .map(|theme| theme.name.to_string())
            .collect::<Vec<_>>();
        let defaults = AppearanceConfig::default();
        assert!(names.iter().any(|name| name == &defaults.light_theme));
        assert!(names.iter().any(|name| name == &defaults.dark_theme));
    }

    #[test]
    fn embedded_theme_packages_expose_localized_display_names() {
        let packages = load_embedded_theme_packages();
        let defaults = AppearanceConfig::default();
        let package = packages
            .iter()
            .find(|package| {
                package
                    .themes
                    .iter()
                    .any(|theme| theme.name.as_ref() == defaults.light_theme.as_str())
            })
            .expect("应存在默认主题包");

        assert_eq!(package.localized_name("zh-CN"), "默认");
        assert_eq!(package.localized_name("en-US"), "Default");
    }

    #[test]
    fn fixed_appearance_modes_do_not_follow_the_system() {
        assert_eq!(
            effective_theme_mode(AppearanceMode::System, ThemeMode::Dark),
            ThemeMode::Dark
        );
        assert_eq!(
            effective_theme_mode(AppearanceMode::Light, ThemeMode::Dark),
            ThemeMode::Light
        );
        assert_eq!(
            effective_theme_mode(AppearanceMode::Dark, ThemeMode::Light),
            ThemeMode::Dark
        );
    }
}
