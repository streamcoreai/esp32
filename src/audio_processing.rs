//! Half-duplex tracking + fallback software gain (used when AFE is unavailable).

use log::info;

const PLAYBACK_TAIL_FRAMES: u32 = 15;
const FALLBACK_SOFTWARE_GAIN: f32 = 16.0;

pub struct AudioProcessor {
    playback_tail: u32,
    #[allow(dead_code)]
    use_afe: bool,
}

impl AudioProcessor {
    pub fn new(use_afe: bool) -> Self {
        if use_afe {
            info!("AudioProcessor: AFE mode (AGC + NS from esp-sr)");
        } else {
            info!(
                "AudioProcessor: fallback mode, gain={}x (~{:.0}dB)",
                FALLBACK_SOFTWARE_GAIN,
                20.0 * FALLBACK_SOFTWARE_GAIN.log10(),
            );
        }
        Self {
            playback_tail: 0,
            use_afe,
        }
    }

    pub fn notify_playback(&mut self, _speaker_peak: i32) {
        self.playback_tail = PLAYBACK_TAIL_FRAMES;
    }

    pub fn should_mute(&mut self) -> bool {
        if self.playback_tail > 0 {
            self.playback_tail -= 1;
            true
        } else {
            false
        }
    }

    pub fn process_fallback(&mut self, pcm: &mut [u8]) -> u16 {
        if self.should_mute() {
            pcm.fill(0);
            return 0;
        }

        let n = pcm.len() / 2;
        if n == 0 {
            return 0;
        }

        let mut out_peak: i32 = 0;
        for i in 0..n {
            let s = i16::from_le_bytes([pcm[i * 2], pcm[i * 2 + 1]]) as f32;
            let gained =
                (s * FALLBACK_SOFTWARE_GAIN).clamp(i16::MIN as f32, i16::MAX as f32) as i16;
            let abs = gained.unsigned_abs() as i32;
            if abs > out_peak {
                out_peak = abs;
            }
            pcm[i * 2..i * 2 + 2].copy_from_slice(&gained.to_le_bytes());
        }

        out_peak as u16
    }

    pub fn peak_from_samples(samples: &[i16]) -> u16 {
        let mut peak: i32 = 0;
        for &s in samples {
            let abs = s.unsigned_abs() as i32;
            if abs > peak {
                peak = abs;
            }
        }
        peak as u16
    }
}
