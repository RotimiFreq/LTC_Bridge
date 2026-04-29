// src/wing/mod.rs
// ─────────────────────────────────────────────────────────────────────────────
// Behringer Wing — confirmed from official documentation:
//
//   PORT 2222 (native UDP):
//     Send "WING?" → Wing replies with ID string + starts pushing meter data
//     Keep sending "WING?" every 3 s to maintain the stream
//
//   PORT 2223 (OSC UDP):
//     All parameter get/set commands use proper OSC encoding
//     Confirmed paths: /ch/1/mute (int), /ch/1/fdr (float), /ch/1/$name (string)
//
//   The two sockets are separate — discovery on 2222, OSC on 2223.
// ─────────────────────────────────────────────────────────────────────────────

use anyhow::{Context, Result};
use rosc::{encoder, OscMessage, OscPacket, OscType};
use std::net::{SocketAddr, UdpSocket};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

// ─────────────────────────────────────────────────────────────────────────────
// Public types
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct TimecodeFrame {
    pub hours: u8,
    pub mins: u8,
    pub secs: u8,
    pub frames: u8,
}

#[derive(Debug, Clone)]
pub struct WingConfig {
    pub console_ip: String,
    pub native_port: u16,
    pub osc_port: u16,
    pub discovery_interval: Duration,
}

impl Default for WingConfig {
    fn default() -> Self {
        Self {
            console_ip:         "192.168.1.20".to_string(),
            native_port:        2222,
            osc_port:           2223,
            discovery_interval: Duration::from_secs(3),
        }
    }
}

#[derive(Debug, PartialEq)]
enum ConnectionState {
    Discovering,
    Connected,
    Stale,
}

// ─────────────────────────────────────────────────────────────────────────────
// WingConn
// ─────────────────────────────────────────────────────────────────────────────

pub struct WingConn {
    native_socket: UdpSocket,
    osc_socket: UdpSocket,
    native_addr: SocketAddr,
    osc_addr: SocketAddr,
    config: WingConfig,
    state: ConnectionState,
    last_discovery: Option<Instant>,
    last_reply: Option<Instant>,
    pub last_fader_send: Option<Instant>,  
}

impl WingConn {
    pub fn new(config: WingConfig) -> Result<Self> {
        let native_addr: SocketAddr =
            format!("{}:{}", config.console_ip, config.native_port)
                .parse()
                .with_context(|| format!("Invalid native address: {}", config.console_ip))?;

        let osc_addr: SocketAddr =
            format!("{}:{}", config.console_ip, config.osc_port)
                .parse()
                .with_context(|| format!("Invalid OSC address: {}", config.console_ip))?;

        let native_socket = UdpSocket::bind("0.0.0.0:0")
            .context("Failed to bind native UDP socket")?;
        native_socket.set_broadcast(true)
            .context("Failed to enable broadcast on native socket")?;
        native_socket.set_nonblocking(true)
            .context("Failed to set native socket non-blocking")?;

        let osc_socket = UdpSocket::bind("0.0.0.0:0")
            .context("Failed to bind OSC UDP socket")?;
        osc_socket.set_nonblocking(true)
            .context("Failed to set OSC socket non-blocking")?;

        let local = native_socket.local_addr()
            .map(|a| a.to_string())
            .unwrap_or_else(|_| "?".into());

        println!("[Wing] Native socket {} -> {}", local, native_addr);
        println!("[Wing] OSC socket -> {}", osc_addr);

        Ok(Self {
            native_socket,
            osc_socket,
            native_addr,
            osc_addr,
            config,
            state: ConnectionState::Discovering,
            last_discovery: None,
            last_reply: None,
             last_fader_send: None,
        })
    }

    fn send_discovery(&mut self) -> Result<()> {
        self.native_socket.send_to(b"WING?", self.native_addr)
            .with_context(|| format!("WING? send to {} failed", self.native_addr))?;
        self.last_discovery = Some(Instant::now());
        println!("[Wing] -> WING? to {}", self.native_addr);
        Ok(())
    }

    fn send_osc(&self, path: &str, value: OscType) -> Result<()> {
        let msg = OscPacket::Message(OscMessage {
            addr: path.to_string(),
            args: vec![value],
        });
        let bytes = encoder::encode(&msg)
            .with_context(|| format!("OSC encode failed for path: {}", path))?;
        self.osc_socket.send_to(&bytes, self.osc_addr)
            .with_context(|| format!("OSC send to {} failed", self.osc_addr))?;
        Ok(())
    }

    pub fn send_timecode(&self, tc: TimecodeFrame) -> Result<()> {
        let tc_str = format!(
            "{:02}:{:02}:{:02}:{:02}",
            tc.hours, tc.mins, tc.secs, tc.frames
        );
        self.send_osc("/ch/1/$name", OscType::String(tc_str))
    }

   pub fn send_fader(&mut self, ch: u8, level: f32) -> Result<()> {
        if let Some(t) = self.last_fader_send {
            if t.elapsed() < Duration::from_millis(400) {
                return Ok(());
            }
        }
        self.last_fader_send = Some(Instant::now());
        let path = format!("/ch/{}/fdr", ch);
        self.send_osc(&path, OscType::Float(level))
    }

    pub fn send_mute(&self, ch: u8, muted: bool) -> Result<()> {
        let path = format!("/ch/{}/mute", ch);
        self.send_osc(&path, OscType::Int(if muted { 1 } else { 0 }))
    }

    fn try_recv_native(&self) -> Option<(Vec<u8>, SocketAddr)> {
        let mut buf = [0u8; 1024];
        match self.native_socket.recv_from(&mut buf) {
            Ok((n, src)) => Some((buf[..n].to_vec(), src)),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => None,
            Err(e) => { eprintln!("[Wing] recv error: {}", e); None }
        }
    }

    fn handle_packet(&mut self, data: &[u8], src: SocketAddr) {
        if src.ip().to_string() != self.config.console_ip {
            return;
        }
        self.last_reply = Some(Instant::now());

        if let Ok(text) = std::str::from_utf8(data) {
            let text = text.trim();
            if text.starts_with("WING") && text.contains(',') {
                let parts: Vec<&str> = text.split(',').collect();
                let name     = parts.get(2).unwrap_or(&"?");
                let model    = parts.get(3).unwrap_or(&"?");
                let firmware = parts.get(5).unwrap_or(&"?");
                if self.state != ConnectionState::Connected {
                    println!(
                        "[Wing] Connected -- \"{}\" {} fw:{}",
                        name, model, firmware
                    );
                    self.state = ConnectionState::Connected;
                }
            }
        }
    }

    pub fn tick(&mut self) -> Result<()> {
        while let Some((data, src)) = self.try_recv_native() {
            self.handle_packet(&data, src);
        }

        match self.state {
            ConnectionState::Discovering => {
                if self.last_discovery
                    .map(|t| t.elapsed() > self.config.discovery_interval)
                    .unwrap_or(true)
                {
                    self.send_discovery()?;
                }
            }

            ConnectionState::Connected => {
                if self.last_discovery
                    .map(|t| t.elapsed() > self.config.discovery_interval)
                    .unwrap_or(true)
                {
                    self.native_socket.send_to(b"WING?", self.native_addr).ok();
                    self.last_discovery = Some(Instant::now());
                }
                if self.last_reply
                    .map(|t| t.elapsed() > Duration::from_secs(15))
                    .unwrap_or(false)
                {
                    eprintln!("[Wing] No reply in 15s -- rediscovering...");
                    self.state = ConnectionState::Stale;
                }
            }

            ConnectionState::Stale => {
                self.state = ConnectionState::Discovering;
            }
        }

        Ok(())
    }

    pub fn is_connected(&self) -> bool {
        self.state == ConnectionState::Connected
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Thread B entry point
// ─────────────────────────────────────────────────────────────────────────────

pub fn run_wing_thread(config: WingConfig, rx: Receiver<TimecodeFrame>) -> Result<()> {
    println!("[Wing] Thread starting");
    let mut wing = WingConn::new(config)?;
    let mut cues = crate::cues::build_cue_list();
    let mut cue_started = false;

    // Warmup — wait for 3 stable consecutive frames before trusting TC
    let mut warmup_frames: u8 = 0;
    let mut last_warmup_tc: Option<TimecodeFrame> = None;

    loop {
        let mut last_tc: Option<TimecodeFrame> = None;
        loop {
            match rx.try_recv() {
                Ok(frame) => last_tc = Some(frame),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    println!("[Wing] Channel closed -- exiting.");
                    return Ok(());
                }
            }
        }

        if let Some(tc) = last_tc {
            if wing.is_connected() {
                if warmup_frames < 3 {
                    // Check if this frame is within 2 seconds of the last one
                    let stable = match last_warmup_tc {
                        None => false,
                        Some(prev) => {
                            let prev_secs = prev.hours as i32 * 3600
                                + prev.mins  as i32 * 60
                                + prev.secs  as i32;
                            let curr_secs = tc.hours as i32 * 3600
                                + tc.mins  as i32 * 60
                                + tc.secs  as i32;
                            (curr_secs - prev_secs).abs() <= 2
                        }
                    };
                    if stable {
                        warmup_frames += 1;
                        println!("[LTC] Warmup {}/3 — TC stable at {:02}:{:02}:{:02}:{:02}",
                            warmup_frames, tc.hours, tc.mins, tc.secs, tc.frames);
                    } else {
                        if warmup_frames > 0 {
                            println!("[LTC] Warmup reset — rogue frame {:02}:{:02}:{:02}:{:02}",
                                tc.hours, tc.mins, tc.secs, tc.frames);
                        }
                        warmup_frames = 0;
                    }
                    last_warmup_tc = Some(tc);
                } else {
                    // Check for rogue frame mid-session
                    let is_rogue = match last_warmup_tc {
                        None => false,
                        Some(prev) => {
                            let prev_secs = prev.hours as i32 * 3600
                                + prev.mins  as i32 * 60
                                + prev.secs  as i32;
                            let curr_secs = tc.hours as i32 * 3600
                                + tc.mins  as i32 * 60
                                + tc.secs  as i32;
                            (curr_secs - prev_secs).abs() > 30
                        }
                    };

                    if is_rogue {
                        println!("[LTC] Rogue frame mid-session — ignoring {:02}:{:02}:{:02}:{:02}",
                            tc.hours, tc.mins, tc.secs, tc.frames);
                    } else {
                        last_warmup_tc = Some(tc);
                       if let Err(e) = crate::cues::execute_cues(&mut cues, tc, &mut wing, &mut cue_started) {
                            eprintln!("[Cue] Error: {:#}", e);
                        }
                    }
                }
            }
        }

        if let Err(e) = wing.tick() {
            eprintln!("[Wing] Tick error: {:#}", e);
        }

        std::thread::sleep(Duration::from_millis(100));
    }
}