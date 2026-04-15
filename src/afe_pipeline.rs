//! Safe wrapper for ESP-SR AFE (AGC + noise suppression). The C side runs
//! `fetch()` on a dedicated FreeRTOS task on core 1.

use anyhow::Result;
use log::info;

extern "C" {
    fn voiceagent_afe_create(mic_channels: i32, has_reference: bool) -> i32;
    fn voiceagent_afe_get_feed_chunksize() -> i32;
    fn voiceagent_afe_get_fetch_chunksize() -> i32;
    fn voiceagent_afe_feed(samples: *const i16, count: i32) -> i32;
    #[allow(dead_code)]
    fn voiceagent_afe_fetch(out: *mut i16, out_size: *mut i32) -> i32;
    fn voiceagent_afe_fetch_nonblocking(out: *mut i16, out_size: *mut i32) -> i32;
    fn voiceagent_afe_destroy();
}

pub struct AfePipeline {
    feed_chunk: usize,
    fetch_chunk: usize,
    fetch_buf: alloc::vec::Vec<i16>,
    accum_buf: alloc::vec::Vec<i16>,
}

impl AfePipeline {
    pub fn new(mic_channels: i32, has_reference: bool) -> Result<Self> {
        let ret = unsafe { voiceagent_afe_create(mic_channels, has_reference) };
        if ret != 0 {
            anyhow::bail!("voiceagent_afe_create failed: {ret}");
        }

        let feed_chunk = unsafe { voiceagent_afe_get_feed_chunksize() } as usize;
        let fetch_chunk = unsafe { voiceagent_afe_get_fetch_chunksize() } as usize;

        info!(
            "AFE pipeline created: feed_chunk={} samples, fetch_chunk={} samples",
            feed_chunk, fetch_chunk
        );

        Ok(Self {
            feed_chunk,
            fetch_chunk,
            fetch_buf: alloc::vec![0i16; fetch_chunk],
            accum_buf: alloc::vec::Vec::with_capacity(feed_chunk * 2),
        })
    }

    pub fn feed(&mut self, pcm16: &[i16]) -> i32 {
        self.accum_buf.extend_from_slice(pcm16);

        if self.accum_buf.len() < self.feed_chunk {
            return pcm16.len() as i32; // buffered, not yet fed
        }

        let total = self.accum_buf.len() as i32;
        let consumed = unsafe { voiceagent_afe_feed(self.accum_buf.as_ptr(), total) };

        if consumed > 0 {
            let c = consumed as usize;
            self.accum_buf.drain(..c);
        }

        pcm16.len() as i32
    }

    pub fn fetch(&mut self) -> Option<&[i16]> {
        let mut out_size = self.fetch_chunk as i32;
        let ret =
            unsafe { voiceagent_afe_fetch_nonblocking(self.fetch_buf.as_mut_ptr(), &mut out_size) };
        if ret != 0 || out_size <= 0 {
            return None;
        }
        Some(&self.fetch_buf[..out_size as usize])
    }

    pub fn feed_chunksize(&self) -> usize {
        self.feed_chunk
    }

    pub fn fetch_chunksize(&self) -> usize {
        self.fetch_chunk
    }
}

impl Drop for AfePipeline {
    fn drop(&mut self) {
        info!("Destroying AFE pipeline");
        unsafe { voiceagent_afe_destroy() };
    }
}
