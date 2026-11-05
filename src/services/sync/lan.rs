use serde::{Serialize, de::DeserializeOwned};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    time::Duration,
};

use super::{
    Result, fail,
    protocol::{Envelope, MAX_FRAME},
};

pub fn read<T: DeserializeOwned>(stream: &mut impl Read) -> Result<T> {
    let mut length = [0u8; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_FRAME {
        return fail("局域网消息帧超过限制");
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub fn write<T: Serialize>(stream: &mut impl Write, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_FRAME {
        return fail("局域网消息帧超过限制");
    }
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(&bytes)?;
    stream.flush()?;
    Ok(())
}

pub fn exchange(address: SocketAddr, envelope: &Envelope) -> Result<Envelope> {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(900))?;
    prepare(&stream)?;
    write(&mut stream, envelope)?;
    read(&mut stream)
}

pub fn prepare(stream: &TcpStream) -> Result<()> {
    // Windows 接受的连接可能继承监听器的非阻塞状态。
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(8)))?;
    stream.set_write_timeout(Some(Duration::from_secs(8)))?;
    Ok(())
}

/// 局域网发现广播的目标地址：受限广播加上每块 IPv4 网卡的子网定向广播。
/// Windows 只会把 255.255.255.255 从默认路由网卡发出，ZeroTier 等虚拟网卡需要定向广播才能覆盖。
pub fn broadcast_targets() -> Vec<std::net::Ipv4Addr> {
    let mut targets = vec![std::net::Ipv4Addr::BROADCAST];
    for interface in if_addrs::get_if_addrs().unwrap_or_default() {
        let if_addrs::IfAddr::V4(v4) = interface.addr else {
            continue;
        };
        if v4.ip.is_loopback()
            || v4.ip.is_link_local()
            || v4.netmask == std::net::Ipv4Addr::BROADCAST
        {
            continue;
        }
        let broadcast = v4
            .broadcast
            .unwrap_or_else(|| (u32::from(v4.ip) | !u32::from(v4.netmask)).into());
        if broadcast != v4.ip && !targets.contains(&broadcast) {
            targets.push(broadcast);
        }
    }
    targets
}
