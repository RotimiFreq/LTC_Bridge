use x42ltc_sys as ltc;

pub struct LtcDecoder {
    decoder: *mut ltc::LTCDecoder,
}

impl LtcDecoder {
    pub fn new(sample_rate: u32, fps: f64) -> Self {
        unsafe {
            
            let samples_per_frame = sample_rate as f64 / fps;
            
            let decoder = ltc::ltc_decoder_create(samples_per_frame as i32, 32);
            Self { decoder }
        }
    }

    pub fn write_float_samples(&self, samples: &[f32]) {
        unsafe {
           
            let ptr = samples.as_ptr() as *mut f32;

            let n_samples = samples.len();

            ltc::ltc_decoder_write_float(
                self.decoder, 
                ptr, 
                n_samples, 
                0i64 
            );
        }
    }

   pub fn get_timecode(&self) -> Option<String> {
    unsafe {
        // 1. Create the box for the raw bits (LTCFrameExt)
        let mut frame: ltc::LTCFrameExt = std::mem::zeroed();
        
        // 2. Read bits from the decoder into the frame
        if ltc::ltc_decoder_read(self.decoder, &mut frame) != 0 {

            let mut stimecode: ltc::SMPTETimecode = std::mem::zeroed();
            

            ltc::ltc_frame_to_time(
                &mut stimecode as *mut ltc::SMPTETimecode,
                &mut frame as *mut _ as *mut ltc::LTCFrame, 
                1
                
            );
            
           
            return Some(format!(
                "{:02}:{:02}:{:02}:{:02}",
                stimecode.hours,
                stimecode.mins,
                stimecode.secs,
                stimecode.frame
            ));
        }
        None
    }
}
}

impl Drop for LtcDecoder {
    fn drop(&mut self) {
        unsafe {
            // Crucial: Free the C-memory so we don't have a leak
            ltc::ltc_decoder_free(self.decoder);
        }
    }
}

// Allows the decoder to be shared with the Audio Thread
unsafe impl Send for LtcDecoder {}
unsafe impl Sync for LtcDecoder {}