use cpal::traits::{DeviceTrait, HostTrait};
use anyhow::{Result, Context};

pub struct AudioInterface {
    pub device: cpal::Device,
    pub config: cpal::StreamConfig,
}

pub fn list_input_devices() -> Result<Vec<cpal::Device>> {
    let host = cpal::default_host();
    let devices = host.input_devices()
        .context("Failed to get input devices")?;
    Ok(devices.collect())
}

pub fn setup_device(device: cpal::Device) -> Result<AudioInterface> {
    let config = device.default_input_config()
        .context("Failed to get default input config")?;
    
    Ok(AudioInterface {
        device,
        config: config.into(),
    })
}