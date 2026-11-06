use std::{
    collections::{HashMap, VecDeque},
    io,
    net::{SocketAddr, TcpListener, TcpStream, UdpSocket},
    sync::{
        Arc, Condvar, Mutex, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use super::{
    Result,
    config::{
        CHUNK_BYTES, Config, ConfigStore, EVENT_TTL, MAX_INCOMING_BYTES, MAX_INCOMING_EVENTS,
        PairingCode, Peer,
    },
    dav_cache,
    diagnostics::{self, Failure},
    fail, lan, local_pairing,
    pairing::{Candidate, Invitation, InvitationStatus},
    protocol::{self, Envelope, Event, Mode, Reply, Request, Session},
    repository::{self, Packet},
    webdav,
};
use crate::{
    platform::clipboard::ClipboardWriteSuppressor,
    services::{database::Database, paste::PasteService, storage::RawBlobStore},
};
use gpui::{App, Global};
use uuid::Uuid;

#[derive(Clone, Default)]
pub struct PeerStatus {
    pub lan: Option<Session>,
    pub result: String,
    /// 最近一次局域网握手失败的原因，连接成功后清空。
    pub lan_error: String,
    pub(super) lan_failures: u32,
}
impl PeerStatus {
    pub fn channel(&self, config: &Config, peer: &Peer) -> Option<&'static str> {
        if !config.enabled || !peer.policy.enabled {
            return None;
        }
        if config.lan_allowed(peer) && self.lan.as_ref().is_some_and(|s| s.valid(peer)) {
            Some("LAN")
        } else if config.dav_allowed(peer) {
            Some("WebDAV")
        } else {
            None
        }
    }
}

#[derive(Clone)]
pub struct Snapshot {
    pub config: Option<Config>,
    pub statuses: HashMap<String, PeerStatus>,
    pub discovered: Vec<String>,
    pub message: String,
    pub local_pairing: local_pairing::Snapshot,
    pub dav_requests: usize,
}

pub struct SyncService {
    pub runtime: Option<Arc<Runtime>>,
    error: String,
}
impl Global for SyncService {}

#[derive(Clone)]
struct DavJob {
    config: Config,
    generation: u64,
    peer: Peer,
    entry: dav_cache::Entry,
    manual: bool,
}

struct Incoming {
    event: Event,
    chunks: HashMap<String, Vec<Option<Vec<u8>>>>,
}

struct OperationNotice {
    message: String,
    failed: bool,
    informational: bool,
    /// 后台检测到、需要用户处理的问题，按“需要注意”类通知发送。
    attention: bool,
}

#[derive(Clone)]
struct Job {
    peer: String,
    item_id: i64,
    mode: Mode,
    generation: u64,
    created: Instant,
    session: String,
    sequence: u64,
    expected_hash: Vec<u8>,
    dav_only: bool,
}

#[cfg(test)]
struct TestQueues {
    outgoing: [async_channel::Receiver<Job>; 2],
    incoming: [async_channel::Receiver<DavJob>; 2],
}

/// 后台线程的唤醒信号。计数器避免在检查空闲与开始等待之间错过唤醒。
#[derive(Default)]
struct Wake {
    count: Mutex<u64>,
    signal: Condvar,
}
impl Wake {
    fn seen(&self) -> u64 {
        *self.count.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn wake(&self) {
        *self.count.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        self.signal.notify_all();
    }
    /// 活跃时按原间隔轮询；空闲时一直阻塞到配置变更、开始配对或退出时被唤醒，关闭同步后不留后台任务。
    fn pause(&self, seen: u64, idle: bool, interval: Duration) {
        if !idle {
            thread::sleep(interval);
            return;
        }
        let count = self.count.lock().unwrap_or_else(|e| e.into_inner());
        drop(self.signal.wait_while(count, |count| *count == seen));
    }
}

type ClipboardWriter =
    Box<dyn Fn(&Database, &RawBlobStore, i64) -> std::result::Result<(), String> + Send + Sync>;

pub struct Runtime {
    #[cfg(test)]
    test_queues: Option<TestQueues>,
    #[cfg(test)]
    test_agent: Mutex<Option<ureq::Agent>>,
    config: RwLock<Config>,
    store: ConfigStore,
    data: RwLock<(Database, RawBlobStore)>,
    statuses: Mutex<HashMap<String, PeerStatus>>,
    discovered: Mutex<HashMap<String, Instant>>,
    incoming: Mutex<HashMap<String, Incoming>>,
    session: Mutex<(String, Instant)>,
    generation: AtomicU64,
    stop: AtomicBool,
    control: async_channel::Sender<Job>,
    dav_control: async_channel::Sender<DavJob>,
    dav_bulk: async_channel::Sender<DavJob>,
    downloading: Mutex<std::collections::HashSet<String>>,
    bulk: async_channel::Sender<Job>,
    changed: async_channel::Sender<()>,
    message: Mutex<String>,
    invitation: Mutex<Option<(Invitation, super::config::DavConfig)>>,
    notices: Mutex<VecDeque<OperationNotice>>,
    received_changes: Mutex<HashMap<i64, super::ReceivedChange>>,
    clipboard_writer: Mutex<ClipboardWriter>,
    pub(super) local_pairing: Mutex<local_pairing::State>,
    dav_transport: Mutex<Option<webdav::Client>>,
    dav_cache: Mutex<dav_cache::Cache>,
    dav_poll: Mutex<()>,
    next_pull: Mutex<Instant>,
    dav_traffic: Arc<webdav::Traffic>,
    wake: Arc<Wake>,
    /// 每台设备的时钟偏差（对方时间减本机时间，秒），由本机发起的握手测得。
    clock_offsets: Mutex<HashMap<String, i64>>,
    diagnostics: diagnostics::Gate,
}

impl SyncService {
    pub fn install(
        database: Database,
        blobs: RawBlobStore,
        suppressor: ClipboardWriteSuppressor,
        cx: &mut App,
    ) {
        let (changed, receiver) = async_channel::bounded(1);
        let runtime = Runtime::start(database, blobs, changed, suppressor);
        let (runtime, error) = match runtime {
            Ok(runtime) => (Some(runtime), String::new()),
            Err(error) => {
                tracing::error!("初始化在线同步失败：{error}");
                (None, error.to_string())
            }
        };
        cx.set_global(Self {
            runtime: runtime.clone(),
            error,
        });
        if let Some(runtime) = runtime {
            let weak = Arc::downgrade(&runtime);
            cx.on_app_quit(move |_| {
                if let Some(runtime) = weak.upgrade() {
                    runtime.stop.store(true, Ordering::Release);
                    runtime.wake.wake();
                }
                async {}
            })
            .detach();
        }
        cx.spawn(async move |cx| {
            while receiver.recv().await.is_ok() {
                while receiver.try_recv().is_ok() {}
                cx.update(|cx| {
                    crate::windows::settings::refresh_sync(cx);
                    crate::windows::main::MainWindowService::notify_main_view(cx);
                    if let Ok(runtime) = Self::get(cx) {
                        let changes = runtime.take_received_changes();
                        if !changes.is_empty() {
                            crate::windows::main::MainWindowService::receive_sync_changes(
                                changes, cx,
                            );
                        }
                        let notices = std::mem::take(
                            &mut *runtime.notices.lock().unwrap_or_else(|e| e.into_inner()),
                        );
                        for notice in notices {
                            use crate::windows::notification::{
                                NotificationLevel, NotificationService,
                            };
                            NotificationService::show_message_for(
                                if notice.attention {
                                    crate::config::NotificationKind::Attention
                                } else {
                                    crate::config::NotificationKind::OperationResult
                                },
                                crate::i18n::translator(cx).translate("settings.page.sync"),
                                notice.message,
                                if notice.attention {
                                    NotificationLevel::Warning
                                } else if notice.failed {
                                    NotificationLevel::Error
                                } else if notice.informational {
                                    NotificationLevel::Info
                                } else {
                                    NotificationLevel::Success
                                },
                                cx,
                            );
                        }
                    }
                });
            }
        })
        .detach();
    }
    pub fn snapshot(cx: &App) -> Snapshot {
        let Some(service) = cx.try_global::<Self>() else {
            return Snapshot {
                config: None,
                statuses: HashMap::new(),
                discovered: Vec::new(),
                local_pairing: Default::default(),
                dav_requests: 0,
                message: "同步服务尚未启动".into(),
            };
        };
        if let Some(runtime) = &service.runtime {
            runtime.snapshot()
        } else {
            Snapshot {
                config: None,
                statuses: HashMap::new(),
                discovered: Vec::new(),
                local_pairing: Default::default(),
                dav_requests: 0,
                message: service.error.clone(),
            }
        }
    }
    pub fn get(cx: &App) -> Result<Arc<Runtime>> {
        cx.try_global::<Self>()
            .and_then(|s| s.runtime.clone())
            .ok_or_else(|| super::Error::Message("同步服务不可用".into()))
    }
    pub fn replace_data(database: Database, blobs: RawBlobStore, cx: &App) {
        if let Ok(runtime) = Self::get(cx) {
            let mut data = runtime.data.write().unwrap_or_else(|e| e.into_inner());
            *data = (database, blobs);
            runtime
                .received_changes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
            drop(data);
            let _ = runtime.change(|_| Ok(()), true);
        }
    }
    pub fn invalidate(cx: &App) {
        if let Ok(runtime) = Self::get(cx) {
            let _ = runtime.change(|_| Ok(()), true);
        }
    }
}

impl Runtime {
    #[cfg(test)]
    pub(super) fn for_test(
        config: Config,
        database: Database,
        blobs: RawBlobStore,
        store: ConfigStore,
    ) -> Self {
        let (control, control_rx) = async_channel::bounded(64);
        let (dav_control, dav_control_rx) = async_channel::bounded(64);
        let (dav_bulk, dav_bulk_rx) = async_channel::bounded(16);
        let (bulk, bulk_rx) = async_channel::bounded(16);
        let (changed, _) = async_channel::bounded(1);
        Self {
            test_queues: Some(TestQueues {
                outgoing: [control_rx, bulk_rx],
                incoming: [dav_control_rx, dav_bulk_rx],
            }),
            test_agent: Mutex::new(None),
            config: RwLock::new(config),
            store,
            data: RwLock::new((database, blobs)),
            statuses: Mutex::new(HashMap::new()),
            discovered: Mutex::new(HashMap::new()),
            incoming: Mutex::new(HashMap::new()),
            session: Mutex::new((Uuid::new_v4().to_string(), Instant::now())),
            generation: AtomicU64::new(1),
            stop: AtomicBool::new(false),
            control,
            dav_control,
            dav_bulk,
            downloading: Mutex::new(Default::default()),
            bulk,
            changed,
            message: Mutex::new(String::new()),
            invitation: Mutex::new(None),
            notices: Mutex::new(VecDeque::new()),
            received_changes: Mutex::new(HashMap::new()),
            clipboard_writer: Mutex::new(Box::new(|_, _, _| Ok(()))),
            local_pairing: Mutex::new(Default::default()),
            dav_transport: Mutex::new(None),
            dav_cache: Mutex::new(Default::default()),
            dav_poll: Mutex::new(()),
            next_pull: Mutex::new(Instant::now()),
            dav_traffic: Arc::new(webdav::Traffic::default()),
            wake: Arc::default(),
            clock_offsets: Mutex::new(HashMap::new()),
            diagnostics: Default::default(),
        }
    }
    fn dav_client(&self, config: &super::config::DavConfig) -> Result<webdav::Client> {
        let mut cached = self.dav_transport.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(client) = cached.as_ref().filter(|client| client.matches(config)) {
            return Ok(client.clone());
        }
        let client = webdav::Client::new(config)?.with_traffic(self.dav_traffic.clone());
        #[cfg(test)]
        let client = client.with_agent(
            self.test_agent
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        );
        *cached = Some(client.clone());
        Ok(client)
    }
    pub(crate) fn package_client(&self) -> Result<webdav::Client> {
        Ok(self.dav_client(&self.config().dav)?.for_packages())
    }
    #[cfg(test)]
    pub(super) fn test_relay_agent(&self, agent: ureq::Agent) {
        *self.test_agent.lock().unwrap() = Some(agent);
        *self.dav_transport.lock().unwrap() = None;
    }
    #[cfg(test)]
    pub(super) fn test_session(&self) -> Session {
        self.local_session(&self.config())
    }
    #[cfg(test)]
    pub(super) fn test_relay_send(
        &self,
        peer: &Peer,
        packet: &Packet,
        session: Session,
    ) -> Result<()> {
        self.set_session(&peer.profile.id, session, true);
        let status = self
            .statuses
            .lock()
            .unwrap()
            .get(&peer.profile.id)
            .cloned()
            .unwrap_or_default();
        self.send_packet(
            peer,
            packet,
            &status,
            self.generation.load(Ordering::Acquire),
        )
    }
    #[cfg(test)]
    pub(super) fn test_poll_dav(&self) -> Result<()> {
        let mut first_error = self.poll_dav(false).err();
        for receiver in &self.test_queues.as_ref().unwrap().incoming {
            while let Ok(job) = receiver.try_recv() {
                if let Err(error) = self.receive_dav_job(&job) {
                    first_error.get_or_insert(error);
                }
                self.downloading.lock().unwrap().remove(&job.entry.event.id);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
    #[cfg(test)]
    pub(super) fn test_auto_poll_dav(&self) -> Result<usize> {
        self.poll_dav(true)
    }
    #[cfg(test)]
    pub(super) fn test_send_pending(&self) -> Result<()> {
        for receiver in &self.test_queues.as_ref().unwrap().outgoing {
            while let Ok(job) = receiver.try_recv() {
                self.send_job(&job)?;
            }
        }
        Ok(())
    }
    pub fn test_dav(&self) -> Result<()> {
        let config = self.config();
        let client = self.dav_client(&config.dav)?;
        client.initialize()?;
        client.check_collection("indexes/")
    }
    pub fn create_invitation(&self) -> Result<InvitationStatus> {
        let config = self.config();
        let client = self.dav_client(&config.dav)?;
        let mut active = self.invitation.lock().unwrap_or_else(|e| e.into_inner());
        if active.is_some() {
            return fail("已有连接码，请先取消或等待过期");
        }
        let invitation = Invitation::create(&client, config.profile)?;
        let status = invitation.status(None)?;
        *active = Some((invitation, config.dav));
        drop(active);
        // 唤醒 WebDAV 线程，确保连接码过期后能被清理。
        self.notify();
        Ok(status)
    }
    pub fn invitation_status(&self) -> Option<InvitationStatus> {
        self.invitation
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|(i, _)| i.display())
    }
    pub fn check_invitation(&self) -> Result<InvitationStatus> {
        let active = self.invitation.lock().unwrap_or_else(|e| e.into_inner());
        let (invitation, dav) = active
            .as_ref()
            .ok_or_else(|| super::Error::Message("请先生成连接码".into()))?;
        let config = self.config();
        check_pairing_dav(&config.dav, dav)?;
        let mut status = invitation.status(Some(&self.dav_client(dav)?))?;
        if let Some(candidate) = &mut status.candidate {
            candidate.dav = Some(dav.clone());
        }
        Ok(status)
    }
    pub fn lookup_invitation(&self, code: &str) -> Result<Candidate> {
        let config = self.config();
        let mut candidate = Invitation::join(&self.dav_client(&config.dav)?, code)?;
        if candidate.device.profile.id == config.profile.id
            || candidate.device.profile.key == config.profile.key
        {
            return fail("这是本机的连接码，请在另一台设备输入");
        }
        candidate.dav = Some(config.dav);
        Ok(candidate)
    }
    pub fn confirm_candidate(&self, candidate: Candidate) -> Result<()> {
        let config = self.config();
        let mut next = config.clone();
        next.peers.push(Peer {
            profile: candidate.device.profile.clone(),
            address: candidate.device.address.clone(),
            policy: Default::default(),
        });
        next.validate()?;
        if let Some(invitation) = &candidate.invitation {
            let dav = candidate
                .dav
                .as_ref()
                .ok_or_else(|| super::Error::Message("连接资料已失效，请重试".into()))?;
            check_pairing_dav(&config.dav, dav)?;
            let client = self.dav_client(dav)?;
            if candidate.host {
                let active = self.invitation.lock().unwrap_or_else(|e| e.into_inner());
                if !active
                    .as_ref()
                    .is_some_and(|(current, _)| current.matches(invitation))
                {
                    return fail("连接码已取消，请重新生成");
                }
                let current = invitation.status(Some(&client))?.candidate;
                if !current.is_some_and(|c| c.device.profile == candidate.device.profile) {
                    return fail("连接请求发生变化，请重新检查设备");
                }
            } else {
                invitation.respond(&client, config.profile.clone())?;
            }
        }
        self.change(
            |current| {
                if let Some(dav) = &candidate.dav {
                    check_pairing_dav(&current.dav, dav)?;
                }
                if current.profile.id != config.profile.id
                    || current.profile.key != config.profile.key
                {
                    return fail("本机身份发生变化，请重新添加设备");
                }
                current.peers.push(Peer {
                    profile: candidate.device.profile,
                    address: candidate.device.address,
                    policy: Default::default(),
                });
                Ok(())
            },
            true,
        )?;
        if candidate.host {
            // 信任已保存；云端清理失败交给过期清理重试，不能把成功添加误报为失败。
            if let Err(error) = self.cancel_invitation() {
                tracing::warn!("清理连接邀请失败：{error}");
            }
        }
        Ok(())
    }
    pub fn cancel_invitation(&self) -> Result<()> {
        let mut active = self.invitation.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((invitation, dav)) = active.as_ref() {
            invitation.cleanup(&self.dav_client(dav)?)?;
        }
        *active = None;
        drop(active);
        self.notify();
        Ok(())
    }
    fn cleanup_pairing(&self) {
        let expired = self
            .invitation
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|(i, _)| i.expired());
        if expired && let Err(error) = self.cancel_invitation() {
            tracing::debug!("清理过期连接邀请失败：{error}");
        }
    }
    fn start(
        database: Database,
        blobs: RawBlobStore,
        changed: async_channel::Sender<()>,
        suppressor: ClipboardWriteSuppressor,
    ) -> Result<Arc<Self>> {
        let store = ConfigStore::discover()?;
        let config = store.load()?;
        let (dav_control, dav_control_rx) = async_channel::bounded::<DavJob>(64);
        let (dav_bulk, dav_bulk_rx) = async_channel::bounded::<DavJob>(16);
        let (control, control_rx) = async_channel::bounded::<Job>(64);
        let (bulk, bulk_rx) = async_channel::bounded::<Job>(16);
        let runtime = Arc::new(Self {
            #[cfg(test)]
            test_queues: None,
            #[cfg(test)]
            test_agent: Mutex::new(None),
            config: RwLock::new(config),
            store,
            data: RwLock::new((database, blobs)),
            statuses: Mutex::new(HashMap::new()),
            discovered: Mutex::new(HashMap::new()),
            incoming: Mutex::new(HashMap::new()),
            session: Mutex::new((Uuid::new_v4().to_string(), Instant::now())),
            generation: AtomicU64::new(1),
            stop: AtomicBool::new(false),
            control,
            dav_control,
            dav_bulk,
            downloading: Mutex::new(Default::default()),
            bulk,
            changed,
            message: Mutex::new(String::new()),
            invitation: Mutex::new(None),
            notices: Mutex::new(VecDeque::new()),
            received_changes: Mutex::new(HashMap::new()),
            clipboard_writer: Mutex::new(Box::new(move |database, blobs, item_id| {
                PasteService::with_clipboard_write_suppressor(
                    database.clone(),
                    blobs.clone(),
                    suppressor.clone(),
                )
                .copy_item(item_id)
                .map_err(|error| error.to_string())
            })),
            local_pairing: Mutex::new(Default::default()),
            dav_transport: Mutex::new(None),
            dav_cache: Mutex::new(Default::default()),
            dav_poll: Mutex::new(()),
            next_pull: Mutex::new(Instant::now()),
            dav_traffic: Arc::new(webdav::Traffic::default()),
            wake: Arc::default(),
            clock_offsets: Mutex::new(HashMap::new()),
            diagnostics: Default::default(),
        });
        let _ = super::ACTIVE.set(Arc::downgrade(&runtime));
        for (name, receiver) in [
            ("同步中继文本接收", dav_control_rx.clone()),
            ("同步中继文本接收", dav_control_rx),
            ("同步中继资源接收", dav_bulk_rx.clone()),
            ("同步中继资源接收", dav_bulk_rx),
        ] {
            let weak = Arc::downgrade(&runtime);
            thread::Builder::new().name(name.into()).spawn(move || {
                while let Ok(job) = receiver.recv_blocking() {
                    let Some(runtime) = weak.upgrade() else {
                        break;
                    };
                    if runtime.stop.load(Ordering::Acquire) {
                        break;
                    }
                    if let Err(error) = runtime.receive_dav_job(&job) {
                        if runtime.diagnostics.pass(
                            &format!("dav-receive:{}", job.peer.profile.id),
                            &error.to_string(),
                            diagnostics::LOG_REPEAT,
                        ) {
                            tracing::warn!(
                                device = %job.peer.profile.label(),
                                "WebDAV 同步内容接收失败：{error}"
                            );
                        }
                        runtime.set_message(&error.to_string());
                        runtime.result(&job.peer.profile.id, error.to_string());
                        if job.manual {
                            runtime.operation_notice(error.to_string(), true);
                        }
                    }
                    runtime
                        .downloading
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&job.entry.event.id);
                }
            })?;
        }

        for (label, receiver) in [
            ("同步控制", control_rx.clone()),
            ("同步控制", control_rx),
            ("同步资源", bulk_rx.clone()),
            ("同步资源", bulk_rx),
        ] {
            let weak = Arc::downgrade(&runtime);
            thread::Builder::new().name(label.into()).spawn(move || {
                while let Ok(job) = receiver.recv_blocking() {
                    let Some(runtime) = weak.upgrade() else {
                        break;
                    };
                    if runtime.stop.load(Ordering::Acquire) {
                        break;
                    }
                    if job.generation != runtime.generation.load(Ordering::Acquire)
                        || job.created.elapsed().as_secs() > EVENT_TTL
                    {
                        runtime.finish_manual(
                            &job,
                            true,
                            "同步设置已更改或等待超时，本次发送已取消".into(),
                        );
                        continue;
                    }
                    match runtime.send_job(&job) {
                        Err(error) => {
                            let (_, reason) = diagnostics::describe(&error);
                            if runtime.diagnostics.pass(
                                &format!("send:{}", job.peer),
                                &reason,
                                diagnostics::LOG_REPEAT,
                            ) {
                                tracing::warn!(device = %job.peer, "同步内容发送失败：{reason}");
                            }
                            runtime.result(&job.peer, error.to_string());
                            runtime.finish_manual(&job, true, error.to_string());
                        }
                        Ok(()) => {
                            let message = runtime
                                .statuses
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .get(&job.peer)
                                .map(|s| s.result.clone())
                                .unwrap_or_else(|| "发送完成".into());
                            runtime.finish_manual(&job, false, message);
                        }
                    }
                }
            })?;
        }
        let weak = Arc::downgrade(&runtime);
        let wake = runtime.wake.clone();
        thread::Builder::new()
            .name("同步局域网接收".into())
            .spawn(move || {
                let mut listener: Option<(u16, TcpListener)> = None;
                let (tx, rx) = async_channel::bounded::<TcpStream>(16);
                for _ in 0..4 {
                    let weak = weak.clone();
                    let rx = rx.clone();
                    let _ = thread::Builder::new()
                        .name("同步请求处理".into())
                        .spawn(move || {
                            while let Ok(mut stream) = rx.recv_blocking() {
                                let Some(runtime) = weak.upgrade() else {
                                    break;
                                };
                                if let Err(error) = runtime.handle_lan_stream(&mut stream) {
                                    let source = stream
                                        .peer_addr()
                                        .map(|a| a.ip().to_string())
                                        .unwrap_or_default();
                                    let (_, reason) = diagnostics::describe(&error);
                                    if runtime.diagnostics.pass(
                                        &format!("incoming:{source}"),
                                        &reason,
                                        diagnostics::LOG_REPEAT,
                                    ) {
                                        tracing::warn!(%source, "已拒绝局域网同步请求：{reason}");
                                    }
                                }
                            }
                        });
                }
                loop {
                    let seen = wake.seen();
                    let Some(runtime) = weak.upgrade() else {
                        break;
                    };
                    if runtime.stop.load(Ordering::Acquire) {
                        break;
                    }
                    let config = runtime.config();
                    if !(runtime.local_pairing_active() || config.enabled && config.lan_enabled) {
                        listener = None;
                    } else if listener
                        .as_ref()
                        .is_none_or(|(port, _)| *port != config.port)
                    {
                        match TcpListener::bind(("0.0.0.0", config.port)) {
                            Ok(socket) => {
                                let _ = socket.set_nonblocking(true);
                                if runtime.diagnostics.clear("listen") {
                                    tracing::info!(port = config.port, "局域网同步端口已恢复监听");
                                }
                                listener = Some((config.port, socket));
                            }
                            // 监听失败时每轮都会重试，只在原因变化时记录和提示。
                            Err(error) => runtime.listen_failed(config.port, &error),
                        }
                    }
                    if let Some((_, listener)) = &listener {
                        for _ in 0..16 {
                            match listener.accept() {
                                Ok((stream, _)) => {
                                    let _ = tx.try_send(stream);
                                }
                                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                                Err(_) => break,
                            }
                        }
                    }
                    let idle = runtime.idle();
                    drop(runtime);
                    wake.pause(seen, idle, Duration::from_millis(40));
                }
            })?;
        let weak = Arc::downgrade(&runtime);
        let wake = runtime.wake.clone();
        thread::Builder::new()
            .name("同步局域网发现".into())
            .spawn(move || {
                let mut socket: Option<UdpSocket> = None;
                let mut last = Instant::now() - Duration::from_secs(5);
                loop {
                    let seen = wake.seen();
                    let Some(runtime) = weak.upgrade() else {
                        break;
                    };
                    if runtime.stop.load(Ordering::Acquire) {
                        break;
                    }
                    let config = runtime.config();
                    if (config.enabled && config.lan_enabled) || runtime.local_pairing_active() {
                        if socket.is_none() {
                            match UdpSocket::bind(("0.0.0.0", super::config::PORT)) {
                                Ok(s) => {
                                    let _ = s.set_nonblocking(true);
                                    let _ = s.set_broadcast(true);
                                    if runtime.diagnostics.clear("discovery") {
                                        tracing::info!("局域网发现端口已恢复");
                                    }
                                    socket = Some(s);
                                }
                                Err(error) => runtime.discovery_failed(&error),
                            }
                        }
                        if let Some(s) = &socket {
                            if last.elapsed().as_secs() >= 3 {
                                let targets = super::lan::broadcast_targets();
                                let announce = (config.enabled && config.lan_enabled)
                                    .then(|| format!("QcClip/1/{}", config.port).into_bytes());
                                let advertisement = runtime.pairing_advertisement();
                                for target in &targets {
                                    for payload in announce.iter().chain(advertisement.iter()) {
                                        let _ = s.send_to(payload, (*target, super::config::PORT));
                                    }
                                }
                                last = Instant::now();
                            }
                            let mut buffer = [0u8; 1025];
                            for _ in 0..32 {
                                let Ok((length, address)) = s.recv_from(&mut buffer) else {
                                    break;
                                };
                                runtime.discover_pairing(&buffer[..length], address);
                                if let Some(port) = std::str::from_utf8(&buffer[..length])
                                    .ok()
                                    .and_then(|s| s.strip_prefix("QcClip/1/"))
                                    .and_then(|s| s.parse::<u16>().ok())
                                    .filter(|p| *p != 0)
                                {
                                    let mut found = runtime
                                        .discovered
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner());
                                    found.retain(|_, at| at.elapsed().as_secs() < 15);
                                    if found.len() < 64 {
                                        found.insert(
                                            SocketAddr::new(address.ip(), port).to_string(),
                                            Instant::now(),
                                        );
                                    }
                                }
                            }
                        }
                    } else {
                        socket = None;
                    }
                    let idle = runtime.idle();
                    drop(runtime);
                    wake.pause(seen, idle, Duration::from_millis(200));
                }
            })?;
        let weak = Arc::downgrade(&runtime);
        let wake = runtime.wake.clone();
        thread::Builder::new()
            .name("同步会话维护".into())
            .spawn(move || {
                let mut next = Instant::now();
                let mut probe_round = 0usize;
                let mut was_idle = false;
                loop {
                    let seen = wake.seen();
                    let Some(runtime) = weak.upgrade() else {
                        break;
                    };
                    if runtime.stop.load(Ordering::Acquire) {
                        break;
                    }
                    if Instant::now() >= next {
                        let config = runtime.config();
                        if config.enabled {
                            runtime.maintain_session();
                        }
                        if config.enabled && config.lan_enabled {
                            let discovered = runtime.snapshot().discovered;
                            let peers = Mutex::new(
                                config
                                    .peers
                                    .iter()
                                    .filter(|p| p.policy.enabled && p.policy.lan),
                            );
                            thread::scope(|scope| {
                                for _ in 0..4 {
                                    scope.spawn(|| {
                                        loop {
                                            let peer = peers
                                                .lock()
                                                .unwrap_or_else(|e| e.into_inner())
                                                .next();
                                            let Some(peer) = peer else {
                                                break;
                                            };
                                            runtime.probe_peer(peer, &discovered, probe_round);
                                        }
                                    });
                                }
                            });
                            probe_round = probe_round.wrapping_add(1);
                        }
                        runtime.poll_local_pairing();
                        // 空闲后不再周期唤醒界面，但转入空闲时仍刷新一次，让配对过期等状态及时显示。
                        let idle = runtime.idle();
                        if !(idle && was_idle) {
                            runtime.notify();
                        }
                        was_idle = idle;
                        next = Instant::now() + Duration::from_secs(2);
                    }
                    let idle = runtime.idle();
                    drop(runtime);
                    wake.pause(seen, idle, Duration::from_millis(200));
                }
            })?;
        let weak = Arc::downgrade(&runtime);
        let wake = runtime.wake.clone();
        thread::Builder::new()
            .name("同步 WebDAV 轮询".into())
            .spawn(move || {
                let mut errors = 0u32;
                let mut generation = 0;
                let mut next_cleanup = Instant::now();
                loop {
                    let seen = wake.seen();
                    let Some(runtime) = weak.upgrade() else {
                        break;
                    };
                    if runtime.stop.load(Ordering::Acquire) {
                        break;
                    }
                    let current = runtime.generation.load(Ordering::Acquire);
                    if current != generation {
                        *runtime.next_pull.lock().unwrap_or_else(|e| e.into_inner()) =
                            Instant::now();
                        generation = current;
                        errors = 0;
                    }
                    let enabled = runtime
                        .config
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .enabled;
                    // 托管文件只由同步产生，关闭同步时不做清理；重新开启后会立即补做一次。
                    if enabled && Instant::now() >= next_cleanup {
                        let data = runtime.data.read().unwrap_or_else(|e| e.into_inner());
                        if let Err(error) = repository::cleanup_managed_files(&data.0, &data.1) {
                            tracing::warn!("清理同步托管文件失败：{error}");
                        }
                        if crate::services::capture::CaptureService::new(
                            data.0.clone(),
                            data.1.clone(),
                        )
                        .cleanup_unused_blobs()
                        .is_err()
                        {
                            tracing::warn!("清理同步替换后的无引用原始资源失败");
                        }
                        next_cleanup = Instant::now() + Duration::from_secs(300);
                    }
                    // 连接码可以在同步关闭时生成，其过期清理仍需继续，否则会在云端残留。
                    let pairing = runtime
                        .invitation
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .is_some();
                    if (enabled || pairing)
                        && Instant::now()
                            >= *runtime.next_pull.lock().unwrap_or_else(|e| e.into_inner())
                    {
                        let config = runtime.config();
                        runtime.cleanup_pairing();
                        if config.enabled && config.dav.enabled && config.dav.auto_pull {
                            match runtime.poll_dav(true) {
                                Ok(_) => {
                                    errors = 0;
                                    if runtime.diagnostics.clear("dav-poll") {
                                        tracing::info!("WebDAV 自动拉取已恢复");
                                    }
                                }
                                Err(error) => {
                                    errors = (errors + 1).min(5);
                                    let message = error.to_string();
                                    if runtime.diagnostics.pass(
                                        "dav-poll",
                                        &message,
                                        diagnostics::LOG_REPEAT,
                                    ) {
                                        tracing::warn!("WebDAV 自动拉取失败：{message}");
                                    }
                                    // 偶发网络波动不打扰用户，连续失败后再提示。
                                    if errors >= 3 {
                                        runtime.attention_notice(
                                            "dav-poll",
                                            &message,
                                            format!("WebDAV 自动拉取连续失败：{message}"),
                                        );
                                    }
                                    runtime.set_message(&message);
                                }
                            }
                        }
                        *runtime.next_pull.lock().unwrap_or_else(|e| e.into_inner()) =
                            Instant::now()
                                + Duration::from_secs(
                                    config
                                        .dav
                                        .poll_seconds
                                        .max(1)
                                        .saturating_mul(1u64 << errors)
                                        .min(86400),
                                );
                    }
                    let idle = runtime.idle();
                    drop(runtime);
                    wake.pause(seen, idle, Duration::from_millis(200));
                }
            })?;
        Ok(runtime)
    }

    pub fn config(&self) -> Config {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            config: Some(self.config()),
            dav_requests: self.dav_traffic.requests(),
            local_pairing: self
                .local_pairing
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .snapshot(),
            statuses: self
                .statuses
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            discovered: self
                .discovered
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .filter(|(_, t)| t.elapsed().as_secs() < 15)
                .map(|(a, _)| a.clone())
                .collect(),
            message: self
                .message
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        }
    }
    pub fn change(
        &self,
        apply: impl FnOnce(&mut Config) -> Result<()>,
        rotate: bool,
    ) -> Result<()> {
        let mut config = self.config.write().unwrap_or_else(|e| e.into_inner());
        let mut next = config.clone();
        apply(&mut next)?;
        self.store.save(&next)?;
        *config = next;
        if rotate {
            self.rotate();
            self.message
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
            // 提示随设置变更被清空；问题仍在时由后台线程重新记录并显示。
            self.diagnostics.clear("listen");
            self.diagnostics.clear("discovery");
        }
        drop(config);
        self.notify();
        Ok(())
    }
    pub fn pair(&self, code: PairingCode, address: String) -> Result<()> {
        self.change(
            |config| {
                if config.peers.iter().any(|p| p.profile.id == code.profile.id) {
                    return fail("设备已配对，请先撤销旧配对后重新核对指纹");
                }
                config.peers.push(Peer {
                    profile: code.profile,
                    address,
                    policy: Default::default(),
                });
                Ok(())
            },
            true,
        )
    }
    pub fn revoke(&self, id: &str) -> Result<()> {
        self.change(
            |config| {
                config.peers.retain(|p| p.profile.id != id);
                Ok(())
            },
            true,
        )
    }
    fn rotate(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        *self.session.lock().unwrap_or_else(|e| e.into_inner()) =
            (Uuid::new_v4().to_string(), Instant::now());
        self.incoming
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.statuses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
    fn maintain_session(&self) {
        let mut session = self.session.lock().unwrap_or_else(|e| e.into_inner());
        if session.1.elapsed().as_secs() > 15 {
            // 唤醒后的首次请求会使用新会话；持久化清理由后台轮询完成。
            session.0 = Uuid::new_v4().to_string();
            self.incoming
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
        }
        session.1 = Instant::now();
    }
    fn local_session(&self, config: &Config) -> Session {
        self.maintain_session();
        let id = self
            .session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .0
            .clone();
        Session {
            id,
            expires: protocol::now() + 15,
            profile: config.profile.clone(),
        }
    }
    pub(super) fn notify(&self) {
        let _ = self.changed.try_send(());
        self.wake.wake();
    }
    /// 同步未启用、没有进行中的局域网配对且没有待清理的连接码时，后台线程无事可做。
    fn idle(&self) -> bool {
        !self
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .enabled
            && !self.local_pairing_busy()
            && self
                .invitation
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_none()
    }
    fn take_received_changes(&self) -> Vec<super::ReceivedChange> {
        std::mem::take(
            &mut *self
                .received_changes
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        )
        .into_values()
        .collect()
    }
    fn set_message(&self, message: &str) {
        *self.message.lock().unwrap_or_else(|e| e.into_inner()) = message.into();
        self.notify();
    }
    fn listen_failed(&self, port: u16, error: &io::Error) {
        let message = format!(
            "局域网同步端口 {port} 监听失败，其他设备无法连接本机：{error}。请更换同步端口或关闭占用该端口的程序"
        );
        if self.diagnostics.pass("listen", &message, Duration::MAX) {
            tracing::warn!(port, "{message}");
            self.set_message(&message);
            self.attention_notice("listen", &message, message.clone());
        }
    }
    fn discovery_failed(&self, error: &io::Error) {
        let port = super::config::PORT;
        let message =
            format!("局域网发现端口 {port}（UDP）被占用，无法自动发现设备和更新设备地址：{error}");
        if self.diagnostics.pass("discovery", &message, Duration::MAX) {
            tracing::warn!(port, "{message}");
            self.set_message(&message);
            self.attention_notice("discovery", &message, message.clone());
        }
    }
    fn clock_offset(&self, peer: &str) -> i64 {
        self.clock_offsets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(peer)
            .copied()
            .unwrap_or(0)
    }
    fn record_offset(&self, peer: &Peer, offset: i64) {
        self.clock_offsets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(peer.profile.id.clone(), offset);
        let key = format!("clock:{}", peer.profile.id);
        let name = peer.profile.label();
        // 偏差在阈值附近抖动时不反复记录：超过 30 秒才提示，回到 20 秒内才算恢复。
        if offset.unsigned_abs() > 30 {
            if self.diagnostics.pass(&key, "skew", diagnostics::LOG_REPEAT) {
                tracing::warn!(
                    device = %name,
                    offset_seconds = offset,
                    "设备系统时间与本机相差较大，局域网同步已按偏差自动校正"
                );
            }
            self.attention_notice(
                &key,
                "clock",
                format!(
                    "{name} 与本机的系统时间相差约 {}，局域网同步已自动校正，WebDAV 同步可能受影响。建议两台设备都开启“自动设置时间”",
                    readable_seconds(offset.unsigned_abs())
                ),
            );
        } else if offset.unsigned_abs() <= 20 && self.diagnostics.clear(&key) {
            tracing::info!(device = %name, offset_seconds = offset, "设备系统时间偏差已恢复正常");
        }
    }
    fn lan_connected(&self, peer: &Peer, address: &str) {
        let mut statuses = self.statuses.lock().unwrap_or_else(|e| e.into_inner());
        let status = statuses.entry(peer.profile.id.clone()).or_default();
        status.lan_error.clear();
        status.lan_failures = 0;
        drop(statuses);
        let id = &peer.profile.id;
        self.diagnostics.clear(&format!("lan:{id}"));
        self.diagnostics.clear(&format!("notice:lan:{id}"));
        if self
            .diagnostics
            .pass(&format!("lan-up:{id}"), address, Duration::MAX)
        {
            tracing::info!(device = %peer.profile.label(), %address, "已建立局域网同步连接");
        }
    }
    fn lan_failed(&self, peer: &Peer, address: &str, reason: String, failure: Failure) {
        let id = &peer.profile.id;
        let failures = {
            let mut statuses = self.statuses.lock().unwrap_or_else(|e| e.into_inner());
            let status = statuses.entry(id.clone()).or_default();
            status.lan_error = if address.is_empty() {
                reason.clone()
            } else {
                format!("{address}：{reason}")
            };
            status.lan_failures = status.lan_failures.saturating_add(1);
            status.lan_failures
        };
        let name = peer.profile.label();
        if self.diagnostics.clear(&format!("lan-up:{id}")) {
            tracing::info!(device = %name, "局域网同步连接已断开");
        }
        if self
            .diagnostics
            .pass(&format!("lan:{id}"), &reason, diagnostics::LOG_REPEAT)
        {
            tracing::warn!(device = %name, %address, "局域网握手失败：{reason}");
        }
        // 刚配对时对方可能尚未保存本设备，连续失败数次后才提示用户。
        if failure == Failure::Rejected && failures >= 3 {
            self.attention_notice(
                &format!("lan:{id}"),
                &reason,
                format!("无法与 {name} 建立局域网连接：{reason}"),
            );
        }
    }
    fn result(&self, peer: &str, message: String) {
        self.statuses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(peer.into())
            .or_default()
            .result = message;
        self.notify();
    }
    fn finish_manual(&self, job: &Job, failed: bool, message: String) {
        if !job.mode.manual() {
            return;
        }
        let config = self.config();
        let name = config
            .peers
            .iter()
            .find(|p| p.profile.id == job.peer)
            .map(|p| p.profile.name.as_str())
            .unwrap_or("目标设备");
        self.operation_notice(format!("{name}：{message}"), failed);
    }
    pub(super) fn operation_notice(&self, message: String, failed: bool) {
        self.queue_notice(message, failed, false);
    }
    pub(super) fn pairing_notice(&self, message: String) {
        self.queue_notice(message, false, true);
    }
    fn queue_notice(&self, message: String, failed: bool, informational: bool) {
        self.push_notice(OperationNotice {
            message,
            failed,
            informational,
            attention: false,
        });
    }
    /// 后台问题的通知：同一问题消息不变时按 `NOTICE_REPEAT` 限频，避免反复打扰。
    fn attention_notice(&self, key: &str, reason: &str, message: String) {
        if !self
            .diagnostics
            .pass(&format!("notice:{key}"), reason, diagnostics::NOTICE_REPEAT)
        {
            return;
        }
        self.push_notice(OperationNotice {
            message,
            failed: true,
            informational: false,
            attention: true,
        });
    }
    fn push_notice(&self, notice: OperationNotice) {
        let mut notices = self.notices.lock().unwrap_or_else(|e| e.into_inner());
        if notices.len() >= 32 {
            notices.pop_front();
        }
        notices.push_back(notice);
        drop(notices);
        self.notify();
    }
    fn set_session(&self, peer: &str, session: Session, _lan: bool) {
        let profile = session.profile.clone();
        if self
            .config()
            .peers
            .iter()
            .find(|p| p.profile.id == peer)
            .is_some_and(|p| p.profile != profile)
        {
            let _ = self.change(
                |c| {
                    if let Some(p) = c.peers.iter_mut().find(|p| p.profile.id == peer) {
                        if p.profile.key != profile.key {
                            return fail("设备密钥发生变化，需重新配对");
                        }
                        p.profile = profile;
                    }
                    Ok(())
                },
                false,
            );
        }
        let config = self.config.read().unwrap_or_else(|e| e.into_inner());
        let Some(target) = config.peers.iter().find(|p| p.profile.id == peer) else {
            return;
        };
        if !config.lan_allowed(target) {
            return;
        }
        let mut statuses = self.statuses.lock().unwrap_or_else(|e| e.into_inner());
        let status = statuses.entry(peer.into()).or_default();
        status.lan = Some(session);
    }
    fn probe_peer(&self, peer: &Peer, discovered: &[String], round: usize) {
        let mut addresses = vec![peer.address.clone()];
        let candidates: Vec<_> = discovered.iter().filter(|a| **a != peer.address).collect();
        if !candidates.is_empty() {
            addresses.push(candidates[round % candidates.len()].clone());
        }
        let mut first_error = None;
        for address in addresses {
            if super::config::validate_address(&address).is_err() {
                continue;
            }
            let session = match self.hello(peer, &address) {
                Ok(session) => session,
                Err(error) => {
                    first_error.get_or_insert((address, error));
                    continue;
                }
            };
            self.lan_connected(peer, &address);
            self.set_session(&peer.profile.id, session, true);
            if address != peer.address {
                let _ = self.change(
                    |c| {
                        if let Some(p) = c.peers.iter_mut().find(|p| {
                            p.profile.id == peer.profile.id && p.profile.key == peer.profile.key
                        }) {
                            p.address = address;
                        }
                        Ok(())
                    },
                    false,
                );
            }
            return;
        }
        match first_error {
            Some((address, error)) => {
                let (failure, reason) = diagnostics::describe(&error);
                self.lan_failed(peer, &address, reason, failure);
            }
            None => self.lan_failed(
                peer,
                "",
                "还没有对方的局域网地址，请确认两台设备在同一网络并都已开启同步".into(),
                Failure::Unreachable,
            ),
        }
    }
    fn hello(&self, peer: &Peer, address: &str) -> Result<Session> {
        let config = self.config();
        if !config.lan_allowed(peer) {
            return fail("局域网通道已关闭");
        }
        let challenge = Uuid::new_v4().to_string();
        let request = Request::Hello {
            challenge: challenge.clone(),
            profile: config.profile.clone(),
        };
        let envelope = protocol::seal(&config, peer, &request)?;
        let started = Instant::now();
        let local = protocol::now();
        let response = lan::exchange(super::config::validate_address(address)?, &envelope)?;
        // 响应必须带回本次随机挑战，因此无需再按发送时间防重放；其发送时间用于测量时钟偏差。
        let (_, reply, sent_at): (_, Reply, u64) = protocol::open_online(&config, &response)?;
        if reply.request != challenge {
            return fail("设备握手响应与请求不匹配");
        }
        if !reply.accepted {
            return Err(super::Error::Message(format!(
                "对方拒绝握手：{}",
                reply.message
            )));
        }
        let midpoint = local.saturating_add(started.elapsed().as_secs() / 2);
        self.record_offset(peer, sent_at as i64 - midpoint as i64);
        // 会话有效期按对方时钟给出，换算为本机时间，两台设备时间不一致时也能正确判断。
        let mut session = reply
            .session
            .ok_or_else(|| super::Error::Message("对方未返回在线会话".into()))?;
        session.expires = protocol::now() + session.expires.saturating_sub(sent_at).min(120);
        Some(session)
            .filter(|s| s.valid(peer))
            .ok_or_else(|| super::Error::Message("对方返回的在线会话无效".into()))
    }

    pub fn automatic(&self, database: &Database, item_id: i64, mode: Mode) {
        let data = self.data.read().unwrap_or_else(|e| e.into_inner());
        if data.0.path() != database.path() {
            return;
        }
        drop(data);
        let config = self.config();
        if !config.enabled {
            return;
        }
        for peer in &config.peers {
            if !peer.policy.enabled
                || (mode == Mode::NewContent && !peer.policy.history)
                || (mode == Mode::Touch && (!peer.policy.history || !peer.policy.touches))
                || (mode == Mode::Favorite && !peer.policy.favorites)
            {
                continue;
            }
            self.enqueue(peer, item_id, mode, false);
        }
    }
    pub fn manual(&self, target: Option<&str>, item_id: i64, mode: Mode) -> Result<usize> {
        self.manual_channel(target, item_id, mode, false)
    }
    pub fn upload(&self, target: Option<&str>, item_id: i64, mode: Mode) -> Result<usize> {
        self.manual_channel(target, item_id, mode, true)
    }
    pub fn upload_latest(&self) -> Result<Option<usize>> {
        self.latest_upload()?
            .map(|latest| self.upload(None, latest.item_id, latest.mode))
            .transpose()
    }
    pub fn latest_upload(&self) -> Result<Option<super::activity::LatestUpload>> {
        let data = self.data.read().unwrap_or_else(|e| e.into_inner());
        super::activity::latest(&data.0)
    }
    fn manual_channel(
        &self,
        target: Option<&str>,
        item_id: i64,
        mode: Mode,
        dav_only: bool,
    ) -> Result<usize> {
        if !mode.manual() {
            return fail("手动同步操作无效");
        }
        let config = self.config();
        if !config.enabled {
            return fail("请先开启在线同步");
        }
        let mut count = 0;
        for peer in config
            .peers
            .iter()
            .filter(|p| target.is_none_or(|id| p.profile.id == id))
        {
            if self.enqueue(peer, item_id, mode, dav_only) {
                count += 1;
            }
        }
        if count == 0 {
            if dav_only {
                return fail("没有可用的 WebDAV 目标设备，请先添加设备并允许其使用 WebDAV");
            }
            return fail("没有可用的局域网连接或已启用的 WebDAV 目标设备");
        }
        Ok(count)
    }
    fn enqueue(&self, peer: &Peer, item_id: i64, mode: Mode, dav_only: bool) -> bool {
        let status = self
            .statuses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&peer.profile.id)
            .cloned()
            .unwrap_or_default();
        let config = self.config();
        let lan = !dav_only
            && config.lan_allowed(peer)
            && status
                .lan
                .as_ref()
                .is_some_and(|session| session.valid(peer));
        let dav = config.dav_allowed(peer) && (mode.manual() || config.dav.auto_upload);
        let session = if lan {
            status.lan.as_ref().unwrap().id.clone()
        } else if dav {
            "webdav-cache-v1".into()
        } else {
            return false;
        };
        let data = self.data.read().unwrap_or_else(|e| e.into_inner());
        let metadata = data.0.connect().and_then(|c| c.query_row("SELECT content_hash, total_bytes > 65536 OR EXISTS(SELECT 1 FROM clipboard_formats WHERE item_id = ?1 AND format_kind = 'file_list') FROM clipboard_items WHERE id = ?1", [item_id], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, bool>(1)?))).map_err(|source| crate::services::database::DatabaseError::Inspect { source }));
        drop(data);
        let Ok((expected_hash, large)) = metadata else {
            self.result(&peer.profile.id, "条目已不可用，本次未发送".into());
            return false;
        };
        let mut sequence = 0;
        let allocated = self.change(
            |config| {
                config.sequence = config
                    .sequence
                    .checked_add(1)
                    .filter(|s| *s <= i64::MAX as u64)
                    .ok_or_else(|| {
                        super::Error::Message("设备序号已耗尽，需要重新注册身份".into())
                    })?;
                sequence = config.sequence;
                Ok(())
            },
            false,
        );
        if let Err(error) = allocated {
            self.result(&peer.profile.id, error.to_string());
            return false;
        }
        let large = large && mode != Mode::Touch;
        let job = Job {
            peer: peer.profile.id.clone(),
            item_id,
            mode,
            generation: self.generation.load(Ordering::Acquire),
            created: Instant::now(),
            session,
            sequence,
            expected_hash,
            dav_only,
        };
        self.result(&peer.profile.id, "正在发送…".into());
        let result = if large {
            self.bulk.try_send(job)
        } else {
            self.control.try_send(job)
        };
        if result.is_err() {
            self.result(&peer.profile.id, "当前在线发送队列已满，本次已跳过".into());
            return false;
        }
        true
    }

    fn send_job(&self, job: &Job) -> Result<()> {
        let config = self.config();
        if !config.enabled || job.generation != self.generation.load(Ordering::Acquire) {
            return fail("同步设置已变更，本次发送已取消");
        }
        let peer = config
            .peers
            .iter()
            .find(|p| p.profile.id == job.peer && p.policy.enabled)
            .ok_or_else(|| super::Error::Message("目标设备已被阻止或撤销".into()))?
            .clone();
        let status = self
            .statuses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&peer.profile.id)
            .cloned()
            .unwrap_or_default();
        if status.channel(&config, &peer).is_none() {
            return fail("目标设备已离线，本次不保留待发送内容");
        }
        let session = status
            .lan
            .as_ref()
            .filter(|s| {
                !job.dav_only && config.lan_allowed(&peer) && s.valid(&peer) && s.id == job.session
            })
            .map(|s| s.id.clone())
            .unwrap_or_else(|| "webdav-cache-v1".into());
        let event = Event {
            id: Uuid::new_v4().to_string(),
            sender: config.profile.id.clone(),
            recipient: peer.profile.id.clone(),
            session,
            sequence: job.sequence,
            // 局域网事件由对方按其时钟校验有效期；WebDAV 缓存会把有效期改写为 0。
            expires: protocol::shift(protocol::now(), self.clock_offset(&peer.profile.id))
                + EVENT_TTL
                - job.created.elapsed().as_secs().min(EVENT_TTL),
            item_uuid: String::new(),
            mode: job.mode,
            hash_version: 0,
            source_hash: [0; 32],
            complete: false,
            source_app_name: None,
            formats: Vec::new(),
            files: Vec::new(),
            tags: Vec::new(),
            note: None,
            primary: None,
        };
        let data = self.data.read().unwrap_or_else(|e| e.into_inner());
        let packet = repository::build(&data.0, &data.1, job.item_id, event, &peer.policy)?;
        drop(data);
        if packet.event.source_hash.as_slice() != job.expected_hash {
            return fail("条目在发送前已修改，请手动重新发送");
        }
        let status = if job.dav_only {
            PeerStatus::default()
        } else {
            status
        };
        self.send_packet(&peer, &packet, &status, job.generation)
    }

    fn send_packet(
        &self,
        peer: &Peer,
        packet: &Packet,
        status: &PeerStatus,
        generation: u64,
    ) -> Result<()> {
        let config = self.config();
        let mut lan_error = None;
        if config.lan_allowed(peer)
            && status
                .lan
                .as_ref()
                .is_some_and(|s| s.valid(peer) && s.id == packet.event.session)
        {
            match self.send_lan(peer, packet, generation) {
                Ok(()) => {
                    self.result(&peer.profile.id, success("LAN", packet));
                    return Ok(());
                }
                Err(error) => {
                    let (_, reason) = diagnostics::describe(&error);
                    if self.diagnostics.pass(
                        &format!("send-lan:{}", peer.profile.id),
                        &reason,
                        diagnostics::LOG_REPEAT,
                    ) {
                        tracing::warn!(device = %peer.profile.label(), "局域网发送失败：{reason}");
                    }
                    lan_error = Some(error);
                }
            }
        }
        if config.dav_allowed(peer) && (packet.event.mode.manual() || config.dav.auto_upload) {
            self.send_dav(peer, packet, generation)?;
            Ok(())
        } else {
            Err(lan_error
                .unwrap_or_else(|| super::Error::Message("目标设备没有有效在线通道".into())))
        }
    }

    fn ensure_sending(
        &self,
        peer: &Peer,
        generation: u64,
        event: &Event,
        via_lan: bool,
    ) -> Result<Config> {
        let config = self.config();
        if (via_lan && !config.lan_allowed(peer))
            || (!via_lan && !config.dav_allowed(peer))
            || self.generation.load(Ordering::Acquire) != generation
            || (via_lan
                && event.expires
                    <= protocol::shift(protocol::now(), self.clock_offset(&peer.profile.id)))
            || !config.peers.iter().any(|p| {
                p.profile.id == peer.profile.id
                    && p.policy.enabled
                    && p.profile.key == peer.profile.key
            })
        {
            return fail("会话已结束、设置已变更或消息已过期");
        }
        Ok(config)
    }
    fn rpc(&self, peer: &Peer, request: &Request) -> Result<Reply> {
        let config = self.config();
        if !config.lan_allowed(peer) {
            return fail("局域网通道已关闭");
        }
        // 发送时间按握手测得的偏差换算到对方时钟，响应时间再换算回本机时钟校验。
        let offset = self.clock_offset(&peer.profile.id);
        let envelope = protocol::seal_at(
            &config,
            peer,
            request,
            protocol::shift(protocol::now(), offset),
        )?;
        let response = lan::exchange(super::config::validate_address(&peer.address)?, &envelope)?;
        let (sender, reply, sent_at): (_, Reply, u64) = protocol::open_online(&config, &response)?;
        protocol::check_fresh(protocol::shift(sent_at, offset.saturating_neg()))?;
        if sender.profile.id != peer.profile.id || reply.request != protocol::request_id(request) {
            return fail("同步响应身份不匹配");
        }
        if !reply.accepted {
            return fail(&reply.message);
        }
        Ok(reply)
    }
    fn send_lan(&self, peer: &Peer, packet: &Packet, generation: u64) -> Result<()> {
        self.ensure_sending(peer, generation, &packet.event, true)?;
        self.rpc(peer, &Request::Begin(packet.event.clone()))?;
        for (id, bytes) in &packet.resources {
            for (index, data) in bytes.chunks(CHUNK_BYTES).enumerate() {
                self.ensure_sending(peer, generation, &packet.event, true)?;
                self.rpc(
                    peer,
                    &Request::Chunk {
                        event: packet.event.id.clone(),
                        session: packet.event.session.clone(),
                        resource: id.clone(),
                        index,
                        data: data.to_vec(),
                    },
                )?;
            }
            if bytes.is_empty() {
                self.rpc(
                    peer,
                    &Request::Chunk {
                        event: packet.event.id.clone(),
                        session: packet.event.session.clone(),
                        resource: id.clone(),
                        index: 0,
                        data: Vec::new(),
                    },
                )?;
            }
        }
        self.rpc(
            peer,
            &Request::Commit {
                event: packet.event.id.clone(),
                session: packet.event.session.clone(),
            },
        )?;
        Ok(())
    }

    pub(super) fn handle_lan_stream(&self, stream: &mut TcpStream) -> Result<()> {
        lan::prepare(stream)?;
        let value: serde_json::Value = lan::read(stream)?;
        if value.get("pairing").is_some() {
            if serde_json::to_vec(&value)?.len() > 16_384 {
                return fail("配对请求超过大小限制");
            }
            let wire = serde_json::from_value(value)?;
            let reply = self.receive_local_pairing(wire, stream.peer_addr()?);
            use std::io::Write as _;
            stream.write_all(&(reply.len() as u32).to_be_bytes())?;
            stream.write_all(&reply)?;
            Ok(())
        } else {
            let envelope: Envelope = serde_json::from_value(value)?;
            let reply = self.receive_lan(&envelope)?;
            lan::write(stream, &reply)
        }
    }

    pub(super) fn receive_lan(&self, envelope: &Envelope) -> Result<Envelope> {
        let config_guard = self.config.read().unwrap_or_else(|e| e.into_inner());
        let config = &*config_guard;
        let (peer, request, sent_at): (_, Request, u64) = protocol::open_online(config, envelope)?;
        // 握手只返回加密的会话；不放宽它的时间检查，对方就无法测量两台设备的时钟偏差。其余请求仍须在有效期内。
        if !matches!(request, Request::Hello { .. }) {
            protocol::check_fresh(sent_at)?;
        }
        if !config.lan_allowed(&peer) {
            return fail("当前设备通道已关闭");
        }
        let request_id = protocol::request_id(&request);
        let outcome = self.handle_request(config, &peer, request);
        if let Err(error) = &outcome
            && self.diagnostics.pass(
                &format!("reject:{}", peer.profile.id),
                &error.to_string(),
                diagnostics::LOG_REPEAT,
            )
        {
            tracing::warn!(device = %peer.profile.label(), "已拒绝设备的局域网同步请求：{error}");
        }
        let reply = match outcome {
            Ok((session, applied)) => Reply {
                request: request_id,
                session,
                accepted: true,
                applied,
                message: "已确认".into(),
            },
            Err(error) => Reply {
                request: request_id,
                session: None,
                accepted: false,
                applied: false,
                message: error.to_string(),
            },
        };
        protocol::seal(config, &peer, &reply)
    }
    fn handle_request(
        &self,
        config: &Config,
        peer: &Peer,
        request: Request,
    ) -> Result<(Option<Session>, bool)> {
        if !config.enabled
            || !config.peers.iter().any(|p| {
                p.profile.id == peer.profile.id
                    && p.profile.key == peer.profile.key
                    && p.policy.enabled
            })
        {
            return fail("设备已被阻止、撤销或同步已关闭");
        }
        if let Request::Hello { profile, .. } = request {
            profile.validate()?;
            if profile.id != peer.profile.id || profile.key != peer.profile.key {
                return fail("握手身份与配对公钥不一致");
            }
            return Ok((Some(self.local_session(config)), false));
        }
        let current = self.local_session(config);
        let mut incoming = self.incoming.lock().unwrap_or_else(|e| e.into_inner());
        incoming.retain(|_, value| {
            value.event.expires > protocol::now() && value.event.session == current.id
        });
        match request {
            Request::Begin(event) => {
                event.validate(&peer.profile.id, &config.profile.id, &current.id)?;
                validate_limits(&event, &peer.policy)?;
                if let Some(existing) = incoming.get(&event.id) {
                    if serde_json::to_vec(&existing.event)? != serde_json::to_vec(&event)? {
                        return fail("相同事件标识携带了不同内容");
                    }
                    return Ok((None, false));
                }
                if incoming.len() >= MAX_INCOMING_EVENTS
                    || incoming
                        .values()
                        .map(|i| i.event.raw_size() as u64)
                        .sum::<u64>()
                        + event.raw_size() as u64
                        > MAX_INCOMING_BYTES
                {
                    return fail("接收会话资源已满");
                }
                let chunks = event
                    .formats
                    .iter()
                    .map(|f| &f.resource)
                    .chain(event.files.iter().map(|f| &f.resource))
                    .map(|r| (r.id.clone(), vec![None; r.chunks]))
                    .collect();
                incoming.insert(event.id.clone(), Incoming { event, chunks });
                Ok((None, false))
            }
            Request::Chunk {
                event,
                session,
                resource,
                index,
                data,
            } => {
                if session != current.id {
                    return fail("资源分片属于旧会话");
                }
                let item = incoming
                    .get_mut(&event)
                    .filter(|i| i.event.sender == peer.profile.id)
                    .ok_or_else(|| super::Error::Message("资源事件不存在或已过期".into()))?;
                let descriptor = item
                    .event
                    .formats
                    .iter()
                    .map(|f| &f.resource)
                    .chain(item.event.files.iter().map(|f| &f.resource))
                    .find(|r| r.id == resource)
                    .ok_or_else(|| super::Error::Message("分片资源未在清单中声明".into()))?;
                if index >= descriptor.chunks {
                    return fail("资源分片下标无效");
                }
                let expected = if index + 1 == descriptor.chunks {
                    descriptor.length.saturating_sub(index * CHUNK_BYTES)
                } else {
                    CHUNK_BYTES
                };
                if data.len() != expected {
                    return fail("资源分片长度无效");
                }
                let slot = &mut item
                    .chunks
                    .get_mut(&resource)
                    .ok_or_else(|| super::Error::Message("资源不存在".into()))?[index];
                if slot.as_ref().is_some_and(|old| old != &data) {
                    return fail("重复分片内容不一致");
                }
                *slot = Some(data);
                Ok((None, false))
            }
            Request::Commit { event, session } => {
                if session != current.id {
                    return fail("提交事件属于旧会话");
                }
                let item = incoming
                    .get(&event)
                    .filter(|i| i.event.sender == peer.profile.id)
                    .ok_or_else(|| super::Error::Message("待提交事件不存在".into()))?;
                if item
                    .chunks
                    .values()
                    .any(|chunks| chunks.iter().any(Option::is_none))
                {
                    return fail("资源分片尚未完整到达");
                }
                let item = incoming
                    .remove(&event)
                    .ok_or_else(|| super::Error::Message("待提交事件不存在".into()))?;
                drop(incoming);
                // 完整性校验与入库不占用分片表，其他文本和资源可继续接收。
                let resources = item
                    .chunks
                    .into_iter()
                    .map(|(id, chunks)| (id, chunks.into_iter().flatten().flatten().collect()))
                    .collect();
                Ok((
                    None,
                    self.apply_received(config, peer, &item.event, &resources)?,
                ))
            }
            Request::Hello { .. } => unreachable!(),
        }
    }

    pub fn pull_dav(&self) -> Result<usize> {
        let config = self.config();
        if !config.enabled || !config.dav.enabled {
            return fail("请先开启同步和 WebDAV 通道");
        }
        self.poll_dav(false)
    }
    fn poll_dav(&self, automatic: bool) -> Result<usize> {
        let _poll = self.dav_poll.lock().unwrap_or_else(|e| e.into_inner());
        let config = self.config();
        if !config.enabled || !config.dav.enabled {
            return Ok(0);
        }
        if automatic
            && (!config.dav.auto_pull
                || Instant::now() < *self.next_pull.lock().unwrap_or_else(|e| e.into_inner()))
        {
            return Ok(0);
        }
        let generation = self.generation.load(Ordering::Acquire);
        let client = self.dav_client(&config.dav)?;
        let mut count = 0;
        let mut rejected = 0;
        let mut first_error = None;
        for peer in config.peers.iter().filter(|peer| config.dav_allowed(peer)) {
            if generation != self.generation.load(Ordering::Acquire) {
                break;
            }
            let outcome = match dav_cache::read(&client, &config, peer) {
                Ok(outcome) => outcome,
                Err(error) => {
                    self.result(&peer.profile.id, error.to_string());
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                    continue;
                }
            };
            rejected += outcome.rejected;
            if let Some(error) = outcome.first_error {
                self.result(
                    &peer.profile.id,
                    format!(
                        "已跳过 {} 条无效缓存，其余内容继续接收：{error}",
                        outcome.rejected
                    ),
                );
                first_error.get_or_insert(error);
            }
            for entry in outcome.entries {
                let data = self.data.read().unwrap_or_else(|e| e.into_inner());
                let received = repository::received(&data.0, &entry.event)?;
                drop(data);
                if received {
                    continue;
                }
                if let Err(error) = validate_limits(&entry.event, &peer.policy) {
                    self.result(&peer.profile.id, error.to_string());
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                    continue;
                }
                let id = entry.event.id.clone();
                if !self
                    .downloading
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(id.clone())
                {
                    count += 1;
                    continue;
                }
                let large = entry.event.raw_size() > 65536 || !entry.event.files.is_empty();
                let job = DavJob {
                    config: config.clone(),
                    generation,
                    peer: peer.clone(),
                    entry,
                    manual: !automatic,
                };
                let result = if large {
                    self.dav_bulk.try_send(job)
                } else {
                    self.dav_control.try_send(job)
                };
                if result.is_err() {
                    self.downloading
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&id);
                    self.set_message("WebDAV 接收队列已满，下一次拉取将继续处理");
                } else {
                    count += 1;
                }
            }
        }
        *self.next_pull.lock().unwrap_or_else(|e| e.into_inner()) =
            Instant::now() + Duration::from_secs(config.dav.poll_seconds.max(1));
        self.notify();
        if let Some(error) = first_error {
            let error = if rejected > 0 {
                super::Error::Message(format!(
                    "已安排接收 {count} 条内容，跳过 {rejected} 条无法处理的缓存，其余内容继续接收。首个问题：{error}。请确认两台设备均已更新到同一版本；仍失败的条目可在发送端重新上传。"
                ))
            } else {
                error
            };
            self.set_message(&error.to_string());
            return Err(error);
        }
        self.set_message("");
        Ok(count)
    }
    fn receive_dav_job(&self, job: &DavJob) -> Result<()> {
        if job.generation != self.generation.load(Ordering::Acquire) {
            return Ok(());
        }
        {
            let current = self.config.read().unwrap_or_else(|e| e.into_inner());
            if job.generation != self.generation.load(Ordering::Acquire)
                || !current.dav_allowed(&job.peer)
            {
                return fail("WebDAV 设置已变更，已取消接收");
            }
            validate_limits(&job.entry.event, &job.peer.policy)?;
            let data = self.data.read().unwrap_or_else(|e| e.into_inner());
            if repository::received(&data.0, &job.entry.event)? {
                return Ok(());
            }
        }
        let client = self.dav_client(&job.config.dav)?;
        let resources = dav_cache::resources(&client, &job.config, &job.peer, &job.entry)?;
        // 下载期间可以关闭通道；提交时再次核对当前信任和配置代次。
        let config = self.config.read().unwrap_or_else(|e| e.into_inner());
        if job.generation != self.generation.load(Ordering::Acquire)
            || !config.dav_allowed(&job.peer)
            || !config.peers.iter().any(|peer| {
                peer.profile.key == job.peer.profile.key && peer.profile.id == job.peer.profile.id
            })
        {
            return fail("WebDAV 通道已关闭或设备已撤销，已取消接收");
        }
        job.entry
            .event
            .validate_cache(&job.peer.profile.id, &config.profile.id)?;
        validate_limits(&job.entry.event, &job.peer.policy)?;
        self.apply_received(&config, &job.peer, &job.entry.event, &resources)?;
        self.result(&job.peer.profile.id, "已从 WebDAV 缓存接收".into());
        Ok(())
    }
    fn apply_received(
        &self,
        config: &Config,
        peer: &Peer,
        event: &Event,
        resources: &HashMap<String, Vec<u8>>,
    ) -> Result<bool> {
        // 串行提交和写回，避免并发接收时旧内容晚于新内容写入剪贴板。
        let writer = self
            .clipboard_writer
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let data = self.data.read().unwrap_or_else(|e| e.into_inner());
        let outcome = repository::apply_with_feedback(
            &data.0,
            &data.1,
            event,
            resources,
            &peer.profile.name,
        )?;
        if config.copy_on_receive
            && let Some(item_id) = outcome.clipboard_item
            && let Err(error) = writer(&data.0, &data.1, item_id)
        {
            self.queue_notice(
                format!("内容已接收，但未能写入系统剪贴板，可在 QcClip 中手动复制：{error}"),
                true,
                false,
            );
        }
        self.received_feedback(&outcome);
        Ok(outcome.applied)
    }

    fn received_feedback(&self, outcome: &repository::ApplyOutcome) {
        if let Some(change) = outcome.change {
            self.received_changes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .entry(change.item_id)
                .and_modify(|pending| pending.merge(change))
                .or_insert(change);
        }
        if outcome.note_conflict {
            self.queue_notice("收到的内容带有不同备注，已保留本机备注；内容和标签已接收，可在发送设备查看另一份备注。".into(), false, true);
        }
        self.notify();
    }
    fn send_dav(&self, peer: &Peer, packet: &Packet, generation: u64) -> Result<()> {
        let config = self.ensure_sending(peer, generation, &packet.event, false)?;
        if !packet.event.mode.manual() && !config.dav.auto_upload {
            return fail("WebDAV 自动上传已关闭");
        }
        let client = self.dav_client(&config.dav)?;
        let journal = self.store.cache_journal_path(&dav_cache::scope(&config));
        let prepared = dav_cache::prepare(&config, peer, packet)?;
        let result = (|| {
            if let Some((id, bytes)) = &prepared.object {
                self.ensure_sending(peer, generation, &packet.event, false)?;
                self.dav_cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .stage(&client, &config, &prepared, journal.clone())?;
                // 大资源上传不占索引锁，小条目可先发布；最终索引提交仍然串行。
                client.put(&dav_cache::object_path(&config.profile.key, id)?, bytes)?;
            }
            self.ensure_sending(peer, generation, &packet.event, false)?;
            self.dav_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .publish(&client, &config, &prepared, journal, || {
                    self.ensure_sending(peer, generation, &packet.event, false)
                        .map(|_| ())
                })
        })();
        if let Err(error) = result {
            self.dav_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .abandon(&config, &prepared);
            return Err(error);
        }
        let suffix = if packet.skipped.is_empty() {
            String::new()
        } else {
            format!("；部分上传：{}", packet.skipped.join("；"))
        };
        self.result(
            &peer.profile.id,
            format!("已上传 WebDAV 缓存，对方可稍后拉取{suffix}"),
        );
        Ok(())
    }
}

fn check_pairing_dav(
    current: &super::config::DavConfig,
    expected: &super::config::DavConfig,
) -> Result<()> {
    if current.url != expected.url
        || current.username != expected.username
        || current.password != expected.password
    {
        return fail("WebDAV 设置已改变，请重新添加设备");
    }
    Ok(())
}

fn validate_limits(event: &Event, policy: &super::config::Policy) -> Result<()> {
    let mut total = 0usize;
    for file in &event.files {
        if !policy.file_contents
            || file.resource.length > policy.file_limit_mb as usize * 1024 * 1024
        {
            return fail("接收文件未启用或超过单文件限制");
        }
        total += file.resource.length;
    }
    if total > policy.files_limit_mb as usize * 1024 * 1024 {
        return fail("接收文件总量超过限制");
    }
    for f in &event.formats {
        if f.resource.length > super::config::MAX_EVENT_BYTES {
            return fail("接收格式超过事件上限");
        }
        if f.kind == "image" && f.resource.length > policy.image_limit_mb as usize * 1024 * 1024 {
            return fail("接收图片超过限制");
        }
    }
    Ok(())
}

fn readable_seconds(seconds: u64) -> String {
    match seconds {
        0..120 => format!("{seconds} 秒"),
        120..7200 => format!("{} 分钟", seconds / 60),
        _ => format!("{} 小时", seconds / 3600),
    }
}

fn success(channel: &str, packet: &Packet) -> String {
    let base = format!(
        "已通过 {channel} 入库 · {} 个格式 · {} 字节",
        packet.event.formats.len(),
        packet.event.raw_size()
    );
    if packet.skipped.is_empty() {
        base
    } else {
        format!("{base}；部分发送：{}", packet.skipped.join("；"))
    }
}

#[cfg(test)]
mod update_tests {
    use super::*;
    use crate::services::sync::tests::{Fixture, paired, runtime};

    #[test]
    fn clipboard_receiving_defaults_on_and_can_be_disabled_without_losing_identity() {
        let config = Config::fresh();
        assert!(config.copy_on_receive);
        let mut json = serde_json::to_value(&config).unwrap();
        json.as_object_mut().unwrap().remove("copy_on_receive");
        let loaded: Config = serde_json::from_value(json).unwrap();
        assert!(loaded.copy_on_receive);
        assert_eq!(loaded.profile, config.profile);
        let mut disabled = config;
        disabled.copy_on_receive = false;
        let loaded: Config =
            serde_json::from_slice(&serde_json::to_vec(&disabled).unwrap()).unwrap();
        assert!(!loaded.copy_on_receive);
    }

    #[test]
    fn clipboard_receiving_respects_toggle_duplicates_touches_and_late_events() {
        let source = Fixture::new();
        let target = Fixture::new();
        let (mut config, _) = paired();
        let peer = config.peers[0].clone();
        let mut receiver = runtime(&target, config.clone());
        let writes = Arc::new(Mutex::new(Vec::new()));
        let written = writes.clone();
        receiver.clipboard_writer = Mutex::new(Box::new(move |_, _, item| {
            written.lock().unwrap().push(item);
            Ok(())
        }));
        let item = source.capture("接收后可以直接粘贴");
        let mut packet = source.packet(item, Mode::ManualFavorite, 3);
        packet.event.sender = peer.profile.id.clone();
        assert!(
            receiver
                .apply_received(&config, &peer, &packet.event, &packet.resources)
                .unwrap()
        );
        assert_eq!(target.count("clipboard_history"), 0);
        assert_eq!(writes.lock().unwrap().len(), 1);
        assert!(
            !receiver
                .apply_received(&config, &peer, &packet.event, &packet.resources)
                .unwrap()
        );
        assert_eq!(writes.lock().unwrap().len(), 1);
        let mut late = source.packet(item, Mode::ManualContent, 2);
        late.event.sender = peer.profile.id.clone();
        assert!(
            receiver
                .apply_received(&config, &peer, &late.event, &late.resources)
                .unwrap()
        );
        assert_eq!(writes.lock().unwrap().len(), 1);

        let mut touch = source.packet(item, Mode::Touch, 4);
        touch.event.sender = peer.profile.id.clone();
        assert!(
            receiver
                .apply_received(&config, &peer, &touch.event, &touch.resources)
                .unwrap()
        );
        assert_eq!(writes.lock().unwrap().len(), 2);
        config.copy_on_receive = false;
        let mut next = source.packet(source.capture("只入库"), Mode::NewContent, 5);
        next.event.sender = peer.profile.id.clone();
        assert!(
            receiver
                .apply_received(&config, &peer, &next.event, &next.resources)
                .unwrap()
        );
        assert_eq!(target.count("clipboard_items"), 2);
        assert_eq!(writes.lock().unwrap().len(), 2);
        assert!(
            super::super::activity::latest(&target.db)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn clipboard_failure_keeps_received_content_and_reports_without_retrying_receipt() {
        let source = Fixture::new();
        let target = Fixture::new();
        let (config, _) = paired();
        let peer = &config.peers[0];
        let mut receiver = runtime(&target, config.clone());
        receiver.clipboard_writer =
            Mutex::new(Box::new(|_, _, _| Err("剪贴板被其他程序占用".into())));
        let packet = source.packet(source.capture("写入失败仍保留内容"), Mode::NewContent, 1);
        assert!(
            receiver
                .apply_received(&config, peer, &packet.event, &packet.resources)
                .unwrap()
        );
        assert_eq!(target.count("clipboard_items"), 1);
        assert_eq!(receiver.take_received_changes().len(), 1);
        let notices = receiver.notices.lock().unwrap();
        assert_eq!(notices.len(), 1);
        assert!(notices[0].failed);
        assert!(notices[0].message.contains("已接收"));
        drop(notices);
        assert!(
            !receiver
                .apply_received(&config, peer, &packet.event, &packet.resources)
                .unwrap()
        );
        assert_eq!(receiver.notices.lock().unwrap().len(), 1);
    }

    #[test]
    fn status_wakeups_do_not_discard_or_duplicate_pending_row_changes() {
        let fixture = Fixture::new();
        let (config, _) = paired();
        let mut runtime = runtime(&fixture, config);
        let (sender, receiver) = async_channel::bounded(1);
        runtime.changed = sender;
        runtime.notify();
        let mut change = super::super::ReceivedChange {
            item_id: 7,
            history_id: Some(5),
            favorite_id: None,
            tags_changed: true,
        };
        runtime.received_feedback(&repository::ApplyOutcome {
            applied: true,
            change: Some(change),
            ..Default::default()
        });
        change.favorite_id = Some(9);
        change.tags_changed = false;
        runtime.received_feedback(&repository::ApplyOutcome {
            applied: true,
            change: Some(change),
            ..Default::default()
        });
        runtime.set_message("只更新连接状态");
        assert_eq!(receiver.len(), 1);
        let pending = runtime.take_received_changes();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].history_id, Some(5));
        assert_eq!(pending[0].favorite_id, Some(9));
        assert!(pending[0].tags_changed);
        assert!(runtime.take_received_changes().is_empty());
    }
}
