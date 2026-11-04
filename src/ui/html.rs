use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    mem,
    ops::Range,
    path::PathBuf,
    rc::Rc,
    sync::Arc,
};

use gpui::prelude::*;
use gpui::{
    AnyElement, Div, FontStyle, FontWeight, HighlightStyle, Hsla, ObjectFit, Pixels, RenderImage,
    Rgba, Size, StrikethroughStyle, StyledText, UnderlineStyle, div, img, px, rgba,
};
use html5ever::tendril::TendrilSink;
use html5ever::{ParseOpts, parse_document};
use markup5ever_rcdom::{Handle, NodeData, RcDom};

const MAX_RENDER_NODES: usize = 20_000;
const MAX_LIST_RENDER_NODES: usize = 2_048;
const MAX_RENDER_DEPTH: usize = 64;
const MAX_RENDER_TEXT_BYTES: usize = 64 * 1024;
const MAX_LOCAL_IMAGE_PATHS: usize = 64;
const MAX_PARSED_DOM_CACHE_ENTRIES: usize = 8;
#[allow(dead_code)]
pub const INTERNAL_PREVIEW_LINE_COUNT: f32 = 18.0;

struct ParsedHtml {
    source: String,
    dom: RcDom,
    local_image_paths: RefCell<Option<Vec<PathBuf>>>,
}

thread_local! {
    static PARSED_DOM_CACHE: RefCell<VecDeque<Rc<ParsedHtml>>> = const { RefCell::new(VecDeque::new()) };
}

/// 将剪贴板 HTML 转换为 GPUI 的轻量级展示树。
#[allow(dead_code)]
pub fn render(
    source: &str,
    font_size: Pixels,
    line_height: Pixels,
    fallback_text: String,
) -> AnyElement {
    render_with_images(
        source,
        font_size,
        line_height,
        fallback_text,
        &HashMap::new(),
    )
}

/// 将剪贴板 HTML 转换为 GPUI 的轻量级展示树，并使用已加载的本地图片。
#[allow(dead_code)]
pub fn render_with_images(
    source: &str,
    font_size: Pixels,
    line_height: Pixels,
    fallback_text: String,
    images: &HashMap<PathBuf, Arc<RenderImage>>,
) -> AnyElement {
    render_with_images_and_max_lines(
        source,
        font_size,
        line_height,
        fallback_text,
        images,
        None,
        INTERNAL_PREVIEW_LINE_COUNT as usize,
    )
}

/// 将剪贴板 HTML 转换为预览树，并限制图片和容器的最大预览行数。
pub fn render_with_images_and_max_lines(
    source: &str,
    font_size: Pixels,
    line_height: Pixels,
    fallback_text: String,
    images: &HashMap<PathBuf, Arc<RenderImage>>,
    max_image_width: Option<Pixels>,
    max_preview_lines: usize,
) -> AnyElement {
    let source = crate::domain::html::fragment(source);
    let parsed = parsed_html(source);
    let mut renderer = HtmlRenderer {
        font_size,
        line_height,
        images,
        max_image_width,
        max_preview_lines: max_preview_lines.max(1),
        max_nodes: if max_preview_lines <= INTERNAL_PREVIEW_LINE_COUNT as usize {
            MAX_LIST_RENDER_NODES
        } else {
            MAX_RENDER_NODES
        },
        visited_nodes: 0,
        has_visible_text: false,
    };
    let blocks = renderer.render_children(&parsed.dom.document, HtmlStyle::default(), 0);
    let mut root = div().w_full().flex_col();
    if renderer.has_visible_text {
        root = root.children(blocks);
    } else {
        root = root
            .text_size(font_size)
            .line_height(line_height)
            .child(fallback_text);
    }
    root.into_any_element()
}

/// 用于估算自适应行高的 HTML 预览片段，按文档顺序排列。
pub enum PreviewSegment {
    /// 连续的文本流，块级元素和换行已转换为 `\n`。
    Text(String),
    /// 已加载图片的原始显示尺寸。
    Image(Size<Pixels>),
}

/// 将 HTML 拆分为文本与已加载图片片段，供列表在布局前估算预览高度。
///
/// 尚未加载的图片不会出现在结果中，图片加载完成后需要重新估算。
pub fn preview_segments(
    source: &str,
    images: &HashMap<PathBuf, Arc<RenderImage>>,
) -> Vec<PreviewSegment> {
    let source = crate::domain::html::fragment(source);
    let parsed = parsed_html(source);
    let mut collector = SegmentCollector {
        images,
        segments: Vec::new(),
        text: String::new(),
        visited_nodes: 0,
    };
    collector.collect(&parsed.dom.document, false, 0);
    collector.flush_text();
    collector.segments
}

struct SegmentCollector<'a> {
    images: &'a HashMap<PathBuf, Arc<RenderImage>>,
    segments: Vec<PreviewSegment>,
    text: String,
    visited_nodes: usize,
}

impl SegmentCollector<'_> {
    fn collect(&mut self, node: &Handle, preserve_whitespace: bool, depth: usize) {
        if depth > MAX_RENDER_DEPTH || self.visited_nodes >= MAX_LIST_RENDER_NODES {
            return;
        }
        self.visited_nodes += 1;
        match &node.data {
            NodeData::Text { contents } => {
                for character in contents.borrow().chars() {
                    if self.text.len() >= MAX_RENDER_TEXT_BYTES {
                        break;
                    }
                    if !preserve_whitespace && character.is_whitespace() {
                        if !self.text.is_empty()
                            && !self.text.ends_with(' ')
                            && !self.text.ends_with('\n')
                        {
                            self.text.push(' ');
                        }
                    } else {
                        self.text.push(character);
                    }
                }
            }
            NodeData::Element { .. } => {
                let Some(tag) = element_tag(node) else {
                    return;
                };
                if is_ignored_tag(&tag) {
                    return;
                }
                match tag.as_str() {
                    "br" => self.text.push('\n'),
                    "img" => {
                        let image = attr_value_from_node(node, "src")
                            .and_then(|source| local_image_path(&source))
                            .and_then(|path| self.images.get(&path));
                        if let Some(image) = image {
                            let size = image.size(0);
                            self.flush_text();
                            self.segments.push(PreviewSegment::Image(Size {
                                width: px(size.width.0 as f32),
                                height: px(size.height.0 as f32),
                            }));
                        }
                    }
                    _ => {
                        let block = is_block_tag(&tag);
                        if block {
                            self.break_line();
                        }
                        let preserve_whitespace = preserve_whitespace || tag == "pre";
                        for child in node.children.borrow().iter() {
                            self.collect(child, preserve_whitespace, depth + 1);
                        }
                        if block {
                            self.break_line();
                        }
                    }
                }
            }
            _ => {
                for child in node.children.borrow().iter() {
                    self.collect(child, preserve_whitespace, depth + 1);
                }
            }
        }
    }

    fn break_line(&mut self) {
        let trimmed = self.text.trim_end_matches(' ').len();
        self.text.truncate(trimmed);
        if !self.text.is_empty() && !self.text.ends_with('\n') {
            self.text.push('\n');
        }
    }

    fn flush_text(&mut self) {
        let text = mem::take(&mut self.text);
        let text = text.trim();
        if !text.is_empty() {
            self.segments.push(PreviewSegment::Text(text.to_owned()));
        }
    }
}

/// 提取 HTML 中允许加载的本地图片路径。
///
/// 只返回普通路径和 `file://` 路径，网络、`data:` 以及其它 URI 协议不会被访问。
pub fn local_image_paths(source: &str) -> Vec<PathBuf> {
    let source = crate::domain::html::fragment(source);
    let parsed = parsed_html(source);
    if let Some(paths) = parsed.local_image_paths.borrow().as_ref() {
        return paths.clone();
    }
    let mut paths = Vec::new();
    collect_local_image_paths(&parsed.dom.document, &mut paths, 0);
    *parsed.local_image_paths.borrow_mut() = Some(paths.clone());
    paths
}

fn parsed_html(source: &str) -> Rc<ParsedHtml> {
    PARSED_DOM_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(index) = cache.iter().position(|parsed| parsed.source == source) {
            let parsed = cache.remove(index).expect("HTML DOM 缓存索引应该有效");
            cache.push_back(parsed.clone());
            return parsed.clone();
        }

        let dom = parse_document(RcDom::default(), ParseOpts::default())
            .from_utf8()
            .one(source.as_bytes());
        let parsed = Rc::new(ParsedHtml {
            source: source.to_owned(),
            dom,
            local_image_paths: RefCell::new(None),
        });
        if cache.len() >= MAX_PARSED_DOM_CACHE_ENTRIES {
            cache.pop_front();
        }
        cache.push_back(parsed.clone());
        parsed
    })
}

fn collect_local_image_paths(node: &Handle, paths: &mut Vec<PathBuf>, depth: usize) {
    if depth > MAX_RENDER_DEPTH || paths.len() >= MAX_LOCAL_IMAGE_PATHS {
        return;
    }

    let Some(tag) = element_tag(node) else {
        for child in node.children.borrow().iter() {
            collect_local_image_paths(child, paths, depth + 1);
        }
        return;
    };
    if is_ignored_tag(&tag) {
        return;
    }
    if tag == "img"
        && let Some(path) =
            attr_value_from_node(node, "src").and_then(|source| local_image_path(&source))
        && !paths.contains(&path)
    {
        paths.push(path);
    }

    for child in node.children.borrow().iter() {
        collect_local_image_paths(child, paths, depth + 1);
    }
}

fn attr_value_from_node(node: &Handle, name: &str) -> Option<String> {
    let NodeData::Element { attrs, .. } = &node.data else {
        return None;
    };
    attr_value(attrs, name)
}

fn local_image_path(source: &str) -> Option<PathBuf> {
    let source = source.trim();
    if source.is_empty() {
        return None;
    }

    if source
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("file://"))
    {
        return file_url_path(source);
    }

    if has_uri_scheme(source) {
        return None;
    }
    let path = PathBuf::from(source);
    (path.is_absolute() || is_windows_absolute_path(source)).then_some(path)
}

fn file_url_path(source: &str) -> Option<PathBuf> {
    let remainder = source.get(7..)?;
    let (authority, path) = if remainder.starts_with('/') {
        ("", remainder)
    } else {
        let separator = remainder.find('/').unwrap_or(remainder.len());
        remainder.split_at(separator)
    };
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let path = percent_decode(path)?;
    let authority = percent_decode(authority)?;

    if authority.eq_ignore_ascii_case("localhost") {
        return Some(PathBuf::from(strip_drive_prefix_slash(path)));
    }
    if authority.is_empty() {
        return Some(PathBuf::from(strip_drive_prefix_slash(path)));
    }

    if is_windows_drive_path(&format!("{authority}{path}")) {
        return Some(PathBuf::from(format!("{authority}{path}")));
    }

    let path = path.replace('/', "\\");
    Some(PathBuf::from(format!("\\\\{authority}{path}")))
}

fn strip_drive_prefix_slash(path: String) -> String {
    if path.starts_with('/') && is_windows_drive_path(&path[1..]) {
        path[1..].to_owned()
    } else {
        path
    }
}

fn is_windows_drive_path(source: &str) -> bool {
    let bytes = source.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn is_windows_absolute_path(source: &str) -> bool {
    source.starts_with("\\\\")
        || source.starts_with("//")
        || (is_windows_drive_path(source)
            && source
                .as_bytes()
                .get(2)
                .is_some_and(|separator| matches!(separator, b'/' | b'\\')))
}

fn has_uri_scheme(source: &str) -> bool {
    let Some(colon) = source.find(':') else {
        return false;
    };
    if is_windows_drive_path(source) {
        return false;
    }
    source[..colon]
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic())
        && source[..colon].chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
        })
}

fn percent_decode(source: &str) -> Option<String> {
    let bytes = source.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len() {
            return None;
        }
        let high = hex_digit(bytes[index + 1])?;
        let low = hex_digit(bytes[index + 2])?;
        decoded.push(high << 4 | low);
        index += 3;
    }
    String::from_utf8(decoded).ok()
}

fn hex_digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[derive(Clone, Copy, Default)]
struct HtmlStyle {
    color: Option<Hsla>,
    background_color: Option<Hsla>,
    font_weight: Option<FontWeight>,
    font_style: Option<FontStyle>,
    underline: bool,
    strikethrough: bool,
    link: bool,
    preserve_whitespace: bool,
}

impl HtmlStyle {
    fn for_element(parent: Self, node: &Handle) -> Self {
        let NodeData::Element { name, attrs, .. } = &node.data else {
            return parent;
        };
        let tag = name.local.to_string();
        let mut style = parent;

        match tag.as_str() {
            "strong" | "b" => style.font_weight = Some(FontWeight::BOLD),
            "em" | "i" => style.font_style = Some(FontStyle::Italic),
            "del" | "s" | "strike" => style.strikethrough = true,
            "u" => style.underline = true,
            "a" => style.link = true,
            "code" | "kbd" | "samp" => {
                if style.background_color.is_none() {
                    style.background_color = Some(code_background());
                }
            }
            "mark" => style.background_color = Some(mark_background()),
            "pre" => style.preserve_whitespace = true,
            "font" => {
                if let Some(color) =
                    attr_value(attrs, "color").and_then(|value| parse_color(&value))
                {
                    style.color = Some(color);
                }
            }
            _ => {}
        }

        if let Some(css) = attr_value(attrs, "style") {
            apply_css_declarations(&mut style, &css);
        }
        style
    }

    fn highlight(self) -> HighlightStyle {
        let color = self.color.or_else(|| self.link.then(link_color));
        HighlightStyle {
            color,
            font_weight: self.font_weight,
            font_style: self.font_style,
            background_color: self.background_color,
            underline: self.underline.then(|| UnderlineStyle {
                thickness: px(1.0),
                ..Default::default()
            }),
            strikethrough: self.strikethrough.then(|| StrikethroughStyle {
                thickness: px(1.0),
                ..Default::default()
            }),
            fade_out: None,
        }
    }
}

#[derive(Clone, Default)]
struct InlineContent {
    text: String,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
}

impl InlineContent {
    fn has_visible_text(&self) -> bool {
        self.text
            .chars()
            .any(|character| !character.is_whitespace())
    }

    fn push_text(&mut self, source: &str, style: HtmlStyle) {
        let start = self.text.len();
        for character in source.chars() {
            if self.text.len() >= MAX_RENDER_TEXT_BYTES {
                break;
            }

            if !style.preserve_whitespace && character.is_whitespace() {
                if self.text.ends_with(' ') || self.text.ends_with('\n') {
                    continue;
                }
                self.text.push(' ');
            } else {
                let mut buffer = [0; 4];
                let encoded = character.encode_utf8(&mut buffer);
                if self.text.len() + encoded.len() > MAX_RENDER_TEXT_BYTES {
                    break;
                }
                self.text.push_str(encoded);
            }
        }
        self.push_highlight(start, style);
    }

    fn push_line_break(&mut self, style: HtmlStyle) {
        if self.text.len() >= MAX_RENDER_TEXT_BYTES {
            return;
        }
        let start = self.text.len();
        self.text.push('\n');
        self.push_highlight(start, style);
    }

    fn trim_boundary_line_breaks(&mut self) {
        let leading = self.text.bytes().take_while(|byte| *byte == b'\n').count();
        let trailing = self
            .text
            .bytes()
            .rev()
            .take_while(|byte| *byte == b'\n')
            .count();
        if leading == 0 && trailing == 0 {
            return;
        }

        if leading + trailing >= self.text.len() {
            self.text.clear();
            self.highlights.clear();
            return;
        }

        let end = self.text.len().saturating_sub(trailing);
        self.text = self.text[leading..end].to_owned();
        self.highlights = self
            .highlights
            .iter()
            .filter_map(|(range, style)| {
                let start = range.start.max(leading).min(end);
                let finish = range.end.max(leading).min(end);
                (start < finish).then(|| ((start - leading)..(finish - leading), *style))
            })
            .collect();
    }

    fn push_highlight(&mut self, start: usize, style: HtmlStyle) {
        if start < self.text.len() {
            self.highlights
                .push((start..self.text.len(), style.highlight()));
        }
    }
}

enum FlowChunk {
    Text {
        content: InlineContent,
        style: HtmlStyle,
        font_size: Pixels,
        line_height: Pixels,
    },
    Image {
        image: Arc<RenderImage>,
        style: HtmlStyle,
        line_height: Pixels,
    },
    Block(AnyElement),
}

struct HtmlRenderer<'a> {
    font_size: Pixels,
    line_height: Pixels,
    images: &'a HashMap<PathBuf, Arc<RenderImage>>,
    max_image_width: Option<Pixels>,
    max_preview_lines: usize,
    max_nodes: usize,
    visited_nodes: usize,
    has_visible_text: bool,
}

impl<'a> HtmlRenderer<'a> {
    fn enter(&mut self, depth: usize) -> bool {
        if depth > MAX_RENDER_DEPTH || self.visited_nodes >= self.max_nodes {
            return false;
        }
        self.visited_nodes += 1;
        true
    }

    fn render_children(
        &mut self,
        node: &Handle,
        inherited_style: HtmlStyle,
        depth: usize,
    ) -> Vec<AnyElement> {
        self.render_flow_children(
            node,
            inherited_style,
            depth,
            self.font_size,
            self.line_height,
        )
    }

    fn render_flow_children(
        &mut self,
        node: &Handle,
        inherited_style: HtmlStyle,
        depth: usize,
        font_size: Pixels,
        line_height: Pixels,
    ) -> Vec<AnyElement> {
        if !self.enter(depth) {
            return Vec::new();
        }

        let mut chunks = Vec::new();
        let mut inline = InlineContent::default();
        for child in node.children.borrow().iter() {
            self.append_flow(
                child,
                inherited_style,
                &mut inline,
                &mut chunks,
                depth + 1,
                font_size,
                line_height,
            );
        }
        self.flush_inline(
            &mut inline,
            inherited_style,
            &mut chunks,
            font_size,
            line_height,
        );
        chunks
            .into_iter()
            .map(|chunk| self.render_flow_chunk(chunk))
            .collect()
    }

    fn flush_inline(
        &mut self,
        inline: &mut InlineContent,
        style: HtmlStyle,
        chunks: &mut Vec<FlowChunk>,
        font_size: Pixels,
        line_height: Pixels,
    ) {
        inline.trim_boundary_line_breaks();
        if !inline.has_visible_text() {
            inline.text.clear();
            inline.highlights.clear();
            return;
        }
        let content = mem::take(inline);
        chunks.push(FlowChunk::Text {
            content,
            style,
            font_size,
            line_height,
        });
    }

    fn render_block(
        &mut self,
        node: &Handle,
        inherited_style: HtmlStyle,
        depth: usize,
    ) -> AnyElement {
        if !self.enter(depth) {
            return div().into_any_element();
        }

        let Some(tag) = element_tag(node) else {
            return div().into_any_element();
        };
        let mut style = HtmlStyle::for_element(inherited_style, node);

        match tag.as_str() {
            "p" => {
                let children = self.render_flow_children(
                    node,
                    style,
                    depth + 1,
                    self.font_size,
                    self.line_height,
                );
                apply_block_style(div().w_full().flex_col().children(children), style)
                    .into_any_element()
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = tag[1..].parse::<u8>().unwrap_or(6);
                style.font_weight = Some(match level {
                    1 => FontWeight::BOLD,
                    2 | 3 => FontWeight::SEMIBOLD,
                    _ => FontWeight::MEDIUM,
                });
                let scale = match level {
                    1 => 1.45,
                    2 => 1.3,
                    3 => 1.15,
                    _ => 1.0,
                };
                let children = self.render_flow_children(
                    node,
                    style,
                    depth + 1,
                    self.font_size * scale,
                    self.line_height * scale,
                );
                apply_block_style(div().w_full().flex_col().children(children), style)
                    .into_any_element()
            }
            "pre" => {
                if style.background_color.is_none() {
                    style.background_color = Some(code_background());
                }
                let children = self.render_flow_children(
                    node,
                    style,
                    depth + 1,
                    self.font_size,
                    self.line_height,
                );
                apply_block_style(div().w_full().flex_col().children(children), style)
                    .into_any_element()
            }
            "ul" | "ol" => self.render_list(node, style, depth),
            "table" => self.render_table(node, style, depth),
            "blockquote" => {
                let children = self.render_children(node, style, depth + 1);
                let element = div()
                    .w_full()
                    .flex_col()
                    .border_l_2()
                    .border_color(table_border())
                    .pl_2()
                    .children(children);
                apply_block_style(element, style).into_any_element()
            }
            "hr" => div()
                .w_full()
                .h(px(1.0))
                .bg(table_border())
                .into_any_element(),
            _ => {
                let children = self.render_children(node, style, depth + 1);
                let element = div().w_full().flex_col().children(children);
                apply_block_style(element, style).into_any_element()
            }
        }
    }

    fn append_flow(
        &mut self,
        node: &Handle,
        inherited_style: HtmlStyle,
        content: &mut InlineContent,
        chunks: &mut Vec<FlowChunk>,
        depth: usize,
        font_size: Pixels,
        line_height: Pixels,
    ) {
        if !self.enter(depth) {
            return;
        }

        match &node.data {
            NodeData::Text { contents } => {
                let text = contents.borrow();
                self.has_visible_text |= text.chars().any(|character| !character.is_whitespace());
                content.push_text(text.as_ref(), inherited_style);
            }
            NodeData::Element { .. } => {
                let Some(tag) = element_tag(node) else {
                    return;
                };
                if is_ignored_tag(&tag) {
                    return;
                }
                let style = HtmlStyle::for_element(inherited_style, node);
                match tag.as_str() {
                    "br" => content.push_line_break(style),
                    "img" => {
                        self.flush_inline(content, inherited_style, chunks, font_size, line_height);
                        if let Some(image) = self.image_chunk(node, style, line_height) {
                            chunks.push(image);
                        }
                    }
                    _ if is_block_tag(&tag) => {
                        self.flush_inline(content, inherited_style, chunks, font_size, line_height);
                        chunks.push(FlowChunk::Block(self.render_block(
                            node,
                            inherited_style,
                            depth,
                        )));
                    }
                    _ => {
                        for child in node.children.borrow().iter() {
                            self.append_flow(
                                child,
                                style,
                                content,
                                chunks,
                                depth + 1,
                                font_size,
                                line_height,
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn image_chunk(
        &mut self,
        node: &Handle,
        style: HtmlStyle,
        line_height: Pixels,
    ) -> Option<FlowChunk> {
        let path =
            attr_value_from_node(node, "src").and_then(|source| local_image_path(&source))?;
        let image = self.images.get(&path)?.clone();
        self.has_visible_text = true;
        Some(FlowChunk::Image {
            image,
            style,
            line_height,
        })
    }

    fn render_flow_chunk(&self, chunk: FlowChunk) -> AnyElement {
        match chunk {
            FlowChunk::Text {
                content,
                style,
                font_size,
                line_height,
            } => self.render_text_block(content, style, font_size, line_height),
            FlowChunk::Image {
                image,
                style,
                line_height,
            } => {
                let max_height = line_height * self.max_preview_lines as f32;
                let image_element = if let Some(max_width) = self.max_image_width {
                    let image_size = image.size(0);
                    let image_width = image_size.width.0 as f32;
                    let image_height = image_size.height.0 as f32;
                    let scale = (f32::from(max_width) / image_width)
                        .min(f32::from(max_height) / image_height)
                        .min(1.0);
                    img(image)
                        .w(px(image_width * scale))
                        .h(px(image_height * scale))
                        .object_fit(ObjectFit::Contain)
                } else {
                    img(image)
                        .max_w_full()
                        .max_h(max_height)
                        .object_fit(ObjectFit::Contain)
                };
                let element = div().w_full().flex().items_start().child(image_element);
                apply_block_style(element, style).into_any_element()
            }
            FlowChunk::Block(element) => element,
        }
    }

    fn render_text_block(
        &self,
        content: InlineContent,
        style: HtmlStyle,
        font_size: Pixels,
        line_height: Pixels,
    ) -> AnyElement {
        if !content.has_visible_text() {
            return div().into_any_element();
        }

        let mut text = StyledText::new(content.text);
        if !content.highlights.is_empty() {
            text = text.with_highlights(content.highlights);
        }
        let element = div()
            .w_full()
            .pb(px(2.0))
            .text_size(font_size)
            .line_height(line_height)
            .child(text);
        apply_block_style(element, style).into_any_element()
    }

    fn render_list(&mut self, node: &Handle, style: HtmlStyle, depth: usize) -> AnyElement {
        let ordered = element_tag(node).as_deref() == Some("ol");
        let mut items = Vec::new();
        let mut item_index = 0;
        for child in node.children.borrow().iter() {
            if self.visited_nodes >= self.max_nodes {
                break;
            }
            if element_tag(child).as_deref() != Some("li") {
                continue;
            }
            let item_style = HtmlStyle::for_element(style, child);
            let item_children = self.render_children(child, item_style, depth + 1);
            item_index += 1;
            let marker = if ordered {
                format!("{item_index}.")
            } else {
                "•".to_owned()
            };
            let row = div()
                .w_full()
                .flex()
                .items_start()
                .child(div().w(px(20.0)).flex_none().child(marker))
                .child(div().flex_1().min_w_0().flex_col().children(item_children));
            items.push(row.into_any_element());
        }

        let element = div().w_full().flex().flex_col().children(items);
        apply_block_style(element, style).into_any_element()
    }

    fn render_table(&mut self, node: &Handle, style: HtmlStyle, depth: usize) -> AnyElement {
        let mut rows = Vec::new();
        collect_table_rows(node, &mut rows, 0);
        let mut table = div()
            .w_full()
            .flex()
            .flex_col()
            .border_1()
            .border_color(table_border());

        for (row_index, row) in rows.iter().enumerate() {
            let row_style = HtmlStyle::for_element(style, row);
            let cells = row
                .children
                .borrow()
                .iter()
                .filter(|child| matches!(element_tag(child).as_deref(), Some("td") | Some("th")))
                .cloned()
                .collect::<Vec<_>>();
            if cells.is_empty() {
                continue;
            }

            let row_element = self
                .table_row_cells(&cells, row_style, depth)
                .when(row_index + 1 < rows.len(), |this| this.border_b_1())
                .border_color(table_border());
            table = table.child(apply_block_style(row_element, row_style));
        }

        apply_block_style(table, style).into_any_element()
    }

    fn render_table_row(&mut self, row: &Handle, row_style: HtmlStyle, depth: usize) -> Div {
        let cells = row
            .children
            .borrow()
            .iter()
            .filter(|child| matches!(element_tag(child).as_deref(), Some("td") | Some("th")))
            .cloned()
            .collect::<Vec<_>>();
        apply_block_style(self.table_row_cells(&cells, row_style, depth), row_style)
    }

    fn table_row_cells(&mut self, cells: &[Handle], row_style: HtmlStyle, depth: usize) -> Div {
        let mut row_element = div().w_full().flex();
        for (cell_index, cell) in cells.iter().enumerate() {
            let cell_style = HtmlStyle::for_element(row_style, cell);
            let cell_children = self.render_children(cell, cell_style, depth + 1);
            let mut cell_element = div()
                .flex_1()
                .min_w_0()
                .flex_col()
                .px_2()
                .py_1()
                .when(cell_index + 1 < cells.len(), |this| {
                    this.border_r_1().border_color(table_border())
                })
                .children(cell_children);
            if element_tag(cell).as_deref() == Some("th") {
                cell_element = cell_element.font_weight(FontWeight::BOLD);
            }
            row_element = row_element.child(apply_block_style(cell_element, cell_style));
        }
        row_element
    }
}

/// 文档拆分时最多遍历的节点数，超出后剩余内容不再展示。
const MAX_DOCUMENT_NODES: usize = 200_000;
/// 文档最多拆分出的条目数。
const MAX_DOCUMENT_ITEMS: usize = 50_000;

/// 拆分为独立条目的 HTML 文档，供虚拟列表只渲染可见部分。
///
/// 顶层段落、列表项和表格行分别作为条目；无背景色的普通容器会被展开，
/// 避免整个文档包在一个 `div` 里时只能整体渲染。
pub struct HtmlDocument {
    // 条目持有 DOM 节点引用，文档本身保留解析结果保证节点有效。
    _parsed: Rc<ParsedHtml>,
    items: Vec<HtmlItem>,
    font_size: Pixels,
    line_height: Pixels,
}

enum HtmlItem {
    Text {
        content: InlineContent,
        style: HtmlStyle,
        font_size: Pixels,
        line_height: Pixels,
    },
    Image {
        path: PathBuf,
        style: HtmlStyle,
        line_height: Pixels,
    },
    Block {
        node: Handle,
        inherited_style: HtmlStyle,
        depth: usize,
    },
    ListItem {
        node: Handle,
        list_style: HtmlStyle,
        marker: String,
        depth: usize,
    },
    TableRow {
        node: Handle,
        table_style: HtmlStyle,
        first: bool,
        depth: usize,
    },
}

impl HtmlDocument {
    pub fn parse(
        source: &str,
        font_size: Pixels,
        line_height: Pixels,
        fallback_text: &str,
    ) -> Self {
        let parsed = parsed_html(crate::domain::html::fragment(source));
        let mut collector = DocumentCollector {
            items: Vec::new(),
            visited_nodes: 0,
            has_visible_text: false,
            font_size,
            line_height,
        };
        let mut inline = InlineContent::default();
        collector.collect_children(&parsed.dom.document, HtmlStyle::default(), &mut inline, 0);
        collector.flush_inline(&mut inline, HtmlStyle::default());
        if !collector.has_visible_text {
            collector.items.clear();
            let mut content = InlineContent::default();
            content.push_text(
                fallback_text,
                HtmlStyle {
                    preserve_whitespace: true,
                    ..Default::default()
                },
            );
            if content.has_visible_text() {
                collector.items.push(HtmlItem::Text {
                    content,
                    style: HtmlStyle::default(),
                    font_size,
                    line_height,
                });
            }
        }
        Self {
            _parsed: parsed,
            items: collector.items,
            font_size,
            line_height,
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn render_item(
        &self,
        index: usize,
        images: &HashMap<PathBuf, Arc<RenderImage>>,
    ) -> AnyElement {
        let Some(item) = self.items.get(index) else {
            return div().into_any_element();
        };
        let mut renderer = HtmlRenderer {
            font_size: self.font_size,
            line_height: self.line_height,
            images,
            max_image_width: None,
            max_preview_lines: 40,
            max_nodes: MAX_RENDER_NODES,
            visited_nodes: 0,
            has_visible_text: false,
        };
        match item {
            HtmlItem::Text {
                content,
                style,
                font_size,
                line_height,
            } => renderer.render_text_block(content.clone(), *style, *font_size, *line_height),
            HtmlItem::Image {
                path,
                style,
                line_height,
            } => match images.get(path) {
                Some(image) => renderer.render_flow_chunk(FlowChunk::Image {
                    image: image.clone(),
                    style: *style,
                    line_height: *line_height,
                }),
                None => div().into_any_element(),
            },
            HtmlItem::Block {
                node,
                inherited_style,
                depth,
            } => renderer.render_block(node, *inherited_style, *depth),
            HtmlItem::ListItem {
                node,
                list_style,
                marker,
                depth,
            } => {
                let item_style = HtmlStyle::for_element(*list_style, node);
                let children = renderer.render_children(node, item_style, depth + 1);
                let row = div()
                    .w_full()
                    .flex()
                    .items_start()
                    .child(div().w(px(20.0)).flex_none().child(marker.clone()))
                    .child(div().flex_1().min_w_0().flex_col().children(children));
                apply_block_style(row, *list_style).into_any_element()
            }
            HtmlItem::TableRow {
                node,
                table_style,
                first,
                depth,
            } => {
                let row_style = HtmlStyle::for_element(*table_style, node);
                let row = renderer.render_table_row(node, row_style, *depth);
                let row = div()
                    .w_full()
                    .border_l_1()
                    .border_r_1()
                    .border_b_1()
                    .when(*first, |this| this.border_t_1())
                    .border_color(table_border())
                    .child(row);
                apply_block_style(row, *table_style).into_any_element()
            }
        }
    }
}

struct DocumentCollector {
    items: Vec<HtmlItem>,
    visited_nodes: usize,
    has_visible_text: bool,
    font_size: Pixels,
    line_height: Pixels,
}

impl DocumentCollector {
    fn enter(&mut self, depth: usize) -> bool {
        if depth > MAX_RENDER_DEPTH
            || self.visited_nodes >= MAX_DOCUMENT_NODES
            || self.items.len() >= MAX_DOCUMENT_ITEMS
        {
            return false;
        }
        self.visited_nodes += 1;
        true
    }

    fn flush_inline(&mut self, inline: &mut InlineContent, style: HtmlStyle) {
        inline.trim_boundary_line_breaks();
        if !inline.has_visible_text() {
            inline.text.clear();
            inline.highlights.clear();
            return;
        }
        self.items.push(HtmlItem::Text {
            content: mem::take(inline),
            style,
            font_size: self.font_size,
            line_height: self.line_height,
        });
    }

    fn collect_children(
        &mut self,
        node: &Handle,
        style: HtmlStyle,
        inline: &mut InlineContent,
        depth: usize,
    ) {
        for child in node.children.borrow().iter() {
            self.collect(child, style, inline, depth + 1);
        }
    }

    fn collect(
        &mut self,
        node: &Handle,
        inherited_style: HtmlStyle,
        inline: &mut InlineContent,
        depth: usize,
    ) {
        if !self.enter(depth) {
            return;
        }
        match &node.data {
            NodeData::Text { contents } => {
                let text = contents.borrow();
                self.has_visible_text |= text.chars().any(|character| !character.is_whitespace());
                inline.push_text(text.as_ref(), inherited_style);
            }
            NodeData::Element { .. } => {
                let Some(tag) = element_tag(node) else {
                    return;
                };
                if is_ignored_tag(&tag) {
                    return;
                }
                let style = HtmlStyle::for_element(inherited_style, node);
                match tag.as_str() {
                    "br" => inline.push_line_break(style),
                    "img" => {
                        self.flush_inline(inline, inherited_style);
                        if let Some(path) = attr_value_from_node(node, "src")
                            .and_then(|source| local_image_path(&source))
                        {
                            self.has_visible_text = true;
                            self.items.push(HtmlItem::Image {
                                path,
                                style,
                                line_height: self.line_height,
                            });
                        }
                    }
                    "ul" | "ol" => {
                        self.flush_inline(inline, inherited_style);
                        let ordered = tag == "ol";
                        let mut item_index = 0;
                        for child in node.children.borrow().iter() {
                            if element_tag(child).as_deref() != Some("li") || !self.enter(depth + 1)
                            {
                                continue;
                            }
                            item_index += 1;
                            self.has_visible_text = true;
                            self.items.push(HtmlItem::ListItem {
                                node: child.clone(),
                                list_style: style,
                                marker: if ordered {
                                    format!("{item_index}.")
                                } else {
                                    "•".to_owned()
                                },
                                depth: depth + 1,
                            });
                        }
                    }
                    "table" => {
                        self.flush_inline(inline, inherited_style);
                        let mut rows = Vec::new();
                        collect_table_rows(node, &mut rows, 0);
                        for (index, row) in rows.into_iter().enumerate() {
                            if !self.enter(depth + 1) {
                                break;
                            }
                            self.has_visible_text = true;
                            self.items.push(HtmlItem::TableRow {
                                node: row,
                                table_style: style,
                                first: index == 0,
                                depth: depth + 1,
                            });
                        }
                    }
                    _ if is_transparent_container(&tag) && style.background_color.is_none() => {
                        // 无背景的普通容器只影响继承样式，展开后子块可以单独虚拟化。
                        self.flush_inline(inline, inherited_style);
                        let mut inner = InlineContent::default();
                        self.collect_children(node, style, &mut inner, depth);
                        self.flush_inline(&mut inner, style);
                    }
                    _ if is_block_tag(&tag) => {
                        self.flush_inline(inline, inherited_style);
                        self.has_visible_text = true;
                        self.items.push(HtmlItem::Block {
                            node: node.clone(),
                            inherited_style,
                            depth,
                        });
                    }
                    _ => self.collect_children(node, style, inline, depth),
                }
            }
            _ => self.collect_children(node, inherited_style, inline, depth),
        }
    }
}

fn is_transparent_container(tag: &str) -> bool {
    matches!(
        tag,
        "div"
            | "section"
            | "article"
            | "main"
            | "header"
            | "footer"
            | "nav"
            | "aside"
            | "form"
            | "fieldset"
            | "figure"
            | "address"
            | "body"
            | "html"
    )
}

fn apply_block_style(mut element: Div, style: HtmlStyle) -> Div {
    if let Some(color) = style.color {
        element = element.text_color(color);
    }
    if let Some(background_color) = style.background_color {
        element = element.bg(background_color);
    }
    if let Some(font_weight) = style.font_weight {
        element = element.font_weight(font_weight);
    }
    match style.font_style {
        Some(FontStyle::Normal) => element = element.not_italic(),
        Some(FontStyle::Italic | FontStyle::Oblique) => element = element.italic(),
        None => {}
    }
    if style.underline {
        element = element.underline();
    }
    if style.strikethrough {
        element = element.line_through();
    }
    element
}

fn collect_table_rows(node: &Handle, rows: &mut Vec<Handle>, depth: usize) {
    if depth > MAX_RENDER_DEPTH {
        return;
    }
    for child in node.children.borrow().iter() {
        match element_tag(child).as_deref() {
            Some("tr") => rows.push(child.clone()),
            Some("thead") | Some("tbody") | Some("tfoot") => {
                collect_table_rows(child, rows, depth + 1)
            }
            _ => {}
        }
    }
}

fn element_tag(node: &Handle) -> Option<String> {
    match &node.data {
        NodeData::Element { name, .. } => Some(name.local.to_string().to_ascii_lowercase()),
        _ => None,
    }
}

fn attr_value(attrs: &RefCell<Vec<html5ever::Attribute>>, name: &str) -> Option<String> {
    attrs
        .borrow()
        .iter()
        .find(|attribute| attribute.name.local.as_ref() == name)
        .map(|attribute| attribute.value.to_string())
}

fn is_block_tag(tag: &str) -> bool {
    matches!(
        tag,
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "div"
            | "dl"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hr"
            | "li"
            | "main"
            | "nav"
            | "ol"
            | "p"
            | "pre"
            | "section"
            | "table"
            | "tbody"
            | "td"
            | "tfoot"
            | "th"
            | "thead"
            | "tr"
            | "ul"
    )
}

fn is_ignored_tag(tag: &str) -> bool {
    matches!(
        tag,
        "applet"
            | "audio"
            | "base"
            | "button"
            | "canvas"
            | "embed"
            | "frame"
            | "frameset"
            | "head"
            | "iframe"
            | "input"
            | "link"
            | "math"
            | "meta"
            | "noscript"
            | "object"
            | "option"
            | "script"
            | "select"
            | "source"
            | "style"
            | "svg"
            | "template"
            | "textarea"
            | "title"
            | "track"
            | "video"
    )
}

fn apply_css_declarations(style: &mut HtmlStyle, source: &str) {
    for declaration in source.split(';') {
        let Some((property, value)) = declaration.split_once(':') else {
            continue;
        };
        let property = property.trim().to_ascii_lowercase();
        let value = value.trim().trim_end_matches("!important").trim();
        match property.as_str() {
            "color" => {
                if let Some(color) = parse_color(value) {
                    style.color = Some(color);
                }
            }
            "background-color" => {
                if let Some(color) = parse_color(value) {
                    style.background_color = Some(color);
                }
            }
            "background" if !value.contains(' ') => {
                if let Some(color) = parse_color(value) {
                    style.background_color = Some(color);
                }
            }
            "font-weight" => {
                style.font_weight = parse_font_weight(value).or(style.font_weight);
            }
            "font-style" => {
                style.font_style = match value.to_ascii_lowercase().as_str() {
                    "normal" => Some(FontStyle::Normal),
                    "italic" => Some(FontStyle::Italic),
                    "oblique" => Some(FontStyle::Oblique),
                    _ => style.font_style,
                };
            }
            "text-decoration" | "text-decoration-line" => {
                let value = value.to_ascii_lowercase();
                if value.contains("none") {
                    style.underline = false;
                    style.strikethrough = false;
                } else {
                    if value.contains("underline") {
                        style.underline = true;
                    }
                    if value.contains("line-through") {
                        style.strikethrough = true;
                    }
                }
            }
            "white-space" => {
                style.preserve_whitespace = matches!(value, "pre" | "pre-wrap");
            }
            _ => {}
        }
    }
}

fn parse_font_weight(value: &str) -> Option<FontWeight> {
    match value.to_ascii_lowercase().as_str() {
        "bold" | "bolder" => Some(FontWeight::BOLD),
        "normal" | "lighter" => Some(FontWeight::NORMAL),
        value => value
            .parse::<f32>()
            .ok()
            .map(|weight| FontWeight::from(if weight >= 600.0 { 700.0 } else { 400.0 })),
    }
}

fn parse_color(value: &str) -> Option<Hsla> {
    let color = csscolorparser::Color::from_html(value).ok()?;
    let [r, g, b, a] = color.to_array();
    Some(gpui::rgb_to_hsla(Rgba::new(r, g, b, a)))
}

fn code_background() -> Hsla {
    gpui::rgb_to_hsla(rgba(0x00000018))
}

fn mark_background() -> Hsla {
    gpui::rgb_to_hsla(rgba(0xFDE68AFF))
}

fn link_color() -> Hsla {
    gpui::rgb_to_hsla(rgba(0x2563EBFF))
}

fn table_border() -> Hsla {
    gpui::rgb_to_hsla(rgba(0x64748B99))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_segments_keep_text_and_loaded_image_order() {
        let path = PathBuf::from(r"C:\images\a.png");
        let image = Arc::new(RenderImage::new(vec![image::Frame::new(
            image::RgbaImage::new(40, 20),
        )]));
        let images = HashMap::from([(path, image)]);
        let segments = preview_segments(
            r#"<p>标题</p><p>第二段</p><img src="C:\images\a.png"><div>说明 <b>文字</b></div><img src="C:\images\missing.png">"#,
            &images,
        );

        assert_eq!(segments.len(), 3);
        assert!(matches!(&segments[0], PreviewSegment::Text(text) if text == "标题\n第二段"));
        assert!(matches!(
            &segments[1],
            PreviewSegment::Image(size) if size.width == px(40.0) && size.height == px(20.0)
        ));
        assert!(matches!(&segments[2], PreviewSegment::Text(text) if text == "说明 文字"));
    }

    #[test]
    fn document_splits_wrappers_lists_and_tables_into_items() {
        let html = "<html><body><!--StartFragment--><div><p>first</p><p>second</p>\
            <ul><li>a</li><li>b</li><li>c</li></ul>\
            <table><tr><td>1</td></tr><tr><td>2</td></tr></table>tail</div></body></html>";
        let document = HtmlDocument::parse(html, px(14.0), px(22.0), "");
        // 两个段落、三个列表项、两行表格和末尾文字。
        assert_eq!(document.len(), 8);
    }

    #[test]
    fn document_keeps_styled_containers_whole_and_falls_back_to_text() {
        let styled = HtmlDocument::parse(
            "<div style=\"background-color:#ff0\"><p>a</p><p>b</p></div>",
            px(14.0),
            px(22.0),
            "",
        );
        assert_eq!(styled.len(), 1);
        let empty = HtmlDocument::parse("<div></div>", px(14.0), px(22.0), "plain text");
        assert_eq!(empty.len(), 1);
        assert!(HtmlDocument::parse("<div></div>", px(14.0), px(22.0), "").is_empty());
    }

    #[test]
    fn css_declarations_parse_basic_html_styles() {
        let mut style = HtmlStyle::default();
        apply_css_declarations(
            &mut style,
            "color: rgb(255, 0, 0); background-color: #00ff00; font-weight: 700; font-style: italic; text-decoration: underline line-through; white-space: pre-wrap",
        );

        assert!(style.color.is_some());
        assert!(style.background_color.is_some());
        assert_eq!(style.font_weight, Some(FontWeight::BOLD));
        assert_eq!(style.font_style, Some(FontStyle::Italic));
        assert!(style.underline);
        assert!(style.strikethrough);
        assert!(style.preserve_whitespace);
    }

    #[test]
    fn dangerous_tags_are_removed_as_subtrees() {
        assert!(is_ignored_tag("script"));
        assert!(is_ignored_tag("style"));
        assert!(is_ignored_tag("iframe"));
        assert!(is_ignored_tag("svg"));
        assert!(!is_ignored_tag("a"));
        assert!(!is_ignored_tag("table"));
    }

    #[test]
    fn normal_text_whitespace_is_collapsed() {
        let mut content = InlineContent::default();
        content.push_text("  hello\n   world  ", HtmlStyle::default());
        assert_eq!(content.text, " hello world ");
    }

    #[test]
    fn boundary_line_breaks_do_not_create_empty_preview_lines() {
        let mut content = InlineContent::default();
        content.push_line_break(HtmlStyle::default());
        content.push_text("前", HtmlStyle::default());
        content.push_line_break(HtmlStyle::default());
        content.trim_boundary_line_breaks();
        assert_eq!(content.text, "前");

        let mut content = InlineContent::default();
        content.push_text("前", HtmlStyle::default());
        content.push_line_break(HtmlStyle::default());
        content.push_text("后", HtmlStyle::default());
        content.trim_boundary_line_breaks();
        assert_eq!(content.text, "前\n后");

        let mut content = InlineContent::default();
        content.push_line_break(HtmlStyle::default());
        content.trim_boundary_line_breaks();
        assert!(content.text.is_empty());
    }

    #[test]
    fn local_image_paths_accept_file_urls_and_windows_paths() {
        let paths = local_image_paths(
            r#"<p><img src="file:///C:/Users/Test%20User/image.png"><img src="D:\tmp\other.jpg"></p>"#,
        );
        assert_eq!(paths.len(), 2);
        assert_eq!(
            paths[0].to_string_lossy().replace('\\', "/"),
            "C:/Users/Test User/image.png"
        );
        assert_eq!(paths[1], PathBuf::from(r"D:\tmp\other.jpg"));
    }

    #[test]
    fn local_image_paths_reject_non_local_sources_and_dangerous_subtrees() {
        let paths = local_image_paths(
            r#"<script><img src="C:\secret.png"></script><img src="https://example.com/a.png"><img src="data:image/png;base64,abc"><img src="C:\safe.png">"#,
        );
        assert_eq!(paths, vec![PathBuf::from(r"C:\safe.png")]);
        assert!(local_image_path("relative.png").is_none());
    }
}
