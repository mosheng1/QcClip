//! 剪贴板中的完整颜色代码识别，不修改原始文本。

use csscolorparser::Color;

pub(crate) fn parse_color_code(text: &str) -> Option<[f32; 4]> {
    let text = text.trim();
    if text.is_empty() || !text.is_ascii() {
        return None;
    }

    let color = if text.starts_with('#') {
        csscolorparser::parse(text).ok()?
    } else if let Some((name, body)) = text.split_once('(') {
        parse_function(text, name.trim_end(), body.strip_suffix(')')?)?
    } else if text.eq_ignore_ascii_case("transparent") {
        Color::new(0.0, 0.0, 0.0, 0.0)
    } else if let Some([r, g, b]) = csscolorparser::NAMED_COLORS.get(text.into()) {
        Color::from_rgba8(*r, *g, *b, 255)
    } else {
        parse_rgb_triplet(text)?
    };
    let rgba = color.to_array();
    rgba.iter().all(|value| value.is_finite()).then_some(rgba)
}

fn parse_rgb_triplet(text: &str) -> Option<Color> {
    let values = if text.contains(',') {
        text.split(',').map(str::trim).collect::<Vec<_>>()
    } else {
        text.split_ascii_whitespace().collect::<Vec<_>>()
    };
    let [r, g, b] = values.as_slice() else {
        return None;
    };
    Some(Color::from_rgba8(
        r.parse().ok()?,
        g.parse().ok()?,
        b.parse().ok()?,
        255,
    ))
}

fn parse_function(text: &str, name: &str, body: &str) -> Option<Color> {
    let name = name.to_ascii_lowercase();
    if !matches!(
        name.as_str(),
        "rgb"
            | "rgba"
            | "hsl"
            | "hsla"
            | "hwb"
            | "hwba"
            | "hsv"
            | "hsva"
            | "lab"
            | "lch"
            | "oklab"
            | "oklch"
    ) {
        return None;
    }
    let values = body
        .split(|ch: char| ch.is_ascii_whitespace() || matches!(ch, ',' | '/'))
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    // 拒绝 NaN 和无穷值，避免解析库钳制后把无效代码显示为有效颜色。
    for value in &values {
        let number = ["%", "grad", "turn", "deg", "rad"]
            .into_iter()
            .find_map(|unit| value.strip_suffix(unit))
            .unwrap_or(value);
        if !number.parse::<f32>().ok()?.is_finite() {
            return None;
        }
    }
    let color = csscolorparser::parse(text).ok()?;
    if matches!(name.as_str(), "hsl" | "hsla") {
        let [h, s, l, alpha @ ..] = values.as_slice() else {
            return None;
        };
        // 取色工具可能省略百分号，仍按 0～100 的饱和度和亮度解释。
        let s = format!("{}%", s.trim_end_matches('%'));
        let l = format!("{}%", l.trim_end_matches('%'));
        return csscolorparser::parse(&format!(
            "hsl({h} {s} {l} / {})",
            alpha.first().copied().unwrap_or("1")
        ))
        .ok();
    }
    Some(color)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba8(text: &str) -> Option<[u8; 4]> {
        parse_color_code(text).map(|rgba| rgba.map(|value| (value * 255.0).round() as u8))
    }

    #[test]
    fn recognizes_all_issue_124_examples() {
        for (text, expected) in [
            ("#00aeeb", [0, 174, 235, 255]),
            ("#0ae", [0, 170, 238, 255]),
            ("#00aeeb80", [0, 174, 235, 128]),
            ("#0ae8", [0, 170, 238, 136]),
            ("rgb(0,174,235)", [0, 174, 235, 255]),
            ("rgb(0 174 235)", [0, 174, 235, 255]),
            ("rgb(0% 68% 92%)", [0, 173, 235, 255]),
            ("rgba(0,174,235,0.5)", [0, 174, 235, 128]),
            ("rgb(0 174 235 / 0.5)", [0, 174, 235, 128]),
            ("hsl(196,100%,46%)", [0, 172, 235, 255]),
            ("hsl(196 100% 46%)", [0, 172, 235, 255]),
            ("hsl(196,100,46)", [0, 172, 235, 255]),
            ("hsla(196,100%,46%,0.5)", [0, 172, 235, 128]),
            ("255,185,0", [255, 185, 0, 255]),
            ("255 185 0", [255, 185, 0, 255]),
        ] {
            assert_eq!(rgba8(text), Some(expected), "颜色识别错误：{text}");
        }
    }

    #[test]
    fn accepts_whitespace_case_alpha_and_hue_units() {
        for (text, expected) in [
            (" \r\n#00AeEb\t", [0, 174, 235, 255]),
            ("\u{3000}#f00\u{3000}", [255, 0, 0, 255]),
            ("RgBa(0, 174, 235, 50%)", [0, 174, 235, 128]),
            ("HsLa(196 100 46 / 50%)", [0, 172, 235, 128]),
            ("hsl(0.5turn 100% 50%)", [0, 255, 255, 255]),
            ("hsl(-120deg 100% 50%)", [0, 0, 255, 255]),
            ("hsl(0,1,50)", [129, 126, 126, 255]),
            (" 255, 185, 0 ", [255, 185, 0, 255]),
            ("255\t185\n0", [255, 185, 0, 255]),
            ("#fff0", [255, 255, 255, 0]),
            ("#ffffff", [255, 255, 255, 255]),
            ("#000000", [0, 0, 0, 255]),
        ] {
            assert_eq!(rgba8(text), Some(expected), "颜色识别错误：{text}");
        }
    }

    #[test]
    fn recognizes_named_and_additional_color_functions() {
        assert_eq!(rgba8("RebeccaPurple"), Some([102, 51, 153, 255]));
        assert_eq!(rgba8("TRANSPARENT"), Some([0, 0, 0, 0]));
        assert_eq!(rgba8("hwb(0 0% 0%)"), Some([255, 0, 0, 255]));
        assert_eq!(rgba8("hsv(120 100% 100%)"), Some([0, 255, 0, 255]));
        for text in [
            "lab(50% 0 0)",
            "lch(50% 30 120)",
            "oklab(50% 0 0)",
            "oklch(50% 0.1 120)",
        ] {
            assert!(parse_color_code(text).is_some(), "未识别颜色：{text}");
        }
    }

    #[test]
    fn rejects_invalid_codes_and_mixed_content() {
        for text in [
            "",
            " ",
            "abc",
            "bad",
            "123",
            "00aeeb",
            "#12",
            "#12345",
            "#1234567",
            "#ggg",
            "#ff0000 文本",
            "颜色：#00aeeb",
            "#f00\n#0f0",
            "rgb(1,2)",
            "rgb(1,,2,3)",
            "rgb(1 2 3)extra",
            "rgb(1 2 3);",
            "rgb(NaN 0 0)",
            "rgb(inf 0 0)",
            "rgb(1e100 0 0)",
            "rgba(0,0,0,NaN)",
            "hsl(0,NaN,50)",
            "hsl(0,,100,50)",
            "hsl(0 100 50 1 2)",
            "var(--color)",
            "1,2",
            "1,2,3,4",
            "256,0,0",
            "-1 0 0",
            "1,,2,3",
            "1,2 3",
            "1.5 2 3",
            "1，2，3",
        ] {
            assert!(parse_color_code(text).is_none(), "误识别为颜色：{text}");
        }
    }
}
