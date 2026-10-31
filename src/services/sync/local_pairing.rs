use std::{
    collections::HashMap,
    net::SocketAddr,
    time::{Duration, Instant},
};

use crypto_box::aead::{OsRng, rand_core::RngCore};
use serde::{Deserialize, Serialize};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use uuid::Uuid;
use zeroize::Zeroizing;

use super::{
    Result, Runtime,
    config::{Peer, Profile},
    fail, lan, pairing,
};

const WINDOW: u64 = 300;
const REQUEST_TTL: u64 = 180;
const DISCOVERY: &str = "QcClip/pair/1";

#[derive(Clone, Serialize, Deserialize)]
pub struct Nearby {
    marker: String,
    pub profile: Profile,
    pub port: u16,
    token: String,
    #[serde(skip)]
    pub address: String,
}

#[derive(Clone)]
pub struct RequestView {
    pub profile: Profile,
    pub incoming: bool,
    pub pin: Option<String>,
    pub authenticated: bool,
    pub added: bool,
    pub rejected: bool,
}

#[derive(Clone, Default)]
pub struct Snapshot {
    pub active: bool,
    pub nearby: Vec<Nearby>,
    pub request: Option<RequestView>,
    pub message: String,
}

#[derive(Default)]
pub(super) struct State {
    window: Option<(String, Instant)>,
    nearby: HashMap<String, (Nearby, Instant)>,
    pending: Option<Pending>,
    knocks: u8,
    message: String,
}

struct Pending {
    id: String,
    peer: Nearby,
    incoming: bool,
    pin: Option<Zeroizing<String>>,
    key: Option<Zeroizing<String>>,
    attempts: u8,
    authenticated: bool,
    added: bool,
    rejected: bool,
    created: Instant,
}

#[derive(Serialize, Deserialize)]
pub(super) struct Wire {
    pub pairing: Request,
}

#[derive(Serialize, Deserialize)]
pub(super) enum Request {
    Knock {
        token: String,
        profile: Profile,
        port: u16,
    },
    Authenticate {
        id: String,
        message: Vec<u8>,
    },
    Confirm {
        id: String,
        proof: Vec<u8>,
    },
    Status {
        id: String,
        proof: Vec<u8>,
    },
    Cancel {
        id: String,
    },
}

#[derive(Serialize, Deserialize)]
enum Response {
    Knocked { id: String },
    Authentication { message: Vec<u8>, proof: Vec<u8> },
    Status(Vec<u8>),
    Error(String),
}

#[derive(Serialize, Deserialize)]
struct Proof {
    id: String,
    profile: Profile,
    added: bool,
    rejected: bool,
}

impl State {
    pub fn active(&self) -> bool {
        self.window
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed().as_secs() < WINDOW)
    }
    fn pending(&mut self, id: &str) -> Result<&mut Pending> {
        self.pending
            .as_mut()
            .filter(|p| p.id == id && p.created.elapsed().as_secs() < REQUEST_TTL)
            .ok_or_else(|| super::Error::Message("配对请求已结束，请重新选择设备".into()))
    }
    pub fn snapshot(&self) -> Snapshot {
        let active = self.active();
        let mut nearby: Vec<_> = self
            .nearby
            .values()
            .filter(|(_, at)| active && at.elapsed().as_secs() < 12)
            .map(|(peer, _)| peer.clone())
            .collect();
        nearby.sort_by(|a, b| {
            a.profile
                .name
                .cmp(&b.profile.name)
                .then(a.profile.id.cmp(&b.profile.id))
        });
        Snapshot {
            active,
            nearby,
            request: self.pending.as_ref().map(|p| RequestView {
                profile: p.peer.profile.clone(),
                incoming: p.incoming,
                pin: p
                    .pin
                    .as_ref()
                    .filter(|_| p.created.elapsed().as_secs() < REQUEST_TTL && active)
                    .map(|pin| pin.to_string()),
                authenticated: p.authenticated,
                added: p.added,
                rejected: p.rejected
                    || (!p.added
                        && (p.created.elapsed().as_secs() >= REQUEST_TTL
                            || (!active && p.incoming))),
            }),
            message: self.message.clone(),
        }
    }
}

fn identity(profile: &Profile, id: &str) -> Vec<u8> {
    let mut value = b"QcClip/local-pair/1/".to_vec();
    value.extend(id.as_bytes());
    value.extend(serde_json::to_vec(profile).unwrap_or_default());
    value
}

fn shared_key(bytes: Vec<u8>) -> Zeroizing<String> {
    let bytes = Zeroizing::new(bytes);
    Zeroizing::new(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn rpc(address: &str, request: Request) -> Result<Response> {
    let address = super::config::validate_address(address)?;
    let mut stream = std::net::TcpStream::connect_timeout(&address, Duration::from_millis(900))
        .map_err(|_| {
            super::Error::Message(
                "无法连接这台设备，请确认对方仍在添加设备页面，或检查防火墙".into(),
            )
        })?;
    lan::prepare(&stream)?;
    lan::write(&mut stream, &Wire { pairing: request })?;
    let reply: Response = lan::read(&mut stream)?;
    match reply {
        Response::Error(error) => fail(&error),
        reply => Ok(reply),
    }
}

impl Runtime {
    pub fn begin_local_pairing(&self) -> Result<()> {
        if self.config().port == 0 {
            return fail("请先在高级连接设置中指定局域网端口");
        }
        let mut state = self.local_pairing.lock().unwrap_or_else(|e| e.into_inner());
        if !state.active() {
            *state = State::default();
            state.window = Some((Uuid::new_v4().to_string(), Instant::now()));
        }
        drop(state);
        self.notify();
        Ok(())
    }
    pub fn close_local_pairing(&self) {
        let mut state = self.local_pairing.lock().unwrap_or_else(|e| e.into_inner());
        state.window = None;
        state.nearby.clear();
        if let Some(pending) = &mut state.pending
            && !pending.added
        {
            pending.rejected = true;
            pending.pin = None;
        }
        drop(state);
        self.notify();
    }
    pub(super) fn local_pairing_active(&self) -> bool {
        let state = self.local_pairing.lock().unwrap_or_else(|e| e.into_inner());
        state.active()
            || state.pending.as_ref().is_some_and(|p| {
                p.incoming && p.authenticated && p.created.elapsed().as_secs() < REQUEST_TTL
            })
    }
    /// 配对窗口开启或仍有未完成的配对请求时，后台线程需要保持轮询。
    pub(super) fn local_pairing_busy(&self) -> bool {
        let state = self.local_pairing.lock().unwrap_or_else(|e| e.into_inner());
        state.active()
            || state.pending.as_ref().is_some_and(|p| {
                !p.added && !p.rejected && p.created.elapsed().as_secs() < REQUEST_TTL
            })
    }
    pub(super) fn pairing_advertisement(&self) -> Option<Vec<u8>> {
        let config = self.config();
        let state = self.local_pairing.lock().unwrap_or_else(|e| e.into_inner());
        if !state.active() {
            return None;
        }
        serde_json::to_vec(&Nearby {
            marker: DISCOVERY.into(),
            profile: config.profile,
            port: config.port,
            token: state.window.as_ref()?.0.clone(),
            address: String::new(),
        })
        .ok()
    }
    pub(super) fn discover_pairing(&self, bytes: &[u8], address: SocketAddr) {
        if bytes.len() > 1024 {
            return;
        }
        let Ok(mut peer) = serde_json::from_slice::<Nearby>(bytes) else {
            return;
        };
        if peer.marker != DISCOVERY
            || peer.profile.validate().is_err()
            || peer.port == 0
            || Uuid::parse_str(&peer.token).is_err()
        {
            return;
        }
        let config = self.config();
        if peer.profile.id == config.profile.id
            || peer.profile.key == config.profile.key
            || config.peers.iter().any(|p| p.profile.id == peer.profile.id)
        {
            return;
        }
        peer.address = SocketAddr::new(address.ip(), peer.port).to_string();
        let mut state = self.local_pairing.lock().unwrap_or_else(|e| e.into_inner());
        if !state.active() {
            return;
        }
        state
            .nearby
            .retain(|_, (_, at)| at.elapsed().as_secs() < 12);
        if state.nearby.len() < 32 || state.nearby.contains_key(&peer.profile.id) {
            state
                .nearby
                .insert(peer.profile.id.clone(), (peer, Instant::now()));
        }
        drop(state);
        self.notify();
    }
    pub fn request_local_pairing(&self, target: Nearby) -> Result<()> {
        {
            let state = self.local_pairing.lock().unwrap_or_else(|e| e.into_inner());
            if !state.active() {
                return fail("附近设备搜索已结束，请重新开始添加");
            }
            if state.pending.as_ref().is_some_and(|p| {
                !p.added && !p.rejected && p.created.elapsed().as_secs() < REQUEST_TTL
            }) {
                return fail("请先完成或取消当前配对请求");
            }
        }
        let config = self.config();
        let Response::Knocked { id } = rpc(
            &target.address,
            Request::Knock {
                token: target.token.clone(),
                profile: config.profile,
                port: config.port,
            },
        )?
        else {
            return fail("设备返回的配对资料无效");
        };
        if Uuid::parse_str(&id).is_err() {
            return fail("设备返回的配对资料无效");
        }
        let mut state = self.local_pairing.lock().unwrap_or_else(|e| e.into_inner());
        if !state.active()
            || state.pending.as_ref().is_some_and(|p| {
                !p.added && !p.rejected && p.created.elapsed().as_secs() < REQUEST_TTL
            })
        {
            return fail("当前配对状态已变化，请重新选择设备");
        }
        state.pending = Some(Pending {
            id,
            peer: target,
            incoming: false,
            pin: None,
            key: None,
            attempts: 0,
            authenticated: false,
            added: false,
            rejected: false,
            created: Instant::now(),
        });
        state.message.clear();
        drop(state);
        self.notify();
        Ok(())
    }
    pub fn authenticate_local_pairing(&self, pin: &str) -> Result<()> {
        if pin.len() != 6 || !pin.bytes().all(|b| b.is_ascii_digit()) {
            return fail("请输入另一台设备显示的 6 位配对码");
        }
        let (id, target) = {
            let mut state = self.local_pairing.lock().unwrap_or_else(|e| e.into_inner());
            let id = state
                .pending
                .as_ref()
                .map(|p| p.id.clone())
                .unwrap_or_default();
            let p = state.pending(&id)?;
            if p.incoming || p.rejected || p.authenticated {
                return fail("当前请求无需输入配对码");
            }
            (id, p.peer.clone())
        };
        let config = self.config();
        let (exchange, message) = Spake2::<Ed25519Group>::start_a(
            &Password::new(pin.as_bytes()),
            &Identity::new(&identity(&config.profile, &id)),
            &Identity::new(&identity(&target.profile, &id)),
        );
        let Response::Authentication { message, proof } = rpc(
            &target.address,
            Request::Authenticate {
                id: id.clone(),
                message,
            },
        )?
        else {
            return fail("设备返回的配对验证无效");
        };
        let key = shared_key(
            exchange
                .finish(&message)
                .map_err(|_| super::Error::Message("配对验证失败，请重新输入配对码".into()))?,
        );
        let remote: Proof = pairing::open(&key, "local-server", &proof).map_err(|_| {
            super::Error::Message("配对码不正确，请重新输入；连续错误将结束本次请求".into())
        })?;
        if remote.id != id || remote.profile != target.profile {
            return fail("设备身份不匹配，请取消并重新选择设备");
        }
        let proof = pairing::seal(
            &key,
            "local-client",
            &Proof {
                id: id.clone(),
                profile: config.profile,
                added: false,
                rejected: false,
            },
        )?;
        {
            let mut state = self.local_pairing.lock().unwrap_or_else(|e| e.into_inner());
            let pending = state.pending(&id)?;
            if pending.rejected {
                return fail("配对请求已取消");
            }
            pending.key = Some(key.clone());
            pending.authenticated = true;
        }
        let Response::Status(bytes) = rpc(
            &target.address,
            Request::Confirm {
                id: id.clone(),
                proof,
            },
        )?
        else {
            return fail("设备未确认配对验证");
        };
        let reply: Proof = pairing::open(&key, "local-status", &bytes)?;
        if reply.id != id || reply.profile != target.profile || reply.rejected {
            return fail("对方已拒绝或取消配对");
        }
        let mut state = self.local_pairing.lock().unwrap_or_else(|e| e.into_inner());
        let pending = state.pending(&id)?;
        if pending.rejected {
            return fail("配对请求已取消");
        }
        pending.key = Some(key);
        pending.authenticated = true;
        drop(state);
        self.notify();
        Ok(())
    }
    fn trust_local_peer(&self, profile: &Profile, address: &str) -> Result<()> {
        self.change(
            |config| {
                if let Some(existing) = config.peers.iter().find(|p| p.profile.id == profile.id) {
                    if existing.profile.key != profile.key {
                        return fail("设备身份已变化，请撤销旧设备后重新添加");
                    }
                    return Ok(());
                }
                config.peers.push(Peer {
                    profile: profile.clone(),
                    address: address.into(),
                    policy: Default::default(),
                });
                config.validate()
            },
            true,
        )
    }
    pub fn accept_local_pairing(&self) -> Result<()> {
        let mut state = self.local_pairing.lock().unwrap_or_else(|e| e.into_inner());
        let id = state
            .pending
            .as_ref()
            .map(|p| p.id.clone())
            .unwrap_or_default();
        if !state.active() {
            return fail("添加设备已超时，请重新开始");
        }
        let p = state.pending(&id)?;
        if !p.incoming || !p.authenticated || p.rejected {
            return fail("请先让对方输入配对码，通过验证后才能添加");
        }
        self.trust_local_peer(&p.peer.profile, &p.peer.address)?;
        p.added = true;
        p.pin = None;
        drop(state);
        self.notify();
        Ok(())
    }
    pub fn reject_local_pairing(&self) {
        let mut state = self.local_pairing.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = &mut state.pending
            && !p.added
        {
            p.rejected = true;
            p.pin = None;
        }
        drop(state);
        self.notify();
    }
    pub fn cancel_local_request(&self) -> Result<()> {
        let remote = {
            let state = self.local_pairing.lock().unwrap_or_else(|e| e.into_inner());
            state
                .pending
                .as_ref()
                .filter(|p| !p.incoming && !p.added)
                .map(|p| (p.id.clone(), p.peer.address.clone()))
        };
        self.reject_local_pairing();
        if let Some((id, address)) = remote
            && let Err(error) = rpc(&address, Request::Cancel { id })
        {
            tracing::debug!("本机已取消局域网配对，对端将按超时清理：{error}");
        }
        Ok(())
    }
    pub(super) fn poll_local_pairing(&self) {
        let task = {
            let state = self.local_pairing.lock().unwrap_or_else(|e| e.into_inner());
            state
                .pending
                .as_ref()
                .filter(|p| {
                    !p.incoming
                        && p.authenticated
                        && !p.added
                        && !p.rejected
                        && p.created.elapsed().as_secs() < REQUEST_TTL
                })
                .and_then(|p| {
                    p.key
                        .as_ref()
                        .map(|key| (p.id.clone(), p.peer.clone(), key.clone()))
                })
        };
        let Some((id, peer, key)) = task else {
            return;
        };
        let outcome = (|| -> Result<Proof> {
            let proof = pairing::seal(
                &key,
                "local-client",
                &Proof {
                    id: id.clone(),
                    profile: self.config().profile,
                    added: false,
                    rejected: false,
                },
            )?;
            let Response::Status(bytes) = rpc(
                &peer.address,
                Request::Confirm {
                    id: id.clone(),
                    proof,
                },
            )?
            else {
                return fail("配对确认状态无效");
            };
            let proof: Proof = pairing::open(&key, "local-status", &bytes)?;
            if proof.id != id || proof.profile != peer.profile {
                return fail("配对确认身份不匹配");
            }
            Ok(proof)
        })();
        let mut state = self.local_pairing.lock().unwrap_or_else(|e| e.into_inner());
        let Ok(pending) = state.pending(&id) else {
            return;
        };
        if pending.rejected {
            return;
        }
        match outcome {
            Ok(proof) if proof.rejected => {
                pending.rejected = true;
                state.message = "对方已拒绝或取消配对".into();
            }
            Ok(proof) if proof.added => match self.trust_local_peer(&peer.profile, &peer.address) {
                Ok(()) => {
                    pending.added = true;
                    state.message.clear();
                    self.operation_notice(format!("已添加设备：{}", peer.profile.name), false);
                }
                Err(error) => state.message = error.to_string(),
            },
            Ok(_) => state.message.clear(),
            Err(error) => state.message = format!("{}，将自动重试", error),
        }
        drop(state);
        self.notify();
    }
    pub(super) fn receive_local_pairing(&self, wire: Wire, address: SocketAddr) -> Vec<u8> {
        let reply = self
            .process_local_pairing(wire.pairing, address)
            .unwrap_or_else(|error| Response::Error(error.to_string()));
        serde_json::to_vec(&reply).unwrap_or_default()
    }
    fn process_local_pairing(&self, request: Request, address: SocketAddr) -> Result<Response> {
        let config = self.config();
        let mut state = self.local_pairing.lock().unwrap_or_else(|e| e.into_inner());
        let is_confirmation = matches!(&request, Request::Confirm { .. });
        match request {
            Request::Knock {
                token,
                profile,
                port,
            } => {
                profile.validate()?;
                if !state.active() || state.window.as_ref().is_none_or(|w| w.0 != token) {
                    return fail("对方已结束添加设备，请让对方重新打开添加页面");
                }
                if profile.id == config.profile.id
                    || profile.key == config.profile.key
                    || port == 0
                    || config.peers.iter().any(|p| p.profile.id == profile.id)
                {
                    return fail("此设备已添加或身份无效");
                }
                if state
                    .pending
                    .as_ref()
                    .is_some_and(|p| !p.rejected && p.created.elapsed().as_secs() < REQUEST_TTL)
                {
                    return fail("对方正在处理配对请求，请稍后再试");
                }
                if state.knocks >= 6 {
                    return fail("本次添加请求过多，请让对方重新开始添加设备");
                }
                state.knocks += 1;
                let mut number = OsRng.next_u32();
                while number >= u32::MAX - u32::MAX % 1_000_000 {
                    number = OsRng.next_u32();
                }
                let id = Uuid::new_v4().to_string();
                state.pending = Some(Pending {
                    id: id.clone(),
                    peer: Nearby {
                        marker: DISCOVERY.into(),
                        profile: profile.clone(),
                        port,
                        token: String::new(),
                        address: SocketAddr::new(address.ip(), port).to_string(),
                    },
                    incoming: true,
                    pin: Some(Zeroizing::new(format!("{:06}", number % 1_000_000))),
                    key: None,
                    attempts: 0,
                    authenticated: false,
                    added: false,
                    rejected: false,
                    created: Instant::now(),
                });
                state.message.clear();
                drop(state);
                self.pairing_notice(format!(
                    "{} 请求添加设备，请在跨设备同步页面查看配对码",
                    profile.name
                ));
                self.notify();
                Ok(Response::Knocked { id })
            }
            Request::Cancel { id } => {
                let p = state.pending(&id)?;
                let expected = super::config::validate_address(&p.peer.address)?;
                if !p.incoming || p.added || expected.ip() != address.ip() {
                    return fail("无法取消此配对请求");
                }
                p.rejected = true;
                p.pin = None;
                drop(state);
                self.notify();
                Ok(Response::Knocked { id })
            }
            Request::Authenticate { id, message } => {
                let active = state.active();
                let p = state.pending(&id)?;
                if p.attempts >= 3 && !p.authenticated {
                    p.rejected = true;
                    p.pin = None;
                }
                if !active
                    || !p.incoming
                    || p.rejected
                    || p.authenticated
                    || p.attempts >= 3
                    || message.len() != 33
                {
                    return fail("配对验证已结束，请取消后重新添加");
                }
                p.attempts += 1;
                let (exchange, reply) = Spake2::<Ed25519Group>::start_b(
                    &Password::new(p.pin.as_ref().unwrap().as_bytes()),
                    &Identity::new(&identity(&p.peer.profile, &id)),
                    &Identity::new(&identity(&config.profile, &id)),
                );
                let key = shared_key(
                    exchange
                        .finish(&message)
                        .map_err(|_| super::Error::Message("配对验证消息无效".into()))?,
                );
                let proof = pairing::seal(
                    &key,
                    "local-server",
                    &Proof {
                        id,
                        profile: config.profile,
                        added: false,
                        rejected: false,
                    },
                )?;
                p.key = Some(key);
                Ok(Response::Authentication {
                    message: reply,
                    proof,
                })
            }
            Request::Confirm { id, proof } | Request::Status { id, proof } => {
                // 确认与轮询使用不同用途的密文，避免把轮询当成首次身份确认。
                let active = state.active();
                let p = state.pending(&id)?;
                if !active && !p.added {
                    p.rejected = true;
                    p.pin = None;
                }
                if !p.incoming {
                    return fail("配对请求方向无效");
                }
                let key = p
                    .key
                    .as_ref()
                    .ok_or_else(|| super::Error::Message("请先输入配对码完成验证".into()))?;
                if !is_confirmation && !p.authenticated {
                    return fail("请先完成配对码验证");
                }
                let client: Proof = pairing::open(
                    key,
                    if is_confirmation {
                        "local-client"
                    } else {
                        "local-poll"
                    },
                    &proof,
                )?;
                if client.id != id || client.profile != p.peer.profile {
                    return fail("配对身份不匹配");
                }
                if is_confirmation && !p.rejected {
                    p.authenticated = true;
                    p.pin = None;
                }
                let bytes = pairing::seal(
                    key,
                    "local-status",
                    &Proof {
                        id,
                        profile: config.profile,
                        added: p.added,
                        rejected: p.rejected,
                    },
                )?;
                drop(state);
                self.notify();
                Ok(Response::Status(bytes))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::sync::{
        config::Config,
        tests::{Fixture, runtime},
    };
    use std::{
        net::TcpListener,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        thread,
    };

    struct Device {
        runtime: Arc<Runtime>,
        fixture: Fixture,
        stop: Arc<AtomicBool>,
        server: Option<thread::JoinHandle<()>>,
    }
    impl Device {
        fn new() -> Self {
            let fixture = Fixture::new();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let mut config = Config::fresh();
            config.lan_enabled = false;
            config.port = listener.local_addr().unwrap().port();
            let runtime = Arc::new(runtime(&fixture, config));
            let stop = Arc::new(AtomicBool::new(false));
            let device = runtime.clone();
            let stopping = stop.clone();
            let server = thread::spawn(move || {
                while !stopping.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            device.handle_lan_stream(&mut stream).unwrap();
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2))
                        }
                        Err(error) => panic!("配对测试监听失败：{error}"),
                    }
                }
            });
            Self {
                runtime,
                fixture,
                stop,
                server: Some(server),
            }
        }
        fn discover(&self, other: &Self) -> Nearby {
            self.runtime.begin_local_pairing().unwrap();
            other.runtime.begin_local_pairing().unwrap();
            let advertisement = other.runtime.pairing_advertisement().unwrap();
            assert!(advertisement.len() <= 1024);
            self.runtime
                .discover_pairing(&advertisement, "127.0.0.1:1".parse().unwrap());
            self.runtime.snapshot().local_pairing.nearby[0].clone()
        }
        fn pin(&self) -> String {
            self.runtime
                .snapshot()
                .local_pairing
                .request
                .unwrap()
                .pin
                .unwrap()
        }
    }
    impl Drop for Device {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            self.server.take().unwrap().join().unwrap();
        }
    }

    #[test]
    fn discovery_pin_and_confirmation_establish_shared_trust_without_enabling_sync() {
        let a = Device::new();
        let b = Device::new();
        let identity_a = a.runtime.config();
        let identity_b = b.runtime.config();
        let nearby = a.discover(&b);
        assert!(a.runtime.config().peers.is_empty());
        a.runtime.request_local_pairing(nearby).unwrap();
        assert!(b.runtime.accept_local_pairing().is_err());
        let pin = b.pin();
        assert_eq!(pin.len(), 6);
        a.runtime.authenticate_local_pairing(&pin).unwrap();
        assert!(
            b.runtime
                .snapshot()
                .local_pairing
                .request
                .unwrap()
                .authenticated
        );
        assert!(a.runtime.config().peers.is_empty());
        assert!(b.runtime.config().peers.is_empty());
        b.runtime.accept_local_pairing().unwrap();
        a.runtime.poll_local_pairing();
        for (device, own, peer) in [
            (&a, &identity_a, &identity_b),
            (&b, &identity_b, &identity_a),
        ] {
            let config = device.runtime.config();
            assert_eq!(config.profile, own.profile);
            assert_eq!(config.secret, own.secret);
            assert_eq!(config.peers.len(), 1);
            assert_eq!(config.peers[0].profile, peer.profile);
            assert!(!config.enabled);
            assert!(!config.lan_enabled);
            assert!(config.dav.url.is_empty());
            assert!(config.peers[0].policy.lan && config.peers[0].policy.webdav);
            assert_eq!(device.fixture.count("clipboard_history"), 0);
        }
        for device in [&a.runtime, &b.runtime] {
            device
                .change(
                    |config| {
                        config.enabled = true;
                        config.lan_enabled = true;
                        Ok(())
                    },
                    true,
                )
                .unwrap();
        }
        let mut packet = a.fixture.packet(
            a.fixture.capture("附近添加后的真实局域网收发"),
            super::super::protocol::Mode::NewContent,
            1,
        );
        packet.event.sender = identity_a.profile.id.clone();
        packet.event.recipient = identity_b.profile.id.clone();
        let session = b.runtime.test_session();
        packet.event.session = session.id.clone();
        a.runtime
            .test_relay_send(&a.runtime.config().peers[0], &packet, session)
            .unwrap();
        assert_eq!(b.fixture.count("clipboard_history"), 1);
        // 重复读取完成状态不得重复添加；关闭搜索仍给对端保留短暂的确认读取窗口。
        b.runtime.close_local_pairing();
        assert!(b.runtime.local_pairing_active());
        a.runtime.poll_local_pairing();
        assert_eq!(a.runtime.config().peers.len(), 1);
        assert!(!a.runtime.snapshot().local_pairing.request.unwrap().incoming);
    }

    #[test]
    fn wrong_pin_is_rejected_and_online_attempts_are_bounded() {
        let a = Device::new();
        let b = Device::new();
        a.runtime.request_local_pairing(a.discover(&b)).unwrap();
        let correct = b.pin();
        let wrong = if correct == "000000" {
            "000001"
        } else {
            "000000"
        };
        for _ in 0..3 {
            assert!(a.runtime.authenticate_local_pairing(wrong).is_err());
        }
        assert!(a.runtime.authenticate_local_pairing(&correct).is_err());
        assert!(
            !b.runtime
                .snapshot()
                .local_pairing
                .request
                .unwrap()
                .authenticated
        );
        assert!(a.runtime.config().peers.is_empty());
        assert!(b.runtime.config().peers.is_empty());
    }

    #[test]
    fn rejection_expiry_and_stale_discovery_cannot_establish_trust() {
        let a = Device::new();
        let b = Device::new();
        let old = a.discover(&b);
        b.runtime.close_local_pairing();
        b.runtime.begin_local_pairing().unwrap();
        assert!(a.runtime.request_local_pairing(old).is_err());
        let nearby = a.discover(&b);
        a.runtime.request_local_pairing(nearby).unwrap();
        a.runtime.authenticate_local_pairing(&b.pin()).unwrap();
        b.runtime.reject_local_pairing();
        a.runtime.poll_local_pairing();
        assert!(a.runtime.snapshot().local_pairing.request.unwrap().rejected);
        assert!(a.runtime.config().peers.is_empty());
        assert!(b.runtime.config().peers.is_empty());
        b.runtime.close_local_pairing();
        b.runtime.begin_local_pairing().unwrap();
        a.runtime.request_local_pairing(a.discover(&b)).unwrap();
        let pin = b.pin();
        b.runtime
            .local_pairing
            .lock()
            .unwrap()
            .pending
            .as_mut()
            .unwrap()
            .created = Instant::now() - Duration::from_secs(REQUEST_TTL + 1);
        assert!(a.runtime.authenticate_local_pairing(&pin).is_err());
        assert!(b.runtime.accept_local_pairing().is_err());
    }

    #[test]
    fn cancelling_outgoing_request_releases_peer_without_waiting_for_timeout() {
        let a = Device::new();
        let b = Device::new();
        a.runtime.request_local_pairing(a.discover(&b)).unwrap();
        a.runtime.cancel_local_request().unwrap();
        assert!(a.runtime.snapshot().local_pairing.request.unwrap().rejected);
        assert!(b.runtime.snapshot().local_pairing.request.unwrap().rejected);
        a.runtime.request_local_pairing(a.discover(&b)).unwrap();
        assert!(!b.runtime.snapshot().local_pairing.request.unwrap().rejected);
        assert!(a.runtime.config().peers.is_empty());
        assert!(b.runtime.config().peers.is_empty());
    }

    #[test]
    fn discovery_is_temporary_bounded_and_does_not_accept_malformed_or_self_profiles() {
        let a = Device::new();
        let b = Device::new();
        b.runtime.begin_local_pairing().unwrap();
        let bytes = b.runtime.pairing_advertisement().unwrap();
        a.runtime
            .discover_pairing(&bytes, "127.0.0.1:1".parse().unwrap());
        assert!(a.runtime.snapshot().local_pairing.nearby.is_empty());
        a.runtime.begin_local_pairing().unwrap();
        let own = a.runtime.pairing_advertisement().unwrap();
        a.runtime
            .discover_pairing(&own, "127.0.0.1:1".parse().unwrap());
        a.runtime
            .discover_pairing(&[b'0'; 1025], "127.0.0.1:1".parse().unwrap());
        assert!(a.runtime.snapshot().local_pairing.nearby.is_empty());
        a.runtime
            .discover_pairing(&bytes, "127.0.0.1:1".parse().unwrap());
        assert_eq!(a.runtime.snapshot().local_pairing.nearby.len(), 1);
        a.runtime
            .local_pairing
            .lock()
            .unwrap()
            .window
            .as_mut()
            .unwrap()
            .1 = Instant::now() - Duration::from_secs(WINDOW + 1);
        assert!(a.runtime.pairing_advertisement().is_none());
        assert!(a.runtime.snapshot().local_pairing.nearby.is_empty());
        a.runtime.begin_local_pairing().unwrap();
        let receive = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        receive
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let send = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        send.send_to(&bytes, receive.local_addr().unwrap()).unwrap();
        let mut buffer = [0u8; 1025];
        let (length, address) = receive.recv_from(&mut buffer).unwrap();
        a.runtime.discover_pairing(&buffer[..length], address);
        assert_eq!(
            a.runtime.snapshot().local_pairing.nearby[0].address,
            format!("127.0.0.1:{}", b.runtime.config().port)
        );
    }

    #[test]
    fn pairing_ciphertexts_reject_tampering_wrong_direction_and_replay_to_other_identity() {
        let a = Config::fresh().profile;
        let b = Config::fresh().profile;
        let id = Uuid::new_v4().to_string();
        let (first, first_message) = Spake2::<Ed25519Group>::start_a(
            &Password::new(b"123456"),
            &Identity::new(&identity(&a, &id)),
            &Identity::new(&identity(&b, &id)),
        );
        let (second, second_message) = Spake2::<Ed25519Group>::start_b(
            &Password::new(b"123456"),
            &Identity::new(&identity(&a, &id)),
            &Identity::new(&identity(&b, &id)),
        );
        let first = shared_key(first.finish(&second_message).unwrap());
        let second = shared_key(second.finish(&first_message).unwrap());
        assert_eq!(first, second);
        let bytes = pairing::seal(
            &first,
            "local-client",
            &Proof {
                id,
                profile: a,
                added: false,
                rejected: false,
            },
        )
        .unwrap();
        assert!(pairing::open::<Proof>(&second, "local-client", &bytes).is_ok());
        assert!(pairing::open::<Proof>(&second, "local-poll", &bytes).is_err());
        let mut tampered = bytes;
        tampered[30] ^= 1;
        assert!(pairing::open::<Proof>(&second, "local-client", &tampered).is_err());
    }
}
