use std::{
    collections::{HashMap, HashSet},
    sync::OnceLock,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u32)]
pub(crate) enum SymbolGroupId {
    Recent,
    Punctuation,
    Arrows,
    Math,
    Currency,
    Geometric,
    Box,
    Misc,
    Technical,
    Letterlike,
    Numbers,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SymbolGroupDefinition {
    pub(crate) id: SymbolGroupId,
    pub(crate) label_key: &'static str,
    pub(crate) icon: &'static str,
}

pub(crate) const SYMBOL_GROUPS: &[SymbolGroupDefinition] = &[
    SymbolGroupDefinition {
        id: SymbolGroupId::Recent,
        icon: "clock",
        label_key: "symbol.recent",
    },
    SymbolGroupDefinition {
        id: SymbolGroupId::Punctuation,
        icon: "quote",
        label_key: "symbol.punctuation",
    },
    SymbolGroupDefinition {
        id: SymbolGroupId::Arrows,
        icon: "arrow-right",
        label_key: "symbol.arrows",
    },
    SymbolGroupDefinition {
        id: SymbolGroupId::Math,
        icon: "math-symbols",
        label_key: "symbol.math",
    },
    SymbolGroupDefinition {
        id: SymbolGroupId::Currency,
        icon: "currency-dollar",
        label_key: "symbol.currency",
    },
    SymbolGroupDefinition {
        id: SymbolGroupId::Geometric,
        icon: "cube",
        label_key: "symbol.geometric",
    },
    SymbolGroupDefinition {
        id: SymbolGroupId::Box,
        icon: "border-corners",
        label_key: "symbol.box",
    },
    SymbolGroupDefinition {
        id: SymbolGroupId::Misc,
        icon: "star",
        label_key: "symbol.misc",
    },
    SymbolGroupDefinition {
        id: SymbolGroupId::Technical,
        icon: "hexagon-3d",
        label_key: "symbol.technical",
    },
    SymbolGroupDefinition {
        id: SymbolGroupId::Letterlike,
        icon: "letter-a",
        label_key: "symbol.letterlike",
    },
    SymbolGroupDefinition {
        id: SymbolGroupId::Numbers,
        icon: "numbers",
        label_key: "symbol.numbers",
    },
];

#[derive(Clone, Debug)]
pub(crate) struct SymbolItem {
    pub(crate) glyph: String,
    pub(crate) name: String,
    pub(crate) search_text: String,
    pub(crate) category: SymbolGroupId,
}

pub(crate) struct SymbolData {
    pub(crate) items: Vec<SymbolItem>,
}

pub(crate) fn data() -> &'static SymbolData {
    static DATA: OnceLock<SymbolData> = OnceLock::new();
    DATA.get_or_init(|| {
        let source = crate::assets::unpack_text(include_bytes!(concat!(
            env!("OUT_DIR"),
            "/UnicodeData.txt.zst"
        )));
        parse_unicode_data(&source)
    })
}

fn parse_unicode_data(source: &str) -> SymbolData {
    let names = parse_character_names(source);
    let mut items = Vec::new();
    let mut seen = HashSet::new();

    for definition in SYMBOL_GROUPS {
        if definition.id == SymbolGroupId::Recent {
            continue;
        }
        for code_point in code_points_for_group(definition.id) {
            let Some(name) = names.get(&code_point) else {
                continue;
            };
            if !is_displayable_symbol(code_point, name) || !seen.insert(code_point) {
                continue;
            }
            let Some(glyph) = char::from_u32(code_point).map(|character| character.to_string())
            else {
                continue;
            };
            let search_text = format!("{glyph} {name}").to_lowercase();
            items.push(SymbolItem {
                glyph,
                name: format_symbol_name(name),
                search_text,
                category: definition.id,
            });
        }
    }

    SymbolData { items }
}

fn parse_character_names(source: &str) -> HashMap<u32, String> {
    let mut names = HashMap::new();
    let mut range_start = None;
    let mut range_name = String::new();
    let mut range_category = String::new();

    for line in source.lines() {
        let fields = line.split(';').collect::<Vec<_>>();
        if fields.len() < 3 {
            continue;
        }
        let Ok(code_point) = u32::from_str_radix(fields[0], 16) else {
            continue;
        };
        let name = fields[1];
        let category = fields[2];
        if let Some(base_name) = name
            .strip_suffix(", First>")
            .and_then(|name| name.strip_prefix('<'))
        {
            range_start = Some(code_point);
            range_name = base_name.to_owned();
            range_category = category.to_owned();
            continue;
        }
        if let Some(start) = range_start.take() {
            if name.ends_with(", Last>") {
                for value in start..=code_point {
                    names.insert(value, format!("{range_name} {}", value - start));
                }
                range_name.clear();
                range_category.clear();
                continue;
            }
        }
        if !name.starts_with('<') && !category.starts_with('C') {
            names.insert(code_point, name.to_owned());
        }
    }

    names
}

fn code_points_for_group(group: SymbolGroupId) -> Vec<u32> {
    let ranges: &[(u32, u32)] = match group {
        SymbolGroupId::Recent => &[],
        SymbolGroupId::Punctuation => &[(0x3000, 0x303f), (0xff01, 0xff5e)],
        SymbolGroupId::Arrows => &[(0x2190, 0x21ff), (0x27f0, 0x27ff), (0x2900, 0x297f)],
        SymbolGroupId::Math => &[(0x2200, 0x22ff), (0x27c0, 0x27ef), (0x2980, 0x29ff)],
        SymbolGroupId::Currency => &[(0x20a0, 0x20cf)],
        SymbolGroupId::Geometric => &[(0x25a0, 0x25ff)],
        SymbolGroupId::Box => &[(0x2500, 0x257f)],
        SymbolGroupId::Misc => &[(0x2600, 0x26ff), (0x2700, 0x27bf)],
        SymbolGroupId::Technical => &[(0x2300, 0x23ff)],
        SymbolGroupId::Letterlike => &[(0x2100, 0x214f)],
        SymbolGroupId::Numbers => &[(0x2460, 0x24ff)],
    };
    let mut code_points = ranges
        .iter()
        .flat_map(|(start, end)| *start..=*end)
        .collect::<Vec<_>>();
    if group == SymbolGroupId::Punctuation {
        code_points.extend(
            "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~‘’“”…—–·•©®™°±×÷"
                .chars()
                .map(u32::from),
        );
    }
    code_points
}

fn is_displayable_symbol(code_point: u32, name: &str) -> bool {
    let Some(character) = char::from_u32(code_point) else {
        return false;
    };
    !character.is_whitespace()
        && !character.is_control()
        && !name.starts_with("<")
        && !(0xd800..=0xdfff).contains(&code_point)
        && !(0xe000..=0xf8ff).contains(&code_point)
        && !(0xf0000..=0xffffd).contains(&code_point)
        && !(0x100000..=0x10fffd).contains(&code_point)
}

fn format_symbol_name(name: &str) -> String {
    name.replace('_', " ").to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::{SymbolGroupId, data};

    #[test]
    fn loads_classified_unicode_symbols() {
        let data = data();
        assert!(data.items.len() > 500);
        assert!(data.items.iter().any(|item| item.glyph == "→"));
        assert!(
            data.items
                .iter()
                .any(|item| item.glyph == "→" && item.category == SymbolGroupId::Arrows)
        );
        assert!(data.items.iter().any(|item| item.glyph == "∑"));
        assert!(data.items.iter().any(|item| item.glyph == "€"));
        assert!(data.items.iter().all(|item| {
            item.glyph.chars().count() == 1 && !item.glyph.chars().next().unwrap().is_control()
        }));
        let unique_count = data
            .items
            .iter()
            .map(|item| &item.glyph)
            .collect::<std::collections::HashSet<_>>()
            .len();
        assert_eq!(unique_count, data.items.len());
    }
}
