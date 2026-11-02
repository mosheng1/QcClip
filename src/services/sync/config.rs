use std::{fs, io::Write, path::PathBuf};

use atomic_write_file::AtomicWriteFile;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use crypto_box::{SecretKey, aead::OsRng};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{Result, fail};

pub const PORT: u16 = 39827;
pub const MAX_EVENT_BYTES: usize = 256 * 1024 * 1024;
pub const CHUNK_BYTES: usize = 256 * 1024;
pub const MAX_INCOMING_EVENTS: usize = 128;
pub const MAX_INCOMING_BYTES: u64 = 512 * 1024 * 1024;
pub const EVENT_TTL: u64 = 90;

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Policy {
    pub enabled: bool,
    pub history: bool,
    pub favorites: bool,
    pub touches: bool,
    pub lan: bool,
    pub webdav: bool,
    pub text: bool,
    pub rich_text: bool,
    pub images: bool,
    pub custom: bool,
    pub file_paths: bool,
    pub file_contents: bool,
    pub common_only: bool,
    pub excluded_formats: Vec<String>,
    pub image_limit_mb: u32,
    pub file_limit_mb: u32,
    pub files_limit_mb: u32,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            enabled: true,
            history: true,
            favorites: true,
            touches: true,
            lan: true,
            webdav: true,
            text: true,
            rich_text: true,
            images: true,
            custom: true,
            file_paths: false,
            file_contents: false,
            common_only: false,
            excluded_formats: Vec::new(),
            image_limit_mb: 32,
            file_limit_mb: 64,
            files_limit_mb: 128,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub app_version: String,
    pub protocol: u32,
    pub key: [u8; 32],
}

impl Profile {
    pub fn label(&self) -> String {
        format!(
            "{} · {} · {}",
            self.name,
            self.platform,
            &self.id[..8.min(self.id.len())]
        )
    }
    pub fn validate(&self) -> Result<()> {
        if Uuid::parse_str(&self.id).is_err()
            || self.name.is_empty()
            || self.name.chars().count() > 64
            || self.name.chars().any(char::is_control)
            || !matches!(self.platform.as_str(), "windows" | "macos" | "linux")
            || self.protocol != super::protocol::VERSION
            || self.key == [0; 32]
        {
            return fail("设备资料或协议版本无效");
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Peer {
    pub profile: Profile,
    pub address: String,
    pub policy: Policy,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DavConfig {
    pub enabled: bool,
    pub url: String,
    pub username: String,
    pub password: String,
    pub poll_seconds: u64,
    pub auto_upload: bool,
    pub auto_pull: bool,
    pub cache_entries: usize,
    pub cache_mb: usize,
}

impl Default for DavConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
            username: String::new(),
            password: String::new(),
            poll_seconds: 60,
            auto_upload: true,
            auto_pull: true,
            cache_entries: 100,
            cache_mb: 100,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    pub enabled: bool,
    pub lan_enabled: bool,
    #[serde(default = "default_copy_on_receive")]
    pub copy_on_receive: bool,
    pub profile: Profile,
    pub secret: [u8; 32],
    pub sequence: u64,
    pub port: u16,
    pub peers: Vec<Peer>,
    pub dav: DavConfig,
}

impl Config {
    pub fn fresh() -> Self {
        let secret = SecretKey::generate(&mut OsRng);
        let id = Uuid::new_v4().to_string();
        let name = std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| format!("我的设备 {}", &id[..8]));
        Self {
            enabled: false,
            lan_enabled: true,
            copy_on_receive: true,
            profile: Profile {
                id,
                name: clean_name(&name),
                platform: std::env::consts::OS.to_owned(),
                app_version: env!("CARGO_PKG_VERSION").to_owned(),
                protocol: super::protocol::VERSION,
                key: *secret.public_key().as_bytes(),
            },
            secret: secret.to_bytes(),
            sequence: 0,
            port: PORT,
            peers: Vec::new(),
            dav: DavConfig::default(),
        }
    }
    pub fn validate(&self) -> Result<()> {
        self.profile.validate()?;
        if SecretKey::from(self.secret).public_key().as_bytes() != &self.profile.key {
            return fail("设备私钥与公钥不匹配");
        }
        if (self.lan_enabled && self.port == 0)
            || self.peers.len() > 32
            || (self.dav.enabled
                && (!(1..=86400).contains(&self.dav.poll_seconds)
                    || !(1..=1000).contains(&self.dav.cache_entries)
                    || !(1..=1024).contains(&self.dav.cache_mb)))
        {
            return fail("端口、设备数量、拉取间隔或缓存容量无效");
        }
        let mut ids = std::collections::HashSet::new();
        let mut keys = std::collections::HashSet::new();
        for peer in &self.peers {
            peer.profile.validate()?;
            if peer.profile.id == self.profile.id
                || peer.profile.key == self.profile.key
                || !ids.insert(&peer.profile.id)
                || !keys.insert(peer.profile.key)
            {
                return fail("设备身份重复，请重新配对");
            }
            if self.lan_enabled && peer.policy.lan && !peer.address.is_empty() {
                validate_address(&peer.address)?;
            }
            let p = &peer.policy;
            if !(1..=128).contains(&p.image_limit_mb)
                || !(1..=128).contains(&p.file_limit_mb)
                || !(1..=192).contains(&p.files_limit_mb)
                || p.excluded_formats.len() > 128
            {
                return fail("资源限制超出允许范围");
            }
        }
        if self.dav.enabled {
            validate_dav_url(&self.dav.url)?;
        }
        Ok(())
    }
    pub fn lan_allowed(&self, peer: &Peer) -> bool {
        self.enabled && self.lan_enabled && peer.policy.enabled && peer.policy.lan
    }
    pub fn dav_allowed(&self, peer: &Peer) -> bool {
        self.enabled && self.dav.enabled && peer.policy.enabled && peer.policy.webdav
    }
}

fn default_copy_on_receive() -> bool {
    true
}

pub fn validate_dav_url(address: &str) -> Result<url::Url> {
    let url = url::Url::parse(address).map_err(|_| {
        super::Error::Message(
            "WebDAV 地址无效，请填写 HTTP 或 HTTPS 服务器地址，不能填写本地文件夹路径".into(),
        )
    })?;
    if url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return fail("WebDAV 地址必须包含服务器，且不能内嵌账号密码、查询参数或片段");
    }
    if !matches!(url.scheme(), "http" | "https") {
        return fail("WebDAV 地址仅支持 HTTP 或 HTTPS");
    }
    Ok(url)
}

pub fn clean_name(name: &str) -> String {
    let name: String = name.chars().filter(|c| !c.is_control()).take(64).collect();
    let name = name.trim();
    if name.is_empty() {
        "我的设备".to_owned()
    } else {
        name.to_owned()
    }
}

pub fn validate_address(address: &str) -> Result<std::net::SocketAddr> {
    let addr: std::net::SocketAddr = address
        .parse()
        .map_err(|_| super::Error::Message("请输入 IP:端口，例如 192.168.1.10:39827".into()))?;
    if addr.port() == 0 || addr.ip().is_unspecified() || addr.ip().is_multicast() {
        return fail("局域网地址无效");
    }
    Ok(addr)
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PairingCode {
    pub profile: Profile,
    pub address: String,
}

impl PairingCode {
    pub fn encode(&self) -> Result<String> {
        Ok(format!("qcclip1:{}", B64.encode(serde_json::to_vec(self)?)))
    }
    pub fn decode(code: &str) -> Result<Self> {
        if code.len() > 4096 {
            return fail("配对码过长");
        }
        let bytes = B64
            .decode(
                code.trim()
                    .strip_prefix("qcclip1:")
                    .ok_or_else(|| super::Error::Message("配对码格式无效".into()))?,
            )
            .map_err(|_| super::Error::Message("配对码编码无效".into()))?;
        let value: Self = serde_json::from_slice(&bytes)?;
        value.profile.validate()?;
        if !value.address.is_empty() {
            validate_address(&value.address)?;
        }
        Ok(value)
    }
    pub fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(self.profile.key)
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .chunks(4)
            .map(|s| s.concat())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    pub fn discover() -> Result<Self> {
        let base = directories::BaseDirs::new()
            .ok_or_else(|| super::Error::Message("无法确定设备身份目录".into()))?;
        Ok(Self {
            path: base.data_local_dir().join("QcClipDevice").join("sync.bin"),
        })
    }
    #[cfg(test)]
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }
    pub(super) fn cache_journal_path(&self, scope: &str) -> PathBuf {
        self.path.with_file_name(format!(
            "sync-cache-{}.json",
            super::protocol::hash(scope.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        ))
    }
    pub fn load(&self) -> Result<Config> {
        if !self.path.exists() {
            let config = Config::fresh();
            self.save(&config)?;
            return Ok(config);
        }
        let bytes = fs::read(&self.path)?;
        let plain = zeroize::Zeroizing::new(unprotect(&bytes)?);
        let config: Config = serde_json::from_slice(&plain)?;
        config.validate()?;
        Ok(config)
    }
    pub fn save(&self, config: &Config) -> Result<()> {
        config.validate()?;
        fs::create_dir_all(
            self.path
                .parent()
                .ok_or_else(|| super::Error::Message("身份存储目录无效".into()))?,
        )?;
        let plain = zeroize::Zeroizing::new(serde_json::to_vec(config)?);
        let mut file = AtomicWriteFile::open(&self.path)?;
        file.write_all(&protect(&plain)?)?;
        file.commit()?;
        Ok(())
    }
}

#[cfg(target_os = "windows")]
fn crypt(bytes: &[u8], encrypt: bool) -> Result<Vec<u8>> {
    use windows::Win32::{
        Foundation::{HLOCAL, LocalFree},
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
    };
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes
            .len()
            .try_into()
            .map_err(|_| super::Error::Message("身份数据过大".into()))?,
        pbData: bytes.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // 安全：输入在调用期间有效；输出由 DPAPI 分配，复制后使用 LocalFree 释放。
    unsafe {
        let result = if encrypt {
            CryptProtectData(
                &input,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        result.map_err(|_| super::Error::Message("Windows 设备凭据保护失败".into()))?;
        let result = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        if !encrypt {
            std::ptr::write_bytes(output.pbData, 0, output.cbData as usize);
        }
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        Ok(result)
    }
}
#[cfg(target_os = "windows")]
fn protect(bytes: &[u8]) -> Result<Vec<u8>> {
    crypt(bytes, true)
}
#[cfg(target_os = "windows")]
fn unprotect(bytes: &[u8]) -> Result<Vec<u8>> {
    crypt(bytes, false)
}
#[cfg(not(target_os = "windows"))]
fn protect(_: &[u8]) -> Result<Vec<u8>> {
    fail("当前平台尚未实现设备密钥安全存储")
}
#[cfg(not(target_os = "windows"))]
fn unprotect(_: &[u8]) -> Result<Vec<u8>> {
    fail("当前平台尚未实现设备密钥安全存储")
}
