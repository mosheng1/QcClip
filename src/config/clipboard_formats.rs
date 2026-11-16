use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ClipboardFormatConfig {
    pub all: bool,
    pub names: Vec<String>,
}

impl Default for ClipboardFormatConfig {
    fn default() -> Self {
        Self {
            all: false,
            names: [
                "CF_UNICODETEXT",
                "CF_TEXT",
                "CF_OEMTEXT",
                "CF_HDROP",
                "HTML Format",
                "Rich Text Format",
                "PNG",
                "WEBP",
                "JPEG",
                "JPG",
                "GIF",
                "BMP",
                "TIFF",
                "TIF",
                "ICO",
                "CF_DIBV5",
                "CF_DIB",
                "CF_TIFF",
                "JFIF",
                "text/html",
                "text/rtf",
                "application/rtf",
                "image/png",
                "image/webp",
                "image/jpeg",
                "image/bmp",
                "image/tiff",
                "image/gif",
                "image/x-icon",
                "image/vnd.microsoft.icon",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        }
    }
}

impl ClipboardFormatConfig {
    pub fn valid_name(name: &str) -> bool {
        let name = name.trim();
        !name.is_empty()
            && !name.chars().any(char::is_control)
            && name.encode_utf16().count() <= 255
    }

    pub fn includes(&self, native_name: &str) -> bool {
        self.all
            || self
                .names
                .iter()
                .any(|name| name.eq_ignore_ascii_case(native_name))
    }

    pub(crate) fn normalized(mut self) -> Self {
        let mut names = Vec::<String>::with_capacity(self.names.len());
        for name in self.names {
            let name = name.trim();
            if Self::valid_name(name) && !names.iter().any(|item| item.eq_ignore_ascii_case(name)) {
                names.push(name.to_owned());
            }
        }
        self.names = names;
        self
    }
}
