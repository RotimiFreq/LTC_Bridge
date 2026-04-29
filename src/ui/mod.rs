// src/ui.rs
use crate::network::{self, WingProbeResult};
use anyhow::{bail, Context, Result};
use std::io::{self, Write};
use std::net::Ipv4Addr;
use std::time::Duration;

const WIDTH: usize = 57;
const VERSION: &str = "v0.2";

fn top_border()    { println!("┌{}┐", "─".repeat(WIDTH - 2)); }
fn bottom_border() { println!("└{}┘", "─".repeat(WIDTH - 2)); }
fn divider()       { println!("├{}┤", "─".repeat(WIDTH - 2)); }
fn line(text: &str) {
    let inner = WIDTH - 4;
    let truncated: String = text.chars().take(inner).collect();
    println!("│  {:<width$}  │", truncated, width = inner);
}
fn blank() { line(""); }

pub struct BootResult {
    pub wing_ip: Ipv4Addr,
    //pub broadcast: Ipv4Addr,
}

pub fn run_boot_sequence() -> Result<BootResult> {
    top_border();
    line(&format!("LTC BRIDGE  {}  —  Behringer Wing", VERSION));
    divider();

    let ifaces = network::list_local_interfaces()
        .context("Could not enumerate network interfaces")?;

    if ifaces.is_empty() {
        bottom_border();
        bail!("No active network interfaces found.");
    }

    line("Your machine's network interfaces:");
    blank();
    for iface in &ifaces {
        let prefix = iface.prefix_len.map(|p| format!("/{}", p)).unwrap_or_default();
        let bcast  = iface.broadcast.map(|b| format!("  bcast {}", b)).unwrap_or_default();
        let kind   = describe_adapter(&iface.name);
        line(&format!("  {:8}  {:16}  {}{}  {}", iface.name, iface.ip, prefix, bcast, kind));
    }
    blank();
    divider();

    let wing_ip = ask_for_wing_ip()?;

    // Find which interface's broadcast to use — pick the one on same subnet
    let broadcast = ifaces.iter()
        .filter_map(|i| i.broadcast)
        .find(|b| {
            // Same first two octets as Wing (rough check)
            let wo = wing_ip.octets();
            let bo = b.octets();
            wo[0] == bo[0] && wo[1] == bo[1]
        })
        .unwrap_or(Ipv4Addr::BROADCAST); // fallback to 255.255.255.255

    divider();
    let result = confirm_wing(wing_ip)?;
    bottom_border();
    Ok(result)
}

fn describe_adapter(name: &str) -> &'static str {
    let n = name.to_lowercase();
    if n.starts_with("eth") || n.starts_with("en") { "(Ethernet)" }
    else if n.starts_with("wlan") || n.starts_with("wi") { "(WiFi)" }
    else if n.contains("vpn") || n.contains("tun") { "(VPN)" }
    else { "" }
}

fn ask_for_wing_ip() -> Result<Ipv4Addr> {
    loop {
        let inner = WIDTH - 4;
        print!("│  {:<width$}", "Enter the Wing's IP address: ", width = inner);
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        match input.trim().parse::<Ipv4Addr>() {
            Ok(ip) => return Ok(ip),
            Err(_) => {
                line(&format!("  ✗  '{}' is not a valid IPv4.", input.trim()));
                line("     Example:  192.168.1.20");
                blank();
            }
        }
    }
}

fn confirm_wing(wing_ip: Ipv4Addr) -> Result<BootResult> {
    let spinner = ['⠋','⠙','⠹','⠸','⠼','⠴','⠦','⠧','⠇','⠏'];
    let mut spin_idx = 0usize;
    let mut attempt = 0u32;

    loop {
        attempt += 1;
        let handle = std::thread::spawn(move || {
            network::probe_wing(wing_ip, Duration::from_secs(5))
        });

        loop {
            if handle.is_finished() { break; }
            print!(
                "\r│  {} Probing {} via WING? broadcast...{}",
                spinner[spin_idx % spinner.len()], wing_ip, " ".repeat(6)
            );
            io::stdout().flush()?;
            spin_idx += 1;
            std::thread::sleep(Duration::from_millis(120));
        }

        print!("\r{}\r", " ".repeat(WIDTH + 2));
        io::stdout().flush()?;

        match handle.join().unwrap() {
            Some(WingProbeResult { ip, model, serial, firmware, }) => {
                line(&format!("  ✓  Wing confirmed at {}", ip));
                line(&format!("     Model:    {}", model));
                line(&format!("     Serial:   {}", serial));
                line(&format!("     Firmware: {}", firmware));
                blank();
                return Ok(BootResult { wing_ip: ip, });
            }
            None => {
                line(&format!("  ✗  No reply from {}  (attempt {})", wing_ip, attempt));
                line("     Close Wing app if open. Wing must be powered on.");
                line("     Retrying...  (Ctrl-C to quit)");
                blank();
                std::thread::sleep(Duration::from_secs(2));
                print!("\x1B[4A");
                for _ in 0..4 { print!("\r{}\n", " ".repeat(WIDTH + 2)); }
                print!("\x1B[4A");
                io::stdout().flush()?;
            }
        }
    }
}

pub fn print_timecode(tc: &str, wing_ip: Ipv4Addr) {
    print!("\r  ► LTC  {}   →  Wing {}        ", tc, wing_ip);
    let _ = io::stdout().flush();
}