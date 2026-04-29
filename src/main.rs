// src/main.rs
mod audio;
mod ltc;
mod network;
mod ui;
mod wing;
mod cues;

use cpal::traits::{DeviceTrait, StreamTrait};
use ltc::LtcDecoder;
use std::io::{self, Write};
use std::sync::{mpsc, Arc};

fn main() -> anyhow::Result<()> {
    // Boot sequence — discovers Wing via WING? broadcast
    let boot = ui::run_boot_sequence()?;

    // Audio device selection
    let devices = audio::list_input_devices()?;
    println!("\n--- SELECT AUDIO SOURCE ---");
    for (i, dev) in devices.iter().enumerate() {
        let name = dev.description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| "Unknown".to_string());
        println!("  [{}]  {}", i, name);
    }
    print!("\nEnter device number: ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let index: usize = input.trim().parse().unwrap_or(0);

    let device = devices.into_iter().nth(index).expect("Invalid device index");
    let interface = audio::setup_device(device)?;
    let decoder = Arc::new(LtcDecoder::new(interface.config.sample_rate, 24.0));
    let decoder_clone = Arc::clone(&decoder);

    // Wire up Thread A → Thread B
    let (tx, rx) = mpsc::channel::<wing::TimecodeFrame>();

    let wing_config = wing::WingConfig {
        console_ip: boot.wing_ip.to_string(),
        ..Default::default()
    };

    std::thread::spawn(move || {
        if let Err(e) = wing::run_wing_thread(wing_config, rx) {
            eprintln!("\n[Wing] Fatal: {:#}", e);
        }
    });

    println!(
        "\nBridge running — {}  →  Wing {}\n",
        interface.device.description()?.name(),
        boot.wing_ip
    );

    let wing_ip = boot.wing_ip;

    let stream = interface.device.build_input_stream(
        &interface.config,
        move |data: &[f32], _| {
            decoder_clone.write_float_samples(data);
            if let Some(tc_str) = decoder_clone.get_timecode() {
                ui::print_timecode(&tc_str, wing_ip);
                if let Some(frame) = parse_timecode(&tc_str) {
                    let _ = tx.send(frame);
                }
            }
        },
        |err| eprintln!("\nAudio Error: {}", err),
        None,
    )?;

    stream.play()?;
    loop { std::thread::sleep(std::time::Duration::from_millis(100)); }
}

fn parse_timecode(s: &str) -> Option<wing::TimecodeFrame> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 4 { return None; }
    Some(wing::TimecodeFrame {
        hours:  parts[0].parse().ok()?,
        mins:   parts[1].parse().ok()?,
        secs:   parts[2].parse().ok()?,
        frames: parts[3].parse().ok()?,
    })
}