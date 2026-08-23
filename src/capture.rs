//! Capture pipeline — the source of outbound audio.
//!
//! Anything that can yield 16 kHz mono PCM frames can drive the [`Agent`],
//! which makes the SDK easy to extend: implement [`Capturer`] for a stub,
//! a file reader, a different ADC, or a USB mic.
//!
//! The SDK ships [`I2sMicCapturer`] as the default on-device implementation
//! — INMP441-style I2S mic + ESP-SR AFE (AGC + noise suppression) with a
//! software-gain fallback if AFE fails to initialise.

use alloc::vec::Vec;
use anyhow::Result;

use esp_idf_svc::hal::gpio::AnyIOPin;
use esp_idf_svc::hal::i2s::config::{
    Config as I2sConfig, DataBitWidth, SlotMode, StdClkConfig, StdConfig, StdGpioConfig,
    StdSlotConfig, StdSlotMask,
};
use esp_idf_svc::hal::i2s::{I2sDriver, I2S1};
use log::{info, warn};

use crate::afe_pipeline::AfePipeline;
use crate::audio::{MicDriver, SAMPLE_RATE_HZ};

/// 20 ms of 16 kHz mono audio = one capturer frame.
pub const FRAME_SAMPLES: usize = 320;

/// Trait for a source of outbound audio.
///
/// The agent thread calls [`read_frame`](Capturer::read_frame) continuously
/// while the agent is connected. Return `None` if no frame is currently
/// available (mic disabled, buffer not full yet, etc.); the agent will
/// retry on the next iteration.
///
/// Frames are 16 kHz mono signed 16-bit PCM. The agent handles Opus
/// encoding and RTP packetisation — you only need to produce raw samples.
pub trait Capturer: Send {
    /// Produce one 20 ms frame (320 samples, 16 kHz mono).
    ///
    /// The returned `Vec` may be any length; the agent will split it into
    /// 20 ms chunks for Opus encoding. Returning `None` yields the thread.
    fn read_frame(&mut self) -> Option<Vec<i16>>;

    /// Enable or disable the underlying capture device.
    ///
    /// Called by [`Agent::set_mic_enabled`](crate::Agent::set_mic_enabled).
    /// When disabled, `read_frame` should return `None`.
    fn set_enabled(&mut self, enabled: bool) -> Result<()>;

    /// Returns true if an on-device wake-word event has been detected
    /// since the last call. The default implementation returns false —
    /// only capturers wired to a WakeNet-enabled AFE pipeline override
    /// this. The agent polls it once per worker iteration and fires
    /// `AgentOptions::on_wake_word` on a true result.
    fn consume_wake_event(&mut self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// Default I2S mic capturer
// ---------------------------------------------------------------------------

/// Default on-device capturer: I2S mic (e.g. INMP441) + ESP-SR AFE pipeline.
///
/// Constructed via [`I2sMicCapturer::new`]. Holds the I2S driver, AFE
/// pipeline, and output buffer for its lifetime — drop it to release the
/// I2S peripheral.
///
/// ## Example
/// ```ignore
/// let mic = va::capture::I2sMicCapturer::new(
///     p.i2s1,
///     p.pins.gpio41.downgrade(), // bclk
///     p.pins.gpio2.downgrade(),  // din
///     p.pins.gpio42.downgrade(), // ws
///     true, // AFE on
/// )?;
/// ```
pub struct I2sMicCapturer {
    mic: MicDriver<'static>,
    afe: Option<AfePipeline>,
    afe_out_buf: Vec<i16>,
    enabled: bool,
    frame_count: u64,
}

impl I2sMicCapturer {
    /// Build the I2S mic driver and (optionally) the AFE pipeline.
    ///
    /// If `use_afe` is true but ESP-SR AFE fails to initialise (e.g. low
    /// memory on plain ESP32), the capturer falls back to a raw passthrough
    /// — samples are returned untouched.
    pub fn new(
        i2s: I2S1<'static>,
        bclk: AnyIOPin<'static>,
        din: AnyIOPin<'static>,
        ws: AnyIOPin<'static>,
        use_afe: bool,
    ) -> Result<Self> {
        Self::new_with_wakenet(i2s, bclk, din, ws, use_afe, false)
    }

    /// Same as [`new`](Self::new) but lets you enable on-device wake-word
    /// detection. The wake model is whatever's selected in sdkconfig
    /// (`CONFIG_SR_WN9_HIESP=y` etc.); wake events are surfaced through
    /// [`consume_wake_event`](Self::consume_wake_event) and, when this
    /// capturer is attached to an `Agent`, also through
    /// `AgentOptions::on_wake_word`.
    pub fn new_with_wakenet(
        i2s: I2S1<'static>,
        bclk: AnyIOPin<'static>,
        din: AnyIOPin<'static>,
        ws: AnyIOPin<'static>,
        use_afe: bool,
        enable_wakenet: bool,
    ) -> Result<Self> {
        // INMP441 places its audio on the LEFT slot when L/R is tied to GND
        // and on the RIGHT slot when L/R is tied to VCC. The SDK default
        // assumes LEFT (which matches the streamcoreai/kevin-sp-v3 board).
        // If your board ties L/R to VCC (the xiaozhi-xiaoche schematic does
        // this on some revs), flip the line below to `StdSlotMask::Right`.
        let slot = StdSlotConfig::philips_slot_default(DataBitWidth::Bits32, SlotMode::Mono)
            .slot_mode_mask(SlotMode::Mono, StdSlotMask::Left)
            .left_align(true);
        let cfg = StdConfig::new(
            I2sConfig::default()
                .dma_buffer_count(12)
                .frames_per_buffer(480),
            StdClkConfig::from_sample_rate_hz(SAMPLE_RATE_HZ),
            slot,
            StdGpioConfig::default(),
        );
        let driver = I2sDriver::new_std_rx(i2s, &cfg, bclk, din, AnyIOPin::none(), ws)?;
        let mic = MicDriver::new(driver, FRAME_SAMPLES * 2);

        let afe = if use_afe {
            match AfePipeline::new(1, false, enable_wakenet) {
                Ok(afe) => {
                    info!(
                        "I2sMicCapturer: AFE ready (feed={}, fetch={})",
                        afe.feed_chunksize(),
                        afe.fetch_chunksize()
                    );
                    Some(afe)
                }
                Err(e) => {
                    warn!(
                        "I2sMicCapturer: AFE init failed ({e}); falling back to raw passthrough"
                    );
                    None
                }
            }
        } else {
            None
        };

        Ok(Self {
            mic,
            afe,
            afe_out_buf: Vec::with_capacity(FRAME_SAMPLES * 4),
            enabled: false,
            frame_count: 0,
        })
    }
}

impl Capturer for I2sMicCapturer {
    fn read_frame(&mut self) -> Option<Vec<i16>> {
        if !self.enabled {
            return None;
        }

        // Only touch the I2S DMA when we don't already have a frame queued.
        // The agent worker calls read_frame in a tight inner loop — doing
        // the 20-deep I2S drain on EVERY call burns CPU on no-op timeouts
        // and starves the playback decode stage downstream. This matches
        // the original streamcoreai/esp32 main loop, which drained I2S
        // once per iteration and then emitted all the resulting AFE
        // frames in a tight inner loop.
        if self.afe_out_buf.len() < FRAME_SAMPLES {
            let mut drained = 0u32;
            loop {
                let timeout_ms = if drained == 0 { 10 } else { 1 };
                match self.mic.read_frame_timeout(timeout_ms) {
                    Ok(pcm_bytes) => {
                        drained += 1;
                        self.frame_count += 1;
                        let samples: &[i16] = unsafe {
                            core::slice::from_raw_parts(
                                pcm_bytes.as_ptr() as *const i16,
                                pcm_bytes.len() / 2,
                            )
                        };
                        if let Some(ref mut afe) = self.afe {
                            afe.feed(samples);
                        } else {
                            self.afe_out_buf.extend_from_slice(samples);
                        }
                        if drained >= 20 {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }

            if let Some(ref mut afe) = self.afe {
                while let Some(processed) = afe.fetch() {
                    self.afe_out_buf.extend_from_slice(processed);
                }
            }
        }

        // Bound the buffer to ~1.6 s of audio (80 frames × 320 samples
        // × 2 bytes ≈ 51 KB). The original code had no cap and grew
        // unboundedly until OOM after a couple of minutes; tightening
        // it too far (8 frames / 160 ms) fragments the stream because
        // every call drops a big chunk, breaking STT parsing. 1.6 s is
        // long enough to absorb sustained backlog while still bounding
        // memory — if the buffer ever fills up that means the consumer
        // (Opus encoder + RTP send) is permanently behind real-time and
        // we drop the oldest samples so newer speech still goes through.
        const MAX_BUF: usize = FRAME_SAMPLES * 80;
        if self.afe_out_buf.len() > MAX_BUF {
            let drop_n = self.afe_out_buf.len() - MAX_BUF;
            self.afe_out_buf.drain(..drop_n);
            warn!("I2sMicCapturer: dropped {drop_n} samples (consumer slow)");
        }

        if self.afe_out_buf.len() < FRAME_SAMPLES {
            return None;
        }
        let tail = self.afe_out_buf.split_off(FRAME_SAMPLES);
        let frame = core::mem::replace(&mut self.afe_out_buf, tail);
        Some(frame)
    }

    fn set_enabled(&mut self, enabled: bool) -> Result<()> {
        if enabled == self.enabled {
            return Ok(());
        }
        if enabled {
            self.mic.start()?;
        } else {
            let _ = self.mic.stop();
            self.afe_out_buf.clear();
        }
        self.enabled = enabled;
        Ok(())
    }

    fn consume_wake_event(&mut self) -> bool {
        match self.afe.as_mut() {
            Some(afe) => afe.consume_wake_event(),
            None => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Null capturer — for subscribe-only agents (TTS playback only).
// ---------------------------------------------------------------------------

/// Silent capturer — yields no frames, never publishes audio.
///
/// Use when the device should only receive audio from the server (e.g. a
/// TTS-only kiosk).
pub struct NullCapturer;

impl Capturer for NullCapturer {
    fn read_frame(&mut self) -> Option<Vec<i16>> {
        None
    }
    fn set_enabled(&mut self, _enabled: bool) -> Result<()> {
        Ok(())
    }
}
