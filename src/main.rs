mod audio;
mod ltc;

use cpal::traits::{DeviceTrait, StreamTrait};
use ltc::LtcDecoder;
use std::sync::Arc;
use std::io::{self, Write}; // Added for user input

fn main() -> anyhow::Result<()> {
    // 1. Get the list of devices from your audio mod
    let devices = audio::list_input_devices()?;
    
   println!("--- LTC BRIDGE: SELECT YOUR SOURCE ---");
for (i, dev) in devices.iter().enumerate() {
   
    let name = dev.description()
        .map(|d| d.name().to_string()) // This creates a copy that stays alive
        .unwrap_or_else(|_| "Unknown Device".to_string());
        
    println!("[{}] {}", i, name);
}

    // selecting source of timecode
    print!("\nEnter device number: ");
    io::stdout().flush()?; 
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let index: usize = input.trim().parse().unwrap_or(0);

   
    let device = devices.into_iter().nth(index).expect("Invalid device index");
 
    let interface = audio::setup_device(device)?;
    
   
    let decoder = Arc::new(LtcDecoder::new(interface.config.sample_rate, 24.0));
    let decoder_clone = Arc::clone(&decoder);

    println!("Starting LTC Bridge on {}...", interface.device.description()?.name());

    // 6. The Audio Loop
 let stream = interface.device.build_input_stream(
    &interface.config,
    move |data: &[f32], _| { // <--- THE CONVEYOR BELT STARTS HERE
        
        // 1. ADD THIS: A small check for signal volume
        let sum: f32 = data.iter().map(|&s| s * s).sum();
        let volume = (sum / data.len() as f32).sqrt();

        // If REAPER is playing, you should see these dots!
        if volume > 0.001 {
            print!("."); 
            let _ = std::io::stdout().flush();
        }

        // 2. PUSH: Send that audio to the C-Decoder
        decoder_clone.write_float_samples(data);
        
        // 3. CHECK: Did the C-Decoder find a full 80-bit frame?
        if let Some(timecode) = decoder_clone.get_timecode() {
            // \r clears the dots so the timecode looks clean
            println!("\r[SYNCED] LTC: {}", timecode);
        }
    }, // <--- THE CONVEYOR BELT ENDS HERE
    |err| eprintln!("Audio Error: {}", err),
    None
)?;

    stream.play()?;

    println!("Listening for LTC... (Press Ctrl+C to stop)");
    loop { std::thread::sleep(std::time::Duration::from_millis(100)); }
}