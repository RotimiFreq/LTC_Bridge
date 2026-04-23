// src/wing.rs
// Phase 2: Behringer Wing OSC Bridge
// ─────────────────────────────────────────────────────────────────────────────
// Responsible for:
//   • Encoding / sending OSC packets over UDP (rosc)
//   • A state machine that keeps the Wing subscription alive
//   • Receiving the timecode from Thread A via mpsc and forwarding to the Wing

use anyhow::{Context, Result};
use rosc::{encoder, OscMessage, OscPacket, OscType};
use std::net::{SocketAddr, UdpSocket};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

// ─────────────────────────────────────────────────────────────────────────────
// Public types
// ─────────────────────────────────────────────────────────────────────────────

/// A decoded timecode frame sent from Thread A → Thread B over the mpsc channel.
#[derive(Debug, Clone, Copy)]
pub struct TimecodeFrame {
    pub hours: u8,
    pub mins: u8,
    pub secs: u8,
    pub frames: u8,
}

/// Runtime config.  In Phase 4 this will be loaded from wing.toml via serde.
#[derive(Debug, Clone)]
pub struct WingConfig {
    /// IP address of the Behringer Wing console
    pub console_ip: String,
    /// OSC port — Wing uses 2223 (NOT 10023 like the M32/X32)
    pub console_port: u16,
    /// Local UDP port we bind on to receive replies
    pub local_port: u16,
    /// Re-send /subscribe before Wing's 10 s timeout drops us
    pub heartbeat_interval: Duration,
}

impl Default for WingConfig {
    fn default() -> Self {
        Self {
            console_ip: "192.168.1.100".to_string(),
            console_port: 2222,
            local_port: 57120,
            heartbeat_interval: Duration::from_secs(8),
        }
    }
}

#[derive(Debug, PartialEq)]
enum ConnectionState {
    Disconnected,
    Probing,   // /info sent, waiting for reply
    Connected, // subscription active
    Stale,     // no reply in 15 s → reconnect
}

// ─────────────────────────────────────────────────────────────────────────────
// WingOsc
// ─────────────────────────────────────────────────────────────────────────────

pub struct WingOsc {
    socket: UdpSocket,
    console_addr: SocketAddr,
    config: WingConfig,
    state: ConnectionState,
    last_heartbeat: Option<Instant>,
    last_reply: Option<Instant>,
}

impl WingOsc {
    pub fn new(config: WingConfig) -> Result<Self> {
        let local_bind = format!("0.0.0.0:{}", config.local_port);
        let console_addr: SocketAddr = format!("{}:{}", config.console_ip, config.console_port)
            .parse()
            .context("Invalid console address — check your IP/port")?;

        let socket =
            UdpSocket::bind(&local_bind).with_context(|| format!("Cannot bind {}", local_bind))?;

        // Non-blocking so Thread B never stalls waiting for a UDP reply
        socket
            .set_nonblocking(true)
            .context("Cannot set socket non-blocking")?;

        println!(
            "[Wing] Socket bound on {}  →  console at {}",
            local_bind, console_addr
        );

        Ok(Self {
            socket,
            console_addr,
            config,
            state: ConnectionState::Disconnected,
            last_heartbeat: None,
            last_reply: None,
        })
    }

    // ── Low-level send / recv ─────────────────────────────────────────────────

    fn send(&self, msg: OscMessage) -> Result<()> {
        let bytes =
            encoder::encode(&OscPacket::Message(msg)).context("OSC encode failed")?;
        self.socket
            .send_to(&bytes, self.console_addr)
            .context("UDP send failed — is the Wing reachable?")?;
        Ok(())
    }

    fn try_recv(&self) -> Result<Option<OscPacket>> {
        let mut buf = [0u8; 1024];
        match self.socket.recv_from(&mut buf) {
            Ok((n, _)) => {
                let (_, pkt) =
                    rosc::decoder::decode_udp(&buf[..n]).context("OSC decode failed")?;
                Ok(Some(pkt))
            }
            // WouldBlock is not an error — just means nothing queued yet
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e).context("UDP recv error"),
        }
    }

    // ── Wing-specific OSC messages ────────────────────────────────────────────

    /// Probe: asks the Wing to identify itself.
    /// A valid /info reply means the console is alive → transitions to Connected.
    fn send_info_probe(&mut self) -> Result<()> {
        println!("[Wing] → /info  (probing...)");
        self.last_heartbeat = Some(Instant::now());
        self.send(OscMessage {
            addr: "/info".to_string(),
            args: vec![],
        })
    }

    /// Heartbeat: Wing's subscription model.
    /// Unlike the M32's bare /xremote, Wing wants:
    ///   /subscribe  <path glob>  <alias>  <f32 min>  <f32 max>
    fn send_subscribe(&mut self) -> Result<()> {
        println!("[Wing] → /subscribe  (heartbeat)");
        self.last_heartbeat = Some(Instant::now());
        self.send(OscMessage {
            addr: "/subscribe".to_string(),
            args: vec![
                OscType::String("/*".to_string()),
                OscType::String("ltcbridge".to_string()),
                OscType::Float(0.0),
                OscType::Float(1.0),
            ],
        })
    }

    // ── Timecode → Wing ───────────────────────────────────────────────────────

    /// Write the current timecode to the Wing.
    ///
    /// The Wing has no documented "timecode display" OSC path in its public spec,
    /// so we write to channel 1's name as a proven-working display hack.
    /// When you find the real path via Wireshark (see README), swap it here.
    fn send_timecode(&self, tc: TimecodeFrame) -> Result<()> {
        let tc_str = format!(
            "{:02}:{:02}:{:02}:{:02}",
            tc.hours, tc.mins, tc.secs, tc.frames
        );

        // ── Option A (active): channel name field as TC display ──
        self.send(OscMessage {
            addr: "/ch/1/name".to_string(),
            args: vec![OscType::String(tc_str.clone())],
        })?;

        // ── Option B (swap in once you've sniffed the Wing's TC path): ──
        // self.send(OscMessage {
        //     addr: "/show/time".to_string(),   // <-- replace with real path
        //     args: vec![OscType::String(tc_str)],
        // })?;

        Ok(())
    }

    // ── Incoming reply handler ────────────────────────────────────────────────

    fn handle_packet(&mut self, pkt: OscPacket) {
        self.last_reply = Some(Instant::now());

        match pkt {
            OscPacket::Message(msg) => {
                if msg.addr == "/info" {
                    // Wing replies to /info with strings: name, version, serial, firmware
                    let info: Vec<String> = msg
                        .args
                        .iter()
                        .filter_map(|a| {
                            if let OscType::String(s) = a {
                                Some(s.clone())
                            } else {
                                None
                            }
                        })
                        .collect();

                    println!("[Wing] ✓ Connected — {}", info.join(" | "));
                    self.state = ConnectionState::Connected;

                    // Subscribe immediately on first contact
                    if let Err(e) = self.send_subscribe() {
                        eprintln!("[Wing] Initial subscribe failed: {:#}", e);
                    }
                } else {
                    // Helpful during Phase 3 path discovery
                    println!("[Wing] ← {}  {:?}", msg.addr, msg.args);
                }
            }
            OscPacket::Bundle(bundle) => {
                // Wing can bundle multiple messages — unwrap and handle each
                for inner in bundle.content {
                    self.handle_packet(inner);
                }
            }
        }
    }

    // ── State machine tick ────────────────────────────────────────────────────

    fn tick_connection(&mut self) -> Result<()> {
        // Drain all queued UDP replies first
        loop {
            match self.try_recv()? {
                Some(pkt) => self.handle_packet(pkt),
                None => break,
            }
        }

        match self.state {
            ConnectionState::Disconnected => {
                self.send_info_probe()?;
                self.state = ConnectionState::Probing;
            }

            ConnectionState::Probing => {
                // Retry probe if no reply after 3 s
                if self.last_heartbeat.map(|t| t.elapsed() > Duration::from_secs(3)).unwrap_or(true) {
                    eprintln!("[Wing] Probe timeout — retrying /info...");
                    self.send_info_probe()?;
                }
            }

            ConnectionState::Connected => {
                // Re-subscribe before the Wing drops us
                if self
                    .last_heartbeat
                    .map(|t| t.elapsed() > self.config.heartbeat_interval)
                    .unwrap_or(true)
                {
                    self.send_subscribe()?;
                }

                // If the console has gone silent for 15 s, mark stale
                if self
                    .last_reply
                    .map(|t| t.elapsed() > Duration::from_secs(15))
                    .unwrap_or(false)
                {
                    eprintln!("[Wing] ⚠ No reply in 15 s — marking stale");
                    self.state = ConnectionState::Stale;
                }
            }

            ConnectionState::Stale => {
                eprintln!("[Wing] Reconnecting...");
                self.state = ConnectionState::Disconnected;
            }
        }

        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Thread B entry point  — called from main.rs
// ─────────────────────────────────────────────────────────────────────────────

/// Runs the Wing OSC engine.  Blocks forever until the channel sender drops
/// (which happens when the audio stream stops / Ctrl-C).
///
/// `rx` receives TimecodeFrame values from Thread A (the audio/LTC thread).
pub fn run_wing_thread(config: WingConfig, rx: Receiver<TimecodeFrame>) -> Result<()> {
    let mut wing = WingOsc::new(config)?;

    loop {
        // ── 1. Drain all pending timecode frames from Thread A ──
        // We drain (not block) so the state-machine tick always runs on time.
        let mut last_tc: Option<TimecodeFrame> = None;
        loop {
            match rx.try_recv() {
                Ok(frame) => last_tc = Some(frame), // keep only the newest
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    println!("[Wing] Channel closed — audio thread stopped. Exiting.");
                    return Ok(());
                }
            }
        }

        // ── 2. If we have a fresh frame AND we're connected, send it ──
        if let Some(tc) = last_tc {
            if wing.state == ConnectionState::Connected {
                if let Err(e) = wing.send_timecode(tc) {
                    eprintln!("[Wing] Timecode send error: {:#}", e);
                }
            }
        }

        // ── 3. Run the connection state machine ──
        if let Err(e) = wing.tick_connection() {
            eprintln!("[Wing] Connection error: {:#}", e);
            // Don't crash — keep the thread alive and retry next tick
        }

        // 100 ms tick = plenty for TC display; tighten to 50 ms if you need smoother updates
        std::thread::sleep(Duration::from_millis(30));
    }
}
