//! Opus encoder/decoder using Espressif's direct Opus API (no registration needed).

use core::ffi::c_void;
use log::info;

const ESP_AUDIO_ERR_OK: i32 = 0;
const ESP_AUDIO_ERR_BUFF_NOT_ENOUGH: i32 = -8;

// esp_opus_dec_frame_duration_t
const ESP_OPUS_DEC_FRAME_DURATION_20_MS: i32 = 3;

// esp_opus_enc_frame_duration_t
const ESP_OPUS_ENC_FRAME_DURATION_20_MS: i32 = 3;

// esp_opus_enc_application_t
const ESP_OPUS_ENC_APPLICATION_VOIP: i32 = 0;

#[repr(C)]
struct EspOpusDecCfg {
    sample_rate: u32,
    channel: u8,
    frame_duration: i32,
    self_delimited: bool,
}

#[repr(C)]
struct EspAudioDecInRaw {
    buffer: *const u8,
    len: u32,
    consumed: u32,
    frame_recover: i32,
}

#[repr(C)]
struct EspAudioDecOutFrame {
    buffer: *mut u8,
    len: u32,
    needed_size: u32,
    decoded_size: u32,
}

#[repr(C)]
struct EspAudioDecInfo {
    sample_rate: u32,
    bits_per_sample: u8,
    channel: u8,
    bitrate: u32,
    frame_size: u32,
}

#[repr(C)]
struct EspOpusEncConfig {
    sample_rate: i32,
    channel: i32,
    bits_per_sample: i32,
    bitrate: i32,
    frame_duration: i32,
    application_mode: i32,
    complexity: i32,
    enable_fec: bool,
    enable_dtx: bool,
    enable_vbr: bool,
}

#[repr(C)]
struct EspAudioEncInFrame {
    buffer: *const u8,
    len: u32,
}

#[repr(C)]
struct EspAudioEncOutFrame {
    buffer: *mut u8,
    len: u32,
    encoded_bytes: u32,
    pts: u64,
}

extern "C" {
    fn esp_opus_dec_open(cfg: *const c_void, cfg_sz: u32, handle: *mut *mut c_void) -> i32;
    fn esp_opus_dec_decode(
        handle: *mut c_void,
        raw: *mut EspAudioDecInRaw,
        frame: *mut EspAudioDecOutFrame,
        info: *mut EspAudioDecInfo,
    ) -> i32;
    fn esp_opus_dec_close(handle: *mut c_void) -> i32;

    fn esp_opus_enc_open(cfg: *const c_void, cfg_sz: u32, handle: *mut *mut c_void) -> i32;
    fn esp_opus_enc_process(
        handle: *mut c_void,
        in_frame: *mut EspAudioEncInFrame,
        out_frame: *mut EspAudioEncOutFrame,
    ) -> i32;
    fn esp_opus_enc_get_frame_size(
        handle: *mut c_void,
        in_size: *mut i32,
        out_size: *mut i32,
    ) -> i32;
    fn esp_opus_enc_close(handle: *mut c_void);
}

pub struct OpusDecoder {
    handle: *mut c_void,
    pcm_buf: alloc::vec::Vec<u8>,
}

unsafe impl Send for OpusDecoder {}

impl OpusDecoder {
    pub fn new(sample_rate: u32, channels: u8) -> anyhow::Result<Self> {
        let cfg = EspOpusDecCfg {
            sample_rate,
            channel: channels,
            frame_duration: ESP_OPUS_DEC_FRAME_DURATION_20_MS,
            self_delimited: false,
        };
        let mut handle: *mut c_void = core::ptr::null_mut();
        let ret = unsafe {
            esp_opus_dec_open(
                &cfg as *const _ as *const c_void,
                core::mem::size_of::<EspOpusDecCfg>() as u32,
                &mut handle,
            )
        };
        if ret != ESP_AUDIO_ERR_OK || handle.is_null() {
            anyhow::bail!("esp_opus_dec_open failed: {ret}");
        }
        let pcm_buf_size = (sample_rate as usize / 50) * (channels as usize) * 2;
        let pcm_buf_size = pcm_buf_size.max(4096);
        info!("Opus decoder opened (sr={sample_rate}, ch={channels}, pcm_buf={pcm_buf_size})");
        Ok(Self {
            handle,
            pcm_buf: alloc::vec![0u8; pcm_buf_size],
        })
    }

    pub fn decode(&mut self, opus_data: &[u8]) -> anyhow::Result<&[u8]> {
        let mut raw = EspAudioDecInRaw {
            buffer: opus_data.as_ptr(),
            len: opus_data.len() as u32,
            consumed: 0,
            frame_recover: 0,
        };
        let mut frame = EspAudioDecOutFrame {
            buffer: self.pcm_buf.as_mut_ptr(),
            len: self.pcm_buf.len() as u32,
            needed_size: 0,
            decoded_size: 0,
        };
        let mut info = EspAudioDecInfo {
            sample_rate: 0,
            bits_per_sample: 0,
            channel: 0,
            bitrate: 0,
            frame_size: 0,
        };

        let ret = unsafe { esp_opus_dec_decode(self.handle, &mut raw, &mut frame, &mut info) };

        if ret == ESP_AUDIO_ERR_BUFF_NOT_ENOUGH {
            let new_size = frame.needed_size as usize;
            self.pcm_buf.resize(new_size, 0);
            frame.buffer = self.pcm_buf.as_mut_ptr();
            frame.len = new_size as u32;
            frame.decoded_size = 0;
            raw.buffer = opus_data.as_ptr();
            raw.len = opus_data.len() as u32;
            raw.consumed = 0;
            let ret2 = unsafe { esp_opus_dec_decode(self.handle, &mut raw, &mut frame, &mut info) };
            if ret2 != ESP_AUDIO_ERR_OK {
                anyhow::bail!("esp_opus_dec_decode retry failed: {ret2}");
            }
        } else if ret != ESP_AUDIO_ERR_OK {
            anyhow::bail!("esp_opus_dec_decode failed: {ret}");
        }

        Ok(&self.pcm_buf[..frame.decoded_size as usize])
    }
}

impl Drop for OpusDecoder {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { esp_opus_dec_close(self.handle) };
        }
    }
}

pub struct OpusEncoder {
    handle: *mut c_void,
    pub in_frame_size: usize,
    #[allow(dead_code)]
    pub out_frame_size: usize,
    enc_buf: alloc::vec::Vec<u8>,
}

unsafe impl Send for OpusEncoder {}

impl OpusEncoder {
    pub fn new(sample_rate: u32, channels: u8, bitrate: i32) -> anyhow::Result<Self> {
        let cfg = EspOpusEncConfig {
            sample_rate: sample_rate as i32,
            channel: channels as i32,
            bits_per_sample: 16,
            bitrate: bitrate as i32,
            frame_duration: ESP_OPUS_ENC_FRAME_DURATION_20_MS,
            application_mode: ESP_OPUS_ENC_APPLICATION_VOIP,
            complexity: 0,
            enable_fec: false,
            enable_dtx: false,
            enable_vbr: false,
        };
        let mut handle: *mut c_void = core::ptr::null_mut();
        let ret = unsafe {
            esp_opus_enc_open(
                &cfg as *const _ as *const c_void,
                core::mem::size_of::<EspOpusEncConfig>() as u32,
                &mut handle,
            )
        };
        if ret != ESP_AUDIO_ERR_OK || handle.is_null() {
            anyhow::bail!("esp_opus_enc_open failed: {ret}");
        }

        let mut in_size: i32 = 0;
        let mut out_size: i32 = 0;
        let ret = unsafe { esp_opus_enc_get_frame_size(handle, &mut in_size, &mut out_size) };
        if ret != ESP_AUDIO_ERR_OK {
            unsafe { esp_opus_enc_close(handle) };
            anyhow::bail!("esp_opus_enc_get_frame_size failed: {ret}");
        }

        info!(
            "Opus encoder opened (sr={sample_rate}, ch={channels}, br={bitrate}, in={in_size}, out={out_size})"
        );
        // esp_opus_enc_get_frame_size may under-report; use max Opus packet size.
        let buf_size = core::cmp::max(out_size as usize, 1275);
        let enc_buf = alloc::vec![0u8; buf_size];
        Ok(Self {
            handle,
            in_frame_size: in_size as usize,
            out_frame_size: out_size as usize,
            enc_buf,
        })
    }

    pub fn encode(&mut self, pcm: &[u8]) -> anyhow::Result<&[u8]> {
        let mut in_frame = EspAudioEncInFrame {
            buffer: pcm.as_ptr(),
            len: pcm.len() as u32,
        };
        let mut out_frame = EspAudioEncOutFrame {
            buffer: self.enc_buf.as_mut_ptr(),
            len: self.enc_buf.len() as u32,
            encoded_bytes: 0,
            pts: 0,
        };

        let ret = unsafe { esp_opus_enc_process(self.handle, &mut in_frame, &mut out_frame) };
        if ret != ESP_AUDIO_ERR_OK {
            anyhow::bail!("esp_opus_enc_process failed: {ret}");
        }
        Ok(&self.enc_buf[..out_frame.encoded_bytes as usize])
    }
}

impl Drop for OpusEncoder {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { esp_opus_enc_close(self.handle) };
        }
    }
}
