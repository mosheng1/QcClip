use super::{
    config::{Config, ConfigStore, EVENT_TTL, PairingCode, Peer, Policy},
    protocol::{self, Event, Mode, Reply, Request, Session},
    repository::{self, Packet},
    runtime::Runtime,
};
use crate::{
    domain::capture::{CapturedClipboard, FormatKind, RawFormat, SourceApplication},
    services::{
        capture::{CONTENT_HASH_VERSION, CaptureService},
        database::Database,
        storage::RawBlobStore,
    },
};
use rusqlite::{Connection, params};
use std::{collections::HashMap, sync::Arc};
use uuid::Uuid;

pub(super) struct Fixture {
    _dir: tempfile::TempDir,
    pub db: Database,
    pub blobs: RawBlobStore,
}
impl Fixture {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("qcclip.sqlite3")).unwrap();
        let blobs = RawBlobStore::new(dir.path()).unwrap();
        Self {
            _dir: dir,
            db,
            blobs,
        }
    }
    pub fn capture(&self, text: &str) -> i64 {
        CaptureService::new(self.db.clone(), self.blobs.clone())
            .store_captured_clipboard_with_status(&text_capture(text))
            .unwrap()
            .item_id
    }
    pub fn count(&self, table: &str) -> i64 {
        self.db
            .connect()
            .unwrap()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
    pub fn packet(&self, item: i64, mode: Mode, sequence: u64) -> Packet {
        self.packet_with_policy(item, mode, sequence, &Policy::default())
    }
    pub fn packet_with_policy(
        &self,
        item: i64,
        mode: Mode,
        sequence: u64,
        policy: &Policy,
    ) -> Packet {
        repository::build(
            &self.db,
            &self.blobs,
            item,
            Event {
                id: Uuid::new_v4().to_string(),
                sender: Uuid::new_v4().to_string(),
                recipient: Uuid::new_v4().to_string(),
                session: Uuid::new_v4().to_string(),
                sequence,
                expires: protocol::now() + EVENT_TTL,
                item_uuid: String::new(),
                mode,
                hash_version: CONTENT_HASH_VERSION,
                source_hash: [0; 32],
                complete: false,
                source_app_name: None,
                formats: Vec::new(),
                files: Vec::new(),
                tags: Vec::new(),
                note: None,
                primary: None,
            },
            policy,
        )
        .unwrap()
    }
}

fn text_capture(text: &str) -> CapturedClipboard {
    CapturedClipboard {
        formats: vec![RawFormat::captured(
            "windows",
            "CF_UNICODETEXT",
            "CF_UNICODETEXT",
            FormatKind::Text,
            text.encode_utf16()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect(),
            0,
            true,
        )],
        primary_format_identity: None,
        preview_candidate: None,
        source_application: SourceApplication::default(),
    }
}

pub(super) fn paired() -> (Config, Config) {
    let mut a = Config::fresh();
    let mut b = Config::fresh();
    a.enabled = true;
    b.enabled = true;
    a.peers.push(Peer {
        profile: b.profile.clone(),
        address: String::new(),
        policy: Policy::default(),
    });
    b.peers.push(Peer {
        profile: a.profile.clone(),
        address: String::new(),
        policy: Policy::default(),
    });
    (a, b)
}

pub(super) fn runtime(f: &Fixture, config: Config) -> Runtime {
    Runtime::for_test(
        config,
        f.db.clone(),
        f.blobs.clone(),
        ConfigStore::at(f._dir.path().join("identity.bin")),
    )
}

fn rpc(runtime: &Runtime, sender: &Config, request: &Request) -> Reply {
    let envelope = protocol::seal(sender, &sender.peers[0], request).unwrap();
    let reply = runtime.receive_lan(&envelope).unwrap();
    protocol::open::<Reply>(sender, &reply).unwrap().1
}

fn handshake(runtime: &Runtime, sender: &Config) -> Session {
    let challenge = Uuid::new_v4().to_string();
    let reply = rpc(
        runtime,
        sender,
        &Request::Hello {
            challenge: challenge.clone(),
            profile: sender.profile.clone(),
        },
    );
    assert!(reply.accepted);
    assert_eq!(reply.request, challenge);
    reply.session.unwrap()
}

#[test]
fn channels_are_independent_and_disabled_channels_reject_cached_sessions() {
    let (mut a, b) = paired();
    let session = Session {
        id: Uuid::new_v4().to_string(),
        expires: protocol::now() + 30,
        profile: b.profile.clone(),
    };
    let status = super::runtime::PeerStatus {
        lan: Some(session.clone()),
        ..Default::default()
    };
    a.dav.enabled = true;
    a.dav.url = "https://dav.example.com".into();
    assert_eq!(status.channel(&a, &a.peers[0]), Some("LAN"));
    a.lan_enabled = false;
    a.port = 0;
    a.peers[0].address = "旧的无效局域网地址".into();
    assert!(a.validate().is_ok());
    assert_eq!(status.channel(&a, &a.peers[0]), Some("WebDAV"));
    let f = Fixture::new();
    let dav_only = runtime(&f, a.clone());
    let hello = protocol::seal(
        &b,
        &b.peers[0],
        &Request::Hello {
            challenge: "独立通道".into(),
            profile: b.profile.clone(),
        },
    )
    .unwrap();
    assert!(dav_only.receive_lan(&hello).is_err());
    a.dav.enabled = false;
    a.dav.url = "未配置 WebDAV".into();
    a.dav.poll_seconds = u64::MAX;
    assert_eq!(status.channel(&a, &a.peers[0]), None);
    a.lan_enabled = true;
    a.port = super::config::PORT;
    a.peers[0].address.clear();
    assert!(a.validate().is_ok());
    assert_eq!(status.channel(&a, &a.peers[0]), Some("LAN"));
    let lan_only = runtime(&f, a.clone());
    assert!(lan_only.receive_lan(&hello).is_ok());
    a.enabled = false;
    assert_eq!(status.channel(&a, &a.peers[0]), None);
}

fn deliver(runtime: &Runtime, sender: &Config, packet: &Packet) -> Reply {
    let begin = rpc(runtime, sender, &Request::Begin(packet.event.clone()));
    if !begin.accepted {
        return begin;
    }
    for (id, bytes) in &packet.resources {
        let count = bytes.len().div_ceil(super::config::CHUNK_BYTES).max(1);
        for index in 0..count {
            let start = index * super::config::CHUNK_BYTES;
            let end = (start + super::config::CHUNK_BYTES).min(bytes.len());
            let reply = rpc(
                runtime,
                sender,
                &Request::Chunk {
                    event: packet.event.id.clone(),
                    session: packet.event.session.clone(),
                    resource: id.clone(),
                    index,
                    data: bytes[start..end].to_vec(),
                },
            );
            if !reply.accepted {
                return reply;
            }
        }
    }
    rpc(
        runtime,
        sender,
        &Request::Commit {
            event: packet.event.id.clone(),
            session: packet.event.session.clone(),
        },
    )
}

#[test]
fn ciphertext_authenticates_sender_target_and_contents() {
    let (a, b) = paired();
    let text = "不可出现在网络中的私密剪贴板";
    let mut envelope = protocol::seal(&a, &a.peers[0], &text).unwrap();
    assert!(!serde_json::to_string(&envelope).unwrap().contains(text));
    assert_eq!(protocol::open::<String>(&b, &envelope).unwrap().1, text);
    assert!(protocol::open::<String>(&a, &envelope).is_err());
    envelope.ciphertext[0] ^= 1;
    assert!(protocol::open::<String>(&b, &envelope).is_err());
    let envelope = protocol::seal(&a, &a.peers[0], &text).unwrap();
    let mut unpaired = b.clone();
    unpaired.peers.clear();
    assert!(protocol::open::<String>(&unpaired, &envelope).is_err());
    let mut blocked = b.clone();
    blocked.peers[0].policy.enabled = false;
    assert!(protocol::open::<String>(&blocked, &envelope).is_err());
    let mut wrong_target = b.clone();
    wrong_target.profile.id = Uuid::new_v4().to_string();
    assert!(protocol::open::<String>(&wrong_target, &envelope).is_err());
}

#[test]
fn randomized_envelopes_and_pair_specific_keys() {
    let (a, b) = paired();
    let (_, c) = paired();
    let first = protocol::seal(&a, &a.peers[0], &"相同消息").unwrap();
    let second = protocol::seal(&a, &a.peers[0], &"相同消息").unwrap();
    assert_ne!(first.nonce, second.nonce);
    assert_ne!(first.ciphertext, second.ciphertext);
    let mut impostor = b.clone();
    impostor.secret = c.secret;
    assert!(protocol::open::<String>(&impostor, &first).is_err());
}

#[test]
fn protocol_rejects_expired_wrong_session_and_unknown_hash_version() {
    let f = Fixture::new();
    let item = f.capture("会话校验");
    let packet = f.packet(item, Mode::NewContent, 1);
    let event = &packet.event;
    assert!(
        event
            .validate(&event.sender, &event.recipient, &event.session)
            .is_ok()
    );
    assert!(
        event
            .validate(&event.sender, &event.recipient, "旧会话")
            .is_err()
    );
    let mut bad = event.clone();
    bad.expires = protocol::now() - 1;
    assert!(
        bad.validate(&event.sender, &event.recipient, &event.session)
            .is_err()
    );
    bad = event.clone();
    bad.hash_version += 1;
    assert!(
        bad.validate(&event.sender, &event.recipient, &event.session)
            .is_err()
    );
}

#[test]
fn remote_ingestion_is_atomic_and_does_not_increment_local_statistics() {
    let source = Fixture::new();
    let target = Fixture::new();
    let item = source.capture("远程内容");
    let packet = source.packet(item, Mode::NewContent, 1);
    assert!(
        repository::apply(&target.db, &target.blobs, &packet.event, &packet.resources).unwrap()
    );
    let c = target.db.connect().unwrap();
    let state: (String, i64, Option<String>, Option<String>) = c.query_row("SELECT i.origin_kind, h.capture_count, h.last_source_app_name, h.last_source_app_path FROM clipboard_items i JOIN clipboard_history h ON h.item_id = i.id", [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap();
    assert_eq!(state, ("synced".into(), 0, None, None));
    assert!(
        !repository::apply(&target.db, &target.blobs, &packet.event, &packet.resources).unwrap()
    );
    assert_eq!(target.count("clipboard_history"), 1);
}

#[test]
fn content_dedup_preserves_local_source_and_capture_count() {
    let source = Fixture::new();
    let target = Fixture::new();
    let local = target.capture("完全相同");
    target.db.connect().unwrap().execute("UPDATE clipboard_history SET last_source_app_name = '本机程序', last_source_app_path = 'local.exe', capture_count = 7 WHERE item_id = ?1", [local]).unwrap();
    let packet = source.packet(source.capture("完全相同"), Mode::NewContent, 1);
    repository::apply(&target.db, &target.blobs, &packet.event, &packet.resources).unwrap();
    assert_eq!(target.count("clipboard_items"), 1);
    let result: (i64, String, String) = target.db.connect().unwrap().query_row("SELECT capture_count, last_source_app_name, last_source_app_path FROM clipboard_history", [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
    assert_eq!(result, (7, "本机程序".into(), "local.exe".into()));
}

#[test]
fn watermarks_are_per_item_and_per_stream() {
    let source = Fixture::new();
    let target = Fixture::new();
    let first = source.packet(source.capture("条目一"), Mode::NewContent, 20);
    let mut second = source.packet(source.capture("条目二"), Mode::NewContent, 2);
    second.event.sender = first.event.sender.clone();
    repository::apply(&target.db, &target.blobs, &first.event, &first.resources).unwrap();
    repository::apply(&target.db, &target.blobs, &second.event, &second.resources).unwrap();
    assert_eq!(target.count("clipboard_history"), 2);
    let mut touch = first.event.clone();
    touch.id = Uuid::new_v4().to_string();
    touch.mode = Mode::Touch;
    touch.sequence = 25;
    touch.formats.clear();
    assert!(repository::apply(&target.db, &target.blobs, &touch, &HashMap::new()).unwrap());
    touch.id = Uuid::new_v4().to_string();
    touch.sequence = 24;
    assert!(!repository::apply(&target.db, &target.blobs, &touch, &HashMap::new()).unwrap());
    let mut content = first.event.clone();
    content.id = Uuid::new_v4().to_string();
    content.sequence = 21;
    assert!(repository::apply(&target.db, &target.blobs, &content, &first.resources).unwrap());
}

#[test]
fn touch_does_not_create_missing_content_or_favorite() {
    let source = Fixture::new();
    let target = Fixture::new();
    let packet = source.packet(source.capture("尚未发送的条目"), Mode::Touch, 1);
    assert!(
        !repository::apply(&target.db, &target.blobs, &packet.event, &packet.resources).unwrap()
    );
    assert_eq!(target.count("clipboard_items"), 0);
    assert_eq!(target.count("clipboard_history"), 0);
    assert_eq!(target.count("favorites"), 0);
}

#[test]
fn favorite_only_creates_content_without_history_and_keeps_local_tags() {
    let source = Fixture::new();
    let target = Fixture::new();
    let item = source.capture("仅收藏");
    source.db.connect().unwrap().execute("INSERT INTO clipboard_item_notes (uuid, item_id, note_text, created_at, updated_at) VALUES (?1, ?2, '收藏备注', 1, 1)", params![Uuid::new_v4().to_string(), item]).unwrap();
    let mut packet = source.packet(item, Mode::Favorite, 1);
    let tag_uuid = Uuid::new_v4().to_string();
    packet.event.tags.push(protocol::WireTag {
        uuid: tag_uuid.clone(),
        name: "工作".into(),
        color: "#123456".into(),
    });
    target.db.connect().unwrap().execute("INSERT INTO tags (uuid, name, normalized_name, color, sort_order, created_at, updated_at) VALUES (?1, '本地名称', '本地名称', '#FFFFFF', 0, 1, 1)", [&tag_uuid]).unwrap();
    repository::apply(&target.db, &target.blobs, &packet.event, &packet.resources).unwrap();
    assert_eq!(target.count("clipboard_history"), 0);
    assert_eq!(target.count("favorites"), 1);
    let name: String = target
        .db
        .connect()
        .unwrap()
        .query_row("SELECT name FROM tags", [], |r| r.get(0))
        .unwrap();
    assert_eq!(name, "本地名称");
    assert_eq!(target.count("clipboard_item_notes"), 1);
}

#[test]
fn corrupted_resources_do_not_create_visible_or_pending_items() {
    let source = Fixture::new();
    let target = Fixture::new();
    let mut packet = source.packet(source.capture("哈希校验"), Mode::NewContent, 1);
    packet.resources.values_mut().next().unwrap()[0] ^= 1;
    assert!(
        repository::apply(&target.db, &target.blobs, &packet.event, &packet.resources).is_err()
    );
    assert_eq!(target.count("clipboard_items"), 0);
    assert_eq!(target.count("sync_receipts"), 0);
}

#[test]
fn partial_formats_keep_local_hash_separate_from_source_hash() {
    let source = Fixture::new();
    let target = Fixture::new();
    let mut capture = text_capture("部分格式");
    capture.formats.push(RawFormat::captured(
        "windows",
        "registered:HTML Format",
        "HTML Format",
        FormatKind::Html,
        b"<b>partial</b>".to_vec(),
        1,
        true,
    ));
    let item = CaptureService::new(source.db.clone(), source.blobs.clone())
        .store_captured_clipboard_with_status(&capture)
        .unwrap()
        .item_id;
    let mut packet = source.packet(item, Mode::NewContent, 1);
    let removed = packet.event.formats.pop().unwrap();
    packet.resources.remove(&removed.resource.id);
    packet.event.complete = false;
    repository::apply(&target.db, &target.blobs, &packet.event, &packet.resources).unwrap();
    let (local, remote): (Vec<u8>, Vec<u8>) = target.db.connect().unwrap().query_row("SELECT i.content_hash, s.source_hash FROM clipboard_items i JOIN sync_items s ON s.item_id = i.id", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_ne!(local, remote);
    assert_eq!(remote, packet.event.source_hash);
}

#[test]
fn manual_update_keeps_unsent_formats_and_rejects_local_conflicts_and_old_events() {
    let source = Fixture::new();
    let target = Fixture::new();
    let mut capture = text_capture("原始文本");
    capture.formats.push(RawFormat::captured(
        "windows",
        "registered:HTML Format",
        "HTML Format",
        FormatKind::Html,
        b"<b>original</b>".to_vec(),
        1,
        true,
    ));
    let item = CaptureService::new(source.db.clone(), source.blobs.clone())
        .store_captured_clipboard_with_status(&capture)
        .unwrap()
        .item_id;
    let first = source.packet(item, Mode::NewContent, 1);
    repository::apply(&target.db, &target.blobs, &first.event, &first.resources).unwrap();
    let target_id: i64 = target
        .db
        .connect()
        .unwrap()
        .query_row("SELECT id FROM clipboard_items", [], |r| r.get(0))
        .unwrap();
    let mut update = source.packet(source.capture("更新文本"), Mode::ManualContent, 3);
    update.event.sender = first.event.sender.clone();
    update.event.item_uuid = first.event.item_uuid.clone();
    update.event.complete = false;
    repository::apply(&target.db, &target.blobs, &update.event, &update.resources).unwrap();
    assert_eq!(target.count("clipboard_items"), 1);
    assert_eq!(target.count("clipboard_formats"), 2);
    let mut older = update.event.clone();
    older.id = Uuid::new_v4().to_string();
    older.sequence = 2;
    assert!(!repository::apply(&target.db, &target.blobs, &older, &update.resources).unwrap());
    target
        .db
        .connect()
        .unwrap()
        .execute(
            "UPDATE clipboard_items SET content_hash = zeroblob(32) WHERE id = ?1",
            [target_id],
        )
        .unwrap();
    // 以数据库中记录的本地哈希检测修改，不依赖系统时间。
    let mut conflict = update.event.clone();
    conflict.id = Uuid::new_v4().to_string();
    conflict.sequence = 4;
    assert!(repository::apply(&target.db, &target.blobs, &conflict, &update.resources).is_err());
}

#[test]
fn receiving_opaque_foreign_formats_does_not_create_ready_item() {
    let source = Fixture::new();
    let target = Fixture::new();
    let mut packet = source.packet(source.capture("无恢复能力"), Mode::NewContent, 1);
    packet.event.formats[0].namespace = "foreign-platform".into();
    packet.event.complete = false;
    assert!(
        repository::apply(&target.db, &target.blobs, &packet.event, &packet.resources).is_err()
    );
    assert_eq!(target.count("clipboard_items"), 0);
}

#[test]
fn encrypted_pipeline_is_idempotent_and_rejects_revocation_and_old_sessions() {
    let source = Fixture::new();
    let target = Fixture::new();
    let (a, b) = paired();
    let receiver = runtime(&target, b);
    let session = handshake(&receiver, &a);
    let mut packet = source.packet(source.capture("端到端链路"), Mode::NewContent, 1);
    packet.event.sender = a.profile.id.clone();
    packet.event.recipient = receiver.config().profile.id;
    packet.event.session = session.id;
    let first = deliver(&receiver, &a, &packet);
    assert!(first.accepted && first.applied, "{}", first.message);
    let origin: (String, String) = target
        .db
        .connect()
        .unwrap()
        .query_row(
            "SELECT device_id, device_name FROM sync_sources",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(origin, (a.profile.id.clone(), a.profile.name.clone()));
    let second = deliver(&receiver, &a, &packet);
    assert!(second.accepted && !second.applied, "{}", second.message);
    assert_eq!(target.count("clipboard_history"), 1);
    receiver
        .change(
            |c| {
                c.profile.name = "改名".into();
                Ok(())
            },
            true,
        )
        .unwrap();
    let reply = deliver(&receiver, &a, &packet);
    assert!(!reply.accepted);
    receiver.revoke(&a.profile.id).unwrap();
    let envelope = protocol::seal(&a, &a.peers[0], &Request::Begin(packet.event)).unwrap();
    assert!(receiver.receive_lan(&envelope).is_err());
}

#[test]
fn lan_frames_transmit_encrypted_authenticated_handshake() {
    let target = Fixture::new();
    let (mut a, b) = paired();
    let receiver = Arc::new(runtime(&target, b));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    a.peers[0].address = address.to_string();
    let server = std::thread::spawn(move || {
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(2))
                }
                Err(error) => panic!("测试接收连接失败：{error}"),
            }
        };
        super::lan::prepare(&stream).unwrap();
        let envelope = super::lan::read(&mut stream).unwrap();
        let reply = receiver.receive_lan(&envelope).unwrap();
        super::lan::write(&mut stream, &reply).unwrap();
    });
    let request = Request::Hello {
        challenge: "挑战随机数".into(),
        profile: a.profile.clone(),
    };
    let envelope = protocol::seal(&a, &a.peers[0], &request).unwrap();
    let response = super::lan::exchange(address, &envelope).unwrap();
    let reply = protocol::open::<Reply>(&a, &response).unwrap().1;
    assert_eq!(reply.request, "挑战随机数");
    assert!(reply.accepted && reply.session.is_some());
    server.join().unwrap();
}

#[test]
fn virtual_files_internal_formats_and_unselected_roles_are_excluded() {
    let mut policy = Policy::default();
    let mut format = text_capture("格式策略").formats.remove(0);
    assert!(repository::allowed(&format, &policy));
    policy.text = false;
    assert!(!repository::allowed(&format, &policy));
    policy.text = true;
    format.format_key = "registered:FileContents".into();
    assert!(!repository::allowed(&format, &policy));
    format.format_key = "registered:Chromium internal".into();
    format.format_name = "Chromium internal".into();
    assert!(!repository::allowed(&format, &policy));
    format.format_key = "CF_HDROP".into();
    format.format_name = "CF_HDROP".into();
    format.format_kind = FormatKind::FileList;
    assert!(!repository::allowed(&format, &policy));
}

#[test]
fn pairing_codes_validate_identity_and_bound_input() {
    let config = Config::fresh();
    let code = PairingCode {
        profile: config.profile.clone(),
        address: "192.168.1.10:39827".into(),
    };
    let encoded = code.encode().unwrap();
    let decoded = PairingCode::decode(&encoded).unwrap();
    assert_eq!(decoded.profile, config.profile);
    assert_eq!(decoded.fingerprint(), code.fingerprint());
    assert!(PairingCode::decode(&"x".repeat(8192)).is_err());
    let mut invalid = code;
    invalid.profile.name = "不安全\n名称".into();
    assert!(PairingCode::decode(&invalid.encode().unwrap()).is_err());
}

#[test]
fn webdav_connection_requires_valid_dav_xml() {
    for xml in [
        "<d:multistatus xmlns:d='DAV:'><d:response/></d:multistatus>",
        "<multistatus xmlns='DAV:'/>",
    ] {
        super::webdav::validate_collection(xml).unwrap();
    }
    for xml in [
        "<html>登录页面</html>",
        "<multistatus/>",
        "<d:multistatus xmlns:d='DAV:'>",
        "<!DOCTYPE x [<!ENTITY e SYSTEM 'file:///secret'>]><x>&e;</x>",
    ] {
        assert!(super::webdav::validate_collection(xml).is_err());
    }
}

#[test]
fn webdav_accepts_http_and_https_addresses_with_valid_urls() {
    let mut config = Config::fresh();
    config.dav.enabled = true;
    for address in [
        "http://192.168.1.3:8888/QcClipSync",
        "http://127.0.0.1:8888/",
        "http://localhost:8888/",
        "http://[::1]:8888/",
        "http://[fd00::1]:8888/",
        "http://dav.example.com/sync",
        "http://8.8.8.8:8888/",
        "http://192.168.1.3.example.com/",
        "https://dav.jianguoyun.com/dav/QcClipSync/",
    ] {
        config.dav.url = address.into();
        assert!(config.validate().is_ok(), "应接受测试地址");
        assert!(super::webdav::Client::new(&config.dav).is_ok());
    }
    for address in [
        "http://user:password@127.0.0.1/",
        "https://dav.example.com/sync?token=secret",
        "https://dav.example.com/sync#fragment",
        "ftp://dav.example.com/sync",
        "file:///C:/QcClipSync",
        "C:\\CYRJ\\QcClip\\QcClipSync",
    ] {
        config.dav.url = address.into();
        assert!(config.validate().is_err(), "应拒绝无效地址");
        assert!(super::webdav::Client::new(&config.dav).is_err());
    }
}

#[test]
fn released_database_migrates_preserving_history_and_allows_zero_local_captures() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v1.sqlite3");
    let connection = Connection::open(&path).unwrap();
    let schema = include_str!("../database/schema.sql")
        .replace("CHECK(capture_count >= 0)", "CHECK(capture_count >= 1)");
    connection.execute_batch(&schema).unwrap();
    connection.execute("INSERT INTO clipboard_items (id, uuid, hash_version, content_hash, content_kind, created_at, updated_at) VALUES (1, ?1, 2, zeroblob(32), 'text', 1, 1)", [Uuid::new_v4().to_string()]).unwrap();
    connection.execute("INSERT INTO clipboard_history (id, uuid, item_id, first_seen_at, last_seen_at, capture_count, is_pinned, sort_order, last_source_app_name, updated_at) VALUES (1, ?1, 1, 1, 5, 7, 1, 1000000, '原有程序', 5)", [Uuid::new_v4().to_string()]).unwrap();
    connection
        .execute_batch("PRAGMA user_version = 1;")
        .unwrap();
    drop(connection);
    let db = Database::open(&path).unwrap();
    let c = db.connect().unwrap();
    let version: i64 = c
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 2);
    let count: i64 = c.query_row("SELECT COUNT(*) FROM sqlite_master WHERE name IN ('sync_items', 'sync_watermarks', 'sync_receipts')", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 3);
    let preserved: (i64, i64, i64, String) = c.query_row("SELECT capture_count, is_pinned, sort_order, last_source_app_name FROM clipboard_history WHERE id = 1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap();
    assert_eq!(preserved, (7, 1, 1000000, "原有程序".into()));
    c.execute(
        "UPDATE clipboard_history SET capture_count = 0 WHERE id = 1",
        [],
    )
    .unwrap();
}

#[cfg(target_os = "windows")]
#[test]
fn device_credentials_use_dpapi_and_survive_reload() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sync.bin");
    let store = ConfigStore::at(path.clone());
    let mut config = store.load().unwrap();
    config.sequence = 123;
    config.dav.password = "私密测试凭据".into();
    store.save(&config).unwrap();
    let stored = std::fs::read(&path).unwrap();
    assert!(
        !stored
            .windows(config.dav.password.len())
            .any(|w| w == config.dav.password.as_bytes())
    );
    let restored = store.load().unwrap();
    assert_eq!(restored.sequence, 123);
    assert_eq!(restored.profile, config.profile);
    assert_eq!(restored.secret, config.secret);
    assert_eq!(restored.dav.password, config.dav.password);
    assert_eq!(std::fs::read(&path).unwrap(), stored);
    config.lan_enabled = false;
    store.save(&config).unwrap();
    assert!(!store.load().unwrap().lan_enabled);
    let invalid = "损坏的身份文件".as_bytes();
    std::fs::write(&path, invalid).unwrap();
    assert!(store.load().is_err());
    assert_eq!(std::fs::read(&path).unwrap(), invalid);
}

#[test]
fn file_contents_roundtrip_survives_data_directory_move_and_detects_tampering() {
    let source = Fixture::new();
    let target = Fixture::new();
    let original = source._dir.path().join("真实文件.txt");
    std::fs::write(&original, "文件实际内容").unwrap();
    let captured = CapturedClipboard {
        formats: vec![RawFormat::captured(
            "windows",
            "CF_HDROP",
            "CF_HDROP",
            FormatKind::FileList,
            crate::platform::image_temp_file::hdrop_bytes_for_paths(std::slice::from_ref(
                &original,
            )),
            0,
            true,
        )],
        primary_format_identity: None,
        preview_candidate: None,
        source_application: SourceApplication::default(),
    };
    let item = CaptureService::new(source.db.clone(), source.blobs.clone())
        .store_captured_clipboard_with_status(&captured)
        .unwrap()
        .item_id;
    let policy = Policy {
        file_contents: true,
        ..Default::default()
    };
    let packet = source.packet_with_policy(item, Mode::ManualContent, 1, &policy);
    assert_eq!(packet.event.files.len(), 1);
    assert!(!packet.event.complete);
    repository::apply(&target.db, &target.blobs, &packet.event, &packet.resources).unwrap();
    let id: i64 = target
        .db
        .connect()
        .unwrap()
        .query_row("SELECT id FROM clipboard_items", [], |r| r.get(0))
        .unwrap();
    let files = repository::managed_file_list(
        &target.db.connect().unwrap(),
        &target.blobs,
        target._dir.path(),
        id,
    )
    .unwrap()
    .unwrap();
    assert_eq!(std::fs::read_to_string(&files[0]).unwrap(), "文件实际内容");
    assert_ne!(files[0], original);
    let relocated = tempfile::tempdir().unwrap();
    target
        .db
        .connect()
        .unwrap()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    std::fs::copy(target.db.path(), relocated.path().join("qcclip.sqlite3")).unwrap();
    std::fs::rename(
        target._dir.path().join("data"),
        relocated.path().join("data"),
    )
    .unwrap();
    let moved_db = Database::open(relocated.path().join("qcclip.sqlite3")).unwrap();
    let moved_blobs = RawBlobStore::new(relocated.path()).unwrap();
    let files = repository::managed_file_list(
        &moved_db.connect().unwrap(),
        &moved_blobs,
        relocated.path(),
        id,
    )
    .unwrap()
    .unwrap();
    assert!(files[0].starts_with(relocated.path()));
    assert_eq!(std::fs::read_to_string(&files[0]).unwrap(), "文件实际内容");
    repository::cleanup_managed_files_after(&moved_db, &moved_blobs, std::time::Duration::ZERO)
        .unwrap();
    assert!(files[0].exists());
    std::fs::write(&files[0], "文件已被修改").unwrap();
    assert!(
        repository::managed_file_list(
            &moved_db.connect().unwrap(),
            &moved_blobs,
            relocated.path(),
            id
        )
        .is_err()
    );
    moved_db
        .connect()
        .unwrap()
        .execute("DELETE FROM clipboard_history", [])
        .unwrap();
    moved_db
        .connect()
        .unwrap()
        .execute("DELETE FROM clipboard_items", [])
        .unwrap();
    repository::cleanup_managed_files_after(&moved_db, &moved_blobs, std::time::Duration::ZERO)
        .unwrap();
    assert!(!files[0].exists());
}

#[test]
fn malformed_or_missing_chunks_never_publish_content() {
    let source = Fixture::new();
    let target = Fixture::new();
    let (a, b) = paired();
    let receiver = runtime(&target, b);
    let session = handshake(&receiver, &a);
    let mut packet = source.packet(source.capture("分片原子提交"), Mode::NewContent, 1);
    packet.event.sender = a.profile.id.clone();
    packet.event.recipient = a.peers[0].profile.id.clone();
    packet.event.session = session.id.clone();
    assert!(rpc(&receiver, &a, &Request::Begin(packet.event.clone())).accepted);
    let resource = &packet.event.formats[0].resource;
    for (index, data) in [(resource.chunks, vec![]), (0, vec![1])] {
        assert!(
            !rpc(
                &receiver,
                &a,
                &Request::Chunk {
                    event: packet.event.id.clone(),
                    session: session.id.clone(),
                    resource: resource.id.clone(),
                    index,
                    data
                }
            )
            .accepted
        );
    }
    assert!(
        !rpc(
            &receiver,
            &a,
            &Request::Commit {
                event: packet.event.id.clone(),
                session: session.id
            }
        )
        .accepted
    );
    assert_eq!(target.count("clipboard_items"), 0);
    assert!(deliver(&receiver, &a, &packet).accepted);
    assert_eq!(target.count("clipboard_items"), 1);
}

#[test]
fn manual_favorite_without_metadata_keeps_existing_tags_and_notes() {
    let source = Fixture::new();
    let target = Fixture::new();
    let mut packet = source.packet(source.capture("收藏元数据"), Mode::Favorite, 1);
    packet.event.tags.push(protocol::WireTag {
        uuid: Uuid::new_v4().to_string(),
        name: "旧标签".into(),
        color: "#123456".into(),
    });
    packet.event.note = Some("旧备注".into());
    repository::apply(&target.db, &target.blobs, &packet.event, &packet.resources).unwrap();
    packet.event.id = Uuid::new_v4().to_string();
    packet.event.sequence = 2;
    packet.event.mode = Mode::ManualFavorite;
    packet.event.tags.clear();
    packet.event.note = None;
    repository::apply(&target.db, &target.blobs, &packet.event, &packet.resources).unwrap();
    assert_eq!(target.count("clipboard_item_tags"), 1);
    assert_eq!(target.count("clipboard_item_notes"), 1);
    assert_eq!(target.count("favorites"), 1);
    assert_eq!(target.count("clipboard_history"), 0);
}

#[test]
fn automatic_repeated_content_cannot_clear_local_conflict_detection() {
    let source = Fixture::new();
    let target = Fixture::new();
    let mut packet = source.packet(source.capture("本地冲突"), Mode::NewContent, 1);
    repository::apply(&target.db, &target.blobs, &packet.event, &packet.resources).unwrap();
    target
        .db
        .connect()
        .unwrap()
        .execute("UPDATE clipboard_items SET content_hash = zeroblob(32)", [])
        .unwrap();
    packet.event.id = Uuid::new_v4().to_string();
    packet.event.sequence = 2;
    repository::apply(&target.db, &target.blobs, &packet.event, &packet.resources).unwrap();
    packet.event.id = Uuid::new_v4().to_string();
    packet.event.sequence = 3;
    packet.event.mode = Mode::ManualContent;
    assert!(
        repository::apply(&target.db, &target.blobs, &packet.event, &packet.resources).is_err()
    );
}

#[test]
fn resources_over_hard_limit_are_rejected_before_blob_loading() {
    let source = Fixture::new();
    let item = source.capture("限制预检查");
    let c = source.db.connect().unwrap();
    c.execute("INSERT INTO binary_blobs (id, uuid, sha256, byte_length, relative_path, storage_state, created_at) VALUES (100, ?1, zeroblob(32), 268435457, '不存在的资源', 'ready', 1)", [Uuid::new_v4().to_string()]).unwrap();
    c.execute("UPDATE clipboard_formats SET storage_kind = 'external', inline_data = NULL, blob_id = 100, byte_length = 268435457 WHERE item_id = ?1", [item]).unwrap();
    let error = repository::load_formats(&c, &source.blobs, item)
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("超过事件上限"), "{error}");
}

#[test]
fn image_roundtrip_validates_pixels_and_rejects_corrupt_payloads() {
    let source = Fixture::new();
    let target = Fixture::new();
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgba8(2, 2)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let captured = CapturedClipboard {
        formats: vec![RawFormat::captured(
            "windows",
            "registered:PNG",
            "PNG",
            FormatKind::Image,
            png.into_inner(),
            0,
            true,
        )],
        primary_format_identity: None,
        preview_candidate: None,
        source_application: SourceApplication::default(),
    };
    let item = CaptureService::new(source.db.clone(), source.blobs.clone())
        .store_captured_clipboard_with_status(&captured)
        .unwrap()
        .item_id;
    let mut packet = source.packet(item, Mode::NewContent, 1);
    repository::apply(&target.db, &target.blobs, &packet.event, &packet.resources).unwrap();
    assert_eq!(target.count("clipboard_items"), 1);
    let bad_target = Fixture::new();
    let resource = &mut packet.event.formats[0].resource;
    let bytes = "损坏的图片".as_bytes().to_vec();
    resource.length = bytes.len();
    resource.hash = protocol::hash(&bytes);
    resource.chunks = 1;
    packet.resources.insert(resource.id.clone(), bytes);
    packet.event.complete = false;
    assert!(
        repository::apply(
            &bad_target.db,
            &bad_target.blobs,
            &packet.event,
            &packet.resources
        )
        .is_err()
    );
    assert_eq!(bad_target.count("clipboard_items"), 0);
}

#[test]
fn pending_large_event_does_not_block_small_text_or_cross_session_commit() {
    let source = Fixture::new();
    let target = Fixture::new();
    let (a, b) = paired();
    let receiver = runtime(&target, b);
    let session = handshake(&receiver, &a);
    let mut large = source.packet(source.capture(&"大资源".repeat(50000)), Mode::NewContent, 1);
    large.event.sender = a.profile.id.clone();
    large.event.recipient = a.peers[0].profile.id.clone();
    large.event.session = session.id.clone();
    assert!(rpc(&receiver, &a, &Request::Begin(large.event.clone())).accepted);
    let mut small = source.packet(source.capture("优先小文本"), Mode::NewContent, 2);
    small.event.sender = a.profile.id.clone();
    small.event.recipient = a.peers[0].profile.id.clone();
    small.event.session = session.id;
    assert!(deliver(&receiver, &a, &small).accepted);
    assert_eq!(target.count("clipboard_items"), 1);
    receiver
        .change(
            |c| {
                c.enabled = false;
                Ok(())
            },
            true,
        )
        .unwrap();
    receiver
        .change(
            |c| {
                c.enabled = true;
                Ok(())
            },
            true,
        )
        .unwrap();
    assert!(!deliver(&receiver, &a, &large).accepted);
    assert_eq!(target.count("clipboard_items"), 1);
}
