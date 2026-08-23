//! # voiceagent-esp32
//!
//! ESP32 Rust SDK for the [Streamcore Voice Agent](https://github.com/streamcoreai/streamcore-server)
//! server. API design mirrors LiveKit's `client-sdk-esp32`: create an
//! [`Agent`] handle once, then connect/disconnect/reconnect at will, with
//! all events delivered via callbacks.
//!
//! Built on Espressif's native [`esp_peer`](https://github.com/espressif/esp-webrtc-solution)
//! C library — runs on ESP-IDF / FreeRTOS, no `tokio`, no full OS networking.
//!
//! ## Mental model
//!
//! - **You own the media pipeline.** Implement [`Capturer`] and [`Renderer`]
//!   (or use the bundled [`I2sMicCapturer`] / [`I2sSpeakerRenderer`]) and
//!   hand them to the agent.
//! - **You own the event loop.** [`Agent::connect`] is non-blocking; the
//!   agent runs in its own thread. Your `main()` is free to drive buttons,
//!   LEDs, custom UI, sleep, reconnect on failure — anything.
//! - **Server interaction is callback-driven.** Set
//!   [`AgentOptions::on_state_changed`], `on_transcript`, `on_response`,
//!   etc. for built-in events; use [`Agent::rpc_register`] to expose
//!   device capabilities to the AI; use [`Agent::publish_data`] for
//!   topic-based data packets.
//!
//! ## Quick start
//!
//! ```ignore
//! use esp_idf_svc::hal::gpio::Pin;
//! use esp_idf_svc::hal::peripherals::Peripherals;
//! use voiceagent_esp32 as va;
//!
//! fn main() -> anyhow::Result<()> {
//!     va::system_init();
//!     let p = Peripherals::take()?;
//!
//!     // 1. Connect WiFi (any way you like — SDK ships a helper).
//!     let _wifi = va::wifi::connect_sta(p.modem, env!("WIFI_SSID"), env!("WIFI_PASSWORD"))?;
//!
//!     // 2. Build the media pipeline.
//!     let mic = va::capture::I2sMicCapturer::new(
//!         p.i2s1,
//!         p.pins.gpio41.downgrade(),
//!         p.pins.gpio2.downgrade(),
//!         p.pins.gpio42.downgrade(),
//!         true, // AFE on
//!     )?;
//!     let speaker = va::render::I2sSpeakerRenderer::new(
//!         p.i2s0,
//!         p.pins.gpio46.downgrade(),
//!         p.pins.gpio3.downgrade(),
//!         p.pins.gpio1.downgrade(),
//!         24_000,
//!     )?;
//!
//!     // 3. Create the agent (no network activity yet).
//!     let agent = va::Agent::create(va::AgentOptions {
//!         publish:   Some(va::PublishOptions { capturer: Box::new(mic) }),
//!         subscribe: Some(va::SubscribeOptions { renderer: Box::new(speaker) }),
//!         on_state_changed: Some(Box::new(|s| log::info!("state: {s:?}"))),
//!         on_transcript:    Some(Box::new(|t, f| log::info!("user: {t} ({f})"))),
//!         on_response:      Some(Box::new(|t|    log::info!("ai: {t}"))),
//!         ..Default::default()
//!     })?;
//!
//!     // 4. Expose a tool for the AI.
//!     agent.rpc_register("get_temperature", |inv| {
//!         inv.return_ok(serde_json::json!({"celsius": 22.4}));
//!     })?;
//!
//!     // 5. Connect — returns immediately; status comes via callback.
//!     agent.connect(env!("WHIP_ENDPOINT"), None)?;
//!
//!     // 6. Your main loop drives buttons, sleep, reconnect, etc.
//!     loop {
//!         std::thread::sleep(std::time::Duration::from_secs(1));
//!     }
//! }
//! ```
//!
//! ## Building blocks
//!
//! Every layer is exposed as a `pub` module so power users can compose
//! their own pipeline:
//!
//! - [`agent`]      — [`Agent`], [`AgentOptions`], [`ConnectionState`]
//! - [`capture`]    — [`Capturer`] trait + [`I2sMicCapturer`]
//! - [`render`]     — [`Renderer`] trait + [`I2sSpeakerRenderer`]
//! - [`rpc`]        — [`RpcInvocation`] for handler implementations
//! - [`display`]    — ST7789 240x280 helper that runs on its own thread
//! - [`camera`]     — OV2640 capture + chunked send for vision tools
//! - [`wifi`]       — WiFi STA helper
//! - [`whip`]       — Standalone WHIP signaling client
//! - [`webrtc`]     — Safe wrapper around `esp_peer` (raw)
//! - [`opus_codec`] — Opus encoder/decoder
//! - [`afe_pipeline`] — ESP-SR AFE pipeline (AGC + noise suppression)
//! - [`audio`]      — I2S mic/speaker drivers
//!
//! [`Agent`]: agent::Agent
//! [`AgentOptions`]: agent::AgentOptions
//! [`ConnectionState`]: agent::ConnectionState
//! [`Capturer`]: capture::Capturer
//! [`Renderer`]: render::Renderer
//! [`I2sMicCapturer`]: capture::I2sMicCapturer
//! [`I2sSpeakerRenderer`]: render::I2sSpeakerRenderer
//! [`RpcInvocation`]: rpc::RpcInvocation

extern crate alloc;

pub mod afe_pipeline;
pub mod agent;
pub mod audio;
pub mod audio_processing;
pub mod camera;
pub mod capture;
pub mod display;
pub mod esp_peer_ffi;
pub mod opus_codec;
pub mod protocol;
pub mod render;
pub mod rpc;
pub mod webrtc;
pub mod whip;
pub mod wifi;

// Top-level re-exports for the most common types.
pub use agent::{
    Agent, AgentOptions, ConnectionState, DataCb, PublishOptions, RawCb, StateCb,
    SubscribeOptions, TextCb, TranscriptCb, WakeCb,
};
pub use capture::{Capturer, I2sMicCapturer, NullCapturer};
pub use render::{I2sSpeakerRenderer, NullRenderer, Renderer};
pub use rpc::RpcInvocation;

/// Initialise ESP-IDF logging + linker patches. Call once at the very top
/// of `main()` before anything else (mirrors `livekit_system_init()`).
pub fn system_init() {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
}
