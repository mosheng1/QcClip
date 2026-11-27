use gpui::{Hsla, hsla, hsla_to_rgba, rgb, rgb_to_hsla};
use gpui_component::{Colorize as _, ThemeColor, ThemeMode};

#[derive(Debug, Clone, Copy)]
pub(crate) struct BackgroundPalette {
    dominant: Hsla,
    dark: bool,
}

impl BackgroundPalette {
    pub(crate) fn from_image(image: &image::RgbaImage) -> Option<Self> {
        let sample = image::imageops::thumbnail(image, 96, 96);
        let mut bins = [(0.0_f32, [0.0_f32; 3]); 512];
        let mut lightness = 0.0;
        let mut total = 0.0;
        for pixel in sample.pixels() {
            let [r, g, b, a] = pixel.0;
            let weight = a as f32 / 255.0;
            let color: Hsla = rgb_to_hsla(rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32));
            lightness += color.lightness * weight;
            total += weight;
            let index = ((r as usize >> 5) << 6) | ((g as usize >> 5) << 3) | (b as usize >> 5);
            // 稍微提高彩色区域的权重，避免大面积灰白色完全掩盖图片中的主色。
            let weight = weight * (1.0 + color.saturation * 1.5);
            bins[index].0 += weight;
            for (sum, channel) in bins[index].1.iter_mut().zip([r, g, b]) {
                *sum += channel as f32 * weight;
            }
        }
        if total == 0.0 {
            return None;
        }
        let (weight, channels) = bins.into_iter().max_by(|a, b| a.0.total_cmp(&b.0))?;
        let [r, g, b] = channels.map(|channel| (channel / weight).round() as u32);
        Some(Self {
            dominant: rgb_to_hsla(rgb((r << 16) | (g << 8) | b)),
            dark: lightness / total < 0.55,
        })
    }

    pub(crate) fn mode(self) -> ThemeMode {
        if self.dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        }
    }

    pub(crate) fn colors(self) -> ThemeColor {
        let mut colors = if self.dark {
            *ThemeColor::dark()
        } else {
            *ThemeColor::light()
        };
        let hue = self.dominant.hue.into_positive_degrees() / 360.0;
        let saturation = self.dominant.saturation.min(0.65);
        // 保留默认色板的层次和语义颜色，只给中性表面染上主色的低饱和色调。
        macro_rules! tint_field {
            ($field:ident) => {
                if colors.$field.saturation < 0.12 {
                    colors.$field.hue = self.dominant.hue;
                    colors.$field.saturation = saturation * 0.22;
                }
            };
        }
        for_each_theme_color_field!(tint_field);
        // 直接混合 RGB 通道，避免 Oklab 与黑色混合时把实际亮度压得接近黑色。
        let dominant = hsla_to_rgba(self.dominant);
        let weight = if self.dark { 0.35 } else { 0.28 };
        let neutral = if self.dark { 0.0 } else { 1.0 };
        let mix = |channel: f32| channel * weight + neutral * (1.0 - weight);
        let background = rgb_to_hsla(gpui::Rgba::new(
            mix(dominant.red),
            mix(dominant.green),
            mix(dominant.blue),
            1.0,
        ));
        let foreground = hsla(hue, 0.04, if self.dark { 0.98 } else { 0.08 }, 1.0);
        let muted = hsla(hue, 0.04, if self.dark { 0.86 } else { 0.28 }, 1.0);
        // 按生成的主题底色校验强调色，遮罩强度由用户自行决定。
        let mut primary = hsla(
            hue,
            if saturation < 0.08 {
                0.0
            } else {
                saturation.max(0.35)
            },
            if self.dark { 0.74 } else { 0.34 },
            1.0,
        );
        while contrast(primary, background) < 4.5 {
            primary.lightness += if self.dark { 0.01 } else { -0.01 };
            if !(0.05..0.95).contains(&primary.lightness) {
                break;
            }
        }
        let primary_foreground =
            if contrast(gpui::black(), primary) > contrast(gpui::white(), primary) {
                gpui::black()
            } else {
                gpui::white()
            };
        macro_rules! assign {
            ($value:expr; $($field:ident),+ $(,)?) => { $(colors.$field = $value;)+ };
        }
        assign!(background; background, list, table, popover);
        assign!(foreground; foreground, accent_foreground, secondary_foreground,
            button_foreground, button_secondary_foreground, group_box_foreground,
            popover_foreground, sidebar_foreground, sidebar_accent_foreground,
            tab_foreground, tab_active_foreground, table_head_foreground, table_foot_foreground);
        assign!(muted; muted_foreground, description_list_label_foreground);
        // 图片表面没有不透明底色，普通边框使用半透明前景色，避免默认深色边框形成黑线。
        let border = hsla(hue, saturation * 0.22, foreground.lightness, 0.18);
        assign!(border; border, input, sidebar_border, table_row_border, title_bar_border,
            status_bar_border, window_border);
        assign!(primary; primary, primary_hover, primary_active, button_primary,
            button_primary_hover, button_primary_active, sidebar_primary, ring, caret,
            link, link_hover, link_active, slider_thumb, list_active_border, table_active_border);
        assign!(primary_foreground; primary_foreground, button_primary_foreground, sidebar_primary_foreground);
        assign!(primary.opacity(0.16); accent, sidebar_accent, list_hover, table_hover);
        assign!(primary.opacity(0.24); list_active, table_active, selection);
        colors
    }
}

fn contrast(a: Hsla, b: Hsla) -> f32 {
    let luminance = |color: Hsla| {
        let color = hsla_to_rgba(color);
        let linear = |value: f32| {
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(color.red) + 0.7152 * linear(color.green) + 0.0722 * linear(color.blue)
    };
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dominant_color_ignores_transparent_pixels() {
        let mut image = image::RgbaImage::from_pixel(10, 10, image::Rgba([255, 0, 0, 0]));
        for x in 0..7 {
            for y in 0..10 {
                image.put_pixel(x, y, image::Rgba([0, 180, 0, 255]));
            }
        }
        image.put_pixel(9, 9, image::Rgba([0, 0, 255, 255]));
        let palette = BackgroundPalette::from_image(&image).expect("应提取可见主色");
        assert!((palette.dominant.hue.into_positive_degrees() / 360.0 - 1.0 / 3.0).abs() < 0.01);
        assert!(BackgroundPalette::from_image(&image::RgbaImage::new(2, 2)).is_none());
    }

    #[test]
    fn background_borders_are_translucent_and_keep_the_image_hue() {
        for dark in [false, true] {
            let palette = BackgroundPalette {
                dominant: hsla(0.6, 0.6, 0.5, 1.0),
                dark,
            };
            let colors = palette.colors();
            for border in [
                colors.border,
                colors.input,
                colors.sidebar_border,
                colors.table_row_border,
                colors.title_bar_border,
                colors.status_bar_border,
                colors.window_border,
            ] {
                assert!((border.alpha - 0.18).abs() < 0.001);
                assert!((border.hue.into_positive_degrees() / 360.0 - 0.6).abs() < 0.001);
                assert_eq!(border.lightness, colors.foreground.lightness);
            }
            assert_eq!(colors.list_active_border, colors.primary);
            assert_eq!(colors.table_active_border, colors.primary);
        }
    }

    #[test]
    fn green_background_keeps_its_rgb_share_when_mixed_with_black_or_white() {
        let dominant = rgb_to_hsla(rgb(0x20964b));
        let source = hsla_to_rgba(dominant);
        for dark in [false, true] {
            let colors = BackgroundPalette { dominant, dark }.colors();
            let background = hsla_to_rgba(colors.background);
            let (weight, neutral) = if dark { (0.35, 0.0) } else { (0.28, 1.0) };
            for (actual, channel) in [
                (background.red, source.red),
                (background.green, source.green),
                (background.blue, source.blue),
            ] {
                assert!((actual - (channel * weight + neutral * (1.0 - weight))).abs() < 0.001);
            }
            assert_eq!(colors.popover, colors.background);
        }
    }

    #[test]
    fn generated_text_contrasts_with_theme_background() {
        for hue in [0.0, 0.16, 0.33, 0.5, 0.66, 0.83] {
            for dark in [false, true] {
                let palette = BackgroundPalette {
                    dominant: hsla(hue, 1.0, 0.5, 1.0),
                    dark,
                };
                let colors = palette.colors();
                for text in [colors.foreground, colors.muted_foreground, colors.primary] {
                    assert!(
                        contrast(text, colors.background) >= 4.5,
                        "文字对比不足：{hue} {dark}"
                    );
                }
                assert!(contrast(colors.primary, colors.primary_foreground) >= 4.5);
            }
        }
    }
}
