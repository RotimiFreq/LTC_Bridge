// src/main.rs
mod audio;
mod ltc;
mod wing;

use cpal::traits::{DeviceTrait, StreamTrait};
use ltc::LtcDecoder;
use std::io::{self, Write};
use std::sync::{mpsc, Arc};

fn main() -> anyhow::Result<()> {
    // ── Device selection (unchanged from Phase 1) ─────────────────────────────
    let devices = audio::list_input_devices()?;

    println!("--- LTC BRIDGE: SELECT YOUR SOURCE ---");
    for (i, dev) in devices.iter().enumerate() {
        let name = dev
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| "Unknown Device".to_string());
        println!("[{}] {}", i, name);
    }

    print!("\nEnter device number: ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let index: usize = input.trim().parse().unwrap_or(0);

    let device = devices
        .into_iter()
        .nth(index)
        .expect("Invalid device index");

    let interface = audio::setup_device(device)?;
    let decoder = Arc::new(LtcDecoder::new(interface.config.sample_rate, 24.0));
    let decoder_clone = Arc::clone(&decoder);

    // ── Phase 2: mpsc channel — Thread A sends TC, Thread B forwards to Wing ──
    //
    //   tx  lives in the audio callback (Thread A)
    //   rx  lives in run_wing_thread   (Thread B)
    //
    // The channel is unbounded so the audio callback never blocks.
    let (tx, rx) = mpsc::channel::<wing::TimecodeFrame>();

    // ── Thread B: Wing OSC engine ─────────────────────────────────────────────
    let wing_config = wing::WingConfig {
        console_ip: "192.168.1.100".to_string(), // ← set your Wing's IP here
        ..Default::default()
    };

    std::thread::spawn(move || {
        if let Err(e) = wing::run_wing_thread(wing_config, rx) {
            eprintln!("[Wing] Fatal error: {:#}", e);
        }
    });

    // ── Thread A: Audio / LTC decode loop (runs inside the cpal callback) ─────
    println!(
        "Starting LTC Bridge on {}...",
        interface.device.description()?.name()
    );

    let stream = interface.device.build_input_stream(
        &interface.config,
        move |data: &[f32], _| {
            // Volume indicator (unchanged)
            let rms = (data.iter().map(|&s| s * s).sum::<f32>() / data.len() as f32).sqrt();
            if rms > 0.001 {
                print!(".");
                let _ = io::stdout().flush();
            }

            // Feed samples to libltc
            decoder_clone.write_float_samples(data);

            // When a valid frame comes out, forward it to Thread B
            if let Some(tc_str) = decoder_clone.get_timecode() {
                // get_timecode() returns "HH:MM:SS:FF" — parse it back to numbers
                // so wing.rs has typed values to work with.
                if let Some(frame) = parse_timecode(&tc_str) {
                    println!("\r[SYNCED] LTC: {}", tc_str);
                    // Non-blocking send — if Wing thread is behind, we just
                    // drop the frame rather than stall the audio callback.
                    let _ = tx.send(frame);
                }
            }
        },
        |err| eprintln!("Audio Error: {}", err),
        None,
    )?;

    stream.play()?;
    println!("Listening for LTC... (Press Ctrl+C to stop)");

    // Keep main alive; Wing thread and audio callback run concurrently.
    loop {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Helper: "HH:MM:SS:FF" → TimecodeFrame
// ─────────────────────────────────────────────────────────────────────────────
fn parse_timecode(s: &str) -> Option<wing::TimecodeFrame> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 4 {
        return None;
    }
    Some(wing::TimecodeFrame {
        hours: parts[0].parse().ok()?,
        mins: parts[1].parse().ok()?,
        secs: parts[2].parse().ok()?,
        frames: parts[3].parse().ok()?,
    })
}