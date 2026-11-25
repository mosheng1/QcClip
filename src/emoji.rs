use std::{collections::HashMap, sync::OnceLock};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u32)]
pub(crate) enum EmojiCategoryId {
    Recent,
    SmileysEmotion,
    PeopleBody,
    AnimalsNature,
    FoodDrink,
    TravelPlaces,
    Activities,
    Objects,
    Symbols,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct EmojiCategoryDefinition {
    pub(crate) id: EmojiCategoryId,
    pub(crate) label_key: &'static str,
    pub(crate) icon: &'static str,
}

pub(crate) const EMOJI_CATEGORIES: &[EmojiCategoryDefinition] = &[
    EmojiCategoryDefinition {
        id: EmojiCategoryId::Recent,
        icon: "clock",
        label_key: "emoji.recent",
    },
    EmojiCategoryDefinition {
        id: EmojiCategoryId::SmileysEmotion,
        icon: "mood-smile",
        label_key: "emoji.smileys",
    },
    EmojiCategoryDefinition {
        id: EmojiCategoryId::PeopleBody,
        icon: "user",
        label_key: "emoji.people",
    },
    EmojiCategoryDefinition {
        id: EmojiCategoryId::AnimalsNature,
        icon: "paw",
        label_key: "emoji.animals",
    },
    EmojiCategoryDefinition {
        id: EmojiCategoryId::FoodDrink,
        icon: "apple",
        label_key: "emoji.food",
    },
    EmojiCategoryDefinition {
        id: EmojiCategoryId::TravelPlaces,
        icon: "plane",
        label_key: "emoji.travel",
    },
    EmojiCategoryDefinition {
        id: EmojiCategoryId::Activities,
        icon: "ball-football",
        label_key: "emoji.activities",
    },
    EmojiCategoryDefinition {
        id: EmojiCategoryId::Objects,
        icon: "bulb",
        label_key: "emoji.objects",
    },
    EmojiCategoryDefinition {
        id: EmojiCategoryId::Symbols,
        icon: "abc",
        label_key: "emoji.symbolsCat",
    },
];

#[derive(Clone, Debug)]
pub(crate) struct EmojiItem {
    pub(crate) glyph: String,
    pub(crate) name: String,
    pub(crate) search_text: String,
    pub(crate) category: EmojiCategoryId,
    pub(crate) skin_variants: Vec<String>,
}

pub(crate) struct EmojiData {
    pub(crate) items: Vec<EmojiItem>,
}

pub(crate) fn data() -> &'static EmojiData {
    static DATA: OnceLock<EmojiData> = OnceLock::new();
    DATA.get_or_init(|| parse_emoji_data(include_str!("../assets/data/emoji/emoji-test.txt")))
}

fn parse_emoji_data(source: &str) -> EmojiData {
    let annotations = parse_annotations(include_str!("../assets/data/emoji/annotations-zh.xml"));
    let mut group = None;
    let mut items = Vec::new();
    let mut item_indices = HashMap::new();
    let mut skin_variants: HashMap<String, Vec<String>> = HashMap::new();

    for line in source.lines() {
        let line = line.trim_end();
        if let Some(value) = line.strip_prefix("# group: ") {
            group = category_for_group(value);
            continue;
        }
        let Some((code_points, metadata)) = line.split_once(';') else {
            continue;
        };
        if !metadata.trim_start().starts_with("fully-qualified") {
            continue;
        }
        let Some(category) = group else {
            continue;
        };
        let Some(comment) = metadata.split_once('#').map(|(_, value)| value.trim()) else {
            continue;
        };
        let Some(glyph) = code_points_to_string(code_points) else {
            continue;
        };
        let base_glyph = remove_skin_modifiers(&glyph);
        if base_glyph != glyph {
            skin_variants
                .entry(annotation_key(&base_glyph))
                .or_default()
                .push(glyph);
            continue;
        }
        let mut comment_parts = comment.split_whitespace();
        let _ = comment_parts.next();
        let _ = comment_parts.next();
        let english_name = comment_parts.collect::<Vec<_>>().join(" ");
        let annotation = annotations.get(&annotation_key(&glyph));
        let name = annotation
            .and_then(|annotation| annotation.name.clone())
            .unwrap_or_else(|| english_name.clone());
        let keywords = annotation
            .map(|annotation| annotation.keywords.as_str())
            .unwrap_or("");
        let search_text = format!("{glyph} {name} {english_name} {keywords}").to_lowercase();

        item_indices.insert(annotation_key(&glyph), items.len());
        items.push(EmojiItem {
            glyph,
            name,
            search_text,
            category,
            skin_variants: Vec::new(),
        });
    }

    for (key, variants) in skin_variants {
        if let Some(index) = item_indices.get(&key) {
            items[*index].skin_variants = variants;
        }
    }

    EmojiData { items }
}

fn category_for_group(group: &str) -> Option<EmojiCategoryId> {
    Some(match group {
        "Smileys & Emotion" => EmojiCategoryId::SmileysEmotion,
        "People & Body" => EmojiCategoryId::PeopleBody,
        "Animals & Nature" => EmojiCategoryId::AnimalsNature,
        "Food & Drink" => EmojiCategoryId::FoodDrink,
        "Travel & Places" => EmojiCategoryId::TravelPlaces,
        "Flags" => return None,
        "Activities" => EmojiCategoryId::Activities,
        "Objects" => EmojiCategoryId::Objects,
        "Symbols" => EmojiCategoryId::Symbols,
        "Component" => return None,
        _ => return None,
    })
}

#[derive(Default)]
struct Annotation {
    keywords: String,
    name: Option<String>,
}

fn parse_annotations(source: &str) -> HashMap<String, Annotation> {
    let mut annotations: HashMap<String, Annotation> = HashMap::new();
    for line in source.lines() {
        let Some(cp_start) = line.find("<annotation cp=\"") else {
            continue;
        };
        let cp_start = cp_start + "<annotation cp=\"".len();
        let Some(cp_end) = line[cp_start..].find('"') else {
            continue;
        };
        let cp = &line[cp_start..cp_start + cp_end];
        let Some(text_start) = line[cp_start + cp_end..].find('>') else {
            continue;
        };
        let text_start = cp_start + cp_end + text_start + 1;
        let Some(text_end) = line[text_start..].find("</annotation>") else {
            continue;
        };
        let text = unescape_xml(&line[text_start..text_start + text_end]);
        let entry = annotations.entry(annotation_key(cp)).or_default();
        if line.contains("type=\"tts\"") {
            entry.name = Some(text);
        } else {
            entry.keywords = text;
        }
    }
    annotations
}

fn code_points_to_string(code_points: &str) -> Option<String> {
    code_points
        .split_whitespace()
        .map(|code_point| {
            u32::from_str_radix(code_point, 16)
                .ok()
                .and_then(char::from_u32)
        })
        .collect()
}

pub(crate) fn annotation_key(value: &str) -> String {
    value
        .chars()
        .filter(|character| *character != '\u{fe0f}')
        .collect()
}

fn remove_skin_modifiers(value: &str) -> String {
    value
        .chars()
        .filter(|character| !matches!(*character as u32, 0x1F3FB..=0x1F3FF))
        .collect()
}

fn unescape_xml(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

#[cfg(test)]
mod tests {
    use super::{EmojiCategoryId, data};

    #[test]
    fn loads_complete_unicode_emoji_groups() {
        let data = data();
        assert!(data.items.len() > 1_500);
        assert!(
            data.items
                .iter()
                .any(|item| item.glyph == "😀" && item.category == EmojiCategoryId::SmileysEmotion)
        );
        assert!(data.items.iter().any(|item| item.glyph == "👨‍👩‍👧‍👦"));
        assert!(
            data.items
                .iter()
                .any(|item| item.glyph == "👋" && item.skin_variants.len() == 5)
        );
        assert!(!data.items.iter().any(|item| item.glyph == "👋🏻"));
        assert!(!data.items.iter().any(|item| {
            item.glyph
                .chars()
                .any(|character| matches!(character as u32, 0x1F1E6..=0x1F1FF))
        }));
    }
}
