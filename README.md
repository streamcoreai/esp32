# voiceagent-esp32

[![Crates.io](https://img.shields.io/badge/crate-voiceagent--esp32-orange)](https://crates.io/crates/voiceagent-esp32)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**English** | [简体中文](./README.zh-CN.md)

ESP32 Rust SDK for the [Streamcore Voice Agent](https://github.com/streamcoreai/streamcore-server)
server. API design mirrors LiveKit's [`client-sdk-esp32`](https://github.com/livekit/client-sdk-esp32):
**create an [`Agent`] handle once, connect/disconnect at will, drive everything from your own
main loop.** Built on Espressif's native [`esp_peer`](https://github.com/espressif/esp-webrtc-solution)
C library — runs on ESP-IDF / FreeRTOS, no `tokio`, no full OS networking required.

> **This crate is a library, not firmware.** It has no `main()` and produces
> no flashable image. Runnable firmware lives in
> [`examples/esp32`](../examples/esp32) — start there if you want something
> on a board today, then come back here when you're writing your own app.

---

## Mental model

Three things to know:

1. **You own the media pipeline.** The SDK ships a [`Capturer`] trait + I2S
   default impl, and a [`Renderer`] trait + I2S default impl. Bring your
   own implementations for offline capture, USB mics, custom DACs — the
   SDK only cares that PCM moves in and out.
2. **You own the event loop.** [`Agent::connect`] is non-blocking; the
   agent runs in its own thread. Your `main()` is free to drive buttons,
   LEDs, sleep modes, reconnect logic — anything.
3. **Server interaction is callback + RPC.** Built-in callbacks for state
   changes, transcripts, AI responses, errors, and topic-addressed data
   packets. Custom device tools (camera, sensors, GPIO) are exposed via
   [`Agent::rpc_register`] so the AI can call them by name.

```rust
use voiceagent_esp32 as va;

fn main() -> anyhow::Result<()> {
    va::system_init();
    let p = esp_idf_svc::hal::peripherals::Peripherals::take()?;

    // 1. WiFi (the SDK no longer does this for you).
    let _wifi = va::wifi::connect_sta(p.modem, env!("WIFI_SSID"), env!("WIFI_PASSWORD"))?;

    // 2. Build the media pipeline.
    let mic = va::I2sMicCapturer::new(p.i2s1, /* bclk */, /* din */, /* ws */, true)?;
    let speaker = va::I2sSpeakerRenderer::new(p.i2s0, /* bclk */, /* dout */, /* ws */, 24_000)?;

    // 3. Create the agent (no network activity yet).
    let agent = va::Agent::create(va::AgentOptions {
        publish:   Some(va::PublishOptions { capturer: Box::new(mic) }),
        subscribe: Some(va::SubscribeOptions { renderer: Box::new(speaker) }),
        on_state_changed: Some(Box::new(|s| log::info!("state: {s:?}"))),
        on_transcript:    Some(Box::new(|t, f| log::info!("user: {t} ({f})"))),
        on_response:      Some(Box::new(|t|    log::info!("ai: {t}"))),
        ..Default::default()
    })?;

    // 4. Expose a tool the AI can call.
    agent.rpc_register("get_temperature", |inv| {
        inv.return_ok(serde_json::json!({"celsius": 22.4}));
    })?;

    // 5. Connect — returns immediately.
    agent.connect(env!("WHIP_ENDPOINT"), None)?;

    // 6. Your main loop drives buttons, sleep, reconnect, etc.
    loop { std::thread::sleep(std::time::Duration::from_secs(1)); }
}
```

[`Agent`]: https://docs.rs/voiceagent-esp32/latest/voiceagent_esp32/agent/struct.Agent.html
[`Agent::connect`]: https://docs.rs/voiceagent-esp32/latest/voiceagent_esp32/agent/struct.Agent.html#method.connect
[`Agent::rpc_register`]: https://docs.rs/voiceagent-esp32/latest/voiceagent_esp32/agent/struct.Agent.html#method.rpc_register
[`Capturer`]: https://docs.rs/voiceagent-esp32/latest/voiceagent_esp32/capture/trait.Capturer.html
[`Renderer`]: https://docs.rs/voiceagent-esp32/latest/voiceagent_esp32/render/trait.Renderer.html

---

## Run an example first

```bash
git clone https://github.com/streamcoreai/examples.git
cd examples/esp32
cp .env.example .env        # WiFi + WHIP endpoint
cargo build --release && espflash flash --monitor \
  target/xtensa-esp32s3-espidf/release/voice_agent
```

Full prerequisites, board configuration, and troubleshooting:
[`examples/esp32/README.md`](../examples/esp32/README.md).

Three targets ship there:

- **`voice_agent`** — WiFi + mic + speaker + display + camera + push-to-talk + RPC.
- **`minimal_audio`** — WiFi + mic + speaker, ~80 lines total.
- **`headless`** — no display/camera; demonstrates `publish_data`,
  `on_data`, custom RPC, and topic-based heartbeat.

---

## Using the SDK in your own project

An ESP-IDF binary crate needs more scaffolding than a normal Rust one. The
fastest route is to copy [`examples/esp32`](../examples/esp32) and delete
what you don't need. If you'd rather start clean, here's the complete list.

### 1. Toolchain

```bash
cargo install espup ldproxy espflash
espup install
. $HOME/export-esp.sh        # every shell — exports LIBCLANG_PATH
```

### 2. Dependency

```toml
[dependencies]
voiceagent-esp32 = "0.1"

# Your crate is the binary, so it — not the SDK — needs `binstart`.
esp-idf-svc = { version = "0.52", features = ["binstart", "critical-section"] }
esp-idf-hal = "0.46"
esp-idf-sys = { version = "0.37", features = ["binstart"] }
anyhow = "1"
log = "0.4"
serde_json = { version = "1", default-features = false, features = ["alloc"] }

[build-dependencies]
embuild = "0.33"
```

Working from a checkout of this repo instead:
`voiceagent-esp32 = { path = "../esp32" }`.

`esp_peer` comes from a git submodule inside this crate, contributed to your
build automatically through `[package.metadata.esp-idf-sys] extra_components`.
Initialise it once in the SDK checkout, or the ESP-IDF component resolution
step fails:

```bash
git -C path/to/esp32 submodule update --init --recursive
```

### 3. `build.rs`

```rust
fn main() {
    embuild::espidf::sysenv::output();
}
```

### 4. `.cargo/config.toml`

```toml
[build]
target = "xtensa-esp32s3-espidf"

[target.xtensa-esp32s3-espidf]
linker = "ldproxy"
runner = "espflash flash --monitor"     # optional: makes `cargo run` flash
rustflags = ["--cfg", "espidf_time64"]

[unstable]
build-std = ["std", "panic_abort"]

[env]
ESP_IDF_VERSION = "v5.4"
ESP_IDF_COMPONENT_MANAGER_ENABLED = "1"
MCU = "esp32s3"
```

### 5. `rust-toolchain.toml`

```toml
[toolchain]
channel = "esp"
```

### 6. `idf_component.yml`

The SDK links against three managed ESP-IDF components. They resolve against
the root (binary) crate, so this manifest belongs in *your* project:

```yaml
dependencies:
  espressif/esp_audio_codec: { version: "~2.3.0" }   # Opus encode/decode
  espressif/esp-sr:          { version: "^1.9.0" }   # AFE — AGC + noise suppression
  espressif/esp32-camera:    { version: "^2.0.0" }   # only if you use va::camera
  idf:                       { version: ">=5.1.0" }
```

### 7. `sdkconfig.defaults`

Copy [`sdkconfig.defaults`](./sdkconfig.defaults) verbatim to start. The
settings that are not optional:

| Setting | Why |
| ------- | --- |
| `CONFIG_SPIRAM=y` (+ mode/speed) | Opus, AFE, and DTLS buffers do not fit in internal RAM |
| `CONFIG_MBEDTLS_SSL_PROTO_DTLS=y`, `CONFIG_MBEDTLS_SSL_DTLS_SRTP=y`, `CONFIG_MBEDTLS_X509_CREATE_C=y` | `esp_peer` needs DTLS-SRTP and generates a self-signed cert |
| `CONFIG_MBEDTLS_DEFAULT_MEM_ALLOC=y` | Lets mbedTLS spill to PSRAM; internal-only allocation fails once esp-sr has taken its DRAM |
| `CONFIG_ESP_MAIN_TASK_STACK_SIZE=65536` | Agent setup runs deep on the main task |
| `CONFIG_AFE_INTERFACE_V1=y`, `CONFIG_SR_NSN_WEBRTC=y`, `CONFIG_SR_VADN_WEBRTC=y` | AFE pipeline used by `I2sMicCapturer` |

### 8. `partitions.csv`

The app image is a few MB with ESP-IDF, mbedTLS, and the esp-sr models
linked in. A 3 MB `factory` partition on ≥ 4 MB flash works:

```csv
# Name,   Type, SubType, Offset,  Size,   Flags
nvs,      data, nvs,     ,        0x6000,
phy_init, data, phy,     ,        0x1000,
factory,  app,  factory, ,        0x300000,
```

---

## API reference

### `Agent`

`Agent` is `Clone` (an `Arc` inside) and `Send + Sync` — clone it into
callbacks and other threads freely.

| Method | Behaviour |
| ------ | --------- |
| `Agent::create(AgentOptions) -> Result<Agent>` | Spawns the worker thread. No network activity yet. |
| `connect(whip_endpoint: &str, token: Option<&str>) -> Result<()>` | Non-blocking. `token` is a **bearer token**, sent as `Authorization: Bearer …`. Progress arrives on `on_state_changed`. |
| `disconnect() -> Result<()>` | Tears down the peer and sends the WHIP `DELETE`. |
| `state() -> ConnectionState` | `Disconnected` \| `Connecting` \| `Connected` \| `Failed`. |
| `set_mic_enabled(bool) -> Result<()>` | Push-to-talk. Calls `Capturer::set_enabled` on the worker thread. |
| `publish_data(topic: &str, data: &[u8]) -> Result<()>` | Topic-addressed packet over the data channel. |
| `rpc_register(method, handler) -> Result<()>` | Handler is `FnMut(RpcInvocation) + Send + 'static`. |
| `rpc_unregister(method) -> Result<()>` | Removes a handler. |

Need a JWT rather than a static key? `whip::fetch_token(token_url, api_key)`
POSTs to your server's token endpoint and returns the `token` field, ready
to hand to `connect`.

### `AgentOptions`

Every field is optional; `..Default::default()` covers the rest.

| Field | Type | Purpose |
| ----- | ---- | ------- |
| `publish` | `Option<PublishOptions>` | Outbound audio. Omit for subscribe-only devices. |
| `subscribe` | `Option<SubscribeOptions>` | Inbound audio. Omit for publish-only devices. |
| `stun_server` | `Option<String>` | e.g. `"stun:stun.l.google.com:19302"`. Not needed on a LAN. |
| `on_state_changed` | `FnMut(ConnectionState)` | Connection lifecycle. |
| `on_transcript` | `FnMut(&str, bool)` | User speech; the `bool` is `is_final`. |
| `on_response` | `FnMut(&str)` | AI text, chunk by chunk. |
| `on_error` | `FnMut(&str)` | Server-reported error. |
| `on_data` | `FnMut(&str, &[u8])` | Inbound topic + payload. |
| `on_raw_event` | `FnMut(&str)` | Any data-channel JSON the SDK didn't handle. |
| `on_wake_word` | `FnMut()` | On-device wake word; requires a WakeNet capturer (below). |

Callbacks run on the agent worker thread. Keep them short — blocking here
stalls the audio path.

### Capture and render

```rust
// 16 kHz mono I2S mic + ESP-SR AFE (AGC + noise suppression).
let mic = va::I2sMicCapturer::new(i2s1, bclk, din, ws, /* use_afe */ true)?;

// Same, plus on-device wake-word detection. The model comes from sdkconfig
// (CONFIG_SR_WN9_HIESP=y etc.); detections fire `on_wake_word`.
let mic = va::I2sMicCapturer::new_with_wakenet(i2s1, bclk, din, ws, true, true)?;

// I2S speaker. Output rate must be 16_000 or 24_000.
let speaker = va::I2sSpeakerRenderer::new(i2s0, bclk, dout, ws, 24_000)?;
```

Both are trait objects, so swapping in your own hardware is two methods:

```rust
pub trait Capturer: Send {
    fn read_frame(&mut self) -> Option<Vec<i16>>;   // 16 kHz mono i16, ~320 samples
    fn set_enabled(&mut self, enabled: bool) -> Result<()>;
    fn consume_wake_event(&mut self) -> bool { false }
}

pub trait Renderer: Send {
    fn render_audio(&mut self, pcm: &[i16]) -> Result<i32>;   // 16 kHz mono i16
}
```

`NullCapturer` and `NullRenderer` are there for one-way devices and bring-up.

### RPC

```rust
agent.rpc_register("set_led", |inv| {
    // inv.id, inv.method, inv.params (serde_json::Value)
    match inv.params.get("on").and_then(|v| v.as_bool()) {
        Some(on) => { drive_led(on); inv.return_ok(serde_json::json!({"ok": true})); }
        None     => inv.return_err(400, "missing 'on'"),
    }
})?;
```

Exactly one of `return_ok` / `return_err` per invocation — the server is
waiting on a matching `rpc.response`.

---

## Capabilities

| Stage          | Implementation                                                  |
| -------------- | --------------------------------------------------------------- |
| WiFi STA       | `esp-idf-svc::wifi` (helper: `va::wifi::connect_sta`)           |
| WHIP signaling | ESP-IDF HTTP client (`esp_http_client`)                         |
| WebRTC stack   | `esp_peer` (ICE + DTLS + SRTP + SCTP data channel)              |
| Mic capture    | `I2sMicCapturer` — I2S RX 16 kHz mono + ESP-SR AFE              |
| Speaker output | `I2sSpeakerRenderer` — I2S TX 16 kHz or 24 kHz                  |
| Audio codec    | Opus 16 kHz @ 32 kbps (via `esp_audio_codec`)                   |
| Wake word      | Optional ESP-SR WakeNet via `new_with_wakenet`                  |
| Display        | Optional ST7789 240×280 helper (`va::display::DisplayHandle`)   |
| Camera         | Optional OV2640 JPEG capture (`va::camera`)                     |
| RPC            | Topic-based remote method calls + structured invocations        |
| Data           | Topic-addressed packets via `publish_data` / `on_data`          |

---

## Wire protocol (data channel)

The SDK speaks a small JSON protocol over the WebRTC data channel.
Anything not in this list is delivered verbatim to your `on_raw_event`
callback so you can extend the protocol freely.

| `type`         | Direction | Purpose                                |
| -------------- | --------- | -------------------------------------- |
| `transcript`   | inbound   | User speech, partial or final          |
| `response`     | inbound   | AI text response chunk                 |
| `error`        | inbound   | Server-reported error                  |
| `rpc.request`  | inbound   | Server invokes a registered RPC method |
| `rpc.response` | outbound  | Device answers an `rpc.request`        |
| `data`         | both      | Application-defined topic + payload    |

---

## Building blocks (advanced)

Every layer is exposed as a `pub` module so you can compose your own pipeline:

| Module           | Contents                                              |
| ---------------- | ----------------------------------------------------- |
| `agent`          | `Agent`, `AgentOptions`, `ConnectionState`            |
| `capture`        | `Capturer`, `I2sMicCapturer`, `NullCapturer`          |
| `render`         | `Renderer`, `I2sSpeakerRenderer`, `NullRenderer`      |
| `rpc`            | `RpcInvocation`                                       |
| `display`        | ST7789 thread + `DisplayHandle`                       |
| `camera`         | OV2640 init + base64 chunked send                     |
| `wifi`           | Standalone WiFi STA helper                            |
| `whip`           | WHIP HTTP client + `fetch_token`                      |
| `webrtc`         | Safe wrapper around `esp_peer` (raw)                  |
| `opus_codec`     | Opus encode/decode                                    |
| `afe_pipeline`   | ESP-SR AFE pipeline (AGC + NS)                        |
| `audio`          | Low-level I2S mic/speaker drivers                     |

---

## Repository layout

```
esp32/                          this crate — the SDK library
├── src/                        agent, capture, render, rpc, whip, webrtc, …
├── components/
│   ├── esp-webrtc-solution/    git submodule — provides esp_peer
│   └── voiceagent_vc_frontend/ ESP-SR AFE wrapper component
└── sdkconfig.defaults          reference config to copy into your app

examples/esp32/                 firmware that uses this crate
└── src/bin/{voice_agent,minimal_audio,headless}.rs
```

`cargo build` here type-checks the library. It does not produce a flashable
image — that's [`examples/esp32`](../examples/esp32)'s job.

---

## License

MIT — see [LICENSE](LICENSE).
