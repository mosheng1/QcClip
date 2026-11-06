use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use crypto_box::{
    PublicKey, SalsaBox, SecretKey,
    aead::{Aead, AeadCore, OsRng},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    Result,
    config::{Config, EVENT_TTL, MAX_EVENT_BYTES, Peer, Profile},
    fail,
};

pub const VERSION: u32 = 1;
pub const MAX_FRAME: usize = 2 * 1024 * 1024;

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
pub fn hash(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

pub fn route(sender: &[u8; 32], recipient: &[u8; 32]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"QcClip/pair-route/v1");
    digest.update(sender);
    digest.update(recipient);
    digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub mod bytes {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        value: &[u8],
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&B64.encode(value))
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Vec<u8>, D::Error> {
        let value = String::deserialize(deserializer)?;
        B64.decode(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub sender: String,
    pub nonce: [u8; 24],
    #[serde(with = "bytes")]
    pub ciphertext: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct BoundMessage<T> {
    version: u32,
    sender: String,
    recipient: String,
    sent_at: u64,
    purpose: String,
    payload: T,
}

pub fn seal<T: Serialize>(config: &Config, peer: &Peer, payload: &T) -> Result<Envelope> {
    seal_bound(config, peer, payload, "online", now())
}

/// 以指定的发送时间封装在线消息；局域网发送方用它把时间戳换算到对方时钟。
pub(super) fn seal_at<T: Serialize>(
    config: &Config,
    peer: &Peer,
    payload: &T,
    sent_at: u64,
) -> Result<Envelope> {
    seal_bound(config, peer, payload, "online", sent_at)
}

pub(super) fn seal_cache<T: Serialize>(
    config: &Config,
    peer: &Peer,
    payload: &T,
    purpose: &str,
) -> Result<Envelope> {
    seal_bound(config, peer, payload, purpose, now())
}

fn seal_bound<T: Serialize>(
    config: &Config,
    peer: &Peer,
    payload: &T,
    purpose: &str,
    sent_at: u64,
) -> Result<Envelope> {
    let bound = BoundMessage {
        version: VERSION,
        sender: config.profile.id.clone(),
        recipient: peer.profile.id.clone(),
        sent_at,
        purpose: purpose.into(),
        payload,
    };
    let plain = zeroize::Zeroizing::new(serde_json::to_vec(&bound)?);
    let cipher = SalsaBox::new(
        &PublicKey::from(peer.profile.key),
        &SecretKey::from(config.secret),
    );
    let nonce = SalsaBox::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, plain.as_slice())
        .map_err(|_| super::Error::Message("同步消息加密失败".into()))?;
    Ok(Envelope {
        sender: route(&config.profile.key, &peer.profile.key),
        nonce: nonce.into(),
        ciphertext,
    })
}

#[cfg(test)]
pub fn open<T: DeserializeOwned>(config: &Config, envelope: &Envelope) -> Result<(Peer, T)> {
    open_bound(config, envelope, "online", true)
}

pub(super) fn open_cache<T: DeserializeOwned>(
    config: &Config,
    envelope: &Envelope,
    purpose: &str,
) -> Result<(Peer, T)> {
    open_bound(config, envelope, purpose, false)
}

/// 解开在线消息但不检查发送时间，返回发送方时钟下的发送时间，由调用方按时钟偏差自行校验。
pub(super) fn open_online<T: DeserializeOwned>(
    config: &Config,
    envelope: &Envelope,
) -> Result<(Peer, T, u64)> {
    open_unchecked(config, envelope, "online")
}

/// 在线消息的有效期检查；`sent_at` 必须已换算到本机时钟。
pub(super) fn check_fresh(sent_at: u64) -> Result<()> {
    check_time(sent_at, true)
}

fn check_time(sent_at: u64, online: bool) -> Result<()> {
    let time = now();
    if sent_at > time.saturating_add(30) || (online && sent_at.saturating_add(EVENT_TTL) < time) {
        return fail("同步消息已过期，或两台设备的系统时间相差过大");
    }
    Ok(())
}

/// 把时间按秒级偏差平移，用于在本机时钟与对方时钟之间换算。
pub fn shift(time: u64, offset: i64) -> u64 {
    time.saturating_add_signed(offset)
}

fn open_bound<T: DeserializeOwned>(
    config: &Config,
    envelope: &Envelope,
    purpose: &str,
    online: bool,
) -> Result<(Peer, T)> {
    let (peer, payload, sent_at) = open_unchecked(config, envelope, purpose)?;
    check_time(sent_at, online)?;
    Ok((peer, payload))
}

fn open_unchecked<T: DeserializeOwned>(
    config: &Config,
    envelope: &Envelope,
    purpose: &str,
) -> Result<(Peer, T, u64)> {
    if !config.enabled {
        return fail("在线同步已关闭");
    }
    let peer = config
        .peers
        .iter()
        .find(|p| route(&p.profile.key, &config.profile.key) == envelope.sender && p.policy.enabled)
        .ok_or_else(|| super::Error::Message("设备未配对或已被阻止".into()))?;
    let cipher = SalsaBox::new(
        &PublicKey::from(peer.profile.key),
        &SecretKey::from(config.secret),
    );
    let plain = zeroize::Zeroizing::new(
        cipher
            .decrypt((&envelope.nonce).into(), envelope.ciphertext.as_slice())
            .map_err(|_| super::Error::Message("同步消息认证失败".into()))?,
    );
    let bound: BoundMessage<T> = serde_json::from_slice(&plain)?;
    if bound.version != VERSION
        || bound.purpose != purpose
        || bound.sender != peer.profile.id
        || bound.recipient != config.profile.id
    {
        return fail("同步消息版本或目标无效");
    }
    Ok((peer.clone(), bound.payload, bound.sent_at))
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub expires: u64,
    pub profile: Profile,
}
impl Session {
    pub fn valid(&self, peer: &Peer) -> bool {
        self.profile.id == peer.profile.id
            && self.profile.key == peer.profile.key
            && self.profile.validate().is_ok()
            && Uuid::parse_str(&self.id).is_ok()
            && self.expires > now()
            && self.expires <= now() + 125
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    NewContent,
    Touch,
    Favorite,
    ManualContent,
    ManualFavorite,
}
impl Mode {
    pub fn stream(self) -> &'static str {
        match self {
            Self::Touch => "touch",
            Self::Favorite | Self::ManualFavorite => "favorite",
            _ => "content",
        }
    }
    pub fn manual(self) -> bool {
        matches!(self, Self::ManualContent | Self::ManualFavorite)
    }
    pub fn favorite(self) -> bool {
        matches!(self, Self::Favorite | Self::ManualFavorite)
    }
    pub fn history(self) -> bool {
        !self.favorite()
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Resource {
    pub id: String,
    pub length: usize,
    pub hash: [u8; 32],
    pub chunks: usize,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct WireFormat {
    pub namespace: String,
    pub key: String,
    pub name: String,
    pub kind: String,
    pub mime: Option<String>,
    pub encoding: Option<String>,
    pub resource: Resource,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct WireFile {
    pub name: String,
    pub resource: Resource,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct WireTag {
    pub uuid: String,
    pub name: String,
    pub color: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub sender: String,
    pub recipient: String,
    pub session: String,
    pub sequence: u64,
    pub expires: u64,
    pub item_uuid: String,
    pub mode: Mode,
    pub hash_version: i64,
    pub source_hash: [u8; 32],
    pub complete: bool,
    pub source_app_name: Option<String>,
    pub formats: Vec<WireFormat>,
    pub files: Vec<WireFile>,
    pub tags: Vec<WireTag>,
    pub note: Option<String>,
    pub primary: Option<(String, String)>,
}

impl Event {
    pub fn validate(&self, sender: &str, recipient: &str, session: &str) -> Result<()> {
        let time = now();
        if self.sender != sender
            || self.recipient != recipient
            || self.session != session
            || self.expires <= time
            || self.expires > time + EVENT_TTL + 30
        {
            return fail("同步事件身份、版本、会话或大小无效");
        }
        self.validate_content(sender, recipient)
    }
    pub(super) fn validate_cache(&self, sender: &str, recipient: &str) -> Result<()> {
        if self.session != "webdav-cache-v1" || self.expires != 0 {
            return fail("WebDAV 缓存消息类型无效");
        }
        self.validate_content(sender, recipient)
    }
    fn validate_content(&self, sender: &str, recipient: &str) -> Result<()> {
        if self.sender != sender
            || self.recipient != recipient
            || self.sequence == 0
            || self.sequence > i64::MAX as u64
            || self.hash_version != crate::services::capture::CONTENT_HASH_VERSION
            || Uuid::parse_str(&self.id).is_err()
            || Uuid::parse_str(&self.item_uuid).is_err()
            || self.formats.len() > 256
            || self.files.len() > 256
            || self.tags.len() > 128
        {
            return fail("同步事件身份、版本、会话或大小无效");
        }
        if self.source_app_name.as_ref().is_some_and(|name| {
            name.trim().is_empty() || name.len() > 512 || name.chars().any(char::is_control)
        }) {
            return fail("同步来源应用名称无效");
        }
        let mut total = 0usize;
        let mut identities = std::collections::HashSet::new();
        let mut resources = std::collections::HashSet::new();
        for f in &self.formats {
            if f.namespace.is_empty()
                || f.namespace.len() > 128
                || f.key.is_empty()
                || f.key.len() > 512
                || f.name.is_empty()
                || f.name.len() > 512
                || f.namespace.chars().any(char::is_control)
                || f.key.chars().any(char::is_control)
                || f.name.chars().any(char::is_control)
                || !identities.insert((&f.namespace, &f.key))
            {
                return fail("同步格式身份重复或无效");
            }
        }
        for resource in self
            .formats
            .iter()
            .map(|f| &f.resource)
            .chain(self.files.iter().map(|f| &f.resource))
        {
            if resource.length > MAX_EVENT_BYTES
                || resource.chunks != resource.length.div_ceil(super::config::CHUNK_BYTES).max(1)
                || Uuid::parse_str(&resource.id).is_err()
                || !resources.insert(&resource.id)
            {
                return fail("资源清单无效");
            }
            total = total
                .checked_add(resource.length)
                .ok_or_else(|| super::Error::Message("资源总量溢出".into()))?;
        }
        if total > MAX_EVENT_BYTES || self.note.as_ref().is_some_and(|n| n.len() > 1024 * 1024) {
            return fail("同步事件超过安全上限");
        }
        Ok(())
    }
    pub fn raw_size(&self) -> usize {
        self.formats
            .iter()
            .map(|f| f.resource.length)
            .chain(self.files.iter().map(|f| f.resource.length))
            .sum()
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub enum Request {
    Hello {
        challenge: String,
        profile: Profile,
    },
    Begin(Event),
    Chunk {
        event: String,
        session: String,
        resource: String,
        index: usize,
        #[serde(with = "bytes")]
        data: Vec<u8>,
    },
    Commit {
        event: String,
        session: String,
    },
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Reply {
    pub request: String,
    pub session: Option<Session>,
    pub accepted: bool,
    pub applied: bool,
    pub message: String,
}

pub fn request_id(request: &Request) -> String {
    match request {
        Request::Hello { challenge, .. } => challenge.clone(),
        Request::Begin(event) => event.id.clone(),
        Request::Chunk {
            event,
            resource,
            index,
            ..
        } => format!("{event}/{resource}/{index}"),
        Request::Commit { event, .. } => event.clone(),
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;

    #[test]
    fn cache_envelopes_survive_long_offline_periods_but_online_frames_expire() {
        let (a, b) = super::super::tests::paired();
        let peer = &a.peers[0];
        for purpose in ["online", "webdav-cache-entry-v1"] {
            let bound = BoundMessage {
                version: VERSION,
                sender: a.profile.id.clone(),
                recipient: peer.profile.id.clone(),
                sent_at: now() - 3600,
                purpose: purpose.into(),
                payload: "离线一小时后拉取",
            };
            let cipher = SalsaBox::new(
                &PublicKey::from(peer.profile.key),
                &SecretKey::from(a.secret),
            );
            let nonce = SalsaBox::generate_nonce(&mut OsRng);
            let envelope = Envelope {
                sender: route(&a.profile.key, &peer.profile.key),
                nonce: nonce.into(),
                ciphertext: cipher
                    .encrypt(&nonce, serde_json::to_vec(&bound).unwrap().as_slice())
                    .unwrap(),
            };
            if purpose == "online" {
                assert!(open::<String>(&b, &envelope).is_err());
            } else {
                assert_eq!(
                    open_cache::<String>(&b, &envelope, purpose).unwrap().1,
                    "离线一小时后拉取"
                );
            }
        }
    }
}
