use std::{collections::HashSet, sync::Arc};

use rusqlite::{Connection, types::Value};
use thiserror::Error;

use crate::domain::{
    capture::{FormatKind, FormatRole},
    clipboard::{
        ApplicationIconSource, ClipboardItemId, ContentKind, FormatPreview, HistoryItemId,
        ImageDimensions, ImagePreviewSource, select_user_visible_previews,
    },
};
use crate::services::storage::{RawBlob, RawBlobStore};

use super::DatabaseError;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum HistoryContentFilter {
    Text,
    Image,
    File,
    Link,
}

impl HistoryContentFilter {
    pub const fn content_kind(self) -> ContentKind {
        match self {
            Self::Text => ContentKind::Text,
            Self::Image => ContentKind::Image,
            Self::File => ContentKind::FileList,
            Self::Link => ContentKind::Link,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PasteFilter {
    Unpasted,
    Pasted,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HistoryFilter {
    Text,
    Image,
    File,
    Link,
    Unpasted,
    Pasted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryQuery {
    pub search: Option<String>,
    pub content_filters: HashSet<HistoryContentFilter>,
    pub paste_filter: Option<PasteFilter>,
    /// 当前列表选择的标签；为空时不按标签筛选。
    pub tag_ids: Vec<i64>,
    /// 多个标签的匹配方式；`false` 表示匹配任意标签，`true` 表示匹配全部标签。
    pub tag_match_all: bool,
    pub include_pinned: bool,
    pub offset: u32,
    pub limit: u32,
    /// 列表预览文本的最大字符数；为 `None` 时不裁剪。
    pub preview_char_limit: Option<usize>,
    /// 是否加载图片和文件列表的原始数据引用。
    pub load_image_sources: bool,
    /// 是否加载 HTML 原始内容供界面渲染。
    pub load_html_content: bool,
    /// HTML 列表预览的最大可见字符数；为 `None` 时不裁剪。
    pub html_content_char_limit: Option<usize>,
}

impl Default for HistoryQuery {
    fn default() -> Self {
        Self {
            search: None,
            content_filters: HashSet::new(),
            paste_filter: None,
            tag_ids: Vec::new(),
            tag_match_all: false,
            include_pinned: true,
            offset: 0,
            limit: 100,
            preview_char_limit: None,
            load_image_sources: false,
            load_html_content: false,
            html_content_char_limit: None,
        }
    }
}

pub(super) fn append_tag_filter(
    conditions: &mut Vec<String>,
    parameters: &mut Vec<Value>,
    query: &HistoryQuery,
) {
    append_tag_filter_for_item(conditions, parameters, query, "i.id");
}

pub(super) fn append_tag_filter_for_item(
    conditions: &mut Vec<String>,
    parameters: &mut Vec<Value>,
    query: &HistoryQuery,
    item_id_expression: &str,
) {
    if query.tag_ids.is_empty() {
        return;
    }

    if query.tag_match_all {
        for tag_id in &query.tag_ids {
            conditions.push(format!(
                "EXISTS (
                    SELECT 1
                    FROM clipboard_item_tags AS cit_filter
                    WHERE cit_filter.item_id = {item_id_expression}
                      AND cit_filter.tag_id = ?
                )"
            ));
            parameters.push(Value::Integer(*tag_id));
        }
    } else {
        let placeholders = vec!["?"; query.tag_ids.len()].join(", ");
        conditions.push(format!(
            "EXISTS (
                SELECT 1
                FROM clipboard_item_tags AS cit_filter
                WHERE cit_filter.item_id = {item_id_expression}
                  AND cit_filter.tag_id IN ({placeholders})
            )"
        ));
        parameters.extend(query.tag_ids.iter().copied().map(Value::Integer));
    }
}

pub(super) fn append_search_filter(
    conditions: &mut Vec<String>,
    parameters: &mut Vec<Value>,
    search: &str,
) {
    let pattern = Value::Text(format!("%{}%", escape_like(search)));
    conditions.push(
        "(COALESCE(i.preview_text, '') LIKE ? ESCAPE '\\'
          OR EXISTS (
              SELECT 1 FROM clipboard_formats AS f_search
              WHERE f_search.item_id = i.id
                AND COALESCE(f_search.preview_text, '') LIKE ? ESCAPE '\\'
          )
          OR EXISTS (
              SELECT 1 FROM clipboard_item_notes AS n_search
              WHERE n_search.item_id = i.id
                AND n_search.note_text LIKE ? ESCAPE '\\'
          )
          OR COALESCE(h.last_source_app_name, '') LIKE ? ESCAPE '\\'
          OR EXISTS (
              SELECT 1 FROM sync_sources AS s_search WHERE s_search.item_id = i.id
                AND (s_search.app_name LIKE ? ESCAPE '\\' OR s_search.device_name LIKE ? ESCAPE '\\')
          ))"
            .to_owned(),
    );
    parameters.push(pattern.clone());
    parameters.push(pattern.clone());
    parameters.push(pattern.clone());
    parameters.push(pattern.clone());
    parameters.push(pattern.clone());
    parameters.push(pattern);
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

impl HistoryQuery {
    pub fn from_filters(filters: impl IntoIterator<Item = HistoryFilter>) -> Self {
        let mut query = Self::default();
        let mut unpasted = false;
        let mut pasted = false;
        for filter in filters {
            match filter {
                HistoryFilter::Text => {
                    query.content_filters.insert(HistoryContentFilter::Text);
                }
                HistoryFilter::Image => {
                    query.content_filters.insert(HistoryContentFilter::Image);
                }
                HistoryFilter::File => {
                    query.content_filters.insert(HistoryContentFilter::File);
                }
                HistoryFilter::Link => {
                    query.content_filters.insert(HistoryContentFilter::Link);
                }
                HistoryFilter::Unpasted => unpasted = true,
                HistoryFilter::Pasted => pasted = true,
            }
        }
        query.paste_filter = match (unpasted, pasted) {
            (true, false) => Some(PasteFilter::Unpasted),
            (false, true) => Some(PasteFilter::Pasted),
            _ => None,
        };
        query
    }
}

#[derive(Debug, Default)]
pub struct ListOrderSnapshot {
    pub ids: Vec<HistoryItemId>,
    pub paste_counts: Vec<u64>,
}

pub(super) fn collect_order_snapshot(
    mut rows: rusqlite::Rows<'_>,
    include_heat: bool,
) -> Result<ListOrderSnapshot, HistoryQueryError> {
    let mut snapshot = ListOrderSnapshot::default();
    while let Some(row) = rows.next()? {
        snapshot.ids.push(row.get(0)?);
        if include_heat {
            let count: i64 = row.get(1)?;
            snapshot
                .paste_counts
                .push(u64::try_from(count).map_err(|_| {
                    HistoryQueryError::InvalidNonNegativeValue("paste_count".to_owned())
                })?);
        }
    }
    Ok(snapshot)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryListRecord {
    /// 当前列表关系 ID：历史查询返回历史 ID，收藏查询返回收藏 ID。
    pub history_id: HistoryItemId,
    pub item_id: ClipboardItemId,
    pub favorite_id: Option<i64>,
    pub tag_ids: Vec<i64>,
    pub note_text: String,
    pub preview_text: String,
    pub preview_char_count: usize,
    pub preview_options: Vec<FormatPreview>,
    pub all_format_options: Vec<FormatPreview>,
    pub content_kind: ContentKind,
    pub last_seen_at: i64,
    pub paste_count: u64,
    pub is_pinned: bool,
    pub sort_order: i64,
    pub source_app_name: Option<String>,
    pub source_device_name: Option<String>,
    pub source_app_icon: Option<ApplicationIconSource>,
}

pub(super) struct LoadedPreviewOptions {
    pub(super) visible: Vec<FormatPreview>,
    pub(super) all: Vec<FormatPreview>,
}

pub(super) fn parse_tag_ids(value: &str) -> Result<Vec<i64>, HistoryQueryError> {
    let mut tag_ids = value
        .split(',')
        .filter(|value| !value.is_empty())
        .map(|value| {
            let tag_id = value
                .parse::<i64>()
                .map_err(|_| HistoryQueryError::InvalidNonNegativeValue("tag_id".to_owned()))?;
            if tag_id < 0 {
                return Err(HistoryQueryError::InvalidNonNegativeValue(
                    "tag_id".to_owned(),
                ));
            }
            Ok(tag_id)
        })
        .collect::<Result<Vec<_>, _>>()?;
    tag_ids.sort_unstable();
    tag_ids.dedup();
    Ok(tag_ids)
}

#[derive(Debug, Error)]
pub enum HistoryQueryError {
    #[error("无法连接历史数据库：{0}")]
    Database(#[from] DatabaseError),
    #[error("历史查询失败：{0}")]
    Sql(#[from] rusqlite::Error),
    #[error("数据库包含无效的图片尺寸")]
    InvalidImageDimensions,
    #[error("数据库包含无效的非负数值：{0}")]
    InvalidNonNegativeValue(String),
    #[error("数据库包含无效的图片数据引用：{0}")]
    InvalidImageSource(String),
    #[error("数据库包含无效的应用图标资源：{0}")]
    InvalidApplicationIcon(String),
    #[error("读取 HTML 原始数据失败：{0}")]
    HtmlStorage(#[from] crate::services::storage::RawBlobError),
    #[error("读取文件列表原始数据失败：{0}")]
    FileListStorage(crate::services::storage::RawBlobError),
    #[error("数据库中的文件列表数据引用无效：{0}")]
    InvalidFileListSource(String),
}

pub(super) fn parse_application_icon_source(
    hash: Option<Vec<u8>>,
    byte_length: Option<i64>,
    relative_path: Option<String>,
) -> Result<Option<ApplicationIconSource>, HistoryQueryError> {
    if hash.is_none() && byte_length.is_none() && relative_path.is_none() {
        return Ok(None);
    }
    let hash =
        hash.ok_or_else(|| HistoryQueryError::InvalidApplicationIcon("缺少图标哈希".to_owned()))?;
    let byte_length = byte_length
        .ok_or_else(|| HistoryQueryError::InvalidApplicationIcon("缺少图标长度".to_owned()))?;
    let relative_path = relative_path
        .ok_or_else(|| HistoryQueryError::InvalidApplicationIcon("缺少图标路径".to_owned()))?;
    let sha256: [u8; 32] = hash.try_into().map_err(|hash: Vec<u8>| {
        HistoryQueryError::InvalidApplicationIcon(format!("图标哈希长度为 {}，应为 32", hash.len()))
    })?;
    let byte_length = u64::try_from(byte_length)
        .map_err(|_| HistoryQueryError::InvalidApplicationIcon("图标长度不能为负数".to_owned()))?;
    if relative_path.trim().is_empty() {
        return Err(HistoryQueryError::InvalidApplicationIcon(
            "图标路径不能为空".to_owned(),
        ));
    }
    Ok(Some(ApplicationIconSource {
        sha256,
        byte_length,
        relative_path,
    }))
}

pub(super) fn parse_content_kind(value: &str) -> Result<ContentKind, HistoryQueryError> {
    match value {
        "text" => Ok(ContentKind::Text),
        "link" => Ok(ContentKind::Link),
        "image" => Ok(ContentKind::Image),
        "file_list" => Ok(ContentKind::FileList),
        "mixed" => Ok(ContentKind::Mixed),
        "custom" => Ok(ContentKind::Custom),
        "unknown" => Ok(ContentKind::Unknown),
        _ => Ok(ContentKind::Unknown),
    }
}

pub(super) fn parse_format_kind(value: &str) -> Result<FormatKind, HistoryQueryError> {
    Ok(FormatKind::from_str(value).unwrap_or(FormatKind::Unknown))
}

pub(super) fn load_preview_options(
    connection: &Connection,
    item_id: ClipboardItemId,
    preview_char_limit: Option<usize>,
    load_image_sources: bool,
    load_html_content: bool,
    html_content_char_limit: Option<usize>,
    blob_store: Option<&RawBlobStore>,
) -> Result<LoadedPreviewOptions, HistoryQueryError> {
    let preview_text = preview_text_expression("preview_text", preview_char_limit);
    let mut statement = connection.prepare(&format!(
        "SELECT
            format_namespace,
            format_key,
            format_name,
            format_order,
            format_kind,
            {preview_text},
            preview_char_count,
            preview_content_kind,
            image_width,
            image_height,
            restore_state,
            f.storage_kind,
            CASE
                WHEN f.format_kind IN ('html', 'image', 'file_list') THEN f.inline_data
                ELSE NULL
            END,
            b.sha256,
            b.byte_length,
            b.relative_path
         FROM clipboard_formats AS f
         LEFT JOIN binary_blobs AS b ON b.id = f.blob_id
         WHERE f.item_id = ?1
         ORDER BY format_order"
    ))?;
    let rows = statement.query_map([item_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, Option<i64>>(6)?,
            row.get::<_, Option<String>>(7)?,
            row.get::<_, Option<i64>>(8)?,
            row.get::<_, Option<i64>>(9)?,
            row.get::<_, String>(10)?,
            row.get::<_, String>(11)?,
            row.get::<_, Option<Vec<u8>>>(12)?,
            row.get::<_, Option<Vec<u8>>>(13)?,
            row.get::<_, Option<i64>>(14)?,
            row.get::<_, Option<String>>(15)?,
        ))
    })?;
    let mut previews = Vec::new();
    let mut image_sources = Vec::<Option<ImagePreviewSource>>::new();
    for row in rows {
        let (
            format_namespace,
            format_key,
            format_name,
            format_order,
            format_kind,
            text,
            char_count,
            content_kind,
            image_width,
            image_height,
            restore_state,
            storage_kind,
            inline_data,
            blob_sha256,
            blob_byte_length,
            blob_relative_path,
        ) = row?;
        let char_count = char_count.unwrap_or(0);
        let content_kind = content_kind.unwrap_or_else(|| "unknown".to_owned());
        let known_format = FormatKind::from_str(&format_kind).is_some();
        let supported_storage = matches!(storage_kind.as_str(), "inline" | "external");
        let format_kind = parse_format_kind(&format_kind)?;
        let image_dimensions = parse_image_dimensions(image_width, image_height)?;
        let mut preview = FormatPreview {
            format_namespace,
            format_key,
            format_name,
            format_order: u32::try_from(format_order).map_err(|_| {
                HistoryQueryError::InvalidNonNegativeValue(
                    "clipboard_formats.format_order".to_owned(),
                )
            })?,
            format_kind,
            text,
            char_count: usize::try_from(char_count).map_err(|_| {
                HistoryQueryError::InvalidNonNegativeValue(
                    "clipboard_formats.preview_char_count".to_owned(),
                )
            })?,
            content_kind: parse_content_kind(&content_kind)?,
            html_content: None,
            image_source: None,
            image_dimensions,
            is_restorable: known_format && supported_storage && restore_state == "restorable",
        };
        let source = (
            storage_kind,
            inline_data,
            blob_sha256,
            blob_byte_length,
            blob_relative_path,
        );
        if supported_storage && load_html_content && preview.role() == FormatRole::Html {
            preview.html_content =
                load_html_content_source(source, html_content_char_limit, blob_store)?;
            image_sources.push(None);
        } else if supported_storage
            && load_image_sources
            && matches!(
                preview.format_kind,
                FormatKind::Image | FormatKind::FileList
            )
        {
            image_sources.push(load_image_preview_source(source)?);
        } else {
            image_sources.push(None);
        }
        previews.push(preview);
    }
    if load_image_sources {
        let all = previews.clone();
        for (preview, image_source) in previews.iter_mut().zip(image_sources) {
            if matches!(
                preview.format_kind,
                FormatKind::Image | FormatKind::FileList
            ) {
                preview.image_source = image_source;
            }
        }

        let mut visible_previews = select_user_visible_previews(&previews);
        let mut image_fallbacks = previews
            .into_iter()
            .filter(|preview| {
                preview.role() == crate::domain::capture::FormatRole::Image
                    && preview.is_restorable
                    && preview.image_source.is_some()
                    && !visible_previews.iter().any(|visible| {
                        visible.format_namespace == preview.format_namespace
                            && visible.format_key == preview.format_key
                    })
            })
            .collect::<Vec<_>>();
        image_fallbacks.sort_by_key(image_fallback_priority);
        visible_previews.extend(image_fallbacks);
        return Ok(LoadedPreviewOptions {
            visible: visible_previews,
            all,
        });
    }
    Ok(LoadedPreviewOptions {
        visible: select_user_visible_previews(&previews),
        all: previews,
    })
}

fn parse_image_dimensions(
    width: Option<i64>,
    height: Option<i64>,
) -> Result<Option<ImageDimensions>, HistoryQueryError> {
    match (width, height) {
        (None, None) => Ok(None),
        (Some(width), Some(height)) if width > 0 && height > 0 => Ok(Some(ImageDimensions {
            width: u32::try_from(width).map_err(|_| HistoryQueryError::InvalidImageDimensions)?,
            height: u32::try_from(height).map_err(|_| HistoryQueryError::InvalidImageDimensions)?,
        })),
        _ => Err(HistoryQueryError::InvalidImageDimensions),
    }
}

const MAX_HTML_CONTENT_BYTES: i64 = 2 * 1024 * 1024;

fn load_html_content_source(
    source: (
        String,
        Option<Vec<u8>>,
        Option<Vec<u8>>,
        Option<i64>,
        Option<String>,
    ),
    max_visible_chars: Option<usize>,
    blob_store: Option<&RawBlobStore>,
) -> Result<Option<Arc<str>>, HistoryQueryError> {
    let bytes = match source {
        (storage_kind, Some(bytes), None, None, None) if storage_kind == "inline" => {
            if bytes.len() as i64 > MAX_HTML_CONTENT_BYTES {
                return Ok(None);
            }
            bytes
        }
        (storage_kind, None, Some(sha256), Some(byte_length), Some(relative_path))
            if storage_kind == "external" && byte_length <= MAX_HTML_CONTENT_BYTES =>
        {
            let Some(blob_store) = blob_store else {
                return Ok(None);
            };
            let sha256: [u8; 32] = sha256.try_into().map_err(|value: Vec<u8>| {
                HistoryQueryError::InvalidImageSource(format!(
                    "HTML 原始数据哈希长度为 {}，应为 32",
                    value.len()
                ))
            })?;
            blob_store.read(&RawBlob::from_record(
                sha256,
                u64::try_from(byte_length).map_err(|_| {
                    HistoryQueryError::InvalidNonNegativeValue(
                        "clipboard_formats.byte_length".to_owned(),
                    )
                })?,
                relative_path,
            ))?
        }
        _ => return Ok(None),
    };

    let html = String::from_utf8_lossy(&bytes);
    let html = match max_visible_chars {
        Some(max_visible_chars) => {
            crate::domain::html::truncate_for_preview(&html, max_visible_chars)
        }
        None => html.into_owned(),
    };
    Ok(Some(html.into()))
}

fn image_fallback_priority(preview: &FormatPreview) -> u8 {
    match preview.format_key.to_ascii_lowercase().as_str() {
        "cf_dib" => 0,
        "cf_dibv5" => 1,
        _ => 2,
    }
}

fn load_image_preview_source(
    source: (
        String,
        Option<Vec<u8>>,
        Option<Vec<u8>>,
        Option<i64>,
        Option<String>,
    ),
) -> Result<Option<ImagePreviewSource>, HistoryQueryError> {
    match source {
        (storage_kind, Some(bytes), None, None, None) if storage_kind == "inline" => {
            Ok(Some(ImagePreviewSource::Inline {
                bytes: bytes.into(),
            }))
        }
        (storage_kind, None, Some(sha256), Some(byte_length), Some(relative_path))
            if storage_kind == "external" =>
        {
            let sha256: [u8; 32] = sha256.try_into().map_err(|value: Vec<u8>| {
                HistoryQueryError::InvalidImageSource(format!(
                    "哈希长度为 {}，应为 32",
                    value.len()
                ))
            })?;
            let byte_length = u64::try_from(byte_length).map_err(|_| {
                HistoryQueryError::InvalidNonNegativeValue(
                    "clipboard_formats.image_byte_length".to_owned(),
                )
            })?;
            Ok(Some(ImagePreviewSource::External {
                sha256,
                byte_length,
                relative_path,
            }))
        }
        (storage_kind, None, None, None, None) if storage_kind == "none" => Ok(None),
        (storage_kind, ..) => Err(HistoryQueryError::InvalidImageSource(format!(
            "存储类型“{storage_kind}”的数据结构不完整"
        ))),
    }
}

pub(super) fn preview_text_expression(column: &str, preview_char_limit: Option<usize>) -> String {
    let column = format!("COALESCE({column}, '')");
    preview_char_limit.map_or(column.clone(), |limit| {
        let limit = limit.min(i64::MAX as usize);
        format!("substr({column}, 1, {limit})")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_filter_states_convert_to_query_parameters() {
        let query = HistoryQuery::from_filters([
            HistoryFilter::Text,
            HistoryFilter::Image,
            HistoryFilter::File,
            HistoryFilter::Link,
            HistoryFilter::Unpasted,
        ]);
        assert_eq!(query.content_filters.len(), 4);
        assert_eq!(query.paste_filter, Some(PasteFilter::Unpasted));

        let both_paste_states =
            HistoryQuery::from_filters([HistoryFilter::Unpasted, HistoryFilter::Pasted]);
        assert_eq!(both_paste_states.paste_filter, None);
    }
}
