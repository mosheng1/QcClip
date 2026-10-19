use serde::{Deserialize, Deserializer, Serialize, Serializer};

mod item_display;
pub mod load;
mod notification;
mod tab_bar;

pub use item_display::{
    ItemDisplayConfig, ItemDisplayField, ItemDisplayPreset, ItemRegionVisibility,
};
pub use notification::{NotificationConfig, NotificationKind, NotificationPosition};
pub use tab_bar::{TabBarButton, TabBarButtonGroup, TabBarLayout, TabBarSection};

pub const CURRENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct AppConfig {
    pub schema_version: u32,
    pub general: GeneralConfig,
    pub clipboard: ClipboardConfig,
    pub application_filter: ApplicationFilterConfig,
    pub history: HistoryConfig,
    pub quick_paste: QuickPasteConfig,
    pub sound: SoundConfig,
    pub appearance: AppearanceConfig,
    pub shortcuts: ShortcutsConfig,
    pub updates: UpdateConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            general: GeneralConfig::default(),
            clipboard: ClipboardConfig::default(),
            application_filter: ApplicationFilterConfig::default(),
            history: HistoryConfig::default(),
            quick_paste: QuickPasteConfig::default(),
            sound: SoundConfig::default(),
            appearance: AppearanceConfig::default(),
            shortcuts: ShortcutsConfig::default(),
            updates: UpdateConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UpdateFrequency {
    #[default]
    Daily,
    EveryThreeDays,
    Weekly,
    Never,
}

impl UpdateFrequency {
    pub const ALL: [Self; 4] = [Self::Daily, Self::EveryThreeDays, Self::Weekly, Self::Never];

    /// 自动检查间隔，`None` 表示永不自动检查。
    pub const fn seconds(self) -> Option<u64> {
        match self {
            Self::Daily => Some(86400),
            Self::EveryThreeDays => Some(3 * 86400),
            Self::Weekly => Some(7 * 86400),
            Self::Never => None,
        }
    }

    pub const fn label_key(self) -> &'static str {
        match self {
            Self::Daily => "updates.frequency.daily",
            Self::EveryThreeDays => "updates.frequency.three_days",
            Self::Weekly => "updates.frequency.weekly",
            Self::Never => "updates.frequency.never",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UpdateProxyMode {
    #[default]
    System,
    Direct,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UpdateConfig {
    pub frequency: UpdateFrequency,
    pub notification_enabled: bool,
    pub proxy_mode: UpdateProxyMode,
    pub proxy_url: String,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            frequency: UpdateFrequency::default(),
            notification_enabled: true,
            proxy_mode: UpdateProxyMode::default(),
            proxy_url: String::new(),
        }
    }
}

/// 按前台应用临时限制 QcClip 功能。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct ApplicationFilterConfig {
    pub enabled: bool,
    pub disable_clipboard_monitor: bool,
    pub disable_global_hotkeys: bool,
    pub disable_edge_hide: bool,
    pub applications: Vec<ApplicationFilterApplication>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct ApplicationFilterApplication {
    pub process_name: String,
    pub process_path: String,
    pub display_name: String,
}

impl Default for ApplicationFilterConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            disable_clipboard_monitor: false,
            disable_global_hotkeys: false,
            disable_edge_hide: false,
            applications: Vec::new(),
        }
    }
}

impl Default for ApplicationFilterApplication {
    fn default() -> Self {
        Self {
            process_name: String::new(),
            process_path: String::new(),
            display_name: String::new(),
        }
    }
}

impl AppConfig {
    pub fn normalized(mut self) -> Self {
        self.schema_version = CURRENT_SCHEMA_VERSION;
        self.clipboard = self.clipboard.normalized();
        self.general.notifications = self.general.notifications.normalized();
        self.shortcuts.middle_mouse = self.shortcuts.middle_mouse.normalized();
        self.quick_paste = self.quick_paste.normalized();
        self.sound = self.sound.normalized();
        self.appearance = self.appearance.normalized();
        self
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum InterfaceLanguage {
    #[serde(rename = "zh-CN")]
    #[default]
    ZhCn,
    #[serde(rename = "en-US")]
    EnUs,
}

impl InterfaceLanguage {
    pub const ALL: [Self; 2] = [Self::ZhCn, Self::EnUs];

    pub const fn code(self) -> &'static str {
        match self {
            Self::ZhCn => "zh-CN",
            Self::EnUs => "en-US",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct GeneralConfig {
    pub language: InterfaceLanguage,
    pub global_notification_enabled: bool,
    pub notifications: NotificationConfig,
    pub launch_at_login: bool,
    pub run_as_administrator: bool,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            language: InterfaceLanguage::default(),
            global_notification_enabled: true,
            notifications: NotificationConfig::default(),
            launch_at_login: false,
            run_as_administrator: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct ShortcutsConfig {
    pub toggle_main_window: Option<ShortcutBinding>,
    pub middle_mouse: MiddleMouseShortcutConfig,
    pub open_settings: Option<ShortcutBinding>,
    pub toggle_clipboard_monitor: Option<ShortcutBinding>,
    pub plain_text_paste: Option<ShortcutBinding>,
    pub quick_paste: Option<ShortcutBinding>,
    pub quick_paste_1_to_9: QuickPasteShortcutConfig,
    pub select_previous_item: SessionShortcutConfig,
    pub select_next_item: SessionShortcutConfig,
    pub select_first_item: SessionShortcutConfig,
    pub select_last_item: SessionShortcutConfig,
    pub previous_tab: SessionShortcutConfig,
    pub next_tab: SessionShortcutConfig,
    pub toggle_window_fixed: SessionShortcutConfig,
    pub focus_search: SessionShortcutConfig,
    pub execute_paste: SessionShortcutConfig,
    pub toggle_favorite: SessionShortcutConfig,
    pub toggle_pinned: SessionShortcutConfig,
    pub delete_item: SessionShortcutConfig,
    pub hide_window: SessionShortcutConfig,
}

impl Default for ShortcutsConfig {
    fn default() -> Self {
        Self {
            toggle_main_window: Some(ShortcutBinding::toggle_main_window_default()),
            middle_mouse: MiddleMouseShortcutConfig::default(),
            open_settings: None,
            toggle_clipboard_monitor: None,
            plain_text_paste: None,
            quick_paste: None,
            quick_paste_1_to_9: QuickPasteShortcutConfig::default(),
            select_previous_item: SessionShortcutConfig::new(None, "up"),
            select_next_item: SessionShortcutConfig::new(None, "down"),
            select_first_item: SessionShortcutConfig::new(None, "home"),
            select_last_item: SessionShortcutConfig::new(None, "end"),
            previous_tab: SessionShortcutConfig::new(None, "left"),
            next_tab: SessionShortcutConfig::new(None, "right"),
            toggle_window_fixed: SessionShortcutConfig::new(Some("control"), "p"),
            focus_search: SessionShortcutConfig::new(None, "tab"),
            execute_paste: SessionShortcutConfig::new(Some("control"), "enter"),
            toggle_favorite: SessionShortcutConfig::new(Some("control"), "d"),
            toggle_pinned: SessionShortcutConfig::new(Some("control"), "t"),
            delete_item: SessionShortcutConfig::new(None, "delete"),
            hide_window: SessionShortcutConfig::new(None, "escape"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct SessionShortcutConfig {
    pub binding: Option<ShortcutBinding>,
    pub only_in_window: bool,
}

impl SessionShortcutConfig {
    fn new(modifier: Option<&str>, key: &str) -> Self {
        let mut binding = ShortcutBinding {
            control: false,
            alt: false,
            shift: false,
            platform: false,
            key: key.to_owned(),
        };
        match modifier {
            Some("control") => binding.control = true,
            Some("alt") => binding.alt = true,
            Some("shift") => binding.shift = true,
            Some("platform") => binding.platform = true,
            None => {}
            Some(_) => unreachable!(),
        }
        Self {
            binding: Some(binding),
            only_in_window: false,
        }
    }
}

impl Default for SessionShortcutConfig {
    fn default() -> Self {
        Self {
            binding: None,
            only_in_window: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SessionShortcutId {
    SelectPreviousItem,
    SelectNextItem,
    SelectFirstItem,
    SelectLastItem,
    PreviousTab,
    NextTab,
    ToggleWindowFixed,
    FocusSearch,
    ExecutePaste,
    ToggleFavorite,
    TogglePinned,
    DeleteItem,
    HideWindow,
}

impl SessionShortcutId {
    pub const ALL: [Self; 13] = [
        Self::SelectPreviousItem,
        Self::SelectNextItem,
        Self::SelectFirstItem,
        Self::SelectLastItem,
        Self::PreviousTab,
        Self::NextTab,
        Self::ToggleWindowFixed,
        Self::FocusSearch,
        Self::ExecutePaste,
        Self::ToggleFavorite,
        Self::TogglePinned,
        Self::DeleteItem,
        Self::HideWindow,
    ];

    pub const fn config_key(self) -> &'static str {
        match self {
            Self::SelectPreviousItem => "selectPreviousItem",
            Self::SelectNextItem => "selectNextItem",
            Self::SelectFirstItem => "selectFirstItem",
            Self::SelectLastItem => "selectLastItem",
            Self::PreviousTab => "previousTab",
            Self::NextTab => "nextTab",
            Self::ToggleWindowFixed => "toggleWindowFixed",
            Self::FocusSearch => "focusSearch",
            Self::ExecutePaste => "executePaste",
            Self::ToggleFavorite => "toggleFavorite",
            Self::TogglePinned => "togglePinned",
            Self::DeleteItem => "deleteItem",
            Self::HideWindow => "hideWindow",
        }
    }

    pub const fn label_key(self) -> &'static str {
        match self {
            Self::SelectPreviousItem => "settings.shortcuts.select_previous_item",
            Self::SelectNextItem => "settings.shortcuts.select_next_item",
            Self::SelectFirstItem => "settings.shortcuts.select_first_item",
            Self::SelectLastItem => "settings.shortcuts.select_last_item",
            Self::PreviousTab => "settings.shortcuts.previous_tab",
            Self::NextTab => "settings.shortcuts.next_tab",
            Self::ToggleWindowFixed => "settings.shortcuts.toggle_window_fixed",
            Self::FocusSearch => "settings.shortcuts.focus_search",
            Self::ExecutePaste => "settings.shortcuts.execute_paste",
            Self::ToggleFavorite => "settings.shortcuts.toggle_favorite",
            Self::TogglePinned => "settings.shortcuts.toggle_pinned",
            Self::DeleteItem => "settings.shortcuts.delete_item",
            Self::HideWindow => "settings.shortcuts.hide_window",
        }
    }

    pub const fn code(self) -> &'static str {
        self.config_key()
    }
}

impl ShortcutsConfig {
    pub fn session(&self, id: SessionShortcutId) -> &SessionShortcutConfig {
        match id {
            SessionShortcutId::SelectPreviousItem => &self.select_previous_item,
            SessionShortcutId::SelectNextItem => &self.select_next_item,
            SessionShortcutId::SelectFirstItem => &self.select_first_item,
            SessionShortcutId::SelectLastItem => &self.select_last_item,
            SessionShortcutId::PreviousTab => &self.previous_tab,
            SessionShortcutId::NextTab => &self.next_tab,
            SessionShortcutId::ToggleWindowFixed => &self.toggle_window_fixed,
            SessionShortcutId::FocusSearch => &self.focus_search,
            SessionShortcutId::ExecutePaste => &self.execute_paste,
            SessionShortcutId::ToggleFavorite => &self.toggle_favorite,
            SessionShortcutId::TogglePinned => &self.toggle_pinned,
            SessionShortcutId::DeleteItem => &self.delete_item,
            SessionShortcutId::HideWindow => &self.hide_window,
        }
    }

    pub fn session_mut(&mut self, id: SessionShortcutId) -> &mut SessionShortcutConfig {
        match id {
            SessionShortcutId::SelectPreviousItem => &mut self.select_previous_item,
            SessionShortcutId::SelectNextItem => &mut self.select_next_item,
            SessionShortcutId::SelectFirstItem => &mut self.select_first_item,
            SessionShortcutId::SelectLastItem => &mut self.select_last_item,
            SessionShortcutId::PreviousTab => &mut self.previous_tab,
            SessionShortcutId::NextTab => &mut self.next_tab,
            SessionShortcutId::ToggleWindowFixed => &mut self.toggle_window_fixed,
            SessionShortcutId::FocusSearch => &mut self.focus_search,
            SessionShortcutId::ExecutePaste => &mut self.execute_paste,
            SessionShortcutId::ToggleFavorite => &mut self.toggle_favorite,
            SessionShortcutId::TogglePinned => &mut self.toggle_pinned,
            SessionShortcutId::DeleteItem => &mut self.delete_item,
            SessionShortcutId::HideWindow => &mut self.hide_window,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortcutBinding {
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    pub platform: bool,
    pub key: String,
}

impl ShortcutBinding {
    pub fn toggle_main_window_default() -> Self {
        Self {
            control: true,
            alt: false,
            shift: true,
            platform: false,
            key: "x".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClipboardDisplayPriority {
    #[default]
    TextHtmlImage,
    TextImageHtml,
    HtmlTextImage,
    HtmlImageText,
    ImageTextHtml,
    ImageHtmlText,
}

impl ClipboardDisplayPriority {
    pub const ALL: [Self; 6] = [
        Self::TextHtmlImage,
        Self::TextImageHtml,
        Self::HtmlTextImage,
        Self::HtmlImageText,
        Self::ImageTextHtml,
        Self::ImageHtmlText,
    ];

    pub const fn code(self) -> &'static str {
        match self {
            Self::TextHtmlImage => "text-html-image",
            Self::TextImageHtml => "text-image-html",
            Self::HtmlTextImage => "html-text-image",
            Self::HtmlImageText => "html-image-text",
            Self::ImageTextHtml => "image-text-html",
            Self::ImageHtmlText => "image-html-text",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClipboardWindowPosition {
    #[default]
    MouseCenter,
    MouseNearby,
    Remember,
    ScreenCenter,
    ScreenLeftCenter,
    ScreenRightCenter,
    ScreenTopCenter,
    ScreenBottomCenter,
}

impl ClipboardWindowPosition {
    pub const ALL: [Self; 8] = [
        Self::MouseCenter,
        Self::MouseNearby,
        Self::Remember,
        Self::ScreenCenter,
        Self::ScreenLeftCenter,
        Self::ScreenRightCenter,
        Self::ScreenTopCenter,
        Self::ScreenBottomCenter,
    ];

    pub const fn code(self) -> &'static str {
        match self {
            Self::MouseCenter => "mouse-center",
            Self::MouseNearby => "mouse-nearby",
            Self::Remember => "remember",
            Self::ScreenCenter => "screen-center",
            Self::ScreenLeftCenter => "screen-left-center",
            Self::ScreenRightCenter => "screen-right-center",
            Self::ScreenTopCenter => "screen-top-center",
            Self::ScreenBottomCenter => "screen-bottom-center",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClipboardMouseLeftBehavior {
    #[default]
    SinglePaste,
    SinglePlainTextPaste,
    SingleCopy,
    DoublePaste,
    DoublePlainTextPaste,
    DoubleCopy,
}

impl ClipboardMouseLeftBehavior {
    pub const ALL: [Self; 6] = [
        Self::SinglePaste,
        Self::SinglePlainTextPaste,
        Self::SingleCopy,
        Self::DoublePaste,
        Self::DoublePlainTextPaste,
        Self::DoubleCopy,
    ];

    pub const fn code(self) -> &'static str {
        match self {
            Self::SinglePaste => "single-paste",
            Self::SinglePlainTextPaste => "single-plain-text-paste",
            Self::SingleCopy => "single-copy",
            Self::DoublePaste => "double-paste",
            Self::DoublePlainTextPaste => "double-plain-text-paste",
            Self::DoubleCopy => "double-copy",
        }
    }

    pub const fn matches_click_count(self, click_count: usize) -> bool {
        match self {
            Self::SinglePaste | Self::SinglePlainTextPaste | Self::SingleCopy => click_count == 1,
            Self::DoublePaste | Self::DoublePlainTextPaste | Self::DoubleCopy => click_count == 2,
        }
    }

    pub const fn is_copy(self) -> bool {
        matches!(self, Self::SingleCopy | Self::DoubleCopy)
    }

    pub const fn is_plain_text_paste(self) -> bool {
        matches!(
            self,
            Self::SinglePlainTextPaste | Self::DoublePlainTextPaste
        )
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClipboardPreviewPriority {
    #[default]
    FollowDisplay,
    TextHtmlImage,
    TextImageHtml,
    HtmlTextImage,
    HtmlImageText,
    ImageTextHtml,
    ImageHtmlText,
}

impl ClipboardPreviewPriority {
    pub const ALL: [Self; 7] = [
        Self::FollowDisplay,
        Self::TextHtmlImage,
        Self::TextImageHtml,
        Self::HtmlTextImage,
        Self::HtmlImageText,
        Self::ImageTextHtml,
        Self::ImageHtmlText,
    ];

    pub const fn code(self) -> &'static str {
        match self {
            Self::FollowDisplay => "follow-display",
            Self::TextHtmlImage => "text-html-image",
            Self::TextImageHtml => "text-image-html",
            Self::HtmlTextImage => "html-text-image",
            Self::HtmlImageText => "html-image-text",
            Self::ImageTextHtml => "image-text-html",
            Self::ImageHtmlText => "image-html-text",
        }
    }

    pub const fn resolve(
        self,
        display_priority: ClipboardDisplayPriority,
    ) -> ClipboardDisplayPriority {
        match self {
            Self::FollowDisplay => display_priority,
            Self::TextHtmlImage => ClipboardDisplayPriority::TextHtmlImage,
            Self::TextImageHtml => ClipboardDisplayPriority::TextImageHtml,
            Self::HtmlTextImage => ClipboardDisplayPriority::HtmlTextImage,
            Self::HtmlImageText => ClipboardDisplayPriority::HtmlImageText,
            Self::ImageTextHtml => ClipboardDisplayPriority::ImageTextHtml,
            Self::ImageHtmlText => ClipboardDisplayPriority::ImageHtmlText,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClipboardMouseMiddleBehavior {
    #[default]
    PlainTextPaste,
    SelectItem,
    PinItem,
    FavoriteItem,
}

impl ClipboardMouseMiddleBehavior {
    pub const ALL: [Self; 4] = [
        Self::PlainTextPaste,
        Self::SelectItem,
        Self::PinItem,
        Self::FavoriteItem,
    ];

    pub const fn code(self) -> &'static str {
        match self {
            Self::PlainTextPaste => "plain-text-paste",
            Self::SelectItem => "select-item",
            Self::PinItem => "pin-item",
            Self::FavoriteItem => "favorite-item",
        }
    }
}

/// 用户自定义搜索引擎：name 为显示名，url_template 中的 {query} 会被替换为搜索词。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchEngine {
    pub name: String,
    pub url_template: String,
    /// 用户为自定义引擎上传的图标本地路径；为空时使用内置品牌图标或通用图标。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_path: Option<String>,
}

impl SearchEngine {
    /// 内置默认引擎对应的品牌图标名（assets/icons 下的 svg 文件名，不含扩展名）。
    /// 非内置引擎返回 None，由调用方回退到通用图标或用户上传图标。
    pub fn builtin_icon_name(&self) -> Option<&'static str> {
        let host = self.url_template.split('/').nth(2)?.to_ascii_lowercase();
        if host.contains("bing.com") {
            Some("brand-bing")
        } else if host.contains("baidu.com") {
            Some("brand-baidu")
        } else if host.contains("google.com") {
            Some("brand-google")
        } else {
            None
        }
    }
}

/// 内置默认搜索引擎。
fn default_search_engines() -> Vec<SearchEngine> {
    vec![
        SearchEngine {
            name: "Bing".to_owned(),
            url_template: "https://www.bing.com/search?q={query}".to_owned(),
            icon_path: None,
        },
        SearchEngine {
            name: "百度".to_owned(),
            url_template: "https://www.baidu.com/s?wd={query}".to_owned(),
            icon_path: None,
        },
        SearchEngine {
            name: "Google".to_owned(),
            url_template: "https://www.google.com/search?q={query}".to_owned(),
            icon_path: None,
        },
    ]
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct ClipboardConfig {
    pub mouse_left_behavior: ClipboardMouseLeftBehavior,
    pub mouse_middle_behavior: ClipboardMouseMiddleBehavior,
    pub display_priority: ClipboardDisplayPriority,
    pub preview_priority: ClipboardPreviewPriority,
    pub hover_preview_enabled: bool,
    pub page_hover_switch: bool,
    pub filter_hover_switch: bool,
    pub page_hover_delay_ms: u16,
    pub filter_hover_delay_ms: u16,
    pub move_pasted_to_first_in_clipboard: bool,
    pub move_pasted_to_first_in_favorites: bool,
    pub auto_scroll_to_top: bool,
    pub auto_focus_search: bool,
    pub auto_clear_search: bool,
    pub clear_tags_on_tab_switch: bool,
    pub confirm_favorite_removal: bool,
    pub window_position: ClipboardWindowPosition,
    pub edge_hide: bool,
    pub edge_hide_show_edge: bool,
    pub search_engines: Vec<SearchEngine>,
    /// 是否把图片以可直接打开的文件形式保存到数据目录。
    ///
    /// 开启后新捕获的图片不再压缩，文件名使用内容哈希加图片扩展名，可由图片软件直接打开。
    /// 只影响后续捕获，已有记录按原路径继续读取，切换开关不会重写历史文件。
    pub save_images_as_files: bool,
}

/// 当前历史范围的时间、条数限制和启动清理设置。
///
/// 超出范围的记录会进入待清理状态，可手动清理或启用启动清理；`None` 表示不启用对应限制。
/// 置顶历史始终保留；收藏关联独立保留在收藏页。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct HistoryConfig {
    pub retention_days: Option<u32>,
    pub max_items: Option<u32>,
    pub cleanup_on_startup: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct QuickPasteConfig {
    pub visible_items: u8,
    pub window_width: u16,
    pub select_with_up_down: bool,
    pub switch_list_with_left_right: bool,
    pub keep_last_position: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QuickPasteShortcutModifier {
    Alt,
    Control,
    Shift,
    AltControl,
    AltShift,
    ControlShift,
}

impl QuickPasteShortcutModifier {
    pub const ALL: [Self; 6] = [
        Self::Alt,
        Self::Control,
        Self::Shift,
        Self::AltControl,
        Self::AltShift,
        Self::ControlShift,
    ];

    pub const fn code(self) -> &'static str {
        match self {
            Self::Alt => "alt",
            Self::Control => "control",
            Self::Shift => "shift",
            Self::AltControl => "alt-control",
            Self::AltShift => "alt-shift",
            Self::ControlShift => "control-shift",
        }
    }

    pub const fn label_key(self) -> &'static str {
        match self {
            Self::Alt => "settings.shortcuts.quick_paste_1_to_9.modifier.alt",
            Self::Control => "settings.shortcuts.quick_paste_1_to_9.modifier.control",
            Self::Shift => "settings.shortcuts.quick_paste_1_to_9.modifier.shift",
            Self::AltControl => "settings.shortcuts.quick_paste_1_to_9.modifier.alt_control",
            Self::AltShift => "settings.shortcuts.quick_paste_1_to_9.modifier.alt_shift",
            Self::ControlShift => "settings.shortcuts.quick_paste_1_to_9.modifier.control_shift",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.code() == code)
    }

    pub(crate) fn binding_flags(self) -> (bool, bool, bool) {
        match self {
            Self::Alt => (false, true, false),
            Self::Control => (true, false, false),
            Self::Shift => (false, false, true),
            Self::AltControl => (true, true, false),
            Self::AltShift => (false, true, true),
            Self::ControlShift => (true, false, true),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MiddleMouseShortcutConfig {
    pub enabled: bool,
    pub modifier: Option<QuickPasteShortcutModifier>,
    pub hold_threshold_ms: u16,
}

impl Default for MiddleMouseShortcutConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            modifier: None,
            hold_threshold_ms: 200,
        }
    }
}

impl MiddleMouseShortcutConfig {
    pub(crate) fn normalized(mut self) -> Self {
        self.hold_threshold_ms = self.hold_threshold_ms.clamp(10, 1000);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QuickPasteShortcutKey {
    Digit,
    Function,
}

impl QuickPasteShortcutKey {
    pub const ALL: [Self; 2] = [Self::Digit, Self::Function];

    pub const fn code(self) -> &'static str {
        match self {
            Self::Digit => "digit",
            Self::Function => "function",
        }
    }

    pub const fn label_key(self) -> &'static str {
        match self {
            Self::Digit => "settings.shortcuts.quick_paste_1_to_9.key.digit",
            Self::Function => "settings.shortcuts.quick_paste_1_to_9.key.function",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.code() == code)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct QuickPasteShortcutConfig {
    pub modifier: QuickPasteShortcutModifier,
    pub key: QuickPasteShortcutKey,
    pub enabled: bool,
    pub include_pinned: bool,
}

impl Default for QuickPasteShortcutConfig {
    fn default() -> Self {
        Self {
            modifier: QuickPasteShortcutModifier::Control,
            key: QuickPasteShortcutKey::Digit,
            enabled: false,
            include_pinned: true,
        }
    }
}

impl QuickPasteShortcutConfig {
    pub fn binding(self, index: usize) -> ShortcutBinding {
        let (control, alt, shift) = self.modifier.binding_flags();
        let key = match self.key {
            QuickPasteShortcutKey::Digit => (index + 1).to_string(),
            QuickPasteShortcutKey::Function => format!("f{}", index + 1),
        };
        ShortcutBinding {
            control,
            alt,
            shift,
            platform: false,
            key,
        }
    }
}

impl QuickPasteConfig {
    pub const VISIBLE_ITEM_COUNTS: [u8; 7] = [3, 5, 7, 9, 11, 13, 15];
    pub const WINDOW_WIDTHS: [u16; 8] = [240, 280, 300, 320, 360, 400, 440, 480];

    pub(crate) fn normalized(self) -> Self {
        let visible_items = if Self::VISIBLE_ITEM_COUNTS.contains(&self.visible_items) {
            self.visible_items
        } else {
            Self::VISIBLE_ITEM_COUNTS
                .into_iter()
                .min_by_key(|candidate| candidate.abs_diff(self.visible_items))
                .unwrap_or_default()
        };
        let window_width = if Self::WINDOW_WIDTHS.contains(&self.window_width) {
            self.window_width
        } else {
            Self::WINDOW_WIDTHS
                .into_iter()
                .min_by_key(|candidate| candidate.abs_diff(self.window_width))
                .unwrap_or_default()
        };
        Self {
            visible_items,
            window_width,
            select_with_up_down: self.select_with_up_down,
            switch_list_with_left_right: self.switch_list_with_left_right,
            keep_last_position: self.keep_last_position,
        }
    }
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            retention_days: None,
            max_items: None,
            cleanup_on_startup: false,
        }
    }
}

impl Default for QuickPasteConfig {
    fn default() -> Self {
        Self {
            visible_items: 7,
            window_width: 300,
            select_with_up_down: true,
            switch_list_with_left_right: true,
            keep_last_position: false,
        }
    }
}

impl Default for ClipboardConfig {
    fn default() -> Self {
        Self {
            mouse_left_behavior: ClipboardMouseLeftBehavior::default(),
            mouse_middle_behavior: ClipboardMouseMiddleBehavior::default(),
            display_priority: ClipboardDisplayPriority::default(),
            preview_priority: ClipboardPreviewPriority::default(),
            hover_preview_enabled: true,
            page_hover_switch: false,
            filter_hover_switch: false,
            page_hover_delay_ms: Self::TAB_BAR_HOVER_DELAY_DEFAULT_MS,
            filter_hover_delay_ms: Self::TAB_BAR_HOVER_DELAY_DEFAULT_MS,
            move_pasted_to_first_in_clipboard: false,
            move_pasted_to_first_in_favorites: false,
            auto_scroll_to_top: false,
            auto_focus_search: false,
            auto_clear_search: false,
            clear_tags_on_tab_switch: false,
            confirm_favorite_removal: true,
            window_position: ClipboardWindowPosition::default(),
            edge_hide: false,
            edge_hide_show_edge: true,
            search_engines: Vec::new(),
            save_images_as_files: false,
        }
    }
}

impl ClipboardConfig {
    pub const TAB_BAR_HOVER_DELAY_MIN_MS: u16 = 0;
    pub const TAB_BAR_HOVER_DELAY_MAX_MS: u16 = 1000;
    pub const TAB_BAR_HOVER_DELAY_DEFAULT_MS: u16 = 40;

    pub(crate) fn normalized(mut self) -> Self {
        self.page_hover_delay_ms = self.page_hover_delay_ms.clamp(
            Self::TAB_BAR_HOVER_DELAY_MIN_MS,
            Self::TAB_BAR_HOVER_DELAY_MAX_MS,
        );
        self.filter_hover_delay_ms = self.filter_hover_delay_ms.clamp(
            Self::TAB_BAR_HOVER_DELAY_MIN_MS,
            Self::TAB_BAR_HOVER_DELAY_MAX_MS,
        );
        self
    }

    const fn is_page_button(button: TabBarButton) -> bool {
        matches!(button.group(), TabBarButtonGroup::Pages)
    }

    /// 按钮所属的栏是否启用了悬停切换。页面按钮归页面栏，筛选、符号分类和标签面板归筛选栏。
    pub const fn hover_switch_enabled(&self, button: TabBarButton) -> bool {
        if Self::is_page_button(button) {
            self.page_hover_switch
        } else {
            self.filter_hover_switch
        }
    }

    /// 按钮所属栏的悬停停留时间。
    pub const fn hover_delay_ms(&self, button: TabBarButton) -> u16 {
        if Self::is_page_button(button) {
            self.page_hover_delay_ms
        } else {
            self.filter_hover_delay_ms
        }
    }

    /// 搜索引擎为空时回落到内置默认列表。
    pub fn search_engines_or_default(&self) -> Vec<SearchEngine> {
        if self.search_engines.is_empty() {
            default_search_engines()
        } else {
            self.search_engines.clone()
        }
    }
}

pub const SOUND_VOLUME_MAX: u8 = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoundPreset {
    Disabled,
    CrispChime,
    BrightPing,
    QuickPasteScroll,
    External(String),
}

impl Default for SoundPreset {
    fn default() -> Self {
        Self::CrispChime
    }
}

impl SoundPreset {
    pub const ALL: [Self; 3] = [Self::Disabled, Self::BrightPing, Self::CrispChime];

    pub const fn builtin_code(&self) -> Option<&'static str> {
        match self {
            Self::Disabled => Some("disabled"),
            Self::CrispChime => Some("crisp-chime"),
            Self::BrightPing => Some("bright-ping"),
            Self::QuickPasteScroll => Some("quick-paste-scroll"),
            Self::External(_) => None,
        }
    }

    pub fn selection_id(&self) -> String {
        match self.builtin_code() {
            Some(code) => code.to_owned(),
            None => format!("external:{}", self.external_file_name().unwrap_or_default()),
        }
    }

    pub fn from_selection_id(value: &str) -> Option<Self> {
        match value {
            "disabled" => Some(Self::Disabled),
            "crisp-chime" => Some(Self::CrispChime),
            "bright-ping" => Some(Self::BrightPing),
            "quick-paste-scroll" => Some(Self::QuickPasteScroll),
            value if value.starts_with("external:") => {
                let file_name = value.strip_prefix("external:")?;
                Self::is_valid_external_file_name(file_name)
                    .then(|| Self::External(file_name.to_owned()))
            }
            _ => None,
        }
    }

    pub(crate) fn is_valid_external_file_name(file_name: &str) -> bool {
        let path = std::path::Path::new(file_name);
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase());
        !file_name.is_empty()
            && file_name != "."
            && file_name != ".."
            && !file_name.contains(['/', '\\', '\0', ':'])
            && path.file_stem().is_some_and(|stem| {
                !stem.is_empty() && Some(stem.to_string_lossy().to_ascii_lowercase()) != extension
            })
            && matches!(extension.as_deref(), Some("wav" | "mp3"))
    }

    pub fn external_file_name(&self) -> Option<&str> {
        match self {
            Self::External(file_name) => Some(file_name),
            _ => None,
        }
    }

    pub fn display_name(&self) -> String {
        self.external_file_name()
            .and_then(|file_name| {
                std::path::Path::new(file_name)
                    .file_stem()
                    .and_then(|stem| stem.to_str())
            })
            .unwrap_or_default()
            .to_owned()
    }
}

impl Serialize for SoundPreset {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.selection_id())
    }
}

impl<'de> Deserialize<'de> for SoundPreset {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_selection_id(&value)
            .ok_or_else(|| serde::de::Error::custom(format!("不支持的音效预设：{value}")))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct SoundEffectConfig {
    pub preset: SoundPreset,
    pub volume: u8,
}

impl Default for SoundEffectConfig {
    fn default() -> Self {
        Self {
            preset: SoundPreset::default(),
            volume: 70,
        }
    }
}

impl SoundEffectConfig {
    pub(crate) fn normalized(mut self) -> Self {
        self.volume = self.volume.min(SOUND_VOLUME_MAX);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct SoundConfig {
    pub enabled: bool,
    pub paste: SoundEffectConfig,
    pub copy: SoundEffectConfig,
    pub quick_paste_scroll: SoundEffectConfig,
}

impl Default for SoundConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            paste: SoundEffectConfig {
                preset: SoundPreset::CrispChime,
                volume: 70,
            },
            copy: SoundEffectConfig {
                preset: SoundPreset::BrightPing,
                volume: 70,
            },
            quick_paste_scroll: SoundEffectConfig {
                preset: SoundPreset::QuickPasteScroll,
                volume: 70,
            },
        }
    }
}

impl SoundConfig {
    pub(crate) fn normalized(mut self) -> Self {
        self.paste = self.paste.normalized();
        self.copy = self.copy.normalized();
        self.quick_paste_scroll = self.quick_paste_scroll.normalized();
        self
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppearanceMode {
    #[default]
    System,
    Light,
    Dark,
}

impl AppearanceMode {
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    pub const fn code(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WindowMaterial {
    #[default]
    Disabled,
    /// 不叠加原生窗口材质，只使用主题推导出的透明表面。
    Transparent,
    Acrylic,
    Blur,
    /// 用户选择的背景图 + 模糊。
    CustomImage,
}

impl WindowMaterial {
    pub const ALL: [Self; 5] = [
        Self::Disabled,
        Self::Transparent,
        Self::Acrylic,
        Self::Blur,
        Self::CustomImage,
    ];
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TitlebarPosition {
    #[default]
    Top,
    Bottom,
    Left,
    Right,
}

impl TitlebarPosition {
    pub const ALL: [Self; 4] = [Self::Top, Self::Bottom, Self::Left, Self::Right];

    pub const fn code(self) -> &'static str {
        match self {
            Self::Top => "top",
            Self::Bottom => "bottom",
            Self::Left => "left",
            Self::Right => "right",
        }
    }

    pub const fn is_vertical(self) -> bool {
        matches!(self, Self::Left | Self::Right)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ListItemStyle {
    Seamless,
    #[default]
    Card,
}

impl ListItemStyle {
    pub const ALL: [Self; 2] = [Self::Seamless, Self::Card];
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ListItemHeight {
    #[default]
    Adaptive,
    Large,
    Medium,
    Small,
    ExtraSmall,
}

impl ListItemHeight {
    pub const ALL: [Self; 5] = [
        Self::Adaptive,
        Self::Large,
        Self::Medium,
        Self::Small,
        Self::ExtraSmall,
    ];

    pub const fn fixed_preview_lines(self) -> Option<usize> {
        match self {
            Self::Adaptive => None,
            Self::Large => Some(4),
            Self::Medium => Some(3),
            Self::Small => Some(2),
            Self::ExtraSmall => Some(1),
        }
    }

    pub const fn shows_meta(self) -> bool {
        !matches!(self, Self::Small | Self::ExtraSmall)
    }
}

pub const ADAPTIVE_MAX_LINES_MIN: u8 = 1;
pub const ADAPTIVE_MAX_LINES_DEFAULT: u8 = 8;
pub const ADAPTIVE_MAX_LINES_MAX: u8 = 15;

/// 窗口表面不透明度的可调范围，取值即窗口表面的实际不透明度。
pub const WINDOW_MATERIAL_OPACITY_MIN: f32 = 0.1;
/// 透明材质默认窗口表面不透明度；其他启用材质使用 `WINDOW_MATERIAL_ACTIVE_OPACITY_DEFAULT`。
pub const WINDOW_MATERIAL_OPACITY_DEFAULT: f32 = 0.35;
/// 亚克力、模糊和自定义背景图共用的默认窗口表面不透明度。
pub const WINDOW_MATERIAL_ACTIVE_OPACITY_DEFAULT: f32 = 0.15;
pub const WINDOW_MATERIAL_OPACITY_MAX: f32 = 1.0;

/// 背景图模糊半径下限，0 表示不模糊。
pub const BACKGROUND_IMAGE_BLUR_MIN: f32 = 0.0;
pub const BACKGROUND_IMAGE_BLUR_DEFAULT: f32 = 8.0;
pub const BACKGROUND_IMAGE_BLUR_MAX: f32 = 48.0;

/// 窗口材质明暗度下限（叠黑罩压暗）。
pub const WINDOW_MATERIAL_BRIGHTNESS_MIN: f32 = 0.0;
/// 启用材质的默认明暗度，100% 表示中性明暗。
pub const WINDOW_MATERIAL_BRIGHTNESS_DEFAULT: f32 = 1.0;
/// 窗口材质明暗度上限（叠白罩提亮）。
pub const WINDOW_MATERIAL_BRIGHTNESS_MAX: f32 = 2.0;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CardItemGap {
    #[serde(rename = "0px")]
    Zero,
    #[serde(rename = "4px")]
    Four,
    #[default]
    #[serde(rename = "8px")]
    Eight,
    #[serde(rename = "12px")]
    Twelve,
    #[serde(rename = "16px")]
    Sixteen,
    #[serde(rename = "20px")]
    Twenty,
}

impl CardItemGap {
    pub const ALL: [Self; 6] = [
        Self::Zero,
        Self::Four,
        Self::Eight,
        Self::Twelve,
        Self::Sixteen,
        Self::Twenty,
    ];

    pub const fn code(self) -> &'static str {
        match self {
            Self::Zero => "0px",
            Self::Four => "4px",
            Self::Eight => "8px",
            Self::Twelve => "12px",
            Self::Sixteen => "16px",
            Self::Twenty => "20px",
        }
    }

    pub const fn pixels(self) -> f32 {
        match self {
            Self::Zero => 0.0,
            Self::Four => 4.0,
            Self::Eight => 8.0,
            Self::Twelve => 12.0,
            Self::Sixteen => 16.0,
            Self::Twenty => 20.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct AppearanceConfig {
    pub mode: AppearanceMode,
    pub ui_scale_percent: u16,
    pub app_font_family: Option<String>,
    pub light_theme: String,
    pub dark_theme: String,
    pub titlebar_position: TitlebarPosition,
    pub tab_bar: TabBarLayout,
    pub animations: AnimationConfig,
    pub gallery_gif_autoplay: bool,
    pub window_material: WindowMaterial,
    pub window_material_opacity: f32,
    pub background_image_path: Option<String>,
    pub background_image_blur: f32,
    /// 用户手动设置的窗口材质明暗度；未设置时使用内置默认值。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_material_brightness: Option<f32>,
    pub list_item_style: ListItemStyle,
    pub card_item_gap: CardItemGap,
    pub list_item_height: ListItemHeight,
    pub adaptive_max_lines: u8,
    pub only_show_notes: bool,
    pub item_display: ItemDisplayConfig,
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            mode: AppearanceMode::default(),
            ui_scale_percent: 100,
            app_font_family: None,
            light_theme: "QcClip Default Light".to_owned(),
            dark_theme: "QcClip Default Dark".to_owned(),
            titlebar_position: TitlebarPosition::default(),
            tab_bar: TabBarLayout::default(),
            animations: AnimationConfig::default(),
            gallery_gif_autoplay: false,
            window_material: WindowMaterial::default(),
            window_material_opacity: default_window_material_opacity(WindowMaterial::default()),
            background_image_path: None,
            background_image_blur: BACKGROUND_IMAGE_BLUR_DEFAULT,
            window_material_brightness: None,
            list_item_style: ListItemStyle::default(),
            card_item_gap: CardItemGap::default(),
            list_item_height: ListItemHeight::default(),
            adaptive_max_lines: ADAPTIVE_MAX_LINES_DEFAULT,
            only_show_notes: false,
            item_display: ItemDisplayConfig::default(),
        }
    }
}

/// 返回材质首次启用时使用的默认不透明度。
///
/// 亚克力、模糊和自定义背景图的默认值相同，避免切换材质时重复维护分支。
pub const fn default_window_material_opacity(material: WindowMaterial) -> f32 {
    match material {
        WindowMaterial::Acrylic | WindowMaterial::Blur | WindowMaterial::CustomImage => {
            WINDOW_MATERIAL_ACTIVE_OPACITY_DEFAULT
        }
        WindowMaterial::Disabled | WindowMaterial::Transparent => WINDOW_MATERIAL_OPACITY_DEFAULT,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct AnimationConfig {
    pub window_visibility: bool,
    pub menu_popup: bool,
    pub quick_paste: bool,
    pub preview_window: bool,
    pub theme_transition: bool,
    pub list_item_hover: bool,
    pub list_item_load: bool,
    pub list_item_reorder: bool,
    pub smooth_scroll: bool,
    pub tag_item_reorder: bool,
    pub tag_indicator_hover: bool,
    pub floating_toolbar: bool,
    pub settings_page_transition: bool,
    pub icon_hover_scale: bool,
    pub notification: bool,
    pub single_select_indicator: bool,
    pub list_actions: bool,
}

impl Default for AnimationConfig {
    fn default() -> Self {
        Self {
            window_visibility: true,
            menu_popup: true,
            quick_paste: true,
            preview_window: true,
            theme_transition: true,
            list_item_hover: true,
            list_item_load: false,
            list_item_reorder: true,
            smooth_scroll: true,
            tag_item_reorder: true,
            tag_indicator_hover: true,
            floating_toolbar: true,
            settings_page_transition: true,
            icon_hover_scale: true,
            notification: true,
            single_select_indicator: true,
            list_actions: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimationKind {
    WindowVisibility,
    MenuPopup,
    QuickPaste,
    PreviewWindow,
    ThemeTransition,
    ListItemHover,
    ListItemLoad,
    ListItemReorder,
    SmoothScroll,
    TagItemReorder,
    TagIndicatorHover,
    FloatingToolbar,
    SettingsPageTransition,
    IconHoverScale,
    Notification,
    SingleSelectIndicator,
    ListActions,
}

impl AnimationConfig {
    pub const ALL: [AnimationKind; 17] = [
        AnimationKind::WindowVisibility,
        AnimationKind::MenuPopup,
        AnimationKind::QuickPaste,
        AnimationKind::PreviewWindow,
        AnimationKind::ThemeTransition,
        AnimationKind::ListItemHover,
        AnimationKind::ListItemLoad,
        AnimationKind::ListItemReorder,
        AnimationKind::SmoothScroll,
        AnimationKind::TagItemReorder,
        AnimationKind::TagIndicatorHover,
        AnimationKind::FloatingToolbar,
        AnimationKind::SettingsPageTransition,
        AnimationKind::IconHoverScale,
        AnimationKind::Notification,
        AnimationKind::SingleSelectIndicator,
        AnimationKind::ListActions,
    ];

    pub const fn enabled(&self, kind: AnimationKind) -> bool {
        match kind {
            AnimationKind::WindowVisibility => self.window_visibility,
            AnimationKind::MenuPopup => self.menu_popup,
            AnimationKind::QuickPaste => self.quick_paste,
            AnimationKind::PreviewWindow => self.preview_window,
            AnimationKind::ThemeTransition => self.theme_transition,
            AnimationKind::ListItemHover => self.list_item_hover,
            AnimationKind::ListItemLoad => self.list_item_load,
            AnimationKind::ListItemReorder => self.list_item_reorder,
            AnimationKind::SmoothScroll => self.smooth_scroll,
            AnimationKind::TagItemReorder => self.tag_item_reorder,
            AnimationKind::TagIndicatorHover => self.tag_indicator_hover,
            AnimationKind::FloatingToolbar => self.floating_toolbar,
            AnimationKind::SettingsPageTransition => self.settings_page_transition,
            AnimationKind::IconHoverScale => self.icon_hover_scale,
            AnimationKind::Notification => self.notification,
            AnimationKind::SingleSelectIndicator => self.single_select_indicator,
            AnimationKind::ListActions => self.list_actions,
        }
    }

    pub fn set_enabled(&mut self, kind: AnimationKind, enabled: bool) {
        match kind {
            AnimationKind::WindowVisibility => self.window_visibility = enabled,
            AnimationKind::MenuPopup => self.menu_popup = enabled,
            AnimationKind::QuickPaste => self.quick_paste = enabled,
            AnimationKind::PreviewWindow => self.preview_window = enabled,
            AnimationKind::ThemeTransition => self.theme_transition = enabled,
            AnimationKind::ListItemHover => self.list_item_hover = enabled,
            AnimationKind::ListItemLoad => self.list_item_load = enabled,
            AnimationKind::ListItemReorder => self.list_item_reorder = enabled,
            AnimationKind::SmoothScroll => self.smooth_scroll = enabled,
            AnimationKind::TagItemReorder => self.tag_item_reorder = enabled,
            AnimationKind::TagIndicatorHover => self.tag_indicator_hover = enabled,
            AnimationKind::FloatingToolbar => self.floating_toolbar = enabled,
            AnimationKind::SettingsPageTransition => self.settings_page_transition = enabled,
            AnimationKind::IconHoverScale => self.icon_hover_scale = enabled,
            AnimationKind::Notification => self.notification = enabled,
            AnimationKind::SingleSelectIndicator => self.single_select_indicator = enabled,
            AnimationKind::ListActions => self.list_actions = enabled,
        }
    }
}

impl AnimationKind {
    pub const fn config_key(self) -> &'static str {
        match self {
            Self::WindowVisibility => "windowVisibility",
            Self::MenuPopup => "menuPopup",
            Self::QuickPaste => "quickPaste",
            Self::PreviewWindow => "previewWindow",
            Self::ThemeTransition => "themeTransition",
            Self::ListItemHover => "listItemHover",
            Self::ListItemLoad => "listItemLoad",
            Self::ListItemReorder => "listItemReorder",
            Self::SmoothScroll => "smoothScroll",
            Self::TagItemReorder => "tagItemReorder",
            Self::TagIndicatorHover => "tagIndicatorHover",
            Self::FloatingToolbar => "floatingToolbar",
            Self::SettingsPageTransition => "settingsPageTransition",
            Self::IconHoverScale => "iconHoverScale",
            Self::Notification => "notification",
            Self::SingleSelectIndicator => "singleSelectIndicator",
            Self::ListActions => "listActions",
        }
    }

    pub const fn label_key(self) -> &'static str {
        match self {
            Self::WindowVisibility => "settings.personalization.animation.window_visibility",
            Self::MenuPopup => "settings.personalization.animation.menu_popup",
            Self::QuickPaste => "settings.personalization.animation.quick_paste",
            Self::PreviewWindow => "settings.personalization.animation.preview_window",
            Self::ThemeTransition => "settings.personalization.animation.theme_transition",
            Self::ListItemHover => "settings.personalization.animation.list_item_hover",
            Self::ListItemLoad => "settings.personalization.animation.list_item_load",
            Self::ListItemReorder => "settings.personalization.animation.list_item_reorder",
            Self::SmoothScroll => "settings.personalization.animation.smooth_scroll",
            Self::TagItemReorder => "settings.personalization.animation.tag_item_reorder",
            Self::TagIndicatorHover => "settings.personalization.animation.tag_indicator_hover",
            Self::FloatingToolbar => "settings.personalization.animation.floating_toolbar",
            Self::SettingsPageTransition => {
                "settings.personalization.animation.settings_page_transition"
            }
            Self::IconHoverScale => "settings.personalization.animation.icon_hover_scale",
            Self::Notification => "settings.personalization.animation.notification",
            Self::SingleSelectIndicator => {
                "settings.personalization.animation.single_select_indicator"
            }
            Self::ListActions => "settings.personalization.animation.list_actions",
        }
    }

    pub const fn description_key(self) -> &'static str {
        match self {
            Self::WindowVisibility => {
                "settings.personalization.animation.window_visibility_description"
            }
            Self::MenuPopup => "settings.personalization.animation.menu_popup_description",
            Self::QuickPaste => "settings.personalization.animation.quick_paste_description",
            Self::PreviewWindow => "settings.personalization.animation.preview_window_description",
            Self::ThemeTransition => {
                "settings.personalization.animation.theme_transition_description"
            }
            Self::ListItemHover => "settings.personalization.animation.list_item_hover_description",
            Self::ListItemLoad => "settings.personalization.animation.list_item_load_description",
            Self::ListItemReorder => {
                "settings.personalization.animation.list_item_reorder_description"
            }
            Self::SmoothScroll => "settings.personalization.animation.smooth_scroll_description",
            Self::TagItemReorder => {
                "settings.personalization.animation.tag_item_reorder_description"
            }
            Self::TagIndicatorHover => {
                "settings.personalization.animation.tag_indicator_hover_description"
            }
            Self::FloatingToolbar => {
                "settings.personalization.animation.floating_toolbar_description"
            }
            Self::SettingsPageTransition => {
                "settings.personalization.animation.settings_page_transition_description"
            }
            Self::IconHoverScale => {
                "settings.personalization.animation.icon_hover_scale_description"
            }
            Self::Notification => "settings.personalization.animation.notification_description",
            Self::SingleSelectIndicator => {
                "settings.personalization.animation.single_select_indicator_description"
            }
            Self::ListActions => "settings.personalization.animation.list_actions_description",
        }
    }
}

pub const UI_SCALE_PERCENT_MIN: u16 = 80;
pub const UI_SCALE_PERCENT_MAX: u16 = 200;
pub const UI_SCALE_PRESETS: [u16; 8] = [80, 90, 100, 110, 125, 150, 175, 200];

impl AppearanceConfig {
    pub fn ui_scale(&self) -> f32 {
        f32::from(
            self.ui_scale_percent
                .clamp(UI_SCALE_PERCENT_MIN, UI_SCALE_PERCENT_MAX),
        ) / 100.0
    }

    pub(crate) fn normalized(mut self) -> Self {
        let default = Self::default();
        self.tab_bar.normalize();
        self.ui_scale_percent = self
            .ui_scale_percent
            .clamp(UI_SCALE_PERCENT_MIN, UI_SCALE_PERCENT_MAX);
        self.app_font_family = self.app_font_family.and_then(|family| {
            let family = family.trim();
            (!family.is_empty()).then(|| family.to_owned())
        });
        self.adaptive_max_lines = self
            .adaptive_max_lines
            .clamp(ADAPTIVE_MAX_LINES_MIN, ADAPTIVE_MAX_LINES_MAX);
        self.window_material_opacity = if self.window_material_opacity.is_finite() {
            self.window_material_opacity
                .clamp(WINDOW_MATERIAL_OPACITY_MIN, WINDOW_MATERIAL_OPACITY_MAX)
        } else {
            default_window_material_opacity(self.window_material)
        };
        if self.light_theme.trim().is_empty() {
            self.light_theme = default.light_theme;
        } else {
            self.light_theme = self.light_theme.trim().to_owned();
        }
        if self.dark_theme.trim().is_empty() {
            self.dark_theme = default.dark_theme;
        } else {
            self.dark_theme = self.dark_theme.trim().to_owned();
        }
        self.background_image_path = self
            .background_image_path
            .and_then(|path| (!path.trim().is_empty()).then(|| path.trim().to_owned()));
        self.background_image_blur = if self.background_image_blur.is_finite() {
            self.background_image_blur
                .clamp(BACKGROUND_IMAGE_BLUR_MIN, BACKGROUND_IMAGE_BLUR_MAX)
        } else {
            BACKGROUND_IMAGE_BLUR_DEFAULT
        };
        self.window_material_brightness = self.window_material_brightness.and_then(|brightness| {
            brightness.is_finite().then(|| {
                brightness.clamp(
                    WINDOW_MATERIAL_BRIGHTNESS_MIN,
                    WINDOW_MATERIAL_BRIGHTNESS_MAX,
                )
            })
        });
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ADAPTIVE_MAX_LINES_DEFAULT, ADAPTIVE_MAX_LINES_MAX, AppConfig, AppearanceConfig,
        AppearanceMode, CardItemGap, ClipboardDisplayPriority, ClipboardMouseLeftBehavior,
        ClipboardWindowPosition, InterfaceLanguage, ListItemHeight, ListItemStyle,
        QuickPasteConfig, SoundPreset, TitlebarPosition, WINDOW_MATERIAL_OPACITY_DEFAULT,
        WINDOW_MATERIAL_OPACITY_MAX, WindowMaterial,
    };

    #[test]
    fn window_material_opacity_is_normalized_to_the_supported_range() {
        let too_large = AppearanceConfig {
            window_material_opacity: 10.0,
            ..AppearanceConfig::default()
        }
        .normalized();
        assert_eq!(
            too_large.window_material_opacity,
            WINDOW_MATERIAL_OPACITY_MAX
        );

        let not_a_number = AppearanceConfig {
            window_material_opacity: f32::NAN,
            ..AppearanceConfig::default()
        }
        .normalized();
        assert_eq!(
            not_a_number.window_material_opacity,
            WINDOW_MATERIAL_OPACITY_DEFAULT
        );
    }

    #[test]
    fn default_config_uses_the_current_runtime_defaults() {
        let config = AppConfig::default();

        assert_eq!(config.general.language, InterfaceLanguage::ZhCn);
        assert!(config.general.global_notification_enabled);
        assert_eq!(
            config.clipboard.display_priority,
            ClipboardDisplayPriority::TextHtmlImage
        );
        assert_eq!(
            config.clipboard.mouse_left_behavior,
            ClipboardMouseLeftBehavior::SinglePaste
        );
        assert_eq!(
            config.clipboard.window_position,
            ClipboardWindowPosition::MouseCenter
        );
        assert!(!config.clipboard.edge_hide);
        assert_eq!(config.quick_paste.visible_items, 7);
        assert!(config.sound.enabled);
        assert_eq!(config.sound.paste.preset, SoundPreset::CrispChime);
        assert_eq!(config.sound.copy.preset, SoundPreset::BrightPing);
        assert_eq!(config.appearance.mode, AppearanceMode::System);
        assert_eq!(config.appearance.titlebar_position, TitlebarPosition::Top);
        assert!(config.appearance.animations.window_visibility);
        assert_eq!(config.appearance.window_material, WindowMaterial::Disabled);
        assert_eq!(config.appearance.list_item_style, ListItemStyle::Card);
        assert_eq!(config.appearance.card_item_gap, CardItemGap::Eight);
        assert_eq!(config.appearance.list_item_height, ListItemHeight::Adaptive);
        assert_eq!(
            config.appearance.adaptive_max_lines,
            ADAPTIVE_MAX_LINES_DEFAULT
        );
        assert_eq!(config.schema_version, 1);
    }

    #[test]
    fn config_serialization_uses_stable_field_names() {
        let value = serde_json::to_value(AppConfig::default()).expect("配置序列化失败");

        assert_eq!(value["schemaVersion"], 1);
        assert_eq!(value["general"]["language"], "zh-CN");
        assert_eq!(value["general"]["globalNotificationEnabled"], true);
        assert_eq!(value["clipboard"]["displayPriority"], "text-html-image");
        assert_eq!(value["clipboard"]["mouseLeftBehavior"], "single-paste");
        assert_eq!(value["clipboard"]["pageHoverDelayMs"], 40);
        assert_eq!(value["clipboard"]["filterHoverDelayMs"], 40);
        assert_eq!(value["clipboard"]["edgeHide"], false);
        assert_eq!(value["quickPaste"]["visibleItems"], 7);
        assert_eq!(value["sound"]["enabled"], true);
        assert_eq!(value["sound"]["paste"]["preset"], "crisp-chime");
        assert_eq!(value["sound"]["paste"]["volume"], 70);
        assert_eq!(value["sound"]["copy"]["volume"], 70);
        assert_eq!(value["appearance"]["lightTheme"], "QcClip Default Light");
        assert_eq!(value["appearance"]["windowMaterial"], "disabled");
        assert_eq!(
            value["appearance"]["windowMaterialOpacity"],
            WINDOW_MATERIAL_OPACITY_DEFAULT
        );
        assert_eq!(value["appearance"]["titlebarPosition"], "top");
        assert_eq!(value["appearance"]["animations"]["windowVisibility"], true);
        assert_eq!(value["appearance"]["listItemStyle"], "card");
        assert_eq!(value["appearance"]["cardItemGap"], "8px");
        assert_eq!(value["appearance"]["listItemHeight"], "adaptive");
        assert_eq!(
            value["appearance"]["adaptiveMaxLines"],
            ADAPTIVE_MAX_LINES_DEFAULT
        );
    }

    #[test]
    fn mouse_left_behavior_matches_only_configured_click_count() {
        use ClipboardMouseLeftBehavior::*;

        let cases = [
            (SinglePaste, 1, false, false),
            (SinglePlainTextPaste, 1, false, true),
            (SingleCopy, 1, true, false),
            (DoublePaste, 2, false, false),
            (DoublePlainTextPaste, 2, false, true),
            (DoubleCopy, 2, true, false),
        ];
        assert_eq!(ClipboardMouseLeftBehavior::ALL, cases.map(|case| case.0));
        for (behavior, click_count, is_copy, is_plain_text_paste) in cases {
            for count in 0..=3 {
                assert_eq!(behavior.matches_click_count(count), count == click_count);
            }
            assert_eq!(behavior.is_copy(), is_copy);
            assert_eq!(behavior.is_plain_text_paste(), is_plain_text_paste);
            let saved = serde_json::to_value(behavior).expect("左键行为应当可以保存");
            assert_eq!(saved, behavior.code());
        }
    }

    #[test]
    fn empty_theme_ids_are_normalized_to_defaults() {
        let mut config = AppConfig::default();
        config.appearance.light_theme = "  ".to_owned();
        config.appearance.dark_theme = "  custom  ".to_owned();

        let config = config.normalized();
        assert_eq!(config.appearance.light_theme, "QcClip Default Light");
        assert_eq!(config.appearance.dark_theme, "custom");
    }

    #[test]
    fn adaptive_max_lines_are_normalized_to_the_supported_range() {
        let mut config = AppConfig::default();
        config.appearance.adaptive_max_lines = u8::MAX;

        let config = config.normalized();

        assert_eq!(config.appearance.adaptive_max_lines, ADAPTIVE_MAX_LINES_MAX);
    }

    #[test]
    fn sound_volumes_are_normalized_to_the_supported_range() {
        let mut config = AppConfig::default();
        config.sound.paste.volume = u8::MAX;

        let config = config.normalized();

        assert_eq!(config.sound.paste.volume, 100);
    }

    #[test]
    fn external_sound_selection_accepts_only_supported_file_names() {
        assert_eq!(
            SoundPreset::from_selection_id("external:轻快提示.WAV"),
            Some(SoundPreset::External("轻快提示.WAV".to_owned()))
        );
        for file_name in [
            "external:提示.ogg",
            "external:../提示.wav",
            "external:子目录\\提示.wav",
            "external:.wav",
            "external:",
        ] {
            assert!(SoundPreset::from_selection_id(file_name).is_none());
        }
    }

    #[test]
    fn external_sound_selection_is_serialized_for_persistence() {
        let preset = SoundPreset::External("清脆提示.mp3".to_owned());
        assert_eq!(
            serde_json::to_string(&preset).expect("音效预设序列化失败"),
            "\"external:清脆提示.mp3\""
        );
        assert_eq!(
            serde_json::from_str::<SoundPreset>("\"external:清脆提示.mp3\"")
                .expect("音效预设反序列化失败"),
            preset
        );
    }

    #[test]
    fn list_item_height_presets_have_expected_preview_lines_and_meta_visibility() {
        assert_eq!(ListItemHeight::Adaptive.fixed_preview_lines(), None);
        assert_eq!(ListItemHeight::Large.fixed_preview_lines(), Some(4));
        assert_eq!(ListItemHeight::Medium.fixed_preview_lines(), Some(3));
        assert_eq!(ListItemHeight::Small.fixed_preview_lines(), Some(2));
        assert_eq!(ListItemHeight::ExtraSmall.fixed_preview_lines(), Some(1));
        assert!(ListItemHeight::Adaptive.shows_meta());
        assert!(ListItemHeight::Large.shows_meta());
        assert!(ListItemHeight::Medium.shows_meta());
        assert!(!ListItemHeight::Small.shows_meta());
        assert!(!ListItemHeight::ExtraSmall.shows_meta());
    }
}
