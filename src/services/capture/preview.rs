use unicode_segmentation::UnicodeSegmentation;

use crate::domain::{
    capture::{CapturedClipboard, FormatRole, RawFormat, classify_format_role},
    clipboard::{
        ContentKind, FormatPreview, has_native_image, is_empty_textual_preview, is_image_only_html,
    },
    html::fragment as html_fragment,
};

use super::CaptureError;
use crate::services::image_preview::probe_image_dimensions;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClipboardPreview {
    pub formats: Vec<FormatPreview>,
    pub content_kind: ContentKind,
}

pub fn generate_preview(captured: &CapturedClipboard) -> Result<ClipboardPreview, CaptureError> {
    let formats = captured
        .formats
        .iter()
        .filter(|format| format.capture_state.has_data() && format.bytes.is_some())
        .map(|format| preview_format(captured, format))
        .collect::<Vec<_>>();
    if formats.is_empty() {
        return Err(CaptureError::NoCapturedFormats);
    }
    let content_kind = classify_content_kind(&formats);
    Ok(ClipboardPreview {
        formats,
        content_kind,
    })
}

fn classify_content_kind(previews: &[FormatPreview]) -> ContentKind {
    let has_rich_text = previews.iter().any(|preview| {
        matches!(preview.role(), FormatRole::Html | FormatRole::Rtf)
            && matches!(
                preview.content_kind,
                ContentKind::Text | ContentKind::Link | ContentKind::Mixed
            )
    });
    let has_image = previews
        .iter()
        .any(|preview| preview.role() == FormatRole::Image);
    let has_image_file = previews
        .iter()
        .any(|preview| preview.content_kind == ContentKind::Image);
    let has_file_list = previews
        .iter()
        .any(|preview| preview.content_kind == ContentKind::FileList);
    let has_text = previews.iter().any(|preview| {
        preview.role() == FormatRole::PlainText
            && matches!(preview.content_kind, ContentKind::Text | ContentKind::Link)
    });
    let has_custom = previews
        .iter()
        .any(|preview| preview.role() == FormatRole::Custom);

    if has_rich_text
        || ((has_image || has_image_file) && (has_text || has_file_list))
        || (has_file_list && has_text)
    {
        return ContentKind::Mixed;
    }
    if has_image || has_image_file {
        return ContentKind::Image;
    }
    if has_file_list {
        return ContentKind::FileList;
    }
    if has_text {
        return if previews
            .iter()
            .any(|preview| preview.content_kind == ContentKind::Link)
        {
            ContentKind::Link
        } else {
            ContentKind::Text
        };
    }
    if has_custom {
        ContentKind::Custom
    } else {
        ContentKind::Unknown
    }
}

fn preview_format(captured: &CapturedClipboard, format: &RawFormat) -> FormatPreview {
    let role = classify_format_role(format.format_kind, &format.format_key, &format.format_name);
    let (mut text, content_kind) = match role {
        FormatRole::PlainText => {
            let text = decode_text(format);
            let content_kind = if text.trim().is_empty() {
                ContentKind::Unknown
            } else if !extract_links(&text).is_empty() {
                ContentKind::Link
            } else {
                ContentKind::Text
            };
            (text, content_kind)
        }
        FormatRole::Html => {
            let html = decode_utf8(format.bytes.as_deref().unwrap_or_default());
            let text = strip_html(&html);
            let has_text = !text.trim().is_empty();
            let has_image = html_contains_image(html_fragment(&html));
            let content_kind = match (has_text, has_image) {
                (true, true) => ContentKind::Mixed,
                (true, false) if is_link(&text) => ContentKind::Link,
                (true, false) => ContentKind::Text,
                (false, true) => ContentKind::Image,
                (false, false) => ContentKind::Unknown,
            };
            (text, content_kind)
        }
        FormatRole::Image => (
            format!(
                "图片（{} KB）",
                format
                    .bytes
                    .as_ref()
                    .map_or(0, |bytes| bytes.len().div_ceil(1024))
            ),
            ContentKind::Image,
        ),
        FormatRole::FileList => (
            captured
                .preview_candidate
                .clone()
                .unwrap_or_else(|| "文件列表".to_owned()),
            classify_file_list_content(format),
        ),
        FormatRole::VirtualFile => ("虚拟文件".to_owned(), ContentKind::FileList),
        FormatRole::Rtf => ("富文本内容".to_owned(), ContentKind::Text),
        FormatRole::Custom => ("无法预览的剪贴板内容".to_owned(), ContentKind::Custom),
        FormatRole::Internal => ("无法预览的剪贴板内容".to_owned(), ContentKind::Unknown),
    };
    text = text.trim_matches('\0').trim().to_owned();
    if text.is_empty() {
        text = if role == FormatRole::Html && content_kind == ContentKind::Image {
            "图片（HTML）".to_owned()
        } else {
            "空内容".to_owned()
        };
    }
    let char_count = UnicodeSegmentation::graphemes(text.as_str(), true).count();
    let image_dimensions = if role == FormatRole::Image {
        format
            .bytes
            .as_deref()
            .and_then(|bytes| probe_image_dimensions(bytes, &format.format_key))
    } else {
        None
    };
    FormatPreview {
        format_namespace: format.format_namespace.clone(),
        format_key: format.format_key.clone(),
        format_name: format.format_name.clone(),
        format_order: format.format_order,
        format_kind: format.format_kind,
        text,
        char_count,
        content_kind,
        html_content: None,
        image_source: None,
        image_dimensions,
        is_restorable: format.restore_state == crate::domain::capture::RestoreState::Restorable,
    }
}

fn html_contains_image(input: &str) -> bool {
    input.to_ascii_lowercase().split('<').any(|fragment| {
        let Some(rest) = fragment.strip_prefix("img") else {
            return false;
        };
        rest.chars().next().is_none_or(|character| {
            character.is_ascii_whitespace() || matches!(character, '/' | '>')
        })
    })
}

fn classify_file_list_content(format: &RawFormat) -> ContentKind {
    let paths = parse_file_list_paths(format.bytes.as_deref().unwrap_or_default());
    if paths.len() == 1 && is_image_file_path(&paths[0]) {
        ContentKind::Image
    } else {
        ContentKind::FileList
    }
}

fn parse_file_list_paths(bytes: &[u8]) -> Vec<String> {
    let (data, wide) = if bytes.len() >= 20 {
        let offset = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
        if offset < bytes.len() {
            (
                &bytes[offset..],
                u32::from_le_bytes(bytes[16..20].try_into().unwrap()) != 0,
            )
        } else {
            (bytes, false)
        }
    } else {
        (bytes, false)
    };

    if wide {
        let mut paths = Vec::new();
        let mut current = Vec::new();
        for chunk in data.chunks_exact(2) {
            let unit = u16::from_le_bytes([chunk[0], chunk[1]]);
            if unit == 0 {
                if current.is_empty() {
                    break;
                }
                paths.push(String::from_utf16_lossy(&current));
                current.clear();
            } else {
                current.push(unit);
            }
        }
        if !current.is_empty() {
            paths.push(String::from_utf16_lossy(&current));
        }
        return paths;
    }

    data.split(|byte| *byte == 0)
        .take_while(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect()
}

fn is_image_file_path(path: &str) -> bool {
    let file_name = path
        .rsplit(|character| character == '\\' || character == '/')
        .next()
        .unwrap_or(path);
    let Some(extension) = file_name.rsplit_once('.').map(|(_, extension)| extension) else {
        return false;
    };
    crate::domain::capture::image_format(extension).is_some()
}

pub fn select_default_preview<'a>(
    captured: &CapturedClipboard,
    previews: &'a [FormatPreview],
) -> Option<&'a FormatPreview> {
    let native_image_available = has_native_image(previews);
    if let Some((primary_namespace, primary_key)) = captured.primary_format_identity.as_ref()
        && let Some(preview) = previews.iter().find(|preview| {
            preview.format_namespace == *primary_namespace
                && preview.format_key == *primary_key
                && !preview.role().is_internal()
                && !is_empty_textual_preview(preview)
                && !(native_image_available && is_image_only_html(preview))
        })
    {
        return Some(preview);
    }
    previews
        .iter()
        .filter(|preview| {
            !(preview.role().is_internal()
                || is_empty_textual_preview(preview)
                || native_image_available && is_image_only_html(preview))
        })
        .min_by_key(|preview| preview.format_order)
}

fn decode_text(format: &RawFormat) -> String {
    let bytes = format.bytes.as_deref().unwrap_or_default();
    let is_utf16 = format.format_key.eq_ignore_ascii_case("CF_UNICODETEXT")
        || format.encoding.as_deref() == Some("utf-16le");
    if is_utf16 {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .take_while(|unit| *unit != 0)
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        decode_utf8(bytes)
    }
}

fn decode_utf8(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_end_matches('\0')
        .to_owned()
}

fn is_link(text: &str) -> bool {
    let text = text.trim();
    !text.chars().any(char::is_whitespace)
        && (text.starts_with("https://") || text.starts_with("http://"))
}

/// 从文本中提取全部可打开的链接。
///
/// 识别 `http(s)://` 与 `www.` 开头的链接；`www.` 链接在使用时补上 `https://`。
/// 返回顺序保持文本中出现顺序，并去除重复。
pub(crate) fn extract_links(text: &str) -> Vec<String> {
    // URL 中不会出现的分隔符：空白、常见成对/引用符号，以及全角标点。
    const SEPARATORS: &[char] = &[
        '<', '>', '"', '\'', '`', '(', ')', '[', ']', '{', '}', '|', '\\', '^', '，', '。', '、',
        '；', '：', '！', '？', '（', '）', '【', '】', '《', '》', '“', '”', '‘', '’', '〈', '〉',
        '〔', '〕', '—',
    ];
    let mut links = Vec::new();
    for token in text.split(|c: char| c.is_whitespace() || SEPARATORS.contains(&c)) {
        if let Some(link) = normalize_link(token) {
            if !links.contains(&link) {
                links.push(link);
            }
        }
    }
    links
}

/// 判断一段 token 是否为可打开链接，是则返回规范化后的形式。
fn normalize_link(token: &str) -> Option<String> {
    // 剥掉黏在 URL 两端的标点（句读、括号残余等）。
    let trimmed = token.trim_matches(|c: char| {
        matches!(
            c,
            '.' | ','
                | ';'
                | ':'
                | '!'
                | '?'
                | '*'
                | '~'
                | '/'
                | '。'
                | '，'
                | '；'
                | '：'
                | '！'
                | '？'
                | '、'
                | '）'
                | '】'
                | '”'
                | '’'
                | '"'
                | '\''
        )
    });
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        // 必须含有主机部分，避免把 "http://" 之类残缺串当成链接。
        let rest = &trimmed[trimmed.find("://").map(|i| i + 3).unwrap_or(0)..];
        if rest.is_empty() {
            return None;
        }
        Some(trimmed.to_owned())
    } else if lower.starts_with("www.") && trimmed.len() > "www.".len() {
        Some(format!("https://{trimmed}"))
    } else {
        None
    }
}

fn strip_html(input: &str) -> String {
    let html = html_fragment(input);
    let mut output = String::with_capacity(html.len());
    let mut inside_tag = false;
    let mut tag = String::new();
    let mut skip_until: Option<&str> = None;
    for character in html.chars() {
        if character == '<' {
            inside_tag = true;
            tag.clear();
            continue;
        }
        if inside_tag {
            if character == '>' {
                inside_tag = false;
                let normalized = tag.trim().to_ascii_lowercase();
                let tag_name = normalized
                    .trim_start_matches('/')
                    .split_whitespace()
                    .next()
                    .unwrap_or_default();
                match (normalized.starts_with('/'), tag_name) {
                    (false, "script") => skip_until = Some("script"),
                    (false, "style") => skip_until = Some("style"),
                    (true, name) if skip_until == Some(name) => skip_until = None,
                    (_, "br" | "p" | "div" | "li") if skip_until.is_none() => output.push('\n'),
                    _ => {}
                }
            } else {
                tag.push(character);
            }
            continue;
        }
        if skip_until.is_none() {
            output.push(character);
        }
    }
    decode_entities(&output)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn decode_entities(input: &str) -> String {
    input
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use crate::domain::capture::{FormatKind, SourceApplication};
    use crate::domain::clipboard::select_user_visible_previews;

    use super::*;

    fn captured(format: RawFormat) -> CapturedClipboard {
        CapturedClipboard {
            primary_format_identity: Some((
                format.format_namespace.clone(),
                format.format_key.clone(),
            )),
            formats: vec![format],
            preview_candidate: None,
            source_application: SourceApplication::default(),
        }
    }

    fn dropfiles(paths: &[&str]) -> Vec<u8> {
        let mut bytes = vec![0_u8; 20];
        bytes[0..4].copy_from_slice(&20_u32.to_le_bytes());
        bytes[16..20].copy_from_slice(&1_u32.to_le_bytes());
        for path in paths {
            bytes.extend(path.encode_utf16().flat_map(u16::to_le_bytes));
            bytes.extend_from_slice(&[0, 0]);
        }
        bytes.extend_from_slice(&[0, 0]);
        bytes
    }

    fn file_format(paths: &[&str]) -> RawFormat {
        RawFormat::captured(
            "windows",
            "CF_HDROP",
            "CF_HDROP",
            FormatKind::FileList,
            dropfiles(paths),
            0,
            true,
        )
    }

    #[test]
    fn text_preview_uses_user_perceived_character_count() {
        let value = captured(RawFormat::captured(
            "mime",
            "text/plain",
            "纯文本",
            FormatKind::Text,
            "👨‍👩‍👧‍👦a".as_bytes().to_vec(),
            0,
            true,
        ));
        let preview = generate_preview(&value).expect("无法生成文本预览");
        assert_eq!(preview.formats[0].text, "👨‍👩‍👧‍👦a");
        assert_eq!(preview.formats[0].char_count, 2);
    }

    #[test]
    fn single_image_file_is_classified_as_image_with_file_list_format() {
        for path in [
            "C:\\图片\\壁纸.PNG",
            "C:\\图片\\壁纸.TIFF",
            "C:\\图片\\图标.ICO",
        ] {
            let preview =
                generate_preview(&captured(file_format(&[path]))).expect("无法生成图片文件预览");
            assert_eq!(preview.content_kind, ContentKind::Image);
            assert_eq!(preview.formats[0].format_kind, FormatKind::FileList);
            assert_eq!(preview.formats[0].content_kind, ContentKind::Image);
        }
    }

    #[test]
    fn multiple_images_or_mixed_files_are_classified_as_file_list() {
        for paths in [
            vec!["C:\\图片\\一.png", "C:\\图片\\二.jpg"],
            vec!["C:\\图片\\一.png", "C:\\文档\\说明.txt"],
        ] {
            let preview =
                generate_preview(&captured(file_format(&paths))).expect("无法生成文件列表预览");
            assert_eq!(preview.content_kind, ContentKind::FileList);
            assert_eq!(preview.formats[0].content_kind, ContentKind::FileList);
        }
    }

    #[test]
    fn html_preview_removes_tags_and_scripts() {
        let value = captured(RawFormat::captured(
            "mime",
            "text/html",
            "HTML",
            FormatKind::Html,
            b"<div>Hello&nbsp;<b>world</b></div><script>secret()</script>".to_vec(),
            0,
            true,
        ));
        let preview = generate_preview(&value).expect("无法生成 HTML 预览");
        assert_eq!(preview.formats[0].text, "Hello world");
        assert!(!preview.formats[0].text.contains("secret"));
    }

    #[test]
    fn image_only_html_is_classified_as_image_and_duplicate_html_is_hidden() {
        let html = RawFormat::captured(
            "windows",
            "registered:HTML Format",
            "HTML Format",
            FormatKind::Html,
            br#"<html><body><img src="data:image/png;base64,abc"></body></html>"#.to_vec(),
            0,
            true,
        );
        let image = RawFormat::captured(
            "windows",
            "registered:PNG",
            "PNG",
            FormatKind::Image,
            b"image-bytes".to_vec(),
            1,
            true,
        );
        let value = CapturedClipboard {
            primary_format_identity: Some((html.format_namespace.clone(), html.format_key.clone())),
            formats: vec![html, image],
            preview_candidate: None,
            source_application: SourceApplication::default(),
        };

        let generated = generate_preview(&value).expect("无法生成图片 HTML 预览");
        assert_eq!(generated.content_kind, ContentKind::Image);
        assert_eq!(generated.formats[0].content_kind, ContentKind::Image);
        let visible = select_user_visible_previews(&generated.formats);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].format_key, "registered:PNG");
        assert_eq!(
            select_default_preview(&value, &generated.formats)
                .expect("无法选择默认图片预览")
                .format_key,
            "registered:PNG"
        );
    }

    #[test]
    fn image_html_is_retained_as_fallback_without_real_image_payload() {
        let value = captured(RawFormat::captured(
            "windows",
            "registered:HTML Format",
            "HTML Format",
            FormatKind::Html,
            br#"<img src="https://example.com/image.png">"#.to_vec(),
            0,
            true,
        ));

        let generated = generate_preview(&value).expect("无法生成图片型 HTML 预览");
        assert_eq!(generated.content_kind, ContentKind::Image);
        assert_eq!(generated.formats[0].text, "图片（HTML）");
        let visible = select_user_visible_previews(&generated.formats);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].format_key, "registered:HTML Format");
    }

    #[test]
    fn empty_html_is_not_classified_as_text() {
        let value = captured(RawFormat::captured(
            "windows",
            "registered:HTML Format",
            "HTML Format",
            FormatKind::Html,
            b"<div><br></div>".to_vec(),
            0,
            true,
        ));
        let preview = generate_preview(&value).expect("无法生成空 HTML 预览");
        assert_eq!(preview.content_kind, ContentKind::Unknown);
        assert_eq!(preview.formats[0].content_kind, ContentKind::Unknown);
        assert!(select_user_visible_previews(&preview.formats).is_empty());
    }

    #[test]
    fn mixed_text_and_image_html_retains_html_candidate() {
        let html = RawFormat::captured(
            "windows",
            "registered:HTML Format",
            "HTML Format",
            FormatKind::Html,
            "<p>图片说明</p><img src=\"data:image/png;base64,abc\">"
                .as_bytes()
                .to_vec(),
            0,
            true,
        );
        let image = RawFormat::captured(
            "windows",
            "registered:PNG",
            "PNG",
            FormatKind::Image,
            b"image-bytes".to_vec(),
            1,
            true,
        );
        let value = CapturedClipboard {
            primary_format_identity: Some((html.format_namespace.clone(), html.format_key.clone())),
            formats: vec![html, image],
            preview_candidate: None,
            source_application: SourceApplication::default(),
        };

        let generated = generate_preview(&value).expect("无法生成混合 HTML 预览");
        assert_eq!(generated.content_kind, ContentKind::Mixed);
        assert_eq!(
            select_user_visible_previews(&generated.formats)
                .iter()
                .map(|preview| preview.format_key.as_str())
                .collect::<Vec<_>>(),
            ["registered:HTML Format", "registered:PNG"]
        );
    }

    #[test]
    fn single_url_is_classified_as_link() {
        let value = captured(RawFormat::captured(
            "mime",
            "text/plain",
            "纯文本",
            FormatKind::Text,
            b"https://example.com".to_vec(),
            0,
            true,
        ));
        assert_eq!(
            generate_preview(&value)
                .expect("无法生成链接预览")
                .content_kind,
            ContentKind::Link
        );
    }

    #[test]
    fn text_containing_url_is_classified_as_link() {
        let value = captured(RawFormat::captured(
            "mime",
            "text/plain",
            "纯文本",
            FormatKind::Text,
            "说明文字：https://example.com/path 以及更多内容"
                .as_bytes()
                .to_vec(),
            0,
            true,
        ));
        let preview = generate_preview(&value).expect("无法生成包含链接的文本预览");
        assert_eq!(preview.content_kind, ContentKind::Link);
        assert_eq!(preview.formats[0].content_kind, ContentKind::Link);
    }

    #[test]
    fn text_and_html_formats_are_classified_as_mixed_content() {
        let text = RawFormat::captured(
            "windows",
            "CF_UNICODETEXT",
            "CF_UNICODETEXT",
            FormatKind::Text,
            "测试".encode_utf16().flat_map(u16::to_le_bytes).collect(),
            0,
            true,
        );
        let html = RawFormat::captured(
            "windows",
            "registered:HTML Format",
            "HTML Format",
            FormatKind::Html,
            "<b>测试</b>".as_bytes().to_vec(),
            1,
            true,
        );
        let value = CapturedClipboard {
            primary_format_identity: Some(("windows".to_owned(), "CF_UNICODETEXT".to_owned())),
            formats: vec![text, html],
            preview_candidate: None,
            source_application: SourceApplication::default(),
        };
        assert_eq!(
            generate_preview(&value)
                .expect("无法生成混合预览")
                .content_kind,
            ContentKind::Mixed
        );
    }

    #[test]
    fn multiple_system_text_representations_are_classified_as_text() {
        let unicode = RawFormat::captured(
            "windows",
            "CF_UNICODETEXT",
            "CF_UNICODETEXT",
            FormatKind::Text,
            "测试".encode_utf16().flat_map(u16::to_le_bytes).collect(),
            0,
            true,
        );
        let ansi = RawFormat::captured(
            "windows",
            "CF_TEXT",
            "CF_TEXT",
            FormatKind::Text,
            "测试".as_bytes().to_vec(),
            1,
            true,
        );
        let value = CapturedClipboard {
            primary_format_identity: Some(("windows".to_owned(), "CF_UNICODETEXT".to_owned())),
            formats: vec![unicode, ansi],
            preview_candidate: None,
            source_application: SourceApplication::default(),
        };
        assert_eq!(
            generate_preview(&value)
                .expect("无法生成文本预览")
                .content_kind,
            ContentKind::Text
        );
    }

    #[test]
    fn multi_format_preview_retains_each_format_candidate() {
        let text = RawFormat::captured(
            "windows",
            "CF_UNICODETEXT",
            "CF_UNICODETEXT",
            FormatKind::Text,
            "测试".encode_utf16().flat_map(u16::to_le_bytes).collect(),
            0,
            true,
        );
        let html = RawFormat::captured(
            "windows",
            "registered:HTML Format",
            "HTML Format",
            FormatKind::Html,
            "<b>测试</b>".as_bytes().to_vec(),
            1,
            true,
        );
        let value = CapturedClipboard {
            primary_format_identity: Some(("windows".to_owned(), "CF_UNICODETEXT".to_owned())),
            formats: vec![text, html],
            preview_candidate: None,
            source_application: SourceApplication::default(),
        };
        let preview = generate_preview(&value).expect("无法生成多格式预览");
        assert_eq!(preview.formats.len(), 2);
        assert_eq!(preview.formats[0].format_key, "CF_UNICODETEXT");
        assert_eq!(preview.formats[1].format_key, "registered:HTML Format");
        assert_eq!(
            select_default_preview(&value, &preview.formats)
                .unwrap()
                .format_key,
            "CF_UNICODETEXT"
        );
    }

    #[test]
    fn extract_links_finds_each_supported_form() {
        let links = extract_links(
            "参考 https://a.com/x?y=1 与 http://b.com，以及 www.c.com 和邮箱 a@b.com。",
        );
        assert_eq!(
            links,
            vec![
                "https://a.com/x?y=1".to_owned(),
                "http://b.com".to_owned(),
                "https://www.c.com".to_owned(),
            ]
        );
    }

    #[test]
    fn extract_links_strips_surrounding_punctuation_and_dedupes() {
        let links = extract_links("(https://a.com), https://a.com。 [https://b.com]");
        assert_eq!(
            links,
            vec!["https://a.com".to_owned(), "https://b.com".to_owned()]
        );
    }

    #[test]
    fn extract_links_returns_empty_without_links() {
        assert!(extract_links("普通文本 example.com 没有链接").is_empty());
    }
}
