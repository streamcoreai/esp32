//! OV2640 camera driver — captures JPEG on demand and sends via data channel.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use anyhow::{bail, Result};
use log::info;

const CAM_PIN_PWDN: i32 = -1; // not used
const CAM_PIN_RESET: i32 = -1; // software reset
const CAM_PIN_XCLK: i32 = 15;
const CAM_PIN_SIOD: i32 = 4; // I2C SDA
const CAM_PIN_SIOC: i32 = 5; // I2C SCL
const CAM_PIN_D7: i32 = 16;
const CAM_PIN_D6: i32 = 17;
const CAM_PIN_D5: i32 = 18;
const CAM_PIN_D4: i32 = 12;
const CAM_PIN_D3: i32 = 10;
const CAM_PIN_D2: i32 = 8;
const CAM_PIN_D1: i32 = 9;
const CAM_PIN_D0: i32 = 11;
const CAM_PIN_VSYNC: i32 = 6;
const CAM_PIN_HREF: i32 = 7;
const CAM_PIN_PCLK: i32 = 13;

const CHUNK_BASE64_SIZE: usize = 12_000;

static IMAGE_COUNTER: AtomicU32 = AtomicU32::new(0);

extern "C" {
    fn esp_camera_init(config: *const camera_config_t) -> i32;
    fn esp_camera_fb_get() -> *mut camera_fb_t;
    fn esp_camera_fb_return(fb: *mut camera_fb_t);
}

const LEDC_CHANNEL_0: i32 = 0;
const LEDC_TIMER_0: i32 = 0;

const PIXFORMAT_JPEG: i32 = 4;

const FRAMESIZE_QVGA: i32 = 5; // 320x240

const JPEG_QUALITY: i32 = 12;

const CAMERA_FB_IN_PSRAM: i32 = 1;

#[repr(C)]
#[allow(non_camel_case_types)]
struct camera_config_t {
    pin_pwdn: i32,
    pin_reset: i32,
    pin_xclk: i32,
    pin_sccb_sda: i32,
    pin_sccb_scl: i32,
    pin_d7: i32,
    pin_d6: i32,
    pin_d5: i32,
    pin_d4: i32,
    pin_d3: i32,
    pin_d2: i32,
    pin_d1: i32,
    pin_d0: i32,
    pin_vsync: i32,
    pin_href: i32,
    pin_pclk: i32,
    xclk_freq_hz: i32,
    ledc_timer: i32,
    ledc_channel: i32,
    pixel_format: i32,
    frame_size: i32,
    jpeg_quality: i32,
    fb_count: usize,
    fb_location: i32,
    grab_mode: i32,
    // sccb_i2c_port: i32,  // Only in newer versions
}

#[repr(C)]
#[allow(non_camel_case_types)]
struct camera_fb_t {
    buf: *mut u8,
    len: usize,
    width: usize,
    height: usize,
    format: i32,
    // Additional fields we don't need to access
}

pub fn init() -> Result<()> {
    let config = camera_config_t {
        pin_pwdn: CAM_PIN_PWDN,
        pin_reset: CAM_PIN_RESET,
        pin_xclk: CAM_PIN_XCLK,
        pin_sccb_sda: CAM_PIN_SIOD,
        pin_sccb_scl: CAM_PIN_SIOC,
        pin_d7: CAM_PIN_D7,
        pin_d6: CAM_PIN_D6,
        pin_d5: CAM_PIN_D5,
        pin_d4: CAM_PIN_D4,
        pin_d3: CAM_PIN_D3,
        pin_d2: CAM_PIN_D2,
        pin_d1: CAM_PIN_D1,
        pin_d0: CAM_PIN_D0,
        pin_vsync: CAM_PIN_VSYNC,
        pin_href: CAM_PIN_HREF,
        pin_pclk: CAM_PIN_PCLK,
        xclk_freq_hz: 20_000_000, // 20 MHz
        ledc_timer: LEDC_TIMER_0,
        ledc_channel: LEDC_CHANNEL_0,
        pixel_format: PIXFORMAT_JPEG,
        frame_size: FRAMESIZE_QVGA, // 320x240
        jpeg_quality: JPEG_QUALITY,
        fb_count: 1,
        fb_location: CAMERA_FB_IN_PSRAM,
        grab_mode: 0, // CAMERA_GRAB_WHEN_EMPTY
    };

    let ret = unsafe { esp_camera_init(&config) };
    if ret != 0 {
        bail!("esp_camera_init failed: error {ret}");
    }
    info!(
        "[camera] OV2640 initialised (QVGA JPEG, quality={})",
        JPEG_QUALITY
    );
    Ok(())
}

fn capture_jpeg() -> Result<Vec<u8>> {
    // Discard stale buffered frame, then grab a fresh one.
    let stale = unsafe { esp_camera_fb_get() };
    if !stale.is_null() {
        unsafe { esp_camera_fb_return(stale) };
    }
    let fb = unsafe { esp_camera_fb_get() };
    if fb.is_null() {
        bail!("esp_camera_fb_get returned null");
    }
    let data = unsafe {
        let slice = core::slice::from_raw_parts((*fb).buf, (*fb).len);
        slice.to_vec()
    };
    unsafe { esp_camera_fb_return(fb) };
    info!("[camera] captured JPEG: {} bytes", data.len());
    Ok(data)
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

pub fn capture_and_send<F>(send_fn: &F) -> Result<()>
where
    F: Fn(&str) -> Result<()>,
{
    let jpeg = capture_jpeg()?;
    let b64 = base64_encode(&jpeg);
    let img_id = format!("img_{}", IMAGE_COUNTER.fetch_add(1, Ordering::Relaxed));

    // image_start
    let start_msg = format!(
        r#"{{"type":"image_start","id":"{}","total_size":{},"mime":"image/jpeg"}}"#,
        img_id,
        jpeg.len()
    );
    send_fn(&start_msg)?;

    // Send base64 data in chunks
    let total_chunks = (b64.len() + CHUNK_BASE64_SIZE - 1) / CHUNK_BASE64_SIZE;
    for (idx, chunk) in b64.as_bytes().chunks(CHUNK_BASE64_SIZE).enumerate() {
        let chunk_str = unsafe { core::str::from_utf8_unchecked(chunk) };
        let chunk_msg = format!(
            r#"{{"type":"image_chunk","id":"{}","index":{},"data":"{}"}}"#,
            img_id, idx, chunk_str
        );
        send_fn(&chunk_msg)?;
        info!(
            "[camera] sent chunk {}/{} ({} bytes)",
            idx + 1,
            total_chunks,
            chunk.len()
        );
    }

    // image_end
    let end_msg = format!(r#"{{"type":"image_end","id":"{}"}}"#, img_id);
    send_fn(&end_msg)?;

    info!(
        "[camera] image {} complete: {} JPEG bytes, {} base64 bytes, {} chunks",
        img_id,
        jpeg.len(),
        b64.len(),
        total_chunks
    );
    Ok(())
}
