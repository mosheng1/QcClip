use std::{
    borrow::{Borrow, BorrowMut},
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

use atomic_write_file::AtomicWriteFile;
use gpui::{App, AppContext, BorrowAppContext, Global, Task};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::warn;

use crate::{
    config::ClipboardListLayout,
    services::word_split::{JoinSeparator, SplitGranularity},
    windows::main::tabs::{SymbolCategoryId, TabId},
};

const PREVIEW_STATE_SAVE_DELAY: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum PersistedQuickPasteTab {
    #[default]
    Clipboard,
    Favorites,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum GallerySortField {
    Name,
    Type,
    ModifiedDate,
    CreatedDate,
    UsageFrequency,
}

impl Default for GallerySortField {
    fn default() -> Self {
        Self::Name
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum GallerySortOrder {
    Ascending,
    Descending,
}

impl Default for GallerySortOrder {
    fn default() -> Self {
        Self::Ascending
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub(crate) struct PersistedGallerySort {
    pub(crate) field: GallerySortField,
    pub(crate) order: GallerySortOrder,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum PersistedEdgeSide {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct PersistedEdgePosition {
    /// 所属显示器的稳定标识。
    pub(crate) display_key: u64,
    /// 贴边方向。
    pub(crate) side: PersistedEdgeSide,
    /// 窗口沿贴边方向的逻辑像素偏移。
    pub(crate) offset: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct PersistedWindowSize {
    /// 主窗口客户区的逻辑宽度。
    pub(crate) width: f32,
    /// 主窗口客户区的逻辑高度。
    pub(crate) height: f32,
}

impl PersistedWindowSize {
    pub(crate) const DEFAULT: Self = Self {
        width: 360.0,
        height: 520.0,
    };
    pub(crate) const MIN_SIZE: f32 = 150.0;

    pub(crate) fn is_valid(self) -> bool {
        self.width.is_finite()
            && self.height.is_finite()
            && self.width >= Self::MIN_SIZE
            && self.height >= Self::MIN_SIZE
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub(crate) struct PersistedMainWindowState {
    pub(crate) active_tab: TabId,
    pub(crate) symbol_category: SymbolCategoryId,
    pub(crate) recent_emojis: Vec<String>,
    pub(crate) recent_symbols: Vec<String>,
    /// 上次使用的搜索引擎地址模板，用于右键菜单父项显示与直接点击。
    pub(crate) last_search_engine_template: Option<String>,
    /// 普通窗口的记忆位置；窗口处于贴边状态时不代表当前窗口位置。
    pub(crate) normal_position: Option<PersistedWindowPosition>,
    /// 单栏窗口的客户区逻辑尺寸，保留旧版本的尺寸记忆。
    pub(crate) size: Option<PersistedWindowSize>,
    /// 左右双栏与上下双栏分别记忆尺寸，单栏沿用原有尺寸。
    pub(crate) split_sizes: [Option<PersistedWindowSize>; 2],
    /// 左右、上下双栏中剪贴板列表占用的比例，分别记忆。
    pub(crate) split_ratios: [f32; 2],
    /// 按左、右、上、下分别保存边缘面板的宽度或高度。
    pub(crate) edge_panel_sizes: [f32; 4],
    /// 侧边标签面板的宽度，使用未经过界面缩放的逻辑像素。
    pub(crate) tags_panel_width: f32,
    /// 当前保存的贴边状态；为空表示上次退出时没有贴边。
    pub(crate) edge: Option<PersistedEdgePosition>,
    pub(crate) gallery_sort: PersistedGallerySort,
    #[serde(default)]
    pub(crate) quick_paste_tab: PersistedQuickPasteTab,
    #[serde(default)]
    pub(crate) quick_paste_index: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub(crate) struct PersistedPreviewWindowState {
    /// 所有图片共用的正方形预览适配框边长，使用逻辑像素。
    pub(crate) image_base_size: f32,
}

impl Default for PersistedPreviewWindowState {
    fn default() -> Self {
        Self {
            image_base_size: Self::DEFAULT_IMAGE_BASE_SIZE,
        }
    }
}

impl PersistedPreviewWindowState {
    pub(crate) const DEFAULT_IMAGE_BASE_SIZE: f32 = 480.0;
    pub(crate) const IMAGE_BASE_SIZE_MIN: f32 = 48.0;
    pub(crate) const IMAGE_BASE_SIZE_MAX: f32 = 2400.0;

    pub(crate) fn restored_image_base_size(&self) -> f32 {
        if self.image_base_size.is_finite() {
            self.image_base_size
                .clamp(Self::IMAGE_BASE_SIZE_MIN, Self::IMAGE_BASE_SIZE_MAX)
        } else {
            Self::DEFAULT_IMAGE_BASE_SIZE
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub(crate) struct PersistedState {
    pub(crate) main_window: PersistedMainWindowState,
    pub(crate) preview_window: PersistedPreviewWindowState,
    pub(crate) legacy_migration_prompt_dismissed: bool,
    pub(crate) software_rendering_warning_dismissed: bool,
    /// 贴在屏幕上的便签，启动时按顺序恢复。
    pub(crate) sticky_notes: Vec<PersistedStickyNote>,
    /// 新便签继承最近一次调整的偏好，关闭所有便签后仍保留。
    pub(crate) sticky_preferences: PersistedStickyPreferences,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub(crate) struct PersistedStickyPreferences {
    /// 用户选择的便签颜色（0xRRGGBB）；为空时跟随主题。
    #[serde(deserialize_with = "deserialize_sticky_color")]
    pub(crate) color: Option<u32>,
    pub(crate) topmost: bool,
    pub(crate) opacity: f32,
    pub(crate) granularity: SplitGranularity,
    pub(crate) include_symbols: bool,
    pub(crate) separator: JoinSeparator,
}

impl Default for PersistedStickyPreferences {
    fn default() -> Self {
        Self {
            color: None,
            topmost: true,
            opacity: 1.0,
            granularity: SplitGranularity::default(),
            include_symbols: false,
            separator: JoinSeparator::default(),
        }
    }
}

impl PersistedStickyPreferences {
    pub(crate) fn normalized(mut self) -> Self {
        self.color = self.color.filter(|color| *color <= 0xFF_FFFF);
        self.opacity = if self.opacity.is_finite() {
            self.opacity.clamp(0.3, 1.0)
        } else {
            Self::default().opacity
        };
        self
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub(crate) struct PersistedStickyNote {
    pub(crate) item_id: i64,
    /// GPUI 全局逻辑坐标下的窗口外框。
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) width: f32,
    pub(crate) height: f32,
    // 保持已发布状态中颜色、置顶和不透明度的字段位置。
    #[serde(flatten)]
    pub(crate) preferences: PersistedStickyPreferences,
    pub(crate) collapsed: bool,
    pub(crate) split: bool,
}

impl PersistedStickyNote {
    pub(crate) fn new(item_id: i64, preferences: PersistedStickyPreferences) -> Self {
        Self {
            item_id,
            preferences: preferences.normalized(),
            ..Default::default()
        }
    }
}

/// 只接受 0xRRGGBB 数值；无法识别的旧值按跟随主题处理，不让整份状态读取失败。
fn deserialize_sticky_color<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value
        .as_u64()
        .and_then(|color| u32::try_from(color).ok())
        .filter(|color| *color <= 0xFF_FFFF))
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct PersistedWindowPosition {
    /// 相对所属显示器左上角的逻辑坐标，不是 Win32 物理设备像素。
    pub(crate) x: f32,
    pub(crate) y: f32,
    /// 根据显示器设备名生成的稳定标识；缺失时兼容旧的全局逻辑坐标。
    pub(crate) display_key: Option<u64>,
}

impl Default for PersistedMainWindowState {
    fn default() -> Self {
        Self {
            active_tab: TabId::Clipboard,
            symbol_category: SymbolCategoryId::Emoji,
            recent_emojis: Vec::new(),
            recent_symbols: Vec::new(),
            last_search_engine_template: None,
            normal_position: None,
            size: None,
            split_sizes: [None; 2],
            split_ratios: [Self::DEFAULT_SPLIT_RATIO; 2],
            edge_panel_sizes: [360.0, 360.0, 260.0, 260.0],
            tags_panel_width: Self::DEFAULT_TAGS_PANEL_WIDTH,
            edge: None,
            gallery_sort: PersistedGallerySort::default(),
            quick_paste_tab: PersistedQuickPasteTab::default(),
            quick_paste_index: 0,
        }
    }
}

impl PersistedMainWindowState {
    pub(crate) const DEFAULT_SPLIT_RATIO: f32 = 0.5;
    pub(crate) const MIN_SPLIT_RATIO: f32 = 0.2;
    pub(crate) const MAX_SPLIT_RATIO: f32 = 0.8;
    pub(crate) const DEFAULT_TAGS_PANEL_WIDTH: f32 = 184.0;
    pub(crate) const MIN_TAGS_PANEL_WIDTH: f32 = 120.0;
    pub(crate) const MAX_TAGS_PANEL_WIDTH: f32 = 360.0;

    pub(crate) fn restored_tags_panel_width(&self) -> f32 {
        if self.tags_panel_width.is_finite() {
            self.tags_panel_width
                .clamp(Self::MIN_TAGS_PANEL_WIDTH, Self::MAX_TAGS_PANEL_WIDTH)
        } else {
            Self::DEFAULT_TAGS_PANEL_WIDTH
        }
    }

    pub(crate) fn restored_size(&self) -> PersistedWindowSize {
        self.size
            .filter(|size| size.is_valid())
            .unwrap_or(PersistedWindowSize::DEFAULT)
    }

    pub(crate) fn restored_layout_size(&self, layout: ClipboardListLayout) -> PersistedWindowSize {
        let (index, fallback) = match layout {
            ClipboardListLayout::Single => return self.restored_size(),
            ClipboardListLayout::SideBySide => (
                0,
                PersistedWindowSize {
                    width: 860.0,
                    height: 560.0,
                },
            ),
            ClipboardListLayout::Stacked => (
                1,
                PersistedWindowSize {
                    width: 420.0,
                    height: 760.0,
                },
            ),
        };
        self.split_sizes[index]
            .filter(|size| size.is_valid())
            .unwrap_or(fallback)
    }

    pub(crate) fn restored_split_ratio(&self, layout: ClipboardListLayout) -> f32 {
        let index = match layout {
            ClipboardListLayout::Single => return Self::DEFAULT_SPLIT_RATIO,
            ClipboardListLayout::SideBySide => 0,
            ClipboardListLayout::Stacked => 1,
        };
        let ratio = self.split_ratios[index];
        if ratio.is_finite() {
            ratio.clamp(Self::MIN_SPLIT_RATIO, Self::MAX_SPLIT_RATIO)
        } else {
            Self::DEFAULT_SPLIT_RATIO
        }
    }

    pub(crate) fn restored_edge_panel_size(&self, side: PersistedEdgeSide) -> f32 {
        let fallback = match side {
            PersistedEdgeSide::Left | PersistedEdgeSide::Right => 360.0,
            PersistedEdgeSide::Top | PersistedEdgeSide::Bottom => 260.0,
        };
        valid_edge_panel_size(self.edge_panel_sizes[side as usize], fallback)
    }
}

fn valid_edge_panel_size(size: f32, fallback: f32) -> f32 {
    if size.is_finite() {
        size.max(PersistedWindowSize::MIN_SIZE)
    } else {
        fallback
    }
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            main_window: PersistedMainWindowState::default(),
            preview_window: PersistedPreviewWindowState::default(),
            legacy_migration_prompt_dismissed: false,
            software_rendering_warning_dismissed: false,
            sticky_notes: Vec::new(),
            sticky_preferences: PersistedStickyPreferences::default(),
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum StateError {
    #[error("无法创建状态目录：{source}")]
    CreateDirectory { source: std::io::Error },
    #[error("无法打开状态文件：{source}")]
    Open { source: std::io::Error },
    #[error("无法写入状态文件：{source}")]
    Write { source: std::io::Error },
    #[error("无法提交状态文件：{source}")]
    Commit { source: std::io::Error },
    #[error("无法序列化状态：{source}")]
    Serialize { source: serde_json::Error },
    #[error("数据正在导入，请稍后重试")]
    DataSwitching,
}

pub(crate) struct StateRuntime {
    state: PersistedState,
    path: PathBuf,
    pending_save: Option<Task<()>>,
    dirty: bool,
}

impl Global for StateRuntime {}

impl StateRuntime {
    pub(crate) fn install(path: impl Into<PathBuf>, cx: &mut App) {
        let path = path.into();
        let state = load_state(&path);
        if !path.exists() {
            if let Err(error) = persist_state(&path, &state) {
                warn!("创建状态文件失败：{error}");
            }
        }
        cx.set_global(Self {
            state,
            path,
            pending_save: None,
            dirty: false,
        });
        cx.on_app_quit(|cx| {
            cx.update_global::<Self, _>(|runtime, _| {
                if let Err(error) = runtime.flush_pending_save() {
                    tracing::error!("退出时保存待写入状态失败：{error}");
                }
            });
            async {}
        })
        .detach();
    }

    pub(crate) fn reload(path: impl Into<PathBuf>, cx: &mut App) {
        let path = path.into();
        let state = load_state(&path);
        cx.update_global::<Self, _>(|runtime, _| {
            runtime.pending_save = None;
            runtime.dirty = false;
            runtime.state = state.clone();
            runtime.path = path.clone();
        });
    }

    fn update_preview_image_base_size(&mut self, image_base_size: f32) -> bool {
        let image_base_size =
            PersistedPreviewWindowState { image_base_size }.restored_image_base_size();
        if self.state.preview_window.image_base_size == image_base_size {
            return false;
        }
        self.state.preview_window.image_base_size = image_base_size;
        self.dirty = true;
        self.pending_save = None;
        true
    }

    fn flush_pending_save(&mut self) -> Result<bool, StateError> {
        self.pending_save = None;
        if !self.dirty {
            return Ok(false);
        }
        persist_state(&self.path, &self.state)?;
        self.dirty = false;
        Ok(true)
    }
}

pub(crate) fn state(cx: &impl AppContext) -> PersistedState {
    cx.read_global::<StateRuntime, _>(|runtime, _| runtime.state.clone())
}

pub(crate) fn set_state<C>(state: PersistedState, cx: &mut C) -> Result<bool, StateError>
where
    C: BorrowMut<App>,
{
    let app: &App = Borrow::borrow(&*cx);
    let (current, path) = app
        .read_global::<StateRuntime, _>(|runtime, _| (runtime.state.clone(), runtime.path.clone()));
    if current == state {
        return Ok(false);
    }

    persist_state(&path, &state)?;
    cx.update_global::<StateRuntime, _>(|runtime, _| {
        runtime.state = state;
        runtime.dirty = false;
        runtime.pending_save = None;
    });
    Ok(true)
}

/// 记录上次使用的搜索引擎地址模板，用于右键菜单父项显示与直接点击。
pub(crate) fn set_last_search_engine_template(template: Option<String>, cx: &mut App) {
    let mut state = state(cx);
    if state.main_window.last_search_engine_template == template {
        return;
    }
    state.main_window.last_search_engine_template = template;
    if let Err(error) = set_state(state, cx) {
        tracing::error!("保存上次搜索引擎失败：{error}");
    }
}

pub(crate) fn set_sticky_notes(notes: Vec<PersistedStickyNote>, cx: &mut App) {
    let mut state = state(cx);
    if state.sticky_notes == notes {
        return;
    }
    state.sticky_notes = notes;
    if let Err(error) = set_state(state, cx) {
        tracing::error!("保存便签状态失败：{error}");
    }
}

pub(crate) fn remember_sticky_preferences(note: PersistedStickyNote, cx: &mut App) {
    let mut state = state(cx);
    state.sticky_preferences = note.preferences;
    if let Some(saved) = state
        .sticky_notes
        .iter_mut()
        .find(|saved| saved.item_id == note.item_id)
    {
        *saved = note;
    }
    if let Err(error) = set_state(state, cx) {
        tracing::error!("保存便签偏好失败：{error}");
    }
}

pub(crate) fn set_main_window_normal_position(position: PersistedWindowPosition, cx: &mut App) {
    let mut state = state(cx);
    if state.main_window.normal_position == Some(position) {
        return;
    }
    state.main_window.normal_position = Some(position);
    if let Err(error) = set_state(state, cx) {
        tracing::error!("保存主窗口位置失败：{error}");
    }
}

pub(crate) fn set_main_window_size<C>(size: PersistedWindowSize, cx: &mut C)
where
    C: BorrowMut<App>,
{
    if !size.is_valid() {
        return;
    }
    let app: &App = Borrow::borrow(&*cx);
    let mut state = state(app);
    let clipboard = crate::config_runtime::clipboard_config(app);
    let stored = match clipboard.list_layout.effective(clipboard.window_position) {
        ClipboardListLayout::Single => &mut state.main_window.size,
        ClipboardListLayout::SideBySide => &mut state.main_window.split_sizes[0],
        ClipboardListLayout::Stacked => &mut state.main_window.split_sizes[1],
    };
    if *stored == Some(size) {
        return;
    }
    *stored = Some(size);
    if let Err(error) = set_state(state, cx) {
        tracing::error!("保存主窗口大小失败：{error}");
    }
}

pub(crate) fn set_main_window_edge_panel_size(side: PersistedEdgeSide, size: f32, cx: &mut App) {
    if !size.is_finite() || size < PersistedWindowSize::MIN_SIZE {
        return;
    }
    let mut state = state(cx);
    let stored = &mut state.main_window.edge_panel_sizes[side as usize];
    if *stored == size {
        return;
    }
    *stored = size;
    if let Err(error) = set_state(state, cx) {
        tracing::error!("保存边缘窗口尺寸失败：{error}");
    }
}

pub(crate) fn set_preview_image_base_size(image_base_size: f32, cx: &mut App) {
    let changed = cx.update_global::<StateRuntime, _>(|runtime, _| {
        runtime.update_preview_image_base_size(image_base_size)
    });
    if !changed {
        return;
    }
    let task = cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(PREVIEW_STATE_SAVE_DELAY)
            .await;
        // 到期时保存最新状态，避免延迟任务覆盖期间更新的其他字段。
        cx.update(|cx| {
            cx.update_global::<StateRuntime, _>(|runtime, _| {
                if let Err(error) = runtime.flush_pending_save() {
                    tracing::error!("保存图片预览大小失败：{error}");
                }
            });
        });
    });
    cx.update_global::<StateRuntime, _>(|runtime, _| runtime.pending_save = Some(task));
}

pub(crate) fn set_main_window_edge(position: Option<PersistedEdgePosition>, cx: &mut App) {
    let mut state = state(cx);
    if state.main_window.edge == position {
        return;
    }
    state.main_window.edge = position;
    if let Err(error) = set_state(state, cx) {
        tracing::error!("保存主窗口贴边位置失败：{error}");
    }
}

fn load_state(path: &Path) -> PersistedState {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return PersistedState::default();
        }
        Err(error) => {
            warn!("读取状态失败，已使用默认状态：{error}");
            return PersistedState::default();
        }
    };

    match serde_json::from_str(&content) {
        Ok(state) => state,
        Err(error) => {
            warn!("状态格式无效，已使用默认状态：{error}");
            PersistedState::default()
        }
    }
}

fn persist_state(path: &Path, state: &PersistedState) -> Result<(), StateError> {
    let _operation =
        crate::services::data_runtime::begin_operation().map_err(|_| StateError::DataSwitching)?;
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|source| StateError::CreateDirectory { source })?;
    }

    let mut content =
        serde_json::to_string_pretty(&state).map_err(|source| StateError::Serialize { source })?;
    content.push('\n');
    let mut file = AtomicWriteFile::open(path).map_err(|source| StateError::Open { source })?;
    file.write_all(content.as_bytes())
        .map_err(|source| StateError::Write { source })?;
    file.commit()
        .map_err(|source| StateError::Commit { source })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        PersistedGallerySort, PersistedMainWindowState, PersistedPreviewWindowState,
        PersistedState, PersistedStickyNote, PersistedStickyPreferences, PersistedWindowPosition,
        PersistedWindowSize, StateRuntime, load_state, persist_state, remember_sticky_preferences,
        set_sticky_notes, state,
    };
    use crate::services::word_split::{JoinSeparator, SplitGranularity};
    use crate::windows::main::tabs::{SymbolCategoryId, TabId};
    use tempfile::tempdir;

    #[test]
    fn tags_panel_width_is_restored_with_valid_bounds() {
        for (width, expected) in [
            (80.0, 120.0),
            (240.0, 240.0),
            (500.0, 360.0),
            (f32::NAN, 184.0),
            (f32::INFINITY, 184.0),
        ] {
            let state = PersistedMainWindowState {
                tags_panel_width: width,
                ..Default::default()
            };
            assert_eq!(state.restored_tags_panel_width(), expected);
        }
    }

    #[test]
    fn edge_panel_sizes_have_independent_defaults_and_minimums() {
        use super::PersistedEdgeSide::{Bottom, Left, Right, Top};
        let state = PersistedMainWindowState::default();
        assert_eq!(state.restored_edge_panel_size(Left), 360.0);
        assert_eq!(state.restored_edge_panel_size(Top), 260.0);
        let state = PersistedMainWindowState {
            edge_panel_sizes: [700.0, 420.0, f32::NAN, 300.0],
            ..Default::default()
        };
        assert_eq!(state.restored_edge_panel_size(Left), 700.0);
        assert_eq!(state.restored_edge_panel_size(Right), 420.0);
        assert_eq!(state.restored_edge_panel_size(Top), 260.0);
        assert_eq!(state.restored_edge_panel_size(Bottom), 300.0);
        assert_eq!(state.restored_size(), PersistedWindowSize::DEFAULT);
    }

    #[test]
    fn restored_window_size_rejects_collapsed_and_non_finite_sizes() {
        for size in [
            PersistedWindowSize {
                width: 360.0,
                height: 1.0,
            },
            PersistedWindowSize {
                width: 360.0,
                height: 149.0,
            },
            PersistedWindowSize {
                width: f32::NAN,
                height: 520.0,
            },
            PersistedWindowSize {
                width: 360.0,
                height: f32::INFINITY,
            },
        ] {
            let state = PersistedMainWindowState {
                size: Some(size),
                ..Default::default()
            };
            assert_eq!(state.restored_size(), PersistedWindowSize::DEFAULT);
        }
        let size = PersistedWindowSize {
            width: 480.0,
            height: 640.0,
        };
        let state = PersistedMainWindowState {
            size: Some(size),
            ..Default::default()
        };
        assert_eq!(state.restored_size(), size);
    }

    #[test]
    fn preview_size_updates_are_coalesced_until_flushed() {
        let directory = tempdir().expect("无法创建临时目录");
        let path = directory.path().join("state.json");
        let state = PersistedState::default();
        persist_state(&path, &state).expect("状态保存失败");
        let mut runtime = StateRuntime {
            state,
            path,
            pending_save: None,
            dirty: false,
        };

        for size in [528.0, 576.0, 720.0] {
            runtime.update_preview_image_base_size(size);
        }
        assert_eq!(runtime.state.preview_window.image_base_size, 720.0);
        assert_eq!(
            load_state(&runtime.path).preview_window.image_base_size,
            480.0
        );
        runtime.state.main_window.active_tab = TabId::Emoji;
        assert!(runtime.flush_pending_save().expect("待写入状态保存失败"));
        assert_eq!(load_state(&runtime.path), runtime.state);
        assert!(!runtime.flush_pending_save().expect("重复保存状态失败"));
    }

    #[test]
    fn persisted_state_can_be_loaded_by_json() {
        let directory = tempdir().expect("无法创建临时目录");
        let path = directory.path().join("state.json");
        let state = PersistedState {
            main_window: PersistedMainWindowState {
                active_tab: TabId::Emoji,
                symbol_category: SymbolCategoryId::Gallery,
                recent_emojis: vec!["😀".to_owned(), "🥰".to_owned()],
                recent_symbols: vec!["★".to_owned(), "→".to_owned()],
                last_search_engine_template: None,
                normal_position: Some(PersistedWindowPosition {
                    x: 120.0,
                    y: 240.0,
                    display_key: None,
                }),
                size: Some(PersistedWindowSize {
                    width: 480.0,
                    height: 640.0,
                }),
                split_sizes: [None; 2],
                split_ratios: [0.6, 0.4],
                edge_panel_sizes: [420.0, 480.0, 280.0, 320.0],
                tags_panel_width: 240.0,
                edge: None,
                gallery_sort: PersistedGallerySort::default(),
                quick_paste_tab: Default::default(),
                quick_paste_index: 0,
            },
            preview_window: PersistedPreviewWindowState {
                image_base_size: 720.0,
            },
            ..Default::default()
        };

        persist_state(&path, &state).expect("状态保存失败");
        let loaded = load_state(&path);
        assert_eq!(loaded, state);
        assert_eq!(loaded.preview_window.restored_image_base_size(), 720.0);
    }

    #[test]
    fn sticky_note_color_accepts_custom_values_and_ignores_unknown_ones() {
        let notes: Vec<super::PersistedStickyNote> = serde_json::from_str(
            r#"[{"item_id":1,"color":3900150},{"item_id":2,"color":"Yellow"},{"item_id":3}]"#,
        )
        .expect("便签状态读取失败");
        assert_eq!(notes[0].preferences.color, Some(0x3B82F6));
        assert_eq!(notes[1].preferences.color, None);
        assert_eq!(notes[2].preferences.color, None);
        let json = serde_json::to_string(&notes[0]).unwrap();
        let loaded: super::PersistedStickyNote = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.preferences.color, Some(0x3B82F6));
    }

    #[test]
    fn released_sticky_note_fields_keep_their_values_and_default_split_preferences() {
        let loaded: PersistedState = serde_json::from_str(
            r#"{"sticky_notes":[{"item_id":1,"x":120.0,"y":240.0,"width":340.0,
                "height":260.0,"color":3900150,"topmost":false,"collapsed":true,
                "opacity":0.65,"split":true}]}"#,
        )
        .expect("已发布便签状态读取失败");
        let note = &loaded.sticky_notes[0];
        assert_eq!(note.preferences.color, Some(0x3B82F6));
        assert!(!note.preferences.topmost);
        assert_eq!(note.preferences.opacity, 0.65);
        assert_eq!(note.preferences.granularity, SplitGranularity::Word);
        assert!(!note.preferences.include_symbols);
        assert_eq!(note.preferences.separator, JoinSeparator::Smart);
        assert!(note.collapsed && note.split);
        assert_eq!(
            (note.x, note.y, note.width, note.height),
            (120.0, 240.0, 340.0, 260.0)
        );
        assert_eq!(
            loaded.sticky_preferences,
            PersistedStickyPreferences::default()
        );

        let json = serde_json::to_value(note).expect("便签状态序列化失败");
        assert_eq!(json["color"], 3900150);
        assert_eq!(json["topmost"], false);
        assert_eq!(json["opacity"].as_f64().unwrap() as f32, 0.65);
        assert!(json.get("preferences").is_none());
    }

    #[test]
    fn sticky_preferences_restore_valid_color_and_opacity() {
        for (opacity, expected) in [
            (0.0, 0.3),
            (0.65, 0.65),
            (2.0, 1.0),
            (f32::NAN, 1.0),
            (f32::INFINITY, 1.0),
        ] {
            let note = PersistedStickyNote::new(
                1,
                PersistedStickyPreferences {
                    color: Some(0x100_0000),
                    opacity,
                    ..Default::default()
                },
            );
            assert_eq!(note.preferences.color, None);
            assert_eq!(note.preferences.opacity, expected);
        }
    }

    #[gpui::test]
    fn sticky_preferences_survive_note_changes_closing_and_reload(cx: &mut gpui::TestAppContext) {
        let directory = tempdir().expect("无法创建临时目录");
        let path = directory.path().join("state.json");
        cx.update(|cx| {
            StateRuntime::install(&path, cx);
            let mut note = PersistedStickyNote::new(1, state(cx).sticky_preferences);
            let other = PersistedStickyNote::new(2, PersistedStickyPreferences::default());
            set_sticky_notes(vec![note.clone(), other.clone()], cx);
            note.preferences = PersistedStickyPreferences {
                color: Some(0x3B82F6),
                topmost: false,
                opacity: 0.65,
                granularity: SplitGranularity::Character,
                include_symbols: true,
                separator: JoinSeparator::Newline,
            };
            remember_sticky_preferences(note.clone(), cx);

            let saved = load_state(&path);
            assert_eq!(saved.sticky_preferences, note.preferences);
            assert_eq!(saved.sticky_notes, vec![note.clone(), other.clone()]);
            let new_note = PersistedStickyNote::new(3, saved.sticky_preferences);
            assert_eq!(new_note.preferences, note.preferences);
            assert!(!new_note.collapsed && !new_note.split);

            // 移动另一张便签不能覆盖最近调整的偏好。
            let mut moved = other;
            moved.x = 200.0;
            set_sticky_notes(vec![note.clone(), moved], cx);
            assert_eq!(load_state(&path).sticky_preferences, note.preferences);

            set_sticky_notes(Vec::new(), cx);
            StateRuntime::reload(&path, cx);
            assert!(state(cx).sticky_notes.is_empty());
            assert_eq!(state(cx).sticky_preferences, note.preferences);
            assert_eq!(load_state(&path).sticky_preferences, note.preferences);
        });
    }

    #[test]
    fn persisted_state_is_grouped_by_window() {
        let state: PersistedState = serde_json::from_str(
            r#"{
                "main_window": {
                    "active_tab": "Clipboard",
                    "symbol_category": "Emoji",
                    "last_search_engine_template": null,
                    "normal_position": null,
                    "size": null,
                    "edge": null
                }
            }"#,
        )
        .expect("状态读取失败");

        assert_eq!(state.main_window.size, None);
        assert_eq!(state.main_window.edge, None);
        assert_eq!(state.main_window.restored_tags_panel_width(), 184.0);
        assert!(state.main_window.recent_emojis.is_empty());
        assert!(state.main_window.recent_symbols.is_empty());
        assert_eq!(state.preview_window.restored_image_base_size(), 480.0);
    }
}
