//! I2S audio driver — simplex mode with separate ports for speaker (TX) and mic (RX).

use anyhow::Result;
use esp_idf_svc::hal::i2s::{I2sDriver, I2sRx, I2sTx};
use log::info;

pub const SAMPLE_RATE_HZ: u32 = 16_000;

pub struct SpeakerDriver<'d> {
    driver: I2sDriver<'d, I2sTx>,
    last_peak: i32,
    /// Reusable scratch buffer for 16-bit PCM → 32-bit I2S slot widening.
    /// Allocated once and reused across all write_bytes calls — allocating
    /// per call (50× per second) was causing speaker DMA underruns
    /// because PSRAM allocations are slow and the render thread couldn't
    /// keep up.
    out_buf: alloc::vec::Vec<u8>,
}

impl<'d> SpeakerDriver<'d> {
    pub fn new(driver: I2sDriver<'d, I2sTx>) -> Self {
        info!("I2S TX (speaker) ready");
        Self {
            driver,
            last_peak: 0,
            // Pre-size for a couple of frames of 24 kHz output (480
            // samples × 3 frames × 4 bytes/sample = 5760 bytes).
            out_buf: alloc::vec::Vec::with_capacity(5760),
        }
    }

    pub fn start(&mut self) -> Result<()> {
        self.driver
            .tx_enable()
            .map_err(|e| anyhow::anyhow!("I2S tx_enable failed: {}", e))?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn write_bytes(&mut self, pcm_bytes: &[u8]) -> Result<i32> {
        self.write_bytes_timeout(pcm_bytes, esp_idf_svc::hal::delay::BLOCK)
    }

    pub fn write_bytes_timeout(&mut self, pcm_bytes: &[u8], timeout_ms: u32) -> Result<i32> {
        let samples = pcm_bytes.len() / 2;
        if samples == 0 {
            return Ok(0);
        }
        let needed = samples * 4;
        if self.out_buf.len() < needed {
            self.out_buf.resize(needed, 0);
        }
        let mut peak: i32 = 0;

        for i in 0..samples {
            let s = i16::from_le_bytes([pcm_bytes[i * 2], pcm_bytes[i * 2 + 1]]);
            let abs = s.unsigned_abs() as i32;
            if abs > peak {
                peak = abs;
            }
            let s32: i32 = (s as i32) << 16;
            self.out_buf[i * 4..i * 4 + 4].copy_from_slice(&s32.to_le_bytes());
        }
        self.last_peak = peak;

        let timeout = if timeout_ms == esp_idf_svc::hal::delay::BLOCK {
            esp_idf_svc::hal::delay::BLOCK
        } else {
            esp_idf_svc::hal::delay::TickType::new_millis(timeout_ms as u64).0
        };
        self.driver
            .write_all(&self.out_buf[..needed], timeout)
            .map_err(|e| anyhow::anyhow!("I2S write failed: {}", e))?;
        Ok(peak)
    }

    #[allow(dead_code)]
    pub fn last_peak(&self) -> i32 {
        self.last_peak
    }

    pub fn stop(&mut self) -> Result<()> {
        self.driver
            .tx_disable()
            .map_err(|e| anyhow::anyhow!("I2S tx_disable failed: {}", e))?;
        Ok(())
    }
}

pub struct MicDriver<'d> {
    driver: I2sDriver<'d, I2sRx>,
    raw_buf: alloc::vec::Vec<u8>,
    pcm_buf: alloc::vec::Vec<u8>,
    frame_samples: usize,
    frame_count: u64,
}

impl<'d> MicDriver<'d> {
    pub fn new(driver: I2sDriver<'d, I2sRx>, frame_size_pcm_bytes: usize) -> Self {
        let frame_samples = frame_size_pcm_bytes / 2;
        info!(
            "I2S RX (mic) ready, frame={} samples ({} pcm bytes)",
            frame_samples, frame_size_pcm_bytes
        );
        Self {
            driver,
            raw_buf: alloc::vec![0u8; frame_samples * 4],
            pcm_buf: alloc::vec![0u8; frame_size_pcm_bytes],
            frame_samples,
            frame_count: 0,
        }
    }

    pub fn start(&mut self) -> Result<()> {
        self.frame_count = 0;
        self.driver
            .rx_enable()
            .map_err(|e| anyhow::anyhow!("I2S rx_enable failed: {}", e))?;
        info!("I2S RX enabled");
        Ok(())
    }

    pub fn read_frame(&mut self) -> Result<&mut [u8]> {
        self.read_frame_timeout(100) // 100ms timeout
    }

    pub fn read_frame_timeout(&mut self, timeout_ms: u32) -> Result<&mut [u8]> {
        let needed = self.frame_samples * 4;
        let mut total_read = 0usize;
        let timeout_ticks = esp_idf_svc::hal::delay::TickType::new_millis(timeout_ms as u64).0;

        while total_read < needed {
            let n = self
                .driver
                .read(&mut self.raw_buf[total_read..needed], timeout_ticks)
                .map_err(|e| anyhow::anyhow!("I2S read failed: {}", e))?;
            if n == 0 {
                if total_read == 0 {
                    return Err(anyhow::anyhow!("I2S read timeout"));
                }
                break;
            }
            total_read += n;
        }

        self.frame_count += 1;

        let samples_read = total_read / 4;
        for i in 0..samples_read {
            let s32 = i32::from_le_bytes([
                self.raw_buf[i * 4],
                self.raw_buf[i * 4 + 1],
                self.raw_buf[i * 4 + 2],
                self.raw_buf[i * 4 + 3],
            ]);
            // Shift 32-bit I2S word → i16. `>> 12` is neutral (no software
            // gain), `>> 12` is +24 dB. INMP441 sensitivity varies a lot
            // across boards; the mic-probe diagnostic in
            // examples/esp32-desktop-car shows you the post-shift peak so
            // you can pick the right value. Targets:
            //   peak post-shift ≈ 15k–25k for normal speech
            //   avg  post-shift ≈ 2k–5k
            //
            // `>> 12` is a good middle ground that doesn't clip on loud
            // boards while still keeping quiet rooms in STT range. If
            // you measure peaks consistently below 5k, drop to `>> 13`
            // or `>> 12`. If you measure peaks above 50k, go to `>> 12`.
            let value = (s32 >> 12).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            let bytes = value.to_le_bytes();
            self.pcm_buf[i * 2] = bytes[0];
            self.pcm_buf[i * 2 + 1] = bytes[1];
        }

        if self.frame_count <= 3 || self.frame_count % 100 == 0 {
            let mut raw_peak: i32 = 0;
            for i in 0..samples_read {
                let s32 = i32::from_le_bytes([
                    self.raw_buf[i * 4],
                    self.raw_buf[i * 4 + 1],
                    self.raw_buf[i * 4 + 2],
                    self.raw_buf[i * 4 + 3],
                ]);
                let abs = (s32 >> 12).abs();
                if abs > raw_peak {
                    raw_peak = abs;
                }
            }
            info!(
                "Mic frame #{}: {} samples, raw_peak={}",
                self.frame_count, samples_read, raw_peak
            );
        }

        Ok(&mut self.pcm_buf[..samples_read * 2])
    }

    pub fn stop(&mut self) -> Result<()> {
        self.driver
            .rx_disable()
            .map_err(|e| anyhow::anyhow!("I2S rx_disable failed: {}", e))?;
        info!("I2S RX disabled");
        Ok(())
    }
}
