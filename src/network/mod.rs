// src/network.rs
use anyhow::{Context, Result};
use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct LocalInterface {
    pub name: String,
    pub ip: Ipv4Addr,
    pub prefix_len: Option<u8>,
    pub broadcast: Option<Ipv4Addr>,
}

#[derive(Debug, Clone)]
pub struct WingProbeResult {
    pub ip: Ipv4Addr,
    pub model: String,
    pub serial: String,
    pub firmware: String,
    //pub broadcast: Ipv4Addr, // the broadcast addr that worked
}

pub fn list_local_interfaces() -> Result<Vec<LocalInterface>> {
    let ifaces = if_addrs::get_if_addrs()
        .context("Failed to enumerate network interfaces")?;

    let mut result = Vec::new();
    for iface in ifaces {
        if iface.is_loopback() { continue; }
        match iface.addr {
            if_addrs::IfAddr::V4(ref v4) => {
                let ip = v4.ip;
                if ip.octets()[0] == 169 && ip.octets()[1] == 254 { continue; }
                let broadcast = v4.broadcast;
                result.push(LocalInterface {
                    name: iface.name.clone(),
                    ip,
                    prefix_len: Some(u32::from(v4.netmask).leading_ones() as u8),
                    broadcast,
                });
            }
            if_addrs::IfAddr::V6(_) => {}
        }
    }
    result.sort_by_key(|i| adapter_priority(&i.name));
    Ok(result)
}

fn adapter_priority(name: &str) -> u8 {
    let n = name.to_lowercase();
    if n.starts_with("eth") || n.starts_with("en") { 0 }
    else if n.starts_with("wlan") || n.starts_with("wi") { 1 }
    else if n.contains("vpn") || n.contains("tun") { 3 }
    else if n.contains("docker") || n.contains("veth") { 4 }
    else { 2 }
}

/// Send "WING?" to the subnet broadcast address on port 2222.
/// Wing reads our source port and replies with its ID string.
/// Filter replies to only accept from target_ip.
pub fn probe_wing(target_ip: Ipv4Addr, timeout: Duration) -> Option<WingProbeResult> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.set_broadcast(true).ok()?;
    socket.set_read_timeout(Some(timeout)).ok()?;

    

    // Send WING? directly to Wing IP — broadcast does not work
    let wing_addr = format!("{}:2222", target_ip);
    socket.send_to(b"WING?", &wing_addr).ok()?;

    let mut buf = [0u8; 512];
    let deadline = std::time::Instant::now();

    loop {
        if deadline.elapsed() >= timeout { return None; }
        match socket.recv_from(&mut buf) {
            Ok((n, src)) => {
                // Only accept from our target Wing
                if src.ip() != IpAddr::V4(target_ip) { continue; }
                let text = String::from_utf8_lossy(&buf[..n]);
                let text = text.trim();
                if text.starts_with("WING") && text.contains(',') {
                    let parts: Vec<&str> = text.split(',').collect();
                    return Some(WingProbeResult {
                        ip:       target_ip,
                        model:    parts.get(3).unwrap_or(&"wing-fullsize").to_string(),
                        serial:   parts.get(4).unwrap_or(&"?").to_string(),
                        firmware: parts.get(5).unwrap_or(&"?").to_string(),
                        //broadcast,
                    });
                }
            }
            Err(_) => return None,
        }
    }
}