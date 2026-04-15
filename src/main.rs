//! ESP32 Voice Agent Client
//!
//! Connects to a Streamcore Voice Agent server over WebRTC (WHIP signaling).

extern crate alloc;

mod afe_pipeline;
mod audio;
mod audio_processing;
mod camera;
mod display;
mod esp_peer_ffi;
mod opus_codec;
mod webrtc;
mod whip;
mod wifi;

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::gpio::{AnyIOPin, PinDriver, Pull};
use esp_idf_svc::hal::i2s::config::{
    Config, DataBitWidth, SlotMode, StdClkConfig, StdConfig, StdGpioConfig, StdSlotConfig,
};
use esp_idf_svc::hal::i2s::I2sDriver;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_sys as _;
use log::{error, info, warn};

use crate::afe_pipeline::AfePipeline;
use crate::audio::{MicDriver, SpeakerDriver, SAMPLE_RATE_HZ};
use crate::audio_processing::AudioProcessor;
use crate::display::{DisplayState, Role};
use crate::opus_codec::{OpusDecoder, OpusEncoder};
use crate::webrtc::{PeerCallbacks, PeerConnection};

macro_rules! env_or {
    ($key:literal, $default:literal) => {
        if let Some(v) = option_env!($key) {
            v
        } else {
            $default
        }
    };
}

const WIFI_SSID: &str = env_or!("WIFI_SSID", "your-wifi-ssid");
const WIFI_PASSWORD: &str = env_or!("WIFI_PASSWORD", "your-wifi-password");
const WHIP_ENDPOINT: &str = env_or!("WHIP_ENDPOINT", "http://192.168.50.33:8080/whip");
const TOKEN_URL: &str = env_or!("TOKEN_URL", "");
const API_KEY: &str = env_or!("API_KEY", "");
const STUN_SERVER: &str = "";

static mut SPK_BUF: [i16; 480] = [0; 480];
static mut SILENCE_16K: [u8; 640] = [0; 640];

fn log_heap(tag: &str) {
    let free_int = unsafe { esp_idf_sys::esp_get_free_internal_heap_size() };
    let free_all = unsafe { esp_idf_sys::esp_get_free_heap_size() };
    let min_int = unsafe { esp_idf_sys::esp_get_minimum_free_heap_size() };
    info!("HEAP[{tag}] free_internal={free_int} free_total={free_all} min_ever_internal={min_int}");
}

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    info!("===== Voice Agent ESP32 Client =====");
    info!("WHIP endpoint : {}", WHIP_ENDPOINT);
    info!("STUN server   : {}", STUN_SERVER);
    log_heap("boot");

    let peripherals = Peripherals::take()?;
    let sysloop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    info!("Connecting to WiFi '{}'...", WIFI_SSID);
    let _wifi = wifi::connect(
        peripherals.modem,
        sysloop.clone(),
        nvs,
        WIFI_SSID,
        WIFI_PASSWORD,
    )?;
    info!("WiFi connected!");
    log_heap("wifi");

    info!("Initialising camera...");
    match camera::init() {
        Ok(()) => info!("Camera ready"),
        Err(e) => error!("Camera init failed: {e} — vision requests will fail"),
    }
    log_heap("camera");

    let display_state = Arc::new(spin::Mutex::new(DisplayState::new()));
    let ds_display = display_state.clone();
    let spi2 = peripherals.spi2;
    let gpio_mosi = peripherals.pins.gpio47;
    let gpio_sclk = peripherals.pins.gpio21;
    let gpio_cs = peripherals.pins.gpio14;
    let gpio_dc = peripherals.pins.gpio45;
    let gpio_bl = peripherals.pins.gpio48;
    std::thread::Builder::new()
        .name("display".into())
        .stack_size(32768)
        .spawn(move || {
            display::display_thread(
                spi2, gpio_mosi, gpio_sclk, gpio_cs, gpio_dc, gpio_bl, ds_display,
            );
        })?;
    info!("Display thread spawned");

    const OUTPUT_SAMPLE_RATE: u32 = 24_000;

    let spk_slot =
        StdSlotConfig::philips_slot_default(DataBitWidth::Bits32, SlotMode::Mono).left_align(true);
    let spk_config = StdConfig::new(
        Config::default()
            .dma_buffer_count(8)
            .frames_per_buffer(480)
            .auto_clear(true),
        StdClkConfig::from_sample_rate_hz(OUTPUT_SAMPLE_RATE),
        spk_slot,
        StdGpioConfig::default(),
    );
    let spk_driver = I2sDriver::new_std_tx(
        peripherals.i2s0,
        &spk_config,
        peripherals.pins.gpio46, // BCLK
        peripherals.pins.gpio3,  // DOUT
        AnyIOPin::none(),
        peripherals.pins.gpio1, // WS
    )?;
    let mut speaker = SpeakerDriver::new(spk_driver);
    speaker.start()?;

    let mic_slot =
        StdSlotConfig::philips_slot_default(DataBitWidth::Bits32, SlotMode::Mono).left_align(true);
    let mic_config = StdConfig::new(
        Config::default()
            .dma_buffer_count(12)
            .frames_per_buffer(480),
        StdClkConfig::from_sample_rate_hz(SAMPLE_RATE_HZ),
        mic_slot,
        StdGpioConfig::default(),
    );
    let mic_driver = I2sDriver::new_std_rx(
        peripherals.i2s1,
        &mic_config,
        peripherals.pins.gpio41, // BCLK
        peripherals.pins.gpio2,  // DIN
        AnyIOPin::none(),
        peripherals.pins.gpio42, // WS
    )?;
    info!(
        "I2S: speaker={}Hz (DMA=8x480), mic={}Hz (DMA=12x480)",
        OUTPUT_SAMPLE_RATE, SAMPLE_RATE_HZ
    );

    let button = PinDriver::input(peripherals.pins.gpio0, Pull::Up)?;
    let mut mic_muted = true;
    let mut button_was_pressed = false;
    info!("Mic MUTED (hold button to talk)");

    const MAX_OPUS_FRAMES: usize = 50;
    let playback_buf: Arc<spin::Mutex<VecDeque<alloc::vec::Vec<u8>>>> =
        Arc::new(spin::Mutex::new(VecDeque::with_capacity(MAX_OPUS_FRAMES)));
    let pb_writer = playback_buf.clone();

    let image_requested = Arc::new(AtomicBool::new(false));

    let callbacks = PeerCallbacks {
        on_connected: Some(Box::new(|| {
            info!("WebRTC: peer connected!");
        })),
        on_disconnected: Some(Box::new({
            let ds = display_state.clone();
            move || {
                warn!("WebRTC: peer disconnected");
                ds.lock().connected = false;
            }
        })),
        on_audio: Some(Box::new(move |audio_data| {
            let mut buf = pb_writer.lock();
            while buf.len() >= MAX_OPUS_FRAMES {
                buf.pop_front();
            }
            buf.push_back(audio_data.to_vec());
        })),
        on_data: Some(Box::new({
            let ds = display_state.clone();
            let cam_flag = image_requested.clone();
            move |text: &str| {
                info!("Data channel: {}", text);
                if let Ok(msg) = serde_json::from_str::<serde_json::Value>(text) {
                    let msg_type = msg.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    match msg_type {
                        "request_image" => {
                            info!("[camera] image requested by server");
                            cam_flag.store(true, Ordering::SeqCst);
                        }
                        _ => {
                            let content = msg.get("text").and_then(|t| t.as_str()).unwrap_or("");
                            let final_flag =
                                msg.get("final").and_then(|f| f.as_bool()).unwrap_or(false);
                            if !content.is_empty() {
                                let mut s = ds.lock();
                                match msg_type {
                                    "transcript" => {
                                        if final_flag {
                                            s.push_transcript(
                                                Role::User,
                                                alloc::string::String::from(content),
                                            );
                                        } else {
                                            s.set_last_or_push(
                                                Role::User,
                                                alloc::string::String::from(content),
                                            );
                                        }
                                    }
                                    "response" => s.append_or_push(Role::Assistant, content),
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            }
        })),
    };

    log_heap("pre-peer");
    let peer = PeerConnection::new(STUN_SERVER, callbacks)?;
    log_heap("post-peer");

    info!("Starting ICE gathering...");
    peer.start_connection()?;

    let mut local_sdp = None;
    let sdp_start = std::time::Instant::now();
    while local_sdp.is_none() {
        let _ = peer.run_loop();
        local_sdp = peer.take_local_sdp();
        if sdp_start.elapsed() > Duration::from_secs(10) {
            anyhow::bail!("Timed out waiting for local SDP");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let offer_sdp = local_sdp.unwrap();
    // RFC 8829 requires "actpass" in offers; esp_peer uses "passive".
    let offer_sdp = offer_sdp.replace("a=setup:passive", "a=setup:actpass");
    info!("Got local SDP ({} bytes):\n{}", offer_sdp.len(), offer_sdp);

    let whip_token: Option<alloc::string::String> = if !TOKEN_URL.is_empty() {
        let api_key: Option<&str> = if API_KEY.is_empty() {
            None
        } else {
            Some(API_KEY)
        };
        info!("Fetching JWT from {}...", TOKEN_URL);
        Some(whip::fetch_token(TOKEN_URL, api_key)?)
    } else {
        None
    };

    info!("Sending WHIP offer to {}...", WHIP_ENDPOINT);
    let whip_result = whip::whip_offer(WHIP_ENDPOINT, &offer_sdp, whip_token.as_deref())?;

    info!(
        "Got remote SDP answer ({} bytes):\n{}",
        whip_result.answer_sdp.len(),
        whip_result.answer_sdp
    );
    peer.set_remote_sdp(&whip_result.answer_sdp)?;

    log_heap("pre-dtls");
    info!("Waiting for WebRTC connection...");
    let conn_start = std::time::Instant::now();
    while !peer.is_connected() {
        let _ = peer.run_loop();
        if conn_start.elapsed() > Duration::from_secs(15) {
            error!("Timed out waiting for WebRTC connection");
            whip::whip_delete(&whip_result.session_url, whip_token.as_deref());
            anyhow::bail!("WebRTC connection timeout");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    info!("WebRTC connection established!");
    display_state.lock().connected = true;

    info!("Waiting for data channel layer...");
    let dc_start = std::time::Instant::now();
    while !peer.is_data_channel_connected() {
        let _ = peer.run_loop();
        if dc_start.elapsed() > Duration::from_secs(10) {
            warn!(
                "Data channel layer not ready after 10s (state={}), trying anyway",
                peer.state()
            );
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    info!(
        "Data channel layer ready (state={}), creating 'events' channel...",
        peer.state()
    );
    peer.create_data_channel("events")?;
    info!("Data channel 'events' created");

    info!("Initialising Opus codec...");
    let mut opus_dec = OpusDecoder::new(16000, 1)?;
    let mut opus_enc = OpusEncoder::new(16000, 1, 32_000)?;
    info!(
        "Opus ready (enc in_frame_size={} bytes)",
        opus_enc.in_frame_size
    );

    const MIC_FRAME_SAMPLES: usize = 320;
    const MIC_FRAME_BYTES: usize = MIC_FRAME_SAMPLES * 2;
    let mut mic = MicDriver::new(mic_driver, MIC_FRAME_BYTES);
    let mut mic_started = false;

    let mut audio_proc;

    log_heap("pre-afe");
    info!("Initialising ESP-SR AFE pipeline (AGC + NS)...");
    let mut afe_pipeline: Option<AfePipeline> = match AfePipeline::new(1, false) {
        Ok(afe) => {
            info!(
                "AFE pipeline ready: feed_chunk={}, fetch_chunk={}",
                afe.feed_chunksize(),
                afe.fetch_chunksize()
            );
            audio_proc = AudioProcessor::new(true);
            Some(afe)
        }
        Err(e) => {
            error!("AFE init failed: {e} — falling back to software gain");
            audio_proc = AudioProcessor::new(false);
            None
        }
    };

    info!("Entering audio loop...");
    const PTS_INCREMENT: u32 = 960;
    let mut pts: u32 = 0;
    let mut mic_frame_count: u32 = 0;

    // Bootstrap RTP so the server starts sending the greeting TTS.
    {
        let silence = unsafe {
            core::slice::from_raw_parts(
                core::ptr::addr_of!(SILENCE_16K) as *const u8,
                opus_enc.in_frame_size,
            )
        };
        for _ in 0..5 {
            if let Ok(opus_data) = opus_enc.encode(silence) {
                let _ = peer.send_audio(opus_data, pts);
                pts = pts.wrapping_add(PTS_INCREMENT);
            }
        }
        info!("Sent 5 initial silence frames to bootstrap RTP");
    }

    let mut afe_out_buf: alloc::vec::Vec<i16> = alloc::vec::Vec::with_capacity(1024);

    loop {
        if !peer.is_connected() {
            warn!("Connection lost, exiting main loop");
            break;
        }

        if let Err(e) = peer.run_loop() {
            warn!("run_loop error: {e}");
        }

        let pressed = button.is_low();
        if pressed && !button_was_pressed {
            mic_muted = false;
            display_state.lock().mic_muted = false;
            info!("Mic UNMUTED (PTT held)");
            mic_frame_count = 0;
            match mic.start() {
                Ok(()) => {
                    mic_started = true;
                    info!("I2S RX enabled");
                }
                Err(e) => {
                    error!("Failed to start mic: {e}");
                    mic_muted = true;
                }
            }
        } else if !pressed && button_was_pressed {
            mic_muted = true;
            display_state.lock().mic_muted = true;
            info!("Mic MUTED (PTT released)");
            afe_out_buf.clear();
            if mic_started {
                let _ = mic.stop();
                mic_started = false;
            }
            // Silence burst triggers server-side STT endpointing.
            let silence = unsafe {
                core::slice::from_raw_parts(
                    core::ptr::addr_of!(SILENCE_16K) as *const u8,
                    opus_enc.in_frame_size,
                )
            };
            for _ in 0..30 {
                if let Ok(opus_data) = opus_enc.encode(silence) {
                    let _ = peer.send_audio(opus_data, pts);
                    pts = pts.wrapping_add(PTS_INCREMENT);
                }
            }
            info!("Sent 30 silence frames for endpointing");
        }
        button_was_pressed = pressed;

        // Handle pending image capture request from server.
        if image_requested.load(Ordering::SeqCst) {
            image_requested.store(false, Ordering::SeqCst);
            info!("[camera] capturing image for server...");
            let send_fn = |text: &str| -> anyhow::Result<()> { peer.send_data_channel(text) };
            match camera::capture_and_send(&send_fn) {
                Ok(()) => info!("[camera] image sent successfully"),
                Err(e) => error!("[camera] capture_and_send failed: {e}"),
            }
        }

        if mic_muted {
            display_state.lock().audio_level = 0;
        } else {
            let mut frames_this_iter = 0u32;
            let first = mic.read_frame();
            if let Ok(pcm) = first {
                mic_frame_count += 1;
                frames_this_iter += 1;

                if let Some(ref mut afe) = afe_pipeline {
                    let samples: &[i16] = unsafe {
                        core::slice::from_raw_parts(pcm.as_ptr() as *const i16, pcm.len() / 2)
                    };
                    afe.feed(samples);
                } else {
                    let peak = audio_proc.process_fallback(pcm);
                    display_state.lock().audio_level = peak;
                    if let Ok(opus_data) = opus_enc.encode(pcm) {
                        let _ = peer.send_audio(opus_data, pts);
                        pts = pts.wrapping_add(PTS_INCREMENT);
                    }
                }

                loop {
                    match mic.read_frame_timeout(1) {
                        Ok(pcm) => {
                            mic_frame_count += 1;
                            frames_this_iter += 1;

                            if let Some(ref mut afe) = afe_pipeline {
                                let samples: &[i16] = unsafe {
                                    core::slice::from_raw_parts(
                                        pcm.as_ptr() as *const i16,
                                        pcm.len() / 2,
                                    )
                                };
                                afe.feed(samples);
                            } else {
                                let peak = audio_proc.process_fallback(pcm);
                                display_state.lock().audio_level = peak;
                                if let Ok(opus_data) = opus_enc.encode(pcm) {
                                    let _ = peer.send_audio(opus_data, pts);
                                    pts = pts.wrapping_add(PTS_INCREMENT);
                                }
                            }

                            if frames_this_iter >= 20 {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }

                if frames_this_iter > 1 && (mic_frame_count < 10 || mic_frame_count % 100 == 0) {
                    info!("Drained {} mic frames this iteration", frames_this_iter);
                }
            } else if let Err(e) = first {
                warn!("mic read error: {e}");
            }

            if let Some(ref mut afe) = afe_pipeline {
                while let Some(processed) = afe.fetch() {
                    if !audio_proc.should_mute() {
                        afe_out_buf.extend_from_slice(processed);
                    }
                }

                while afe_out_buf.len() >= MIC_FRAME_SAMPLES {
                    let peak = AudioProcessor::peak_from_samples(&afe_out_buf[..MIC_FRAME_SAMPLES]);
                    display_state.lock().audio_level = peak;

                    if mic_frame_count < 5 || mic_frame_count % 50 == 0 {
                        info!("AFE: {} samples, peak={}", MIC_FRAME_SAMPLES, peak);
                    }

                    let pcm_bytes: &[u8] = unsafe {
                        core::slice::from_raw_parts(
                            afe_out_buf.as_ptr() as *const u8,
                            MIC_FRAME_SAMPLES * 2,
                        )
                    };
                    match opus_enc.encode(pcm_bytes) {
                        Ok(opus_data) => {
                            if mic_frame_count < 3 {
                                let preview: alloc::vec::Vec<u8> =
                                    opus_data.iter().take(8).copied().collect();
                                info!(
                                    "TX: opus_size={} pts={} TOC={:02X?}",
                                    opus_data.len(),
                                    pts,
                                    preview
                                );
                            }
                            let _ = peer.send_audio(opus_data, pts);
                            pts = pts.wrapping_add(PTS_INCREMENT);
                        }
                        Err(e) => warn!("opus encode error: {e}"),
                    }
                    afe_out_buf.drain(..MIC_FRAME_SAMPLES);
                }
            }
        }

        let mut played = false;
        let mut spk_peak: i32 = 0;
        let max_play = 5;
        let mut play_count = 0u32;

        loop {
            if play_count >= max_play {
                break;
            }
            let frame = { playback_buf.lock().pop_front() };
            match frame {
                Some(opus_frame) => match opus_dec.decode(&opus_frame) {
                    Ok(pcm) => {
                        let mono_16k: &[i16] = unsafe {
                            core::slice::from_raw_parts(pcm.as_ptr() as *const i16, pcm.len() / 2)
                        };
                        let input_pairs = (mono_16k.len() / 2).min(160);
                        let out_samples = input_pairs * 3;
                        unsafe {
                            let spk_buf_ptr = core::ptr::addr_of_mut!(SPK_BUF) as *mut i16;
                            for i in 0..input_pairs {
                                let s0 = mono_16k[i * 2];
                                let s1 = mono_16k[i * 2 + 1];
                                *spk_buf_ptr.add(i * 3) = s0;
                                *spk_buf_ptr.add(i * 3 + 1) = ((s0 as i32 + s1 as i32) / 2) as i16;
                                *spk_buf_ptr.add(i * 3 + 2) = s1;
                            }
                        }
                        let spk_bytes = unsafe {
                            let p = core::ptr::addr_of!(SPK_BUF) as *const u8;
                            core::slice::from_raw_parts(p, out_samples * 2)
                        };

                        match speaker.write_bytes_timeout(spk_bytes, 100) {
                            Ok(peak) => {
                                if peak > spk_peak {
                                    spk_peak = peak;
                                }
                            }
                            Err(e) => warn!("speaker write: {e}"),
                        }
                        played = true;
                        play_count += 1;
                    }
                    Err(e) => warn!("opus decode: {e}"),
                },
                None => break,
            }
        }

        if spk_peak > 0 {
            audio_proc.notify_playback(spk_peak);
        }

        display_state.lock().speaking = played;

        if mic_muted {
            std::thread::sleep(Duration::from_millis(20));
        } else if !played {
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    let _ = speaker.stop();
    let _ = mic.stop();
    info!("Shutting down...");
    whip::whip_delete(&whip_result.session_url, whip_token.as_deref());
    drop(peer);
    info!("Bye!");

    Ok(())
}
