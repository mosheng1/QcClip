//! 与界面和持久化实现无关的剪贴板领域模型。

use std::sync::Arc;

use crate::domain::capture::{FormatKind, FormatRole, classify_format_role, image_format_priority};

pub type ClipboardItemId = i64;
pub type HistoryItemId = i64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tag {
    pub id: i64,
    pub uuid: String,
    pub name: String,
    pub normalized_name: String,
    pub color: String,
    pub sort_order: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ContentKind {
    Text,
    Link,
    Image,
    FileList,
    Mixed,
    Custom,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImagePreviewSource {
    Inline {
        bytes: Arc<[u8]>,
    },
    External {
        sha256: [u8; 32],
        byte_length: u64,
        relative_path: String,
    },
}

impl ContentKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Link => "link",
            Self::Image => "image",
            Self::FileList => "file_list",
            Self::Mixed => "mixed",
            Self::Custom => "custom",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageDimensions {
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FormatPreview {
    pub format_namespace: String,
    pub format_key: String,
    pub format_name: String,
    pub format_order: u32,
    pub format_kind: FormatKind,
    pub text: String,
    pub char_count: usize,
    /// 所有格式的聚合类型，仅用于筛选和图标，不替代格式集合。
    /// 所有格式的聚合类型，仅用于筛选和图标，不替代格式集合。
    pub content_kind: ContentKind,
    /// HTML 格式的原始内容，仅在列表查询明确请求时加载。
    pub html_content: Option<Arc<str>>,
    pub image_source: Option<ImagePreviewSource>,
    /// 图片原始尺寸，仅用于按比例计算列表预览高度。
    pub image_dimensions: Option<ImageDimensions>,
    pub is_restorable: bool,
}

impl FormatPreview {
    pub fn role(&self) -> FormatRole {
        classify_format_role(self.format_kind, &self.format_key, &self.format_name)
    }
}

pub fn is_image_only_html(preview: &FormatPreview) -> bool {
    preview.role() == FormatRole::Html && preview.content_kind == ContentKind::Image
}

pub fn is_empty_textual_preview(preview: &FormatPreview) -> bool {
    matches!(preview.role(), FormatRole::PlainText | FormatRole::Html)
        && preview.content_kind == ContentKind::Unknown
}

pub fn has_native_image(previews: &[FormatPreview]) -> bool {
    previews
        .iter()
        .any(|preview| preview.role() == FormatRole::Image && preview.is_restorable)
}

fn is_redundant_html_image(preview: &FormatPreview, has_native_image: bool) -> bool {
    has_native_image && is_image_only_html(preview)
}

pub fn select_user_visible_previews(previews: &[FormatPreview]) -> Vec<FormatPreview> {
    select_user_visible_previews_with_priority(
        previews,
        &[FormatRole::PlainText, FormatRole::Html, FormatRole::Image],
    )
}

pub fn select_user_visible_previews_with_priority(
    previews: &[FormatPreview],
    preferred_roles: &[FormatRole],
) -> Vec<FormatPreview> {
    select_user_visible_previews_with_priority_and_allowed_roles(
        previews,
        preferred_roles,
        &[
            FormatRole::PlainText,
            FormatRole::Html,
            FormatRole::Rtf,
            FormatRole::Image,
            FormatRole::FileList,
            FormatRole::VirtualFile,
            FormatRole::Custom,
        ],
    )
}

pub fn select_user_visible_previews_with_priority_and_allowed_roles(
    previews: &[FormatPreview],
    preferred_roles: &[FormatRole],
    allowed_roles: &[FormatRole],
) -> Vec<FormatPreview> {
    select_user_visible_previews_with_priority_and_filter(previews, preferred_roles, |preview| {
        allowed_roles.contains(&preview.role())
    })
}

pub fn select_user_visible_previews_with_priority_and_filter<F>(
    previews: &[FormatPreview],
    preferred_roles: &[FormatRole],
    allowed: F,
) -> Vec<FormatPreview>
where
    F: Fn(&FormatPreview) -> bool,
{
    const VISIBLE_ROLES: &[FormatRole] = &[
        FormatRole::PlainText,
        FormatRole::Html,
        FormatRole::Rtf,
        FormatRole::Image,
        FormatRole::FileList,
        FormatRole::VirtualFile,
    ];

    let virtual_file_complete = virtual_file_complete(previews);
    let native_image_available = has_native_image(previews);
    let mut selected_roles = Vec::new();
    let mut selected = Vec::new();
    for role in preferred_roles
        .iter()
        .copied()
        .chain(VISIBLE_ROLES.iter().copied())
    {
        if selected_roles.contains(&role)
            || !previews
                .iter()
                .any(|preview| preview.role() == role && allowed(preview))
            || role == FormatRole::VirtualFile && !virtual_file_complete
        {
            continue;
        }
        if let Some(preview) = previews
            .iter()
            .filter(|preview| {
                allowed(preview)
                    && preview.role() == role
                    && (!is_empty_textual_preview(preview)
                        || crate::domain::capture::is_custom_text_format(
                            &preview.format_namespace,
                            &preview.format_key,
                        ))
                    && !is_redundant_html_image(preview, native_image_available)
            })
            .min_by_key(|preview| {
                (
                    if preview.is_restorable { 0 } else { 1 },
                    preview_priority(preview),
                    preview.format_order,
                )
            })
        {
            selected_roles.push(role);
            selected.push(preview.clone());
        }
    }

    // 只有未知自定义格式时才提供一个兜底候选，避免普通内容被完全隐藏。
    if selected.is_empty()
        && let Some(preview) = previews
            .iter()
            .filter(|preview| {
                allowed(preview)
                    && preview.role() == FormatRole::Custom
                    && !is_empty_textual_preview(preview)
                    && !is_redundant_html_image(preview, native_image_available)
            })
            .min_by_key(|preview| preview.format_order)
    {
        selected.push(preview.clone());
    }

    // 候选项按用户设置的顺序排列，未配置的可见类型继续使用默认顺序。
    selected
}

fn virtual_file_complete(previews: &[FormatPreview]) -> bool {
    let virtual_files = previews
        .iter()
        .filter(|preview| preview.role() == FormatRole::VirtualFile)
        .collect::<Vec<_>>();
    virtual_files.iter().any(|preview| {
        let value = preview.format_key.to_ascii_lowercase();
        value.contains("filegroupdescriptor")
    }) && virtual_files.iter().any(|preview| {
        let value = preview.format_key.to_ascii_lowercase();
        value.contains("filecontents")
    }) && virtual_files.iter().all(|preview| preview.is_restorable)
}

fn preview_priority(preview: &FormatPreview) -> u8 {
    let key = preview.format_key.to_ascii_lowercase();
    match preview.role() {
        FormatRole::PlainText => {
            if crate::domain::capture::is_custom_text_format(
                &preview.format_namespace,
                &preview.format_key,
            ) {
                0
            } else if key == "cf_unicodetext" {
                1
            } else if key == "cf_text" {
                2
            } else if key == "cf_oemtext" {
                3
            } else {
                4
            }
        }
        FormatRole::Html => {
            if key.contains("html format") {
                0
            } else {
                1
            }
        }
        FormatRole::Image => image_format_priority(&preview.format_key),
        FormatRole::VirtualFile => {
            if key.contains("filegroupdescriptor") {
                0
            } else {
                1
            }
        }
        FormatRole::Rtf | FormatRole::FileList | FormatRole::Custom | FormatRole::Internal => 0,
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CaptureState {
    Pending,
    Ready,
    Corrupt,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OriginKind {
    Captured,
    Manual,
    Edited,
    Imported,
    Synced,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClipboardItem {
    pub id: ClipboardItemId,
    pub uuid: String,
    pub content_hash: [u8; 32],
    /// 所有格式的聚合类型，仅用于筛选和图标，不替代格式集合。
    pub content_kind: ContentKind,
    pub capture_state: CaptureState,
    pub origin_kind: OriginKind,
    /// 当前默认展示格式的缓存；完整候选项保存在 `preview_options`。
    pub preview_text: String,
    pub preview_char_count: usize,
    pub preview_options: Vec<FormatPreview>,
    pub format_count: u32,
    pub total_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClipboardHistory {
    pub id: HistoryItemId,
    pub uuid: String,
    pub item_id: ClipboardItemId,
    pub last_seen_at: i64,
    pub paste_count: u64,
    pub is_pinned: bool,
    pub sort_order: i64,
    pub source_app_name: Option<String>,
    pub source_app_path: Option<String>,
    pub source_app_icon: Option<ApplicationIconSource>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplicationIconSource {
    pub sha256: [u8; 32],
    pub byte_length: u64,
    pub relative_path: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClipboardListItem {
    /// 当前列表关系 ID：历史页对应 `clipboard_history.id`，收藏页对应 `favorites.id`。
    pub history_id: HistoryItemId,
    pub item_id: ClipboardItemId,
    /// 对应的收藏记录；历史列表中未收藏的内容为空。
    pub favorite_id: Option<i64>,
    /// 当前内容绑定的全部标签 ID。
    pub tag_ids: Vec<i64>,
    /// 历史和收藏共享的内容备注，不参与复制或粘贴。
    pub note_text: String,
    /// 列表预览和常用粘贴菜单使用的格式候选项。
    pub preview_text: String,
    pub preview_char_count: usize,
    pub preview_options: Vec<FormatPreview>,
    /// 同一内容捕获到的全部原始格式，供全部格式菜单使用。
    pub all_format_options: Vec<FormatPreview>,
    /// 所有格式的聚合类型，仅用于筛选和图标，不替代格式集合。
    pub content_kind: ContentKind,
    pub paste_count: u64,
    pub is_pinned: bool,
    pub sort_order: i64,
    pub last_seen_at: i64,
    pub shortcut: String,
    pub source_app_name: Option<String>,
    pub source_device_name: Option<String>,
    pub source_app_icon: Option<ApplicationIconSource>,
    pub is_selected: bool,
}

impl ClipboardListItem {
    pub fn from_models(item: &ClipboardItem, history: &ClipboardHistory) -> Self {
        Self {
            history_id: history.id,
            item_id: item.id,
            favorite_id: None,
            tag_ids: Vec::new(),
            note_text: String::new(),
            preview_text: item.preview_text.clone(),
            preview_char_count: item.preview_char_count,
            preview_options: item.preview_options.clone(),
            all_format_options: item.preview_options.clone(),
            content_kind: item.content_kind,
            paste_count: history.paste_count,
            is_pinned: history.is_pinned,
            sort_order: history.sort_order,
            last_seen_at: history.last_seen_at,
            shortcut: String::new(),
            source_app_name: history.source_app_name.clone(),
            source_device_name: None,
            source_app_icon: history.source_app_icon.clone(),
            is_selected: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preview(key: &str, kind: FormatKind, order: u32, is_restorable: bool) -> FormatPreview {
        let (text, content_kind) = match kind {
            FormatKind::Text | FormatKind::Html => ("测试".to_owned(), ContentKind::Text),
            FormatKind::Image => ("图片".to_owned(), ContentKind::Image),
            FormatKind::FileList => ("文件列表".to_owned(), ContentKind::FileList),
            FormatKind::Rtf => ("富文本内容".to_owned(), ContentKind::Text),
            FormatKind::Custom | FormatKind::Unknown => (String::new(), ContentKind::Unknown),
        };
        FormatPreview {
            format_namespace: "windows".to_owned(),
            format_key: key.to_owned(),
            format_name: key.to_owned(),
            format_order: order,
            format_kind: kind,
            text,
            char_count: 0,
            content_kind,
            html_content: None,
            image_source: None,
            image_dimensions: None,
            is_restorable,
        }
    }

    #[test]
    fn list_projection_preserves_history_and_content_identity() {
        let item = ClipboardItem {
            id: 7,
            uuid: "item-uuid".to_owned(),
            content_hash: [1; 32],
            content_kind: ContentKind::Text,
            capture_state: CaptureState::Ready,
            origin_kind: OriginKind::Captured,
            preview_text: "hello".to_owned(),
            preview_char_count: 5,
            preview_options: vec![FormatPreview {
                format_namespace: "mime".to_owned(),
                format_key: "text/plain".to_owned(),
                format_name: "纯文本".to_owned(),
                format_order: 0,
                format_kind: FormatKind::Text,
                text: "hello".to_owned(),
                char_count: 5,
                content_kind: ContentKind::Text,
                html_content: None,
                image_source: None,
                image_dimensions: None,
                is_restorable: true,
            }],
            format_count: 1,
            total_bytes: 5,
        };
        let history = ClipboardHistory {
            id: 9,
            uuid: "history-uuid".to_owned(),
            item_id: 7,
            last_seen_at: 123,
            paste_count: 0,
            is_pinned: false,
            sort_order: 1,
            source_app_name: Some("测试应用".to_owned()),
            source_app_path: None,
            source_app_icon: None,
        };

        let projection = ClipboardListItem::from_models(&item, &history);
        assert_eq!(projection.history_id, 9);
        assert_eq!(projection.item_id, 7);
        assert_eq!(projection.preview_char_count, 5);
        assert!(!projection.is_selected);
    }

    #[test]
    fn user_visible_previews_merge_compatible_copies_and_hide_internal_formats() {
        let previews = vec![
            preview("CF_TEXT", FormatKind::Text, 0, true),
            preview("CF_UNICODETEXT", FormatKind::Text, 1, true),
            preview("CF_OEMTEXT", FormatKind::Text, 2, true),
            preview("CF_LOCALE", FormatKind::Unknown, 3, true),
            preview(
                "registered:Chromium internal source URL",
                FormatKind::Custom,
                4,
                true,
            ),
            preview("registered:HTML Format", FormatKind::Html, 5, true),
        ];

        let selected = select_user_visible_previews(&previews);
        assert_eq!(
            selected
                .iter()
                .map(|preview| preview.format_key.as_str())
                .collect::<Vec<_>>(),
            ["CF_UNICODETEXT", "registered:HTML Format"]
        );
    }

    #[test]
    fn user_visible_previews_prioritize_semantic_order() {
        let previews = vec![
            preview("registered:HTML Format", FormatKind::Html, 0, true),
            preview("CF_DIBV5", FormatKind::Image, 1, true),
            preview("CF_UNICODETEXT", FormatKind::Text, 2, true),
        ];

        let selected = select_user_visible_previews(&previews);
        assert_eq!(
            selected
                .iter()
                .map(|preview| preview.format_key.as_str())
                .collect::<Vec<_>>(),
            ["CF_UNICODETEXT", "registered:HTML Format", "CF_DIBV5"]
        );
    }

    #[test]
    fn user_visible_previews_follow_configured_role_order() {
        let previews = vec![
            preview("registered:HTML Format", FormatKind::Html, 0, true),
            preview("CF_DIBV5", FormatKind::Image, 1, true),
            preview("CF_UNICODETEXT", FormatKind::Text, 2, true),
        ];

        let selected = select_user_visible_previews_with_priority(
            &previews,
            &[FormatRole::Image, FormatRole::PlainText, FormatRole::Html],
        );
        assert_eq!(
            selected
                .iter()
                .map(|preview| preview.format_key.as_str())
                .collect::<Vec<_>>(),
            ["CF_DIBV5", "CF_UNICODETEXT", "registered:HTML Format"]
        );
    }

    #[test]
    fn user_visible_previews_filter_by_allowed_roles() {
        let previews = vec![
            preview("registered:HTML Format", FormatKind::Html, 0, true),
            preview("CF_DIBV5", FormatKind::Image, 1, true),
            preview("CF_UNICODETEXT", FormatKind::Text, 2, true),
        ];

        let selected = select_user_visible_previews_with_priority_and_allowed_roles(
            &previews,
            &[FormatRole::PlainText, FormatRole::Html, FormatRole::Image],
            &[FormatRole::Image],
        );
        assert_eq!(
            selected
                .iter()
                .map(|preview| preview.format_key.as_str())
                .collect::<Vec<_>>(),
            ["CF_DIBV5"]
        );
    }

    #[test]
    fn image_preview_prefers_png_representation() {
        let previews = vec![
            preview("CF_DIBV5", FormatKind::Image, 0, true),
            preview("CF_DIB", FormatKind::Image, 1, true),
            preview("registered:PNG", FormatKind::Image, 2, true),
        ];

        let selected = select_user_visible_previews(&previews);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].format_key, "registered:PNG");
    }

    #[test]
    fn virtual_file_requires_complete_format_set() {
        let descriptor = preview(
            "registered:FileGroupDescriptorW",
            FormatKind::Custom,
            0,
            true,
        );
        let contents = preview("registered:FileContents", FormatKind::Custom, 1, true);
        let selected = select_user_visible_previews(&[descriptor.clone(), contents.clone()]);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].format_key, descriptor.format_key);

        let incomplete = select_user_visible_previews(&[
            descriptor,
            FormatPreview {
                is_restorable: false,
                ..contents
            },
        ]);
        assert!(incomplete.is_empty());
    }
}
