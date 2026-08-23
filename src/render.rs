//! Render pipeline — the sink for inbound audio.
//!
//! Anything that can consume 16 kHz mono PCM can receive audio from the
//! [`Agent`] — the default [`I2sSpeakerRenderer`] pipes to an I2S amplifier
//! (e.g. MAX98357A), but you could just as easily write to a file, a
//! network stream, or a different DAC.

use anyhow::Result;
use esp_idf_svc::hal::gpio::AnyIOPin;
use esp_idf_svc::hal::i2s::config::{
    Config as I2sConfig, DataBitWidth, SlotMode, StdClkConfig, StdConfig, StdGpioConfig,
    StdSlotConfig,
};
use esp_idf_svc::hal::i2s::{I2sDriver, I2S0};
use log::info;

use crate::audio::SpeakerDriver;

/// Trait for a sink of inbound audio.
///
/// The agent thread calls [`render_audio`](Renderer::render_audio) with
/// decoded PCM frames. Samples are 16 kHz mono signed 16-bit — the
/// renderer is responsible for any resampling needed to drive its output
/// device.
pub trait Renderer: Send {
    /// Consume one chunk of 16 kHz mono PCM. Length is arbitrary (~320
    /// samples per Opus frame in practice).
    fn render_audio(&mut self, pcm: &[i16]) -> Result<i32>;
}

// ---------------------------------------------------------------------------
// Default I2S speaker renderer
// ---------------------------------------------------------------------------

/// Default on-device renderer: I2S speaker output with linear interpolated
/// upsampling from 16 kHz to the output rate.
///
/// Typical output rates: 16 000 (passthrough) or 24 000 (better fidelity
/// for MAX98357A-class amplifiers with low-pass filtering). Other rates
/// are rejected.
pub struct I2sSpeakerRenderer {
    speaker: SpeakerDriver<'static>,
    output_rate: u32,
    /// Reusable scratch buffer for 16 kHz → 24 kHz upsampling. Sized for a
    /// single Opus frame (320 samples → 480 output samples × 2 bytes).
    /// Allocated once and reused — the original main.rs used a static
    /// SPK_BUF for the same reason: allocating 50× per second on PSRAM
    /// adds enough latency to starve the agent worker.
    upsample_buf: alloc::vec::Vec<u8>,
}

impl I2sSpeakerRenderer {
    /// Build the I2S driver at `output_rate` and start the TX channel.
    pub fn new(
        i2s: I2S0<'static>,
        bclk: AnyIOPin<'static>,
        dout: AnyIOPin<'static>,
        ws: AnyIOPin<'static>,
        output_rate: u32,
    ) -> Result<Self> {
        if output_rate != 16_000 && output_rate != 24_000 {
            anyhow::bail!(
                "I2sSpeakerRenderer: unsupported output rate {output_rate} Hz (supported: 16000, 24000)"
            );
        }

        let slot =
            StdSlotConfig::philips_slot_default(DataBitWidth::Bits32, SlotMode::Mono).left_align(true);
        // Speaker DMA: 16 buffers × 480 frames = ~320 ms of headroom at
        // 24 kHz. The original streamcoreai used 8 buffers (160 ms) but
        // ran the whole audio loop on the main task; in the SDK we share
        // the worker with mic encoding, so a deeper DMA buffer keeps the
        // speaker from underrunning while the worker is busy with the
        // capture stage.
        let cfg = StdConfig::new(
            I2sConfig::default()
                .dma_buffer_count(16)
                .frames_per_buffer(480)
                .auto_clear(true),
            StdClkConfig::from_sample_rate_hz(output_rate),
            slot,
            StdGpioConfig::default(),
        );
        let driver = I2sDriver::new_std_tx(i2s, &cfg, bclk, dout, AnyIOPin::none(), ws)?;
        let mut speaker = SpeakerDriver::new(driver);
        speaker.start()?;
        info!("I2sSpeakerRenderer: ready at {output_rate} Hz");
        // 4 Opus frames of headroom for the upsample scratch buffer.
        let upsample_buf = alloc::vec::Vec::with_capacity(320 * 4 * 3 * 2 / 2);
        Ok(Self {
            speaker,
            output_rate,
            upsample_buf,
        })
    }

    /// 16 kHz → 24 kHz: for every pair of input samples emit
    /// `(s0, midpoint, s1)`. Writes into `self.upsample_buf`, growing it
    /// once and reusing the allocation across calls.
    fn upsample_16k_to_24k_into(&mut self, input: &[i16]) {
        let pairs = input.len() / 2;
        let out_bytes = pairs * 3 * 2;
        if self.upsample_buf.len() < out_bytes {
            self.upsample_buf.resize(out_bytes, 0);
        }
        for i in 0..pairs {
            let s0 = input[i * 2] as i32;
            let s1 = input[i * 2 + 1] as i32;
            let a = s0 as i16;
            let b = ((s0 + s1) / 2) as i16;
            let c = s1 as i16;
            self.upsample_buf[i * 6..i * 6 + 2].copy_from_slice(&a.to_le_bytes());
            self.upsample_buf[i * 6 + 2..i * 6 + 4].copy_from_slice(&b.to_le_bytes());
            self.upsample_buf[i * 6 + 4..i * 6 + 6].copy_from_slice(&c.to_le_bytes());
        }
        // Truncate (without freeing) so write_bytes_timeout sees exactly
        // what we produced this call.
        self.upsample_buf.truncate(out_bytes);
    }
}

impl Renderer for I2sSpeakerRenderer {
    fn render_audio(&mut self, pcm: &[i16]) -> Result<i32> {
        if self.output_rate == 24_000 {
            self.upsample_16k_to_24k_into(pcm);
            self.speaker.write_bytes_timeout(&self.upsample_buf, 100)
        } else {
            // Passthrough — reinterpret i16 as LE bytes.
            let bytes = unsafe {
                core::slice::from_raw_parts(pcm.as_ptr() as *const u8, pcm.len() * 2)
            };
            self.speaker.write_bytes_timeout(bytes, 100)
        }
    }
}

// ---------------------------------------------------------------------------
// Null renderer — for publish-only agents.
// ---------------------------------------------------------------------------

/// Discards inbound audio. Use when the device publishes only.
pub struct NullRenderer;

impl Renderer for NullRenderer {
    fn render_audio(&mut self, _pcm: &[i16]) -> Result<i32> {
        Ok(0)
    }
}
