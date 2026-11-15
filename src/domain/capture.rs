pub const CUSTOM_TEXT_FORMAT_NAMESPACE: &str = "qcclip";
pub const CUSTOM_TEXT_FORMAT_KEY: &str = "custom-text";
pub const CUSTOM_TEXT_FORMAT_NAME: &str = "自定义文本";

/// App-owned metadata listing the files managed for a synced file entry.
pub const SYNC_MANAGED_FILES_FORMAT_KEY: &str = "sync-managed-files";

pub fn is_custom_text_format(namespace: &str, key: &str) -> bool {
    namespace == CUSTOM_TEXT_FORMAT_NAMESPACE && key == CUSTOM_TEXT_FORMAT_KEY
}

pub fn is_sync_managed_files_format(namespace: &str, key: &str) -> bool {
    namespace == CUSTOM_TEXT_FORMAT_NAMESPACE && key == SYNC_MANAGED_FILES_FORMAT_KEY
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FormatKind {
    Text,
    Html,
    Rtf,
    Image,
    FileList,
    Custom,
    Unknown,
}

impl FormatKind {
    pub fn from_str(value: &str) -> Option<Self> {
        Some(match value {
            "text" => Self::Text,
            "html" => Self::Html,
            "rtf" => Self::Rtf,
            "image" => Self::Image,
            "file_list" => Self::FileList,
            "custom" => Self::Custom,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Html => "html",
            Self::Rtf => "rtf",
            Self::Image => "image",
            Self::FileList => "file_list",
            Self::Custom => "custom",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FormatRole {
    PlainText,
    Html,
    Rtf,
    Image,
    FileList,
    VirtualFile,
    Custom,
    Internal,
}

impl FormatRole {
    pub const fn is_internal(self) -> bool {
        matches!(self, Self::Internal)
    }
}

pub fn classify_format_role(kind: FormatKind, key: &str, name: &str) -> FormatRole {
    let key = key.to_ascii_lowercase();
    let name = name.to_ascii_lowercase();
    let key_name = key.strip_prefix("registered:").unwrap_or(&key);

    if is_internal_format_name(key_name, &name) {
        return FormatRole::Internal;
    }
    if is_virtual_file_format_name(key_name, &name) {
        return FormatRole::VirtualFile;
    }

    match kind {
        FormatKind::Text => FormatRole::PlainText,
        FormatKind::Html => FormatRole::Html,
        FormatKind::Rtf => FormatRole::Rtf,
        FormatKind::Image => FormatRole::Image,
        FormatKind::FileList => FormatRole::FileList,
        FormatKind::Custom | FormatKind::Unknown => FormatRole::Custom,
    }
}

fn is_internal_format_name(key: &str, name: &str) -> bool {
    matches!(key, "cf_locale" | "cf_palette" | "cf_ownerdisplay")
        || matches!(name, "cf_locale" | "cf_palette" | "cf_ownerdisplay")
        || key == "enterprisedataprotectionid"
        || name == "enterprisedataprotectionid"
        || key.starts_with("cf_dsp")
        || name.starts_with("cf_dsp")
        || name.contains("chromium internal")
        || key.contains("gpui internal")
        || name.contains("gpui internal")
        || name.contains("dataobject")
        || name.contains("shell idlist")
        || name.contains("shell object offsets")
        || name.contains("preferred dropeffect")
        || name.contains("performed dropeffect")
        || name.contains("asyncflag")
        || name.contains("ole private data")
}

fn is_virtual_file_format_name(key: &str, name: &str) -> bool {
    key.contains("filegroupdescriptor")
        || key.contains("filecontents")
        || key == "filename"
        || key == "filenamew"
        || name.contains("filegroupdescriptor")
        || name.contains("filecontents")
        || name == "filename"
        || name == "filenamew"
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FormatCaptureState {
    Captured,
    Generated,
    Unsupported,
    Failed,
}

impl FormatCaptureState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Captured => "captured",
            Self::Generated => "generated",
            Self::Unsupported => "unsupported",
            Self::Failed => "failed",
        }
    }

    pub const fn has_data(self) -> bool {
        matches!(self, Self::Captured | Self::Generated)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RestoreState {
    Pending,
    Restorable,
    NotRestorable,
    NotApplicable,
}

impl RestoreState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Restorable => "restorable",
            Self::NotRestorable => "not_restorable",
            Self::NotApplicable => "not_applicable",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawFormat {
    pub format_namespace: String,
    pub format_key: String,
    pub format_name: String,
    pub native_format_id: Option<u32>,
    pub format_kind: FormatKind,
    pub bytes: Option<Vec<u8>>,
    pub format_order: u32,
    pub capture_state: FormatCaptureState,
    pub restore_state: RestoreState,
    pub restore_error_code: Option<String>,
    pub restore_error_message: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub mime_type: Option<String>,
    pub encoding: Option<String>,
}

impl RawFormat {
    pub fn captured(
        namespace: impl Into<String>,
        key: impl Into<String>,
        name: impl Into<String>,
        kind: FormatKind,
        bytes: Vec<u8>,
        format_order: u32,
        restorable: bool,
    ) -> Self {
        Self {
            format_namespace: namespace.into(),
            format_key: key.into(),
            format_name: name.into(),
            native_format_id: None,
            format_kind: kind,
            bytes: Some(bytes),
            format_order,
            capture_state: FormatCaptureState::Captured,
            restore_state: if restorable {
                RestoreState::Restorable
            } else {
                RestoreState::NotRestorable
            },
            restore_error_code: None,
            restore_error_message: None,
            error_code: None,
            error_message: None,
            mime_type: None,
            encoding: None,
        }
    }

    pub fn failed(
        namespace: impl Into<String>,
        key: impl Into<String>,
        name: impl Into<String>,
        kind: FormatKind,
        format_order: u32,
        error_code: impl Into<String>,
        error_message: impl Into<String>,
    ) -> Self {
        Self {
            format_namespace: namespace.into(),
            format_key: key.into(),
            format_name: name.into(),
            native_format_id: None,
            format_kind: kind,
            bytes: None,
            format_order,
            capture_state: FormatCaptureState::Failed,
            restore_state: RestoreState::NotApplicable,
            restore_error_code: None,
            restore_error_message: None,
            error_code: Some(error_code.into()),
            error_message: Some(error_message.into()),
            mime_type: None,
            encoding: None,
        }
    }

    pub fn unsupported(
        namespace: impl Into<String>,
        key: impl Into<String>,
        name: impl Into<String>,
        kind: FormatKind,
        format_order: u32,
        error_code: impl Into<String>,
        error_message: impl Into<String>,
    ) -> Self {
        let mut format = Self::failed(
            namespace,
            key,
            name,
            kind,
            format_order,
            error_code,
            error_message,
        );
        format.capture_state = FormatCaptureState::Unsupported;
        format
    }

    pub fn stable_identity(&self) -> (&str, &str) {
        (&self.format_namespace, &self.format_key)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SourceApplication {
    pub name: Option<String>,
    pub path: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturedClipboard {
    pub formats: Vec<RawFormat>,
    pub primary_format_identity: Option<(String, String)>,
    pub preview_candidate: Option<String>,
    pub source_application: SourceApplication,
}

pub fn normalize_text_bytes<'a>(namespace: &str, key: &str, bytes: &'a [u8]) -> &'a [u8] {
    if namespace != "windows" {
        return bytes;
    }

    match key {
        "CF_TEXT" | "CF_OEMTEXT" => bytes
            .iter()
            .position(|byte| *byte == 0)
            .map_or(bytes, |index| &bytes[..=index]),
        "CF_UNICODETEXT" => bytes
            .chunks_exact(2)
            .position(|unit| unit == [0, 0])
            .map_or(bytes, |index| &bytes[..(index + 1) * 2]),
        _ => bytes,
    }
}
