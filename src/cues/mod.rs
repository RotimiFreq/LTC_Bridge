// src/cues.rs
use crate::wing::{TimecodeFrame, WingConn};
use anyhow::Result;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TC {
    pub hours: u8,
    pub mins:  u8,
    pub secs:  u8,
    pub frames: u8,
}

impl TC {
    pub fn to_frames(&self, fps: u8) -> u32 {
        let fps = fps as u32;
        (self.hours as u32 * 3600
            + self.mins  as u32 * 60
            + self.secs  as u32) * fps
            + self.frames as u32
    }
}

impl From<TimecodeFrame> for TC {
    fn from(t: TimecodeFrame) -> Self {
        Self { hours: t.hours, mins: t.mins, secs: t.secs, frames: t.frames }
    }
}

#[derive(Debug, Clone)]
pub enum CueAction {
    Fader { ch: u8, level: f32 },
    Mute  { ch: u8, muted: bool },
}

#[derive(Debug, Clone)]
pub struct Cue {
    pub trigger: TC,
    pub action:  CueAction,
    pub fired:   bool,
}

impl Cue {
    fn new(h: u8, m: u8, s: u8, f: u8, action: CueAction) -> Self {
        Self {
            trigger: TC { hours: h, mins: m, secs: s, frames: f },
            action,
            fired: false,
        }
    }
}

/// Builds the automated sequence:
/// 1. Fader movement at 01:01:05:00
/// 2. Mute toggles every 2 seconds from 01:01:07:00 to 01:03:00:00
pub fn build_cue_list() -> Vec<Cue> {
    let mut cues = Vec::new();

    // --- STEP 1: INITIAL FADER MOVEMENT ---
    // Trigger at 01:01:05:00
    cues.push(Cue::new(1, 1, 5, 0, CueAction::Fader { ch: 1, level: 0.75 }));

    // --- STEP 2: GENERATE MUTE INTERVALS ---
    let start_total_secs = 1 * 3600 + 1 * 60 + 7; // 01:01:07
    let end_total_secs   = 1 * 3600 + 3 * 60;     // 01:03:00
    
    // Toggle interval (e.g., every 2 seconds: Mute ON at :07, OFF at :09, etc.)
    let interval_secs = 2; 
    let mut current_secs = start_total_secs;
    let mut is_muted = true;

    while current_secs <= end_total_secs {
        let h = (current_secs / 3600) as u8;
        let m = ((current_secs % 3600) / 60) as u8;
        let s = (current_secs % 60) as u8;

        cues.push(Cue::new(h, m, s, 0, CueAction::Mute { ch: 1, muted: is_muted }));

        // Flip the mute state for the next timestamp
        is_muted = !is_muted;
        current_secs += interval_secs;
    }

    // --- STEP 3: FINAL SAFETY ---
    // Ensure the channel is definitely UNMUTED at the end of the sequence
    cues.push(Cue::new(1, 3, 0, 1, CueAction::Mute { ch: 1, muted: false }));

    // IMPORTANT: Sort by timecode to ensure sequential execution
    cues.sort_by_key(|c| c.trigger.to_frames(24));

    cues
}

pub fn execute_cues(
    cues: &mut Vec<Cue>,
    tc: TimecodeFrame,
    wing: &mut WingConn,
    started: &mut bool,
) -> Result<()> {
    let current = TC::from(tc).to_frames(24);

    // Initial sync: mark past cues as fired so they don't trigger all at once on start
    if !*started {
        *started = true;
        for cue in cues.iter_mut() {
            if current > cue.trigger.to_frames(24) {
                cue.fired = true;
            }
        }
        return Ok(());
    }

    for cue in cues.iter_mut() {
        if cue.fired { continue; }
        
        if current >= cue.trigger.to_frames(24) {
            match cue.action {
                CueAction::Fader { ch, level } => {
                    println!("[Cue] FADER ch{} → {:.3} @ {:02}:{:02}:{:02}:{:02}",
                        ch, level,
                        cue.trigger.hours, cue.trigger.mins,
                        cue.trigger.secs,  cue.trigger.frames);
                    // Ensure your wing module has send_fader implemented
                    wing.send_fader(ch, level)?;
                }
                CueAction::Mute { ch, muted } => {
                    println!("[Cue] MUTE ch{} → {} @ {:02}:{:02}:{:02}:{:02}",
                        ch, if muted { "ON" } else { "OFF" },
                        cue.trigger.hours, cue.trigger.mins,
                        cue.trigger.secs,  cue.trigger.frames);
                    // Ensure your wing module has send_mute implemented
                    wing.send_mute(ch, muted)?;
                }
            }
            cue.fired = true;
            // Only fire one cue per tick to avoid flooding the Wing's OSC buffer
            break; 
        }
    }
    Ok(())
}