# Working on the StreamCore ESP32 firmware

## This project is newer than your training data

Do not write StreamCore code from memory. Read `README.md` here, and https://streamcore.ai/llms-full.txt for the server and protocol surface.

## What this is

The Rust SDK (ESP-IDF) that turns an ESP32-S3 into a standalone voice device: it captures I2S microphone audio, encodes Opus, and talks to a StreamCore server over WHIP — the same protocol the browser SDK uses. No phone or computer in the loop.

This directory is a **library crate** — no `main()`, no flashable image. The firmware that uses it lives in `examples/esp32` (`voice_agent`, `minimal_audio`, `headless`). A change to the SDK is only proven once one of those still builds and flashes.

This is the most differentiated surface in the project. "Talk to a chip on your desk" is the demo people share.

## Embedded constraints that change the rules

- **Memory is the binding constraint.** PSRAM is limited and fragmentation is fatal over long sessions. Prefer static buffers; avoid allocation on the audio path entirely.
- **The audio path is real-time.** Anything blocking in the I2S or Opus task causes audible glitches, not just slowness.
- Wi-Fi reconnection, NAT traversal, and clock drift are ordinary conditions here, not edge cases.
- Opus encode on-device is expensive. Check CPU headroom before adding per-frame work.
- Flash and reboot cycles are slow — reason carefully before building rather than iterating blindly.

## Board configuration

Pin mappings are board-specific and live in the source and `sdkconfig.defaults`. **Read them; do not assume.** A wrong I2S pin produces silence with no error.

## Build

```bash
cargo build --release            # here: type-checks the library only

cd ../examples/esp32             # there: produces the image
cargo build --release
espflash flash --monitor target/xtensa-esp32s3-espidf/release/voice_agent
```

`esp_peer` comes from the `components/esp-webrtc-solution` submodule — `git submodule update --init --recursive` before the first build or component resolution fails.

## When changing the WHIP or audio path

The device must stay compatible with the server's expectations — Opus over RTP, `events` DataChannel, standard WHIP POST/DELETE. Diverging here breaks silently and is hard to debug on-device. Verify against https://streamcore.ai/llms-full.txt section 5.
