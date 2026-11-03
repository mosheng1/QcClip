//! 便签“拆词”使用的文本切分。
//!
//! 先识别链接、邮箱、路径、日期等整体信息，剩余文本交给 ICU 词典分词，
//! 中文、日文按词切分，其他语言按 UAX #29 词边界切分。

use std::{ops::Range, sync::OnceLock};

use icu_segmenter::{
    WordSegmenter,
    options::{WordBreakInvariantOptions, WordType},
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;

/// 单次拆分最多生成的词块数量；界面按行虚拟化渲染，上限只用于限制内存和分词耗时。
pub(crate) const MAX_TOKENS: usize = 50_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TokenKind {
    Word,
    Number,
    Link,
    Email,
    Path,
    Symbol,
}

impl TokenKind {
    /// 链接、邮箱等整体识别出的信息，界面中使用强调样式。
    pub(crate) fn is_entity(self) -> bool {
        matches!(self, Self::Link | Self::Email | Self::Path)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Token {
    pub(crate) text: String,
    pub(crate) kind: TokenKind,
    /// 词块所在的原文行号，界面按行分组展示。
    pub(crate) line: usize,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum SplitGranularity {
    #[default]
    Word,
    Character,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum JoinSeparator {
    /// 中日文之间直接拼接，其余词块之间补空格。
    #[default]
    Smart,
    None,
    Space,
    Newline,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SplitResult {
    pub(crate) tokens: Vec<Token>,
    pub(crate) truncated: bool,
}

pub(crate) fn split(
    text: &str,
    granularity: SplitGranularity,
    include_symbols: bool,
) -> SplitResult {
    let mut result = SplitResult::default();
    for (line, content) in text.lines().enumerate() {
        let done = match granularity {
            SplitGranularity::Word => split_line_words(content, line, include_symbols, &mut result),
            SplitGranularity::Character => {
                split_line_characters(content, line, include_symbols, &mut result)
            }
        };
        if !done {
            result.truncated = true;
            break;
        }
    }
    result
}

/// 按分隔方式拼接选中的词块。
pub(crate) fn join_tokens<'a>(
    tokens: impl IntoIterator<Item = &'a Token>,
    separator: JoinSeparator,
) -> String {
    let mut output = String::new();
    let mut previous: Option<&Token> = None;
    for token in tokens {
        if let Some(previous) = previous {
            match separator {
                JoinSeparator::None => {}
                JoinSeparator::Space => output.push(' '),
                JoinSeparator::Newline => output.push('\n'),
                JoinSeparator::Smart => {
                    if previous.line != token.line {
                        output.push('\n');
                    } else if needs_space_between(&previous.text, &token.text) {
                        output.push(' ');
                    }
                }
            }
        }
        output.push_str(&token.text);
        previous = Some(token);
    }
    output
}

fn needs_space_between(left: &str, right: &str) -> bool {
    let (Some(left), Some(right)) = (left.chars().last(), right.chars().next()) else {
        return false;
    };
    !(is_cjk(left)
        || is_cjk(right)
        || is_closing_punctuation(right)
        || is_opening_punctuation(left))
}

fn is_cjk(character: char) -> bool {
    matches!(
        character as u32,
        0x3000..=0x303F
            | 0x3040..=0x30FF
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xF900..=0xFAFF
            | 0xFF00..=0xFFEF
            | 0xAC00..=0xD7AF
            | 0x20000..=0x2FA1F
    )
}

fn is_closing_punctuation(character: char) -> bool {
    matches!(
        character,
        ',' | '.' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '%'
    )
}

fn is_opening_punctuation(character: char) -> bool {
    matches!(character, '(' | '[' | '{' | '$' | '#' | '@')
}

fn entity_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(concat!(
            r"(?P<link>(?i:https?://|ftp://|www\.)[^\s<>'\x22，。；！？、（）【】《》「」]+)",
            r"|(?P<email>[A-Za-z0-9._%+\-]+@[A-Za-z0-9\-]+(?:\.[A-Za-z0-9\-]+)*\.[A-Za-z]{2,})",
            r"|(?P<path>(?:[A-Za-z]:\\|\\\\)[^\s<>|*?\x22，。；！？]+)",
            r"|(?P<number>\d+(?:[\-/.:]\d+)+)",
        ))
        .expect("拆词实体正则无效")
    })
}

fn word_segmenter() -> &'static icu_segmenter::WordSegmenterBorrowed<'static> {
    static SEGMENTER: OnceLock<icu_segmenter::WordSegmenterBorrowed<'static>> = OnceLock::new();
    SEGMENTER.get_or_init(|| WordSegmenter::new_dictionary(WordBreakInvariantOptions::default()))
}

/// 返回 false 表示已达到词块上限。
fn push_token(result: &mut SplitResult, text: &str, kind: TokenKind, line: usize) -> bool {
    if result.tokens.len() >= MAX_TOKENS {
        return false;
    }
    result.tokens.push(Token {
        text: text.to_owned(),
        kind,
        line,
    });
    true
}

fn split_line_words(
    content: &str,
    line: usize,
    include_symbols: bool,
    result: &mut SplitResult,
) -> bool {
    let mut cursor = 0;
    for captures in entity_pattern().captures_iter(content) {
        let Some(matched) = captures.get(0) else {
            continue;
        };
        let (range, kind) = if captures.name("link").is_some() {
            (trim_link_end(content, matched.range()), TokenKind::Link)
        } else if captures.name("email").is_some() {
            (matched.range(), TokenKind::Email)
        } else if captures.name("path").is_some() {
            (matched.range(), TokenKind::Path)
        } else {
            (matched.range(), TokenKind::Number)
        };
        if range.start < cursor {
            continue;
        }
        if !split_plain_words(&content[cursor..range.start], line, include_symbols, result) {
            return false;
        }
        if !push_token(result, &content[range.clone()], kind, line) {
            return false;
        }
        cursor = range.end;
    }
    split_plain_words(&content[cursor..], line, include_symbols, result)
}

/// 去掉链接末尾常见的句读，例如“访问 https://a.com。”中的句号。
fn trim_link_end(content: &str, range: Range<usize>) -> Range<usize> {
    let trimmed = content[range.clone()].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']']);
    range.start..range.start + trimmed.len()
}

fn split_plain_words(
    content: &str,
    line: usize,
    include_symbols: bool,
    result: &mut SplitResult,
) -> bool {
    if content.trim().is_empty() {
        return true;
    }
    let mut start = 0;
    for (end, word_type) in word_segmenter().segment_str(content).iter_with_word_type() {
        if end == 0 {
            continue;
        }
        let segment = &content[start..end];
        start = end;
        if segment.trim().is_empty() {
            continue;
        }
        let kind = match word_type {
            WordType::Number => TokenKind::Number,
            WordType::Letter => TokenKind::Word,
            _ if segment.chars().any(char::is_alphanumeric) => TokenKind::Word,
            _ => TokenKind::Symbol,
        };
        if kind == TokenKind::Symbol && !include_symbols {
            continue;
        }
        if !push_token(result, segment.trim(), kind, line) {
            return false;
        }
    }
    true
}

fn split_line_characters(
    content: &str,
    line: usize,
    include_symbols: bool,
    result: &mut SplitResult,
) -> bool {
    for grapheme in content.graphemes(true) {
        if grapheme.trim().is_empty() {
            continue;
        }
        let kind = if grapheme.chars().all(|character| character.is_ascii_digit()) {
            TokenKind::Number
        } else if grapheme.chars().any(char::is_alphanumeric) {
            TokenKind::Word
        } else {
            TokenKind::Symbol
        };
        if kind == TokenKind::Symbol && !include_symbols {
            continue;
        }
        if !push_token(result, grapheme, kind, line) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(result: &SplitResult) -> Vec<&str> {
        result
            .tokens
            .iter()
            .map(|token| token.text.as_str())
            .collect()
    }

    #[test]
    fn splits_chinese_into_words() {
        let result = split("我们今天去北京天安门", SplitGranularity::Word, false);
        let words = texts(&result);
        assert!(words.len() >= 3, "{words:?}");
        assert!(words.contains(&"北京"), "{words:?}");
        assert_eq!(words.concat(), "我们今天去北京天安门");
    }

    #[test]
    fn keeps_entities_whole() {
        let text = "联系 test.user@example.com，访问 https://example.com/a?b=1。日期 2026-10-27 地址 192.168.1.1";
        let result = split(text, SplitGranularity::Word, false);
        let words = texts(&result);
        assert!(words.contains(&"test.user@example.com"), "{words:?}");
        assert!(words.contains(&"https://example.com/a?b=1"), "{words:?}");
        assert!(words.contains(&"2026-10-27"), "{words:?}");
        assert!(words.contains(&"192.168.1.1"), "{words:?}");
        let link = result
            .tokens
            .iter()
            .find(|token| token.text.starts_with("https"))
            .unwrap();
        assert_eq!(link.kind, TokenKind::Link);
    }

    #[test]
    fn recognizes_windows_paths() {
        let result = split(
            r"打开 C:\Users\demo\file.txt 查看",
            SplitGranularity::Word,
            false,
        );
        let path = result
            .tokens
            .iter()
            .find(|token| token.kind == TokenKind::Path)
            .unwrap();
        assert_eq!(path.text, r"C:\Users\demo\file.txt");
    }

    #[test]
    fn symbols_are_optional() {
        let without = split("hello, world!", SplitGranularity::Word, false);
        assert_eq!(texts(&without), ["hello", "world"]);
        let with = split("hello, world!", SplitGranularity::Word, true);
        assert_eq!(texts(&with), ["hello", ",", "world", "!"]);
    }

    #[test]
    fn character_mode_skips_whitespace_and_tracks_lines() {
        let result = split("ab 中\n1", SplitGranularity::Character, false);
        assert_eq!(texts(&result), ["a", "b", "中", "1"]);
        assert_eq!(result.tokens[3].line, 1);
        assert_eq!(result.tokens[3].kind, TokenKind::Number);
    }

    #[test]
    fn smart_join_respects_scripts_and_lines() {
        let result = split("hello world\n你好 世界", SplitGranularity::Word, false);
        assert_eq!(
            join_tokens(&result.tokens, JoinSeparator::Smart),
            "hello world\n你好世界"
        );
        assert_eq!(
            join_tokens(&result.tokens, JoinSeparator::Space),
            "hello world 你好 世界"
        );
        assert_eq!(
            join_tokens(&result.tokens, JoinSeparator::None),
            "helloworld你好世界"
        );
    }

    #[test]
    fn stops_at_token_limit() {
        let text = "a ".repeat(MAX_TOKENS + 10);
        let result = split(&text, SplitGranularity::Word, false);
        assert_eq!(result.tokens.len(), MAX_TOKENS);
        assert!(result.truncated);
    }
}
