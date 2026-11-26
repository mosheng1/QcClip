use gpui::{AssetSource, SharedString};
use rust_embed::RustEmbed;
use std::{borrow::Cow, collections::HashMap};

#[derive(RustEmbed)]
#[folder = "assets"]
#[include = "icons/*.svg"]
#[include = "*.png"]
#[include = "data/emoji/twemoji.json.zst"]
#[include = "data/emoji/TWEMOJI-LICENSE.txt"]
struct EmbeddedAssets;

pub struct Assets;

pub(crate) fn unpack_text(packed: &[u8]) -> String {
    let bytes = zstd::stream::decode_all(packed).expect("无法解压内嵌 Unicode 数据");
    String::from_utf8(bytes).expect("内嵌 Unicode 数据不是有效的 UTF-8")
}

impl Assets {
    pub fn new() -> Self {
        Self
    }

    pub(crate) fn load_emoji_images() -> gpui::Result<HashMap<String, String>> {
        let packed = EmbeddedAssets::get("data/emoji/twemoji.json.zst")
            .ok_or_else(|| std::io::Error::other("缺少内嵌 Twemoji 资源包"))?;
        let json = zstd::stream::decode_all(packed.data.as_ref())?;
        Ok(serde_json::from_slice(&json)?)
    }
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let Some(asset_path) = path.strip_prefix("assets/") else {
            // 非内嵌 assets/ 路径按本地文件系统读取（如数据目录里的搜索引擎图标）。
            return match std::fs::read(path) {
                Ok(bytes) => Ok(Some(Cow::Owned(bytes))),
                Err(_) => Ok(None),
            };
        };

        Ok(EmbeddedAssets::get(asset_path).map(|file| file.data))
    }

    fn list(&self, _path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emoji;
    use std::sync::Arc;

    #[test]
    fn packed_unicode_data_preserves_sources_without_duplicate_assets() {
        for (path, packed, source) in [
            (
                "data/symbols/UnicodeData.txt",
                include_bytes!(concat!(env!("OUT_DIR"), "/UnicodeData.txt.zst")).as_slice(),
                include_str!("../assets/data/symbols/UnicodeData.txt"),
            ),
            (
                "data/emoji/emoji-test.txt",
                include_bytes!(concat!(env!("OUT_DIR"), "/emoji-test.txt.zst")).as_slice(),
                include_str!("../assets/data/emoji/emoji-test.txt"),
            ),
            (
                "data/emoji/annotations-zh.xml",
                include_bytes!(concat!(env!("OUT_DIR"), "/annotations-zh.xml.zst")).as_slice(),
                include_str!("../assets/data/emoji/annotations-zh.xml"),
            ),
        ] {
            assert_eq!(unpack_text(packed), source);
            assert!(EmbeddedAssets::get(path).is_none());
        }
        assert!(EmbeddedAssets::iter().all(|path| {
            path.ends_with(".svg")
                || path.ends_with(".png")
                || path == "data/emoji/twemoji.json.zst"
                || path == "data/emoji/TWEMOJI-LICENSE.txt"
        }));
    }

    #[test]
    fn twemoji_covers_picker_and_skin_variants() {
        let images = Assets::load_emoji_images().unwrap();
        let renderer = gpui::SvgRenderer::new(Arc::new(Assets::new()));
        for item in &emoji::data().items {
            for glyph in std::iter::once(&item.glyph).chain(&item.skin_variants) {
                let svg = images
                    .get(&emoji::annotation_key(glyph))
                    .expect("表情缺少 SVG 资源");
                renderer
                    .parse_svg(svg.as_bytes())
                    .expect("表情 SVG 无法解析");
            }
        }
        for glyph in ["😀", "✈️", "#️⃣", "👨‍👩‍👧‍👦", "👋🏻", "🫪"]
        {
            let svg = images.get(&emoji::annotation_key(glyph)).unwrap();
            let image = renderer
                .render_single_frame(svg.as_bytes(), 2.0)
                .expect("表情 SVG 无法渲染");
            assert!(image.size(0).width.0 > 0);
        }
    }
}
