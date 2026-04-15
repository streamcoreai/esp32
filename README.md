# ESP32 Voice Agent Client

A Rust client for ESP32-S3 that connects to a **Streamcore Voice Agent** server over WebRTC using [WHIP](https://datatracker.ietf.org/doc/rfc9725/) signaling.

Uses the [`esp_peer`](https://github.com/espressif/esp-webrtc-solution/tree/main/components/esp_peer) C library from Espressif's esp-webrtc-solution for the on-device WebRTC stack (ICE, DTLS, SRTP, SCTP).

## Hardware

> **Device-specific implementation:** This code targets the **ESP32-S3 WROOM-1** module. The GPIO pin assignments below are for this specific hardware only. **You must change the pins in `src/main.rs` to match your board.**

**Tested target:** ESP32-S3 WROOM-1 with 8MB PSRAM (N8R8 variant)

**Audio (I2S simplex mode - separate TX/RX ports):**

| Function | I2S Port | GPIO | Purpose |
|----------|----------|------|---------|
| Speaker BCLK | I2S0 TX | GPIO 46 | MAX98357A BCLK |
| Speaker DOUT | I2S0 TX | GPIO 3  | MAX98357A DIN |
| Speaker WS | I2S0 TX | GPIO 1  | MAX98357A LRC |
| Mic BCLK | I2S1 RX | GPIO 41 | INMP441 SCK |
| Mic DIN | I2S1 RX | GPIO 2  | INMP441 SD |
| Mic WS | I2S1 RX | GPIO 42 | INMP441 WS |

> **Important:** ESP32-S3 (I2S_HW_VERSION_2) requires `left_align=true` for I2S. The HAL's default config sets this incorrectly — this is handled in `src/main.rs`.

**Display (ST7789 240x280 TFT):**

| Function | GPIO | Purpose |
|----------|------|---------|
| MOSI | GPIO 47 | SPI data |
| SCLK | GPIO 21 | SPI clock |
| CS | GPIO 14 | Chip select |
| DC | GPIO 45 | Data/command |
| BL | GPIO 48 | Backlight |

**Controls:**

| Function | GPIO | Purpose |
|----------|------|---------|
| Boot button | GPIO 0 | Push-to-talk (hold to unmute) |

### Using a different ESP32 board?

Edit `src/main.rs` to change:
- Display SPI pins (lines 91-96)
- Speaker I2S pins (lines 120-127)
- Mic I2S pins (lines 141-148)
- Boot button GPIO (line 154)

## Server

This client connects to the **Streamcore Voice Agent server**. Set it up first:

👉 [streamcoreai/streamcore-server](https://github.com/streamcoreai/streamcore-server) — follow the README there to get the server running.

Once the server is up, set `WHIP_ENDPOINT` below to point at it (e.g. `http://<server-ip>:8080/whip`).

## Prerequisites

1. **Rust ESP toolchain** — install via [espup](https://github.com/esp-rs/espup):
   ```bash
   cargo install espup
   espup install
   # Source the export file (added to your shell profile)
   . $HOME/export-esp.sh
   ```

2. **ESP-IDF v5.4** — pulled automatically by `esp-idf-sys` during the first build into `.embuild/`. This directory is auto-generated and should not be committed.

   > Note: The first build downloads ~2GB of ESP-IDF tooling. Subsequent builds reuse the cached `.embuild/` directory.

3. **ldproxy** and **espflash**:
   ```bash
   cargo install ldproxy espflash
   ```

4. **esp-webrtc-solution** — included as a git submodule. After cloning, init it:
   ```bash
   git submodule update --init
   ```

## Configuration

Copy the example environment file and edit it with your settings:

```bash
cp .env.example .env
```

Then edit `.env`:

```env
WIFI_SSID=your-wifi-ssid
WIFI_PASSWORD=your-wifi-password
WHIP_ENDPOINT=http://192.168.1.100:8080/whip
TOKEN_URL=http://192.168.1.100:8080/token
API_KEY=sk-streamcore-demo-key
```

The `.env` file is read at build time by `build.rs` and baked into the firmware. Variables can also be set as shell environment variables (shell env takes precedence over `.env`).

| Variable         | Default                             | Description                                                                 |
| ---------------- | ----------------------------------- | --------------------------------------------------------------------------- |
| `WIFI_SSID`      | `your-wifi-ssid`                    | WiFi network name                                                           |
| `WIFI_PASSWORD`   | `your-wifi-password`               | WiFi password                                                               |
| `WHIP_ENDPOINT`  | `http://192.168.50.33:8080/whip`    | WHIP signaling endpoint                                                     |
| `TOKEN_URL`      |                                     | Token endpoint URL (e.g. `http://192.168.1.100:8080/token`). Required when the server has JWT auth enabled. |
| `API_KEY`        |                                     | API key sent as `Bearer` header when fetching a token from `TOKEN_URL`.     |

### JWT Authentication

When the server has `jwt_secret` set, all `/whip` requests require a valid JWT. Set `TOKEN_URL` so the device automatically fetches a short-lived token at boot:

```env
TOKEN_URL=http://192.168.1.100:8080/token
API_KEY=sk-streamcore-demo-key
```

> `STUN_SERVER` is currently hardcoded to empty in `src/main.rs`. Edit the constant directly if you need STUN.

## Build & Flash

```bash
# Build
cargo build --release

# Flash and monitor (connect your ESP32-S3 via USB)
espflash flash target/xtensa-esp32s3-espidf/release/voiceagent-esp32 --monitor
```

Or in one step:
```bash
cargo run --release
```
(requires uncommenting the `runner` line in `.cargo/config.toml`)

## Architecture

```
src/
├── main.rs              # Entry point: WiFi → WHIP → WebRTC → audio loop
├── afe_pipeline.rs      # ESP-SR AFE wrapper (AGC + noise suppression)
├── audio.rs             # I2S audio driver (speaker TX + mic RX, simplex mode)
├── audio_processing.rs  # Half-duplex tracking + fallback software gain
├── display.rs           # ST7789 TFT display driver (240x280) + UI rendering
├── esp_peer_ffi.rs      # Raw FFI bindings to the esp_peer C API
├── opus_codec.rs        # Opus encoder/decoder wrapper
├── webrtc.rs            # Safe Rust wrapper around esp_peer C library
├── whip.rs              # WHIP signaling (HTTP POST/DELETE) via ESP-IDF HTTP client
└── wifi.rs              # WiFi STA connection helper
```

### Flow

1. Boot → connect to WiFi → start display thread (ST7789 @ 10 FPS)
2. Open `esp_peer` WebRTC connection (Opus audio + "events" data channel)
3. Create SDP offer → POST to WHIP endpoint → receive SDP answer
4. Set remote description → ICE/DTLS handshake completes
5. **Audio loop:**
   - **Mic path:** I2S RX 16kHz mono → 32-to-16-bit conversion → AFE (AGC + noise suppression) → Opus encode @ 16kHz → send to server
   - **Speaker path:** receive Opus → decode @ 16kHz → upsample 16→24kHz (linear interpolation) → I2S TX playback
6. **Controls:** GPIO0 boot button = push-to-talk (hold to unmute)
7. **Display:** Shows connection status, VU meter, speaking indicator, last user/AI transcript
8. **Data channel:** delivers transcript/response/error JSON (same format as other SDKs)

### Data Channel Messages

The server sends JSON messages on the `events` data channel:

```json
{"type": "transcript", "text": "hello", "final": true}
{"type": "response", "text": "Hi there! How can I help?"}
{"type": "error", "message": "something went wrong"}
```

## Targeting plain ESP32

1. **`sdkconfig.defaults`:** set `CONFIG_IDF_TARGET="esp32"`, remove `CONFIG_SPIRAM_MODE_OCT`, `CONFIG_ESP32S3_DEFAULT_CPU_FREQ_240`
2. **`.cargo/config.toml`:** set `target = "xtensa-esp32-espidf"` and `MCU = "esp32"`
3. **`src/main.rs`:** change all GPIO pin assignments to match your board (the current pins are S3-specific and many don't exist on plain ESP32)

> **Note:** The ESP-SR AFE pipeline is memory-hungry. Plain ESP32 with limited PSRAM may not have enough RAM to run it — the fallback software-gain path will be used instead.

## Why not reuse the rust-sdk?

The existing `rust-sdk/` uses the `webrtc` crate (Pion-based, requires tokio + full OS networking), `reqwest`, and `audiopus` — none of which can run on ESP32's FreeRTOS-based ESP-IDF environment. This project uses Espressif's native `esp_peer` C library instead, called from Rust via FFI.
