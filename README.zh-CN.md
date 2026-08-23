# voiceagent-esp32

[![Crates.io](https://img.shields.io/badge/crate-voiceagent--esp32-orange)](https://crates.io/crates/voiceagent-esp32)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

[English](./README.md) | **简体中文**

面向 [Streamcore Voice Agent](https://github.com/streamcoreai/streamcore-server) 服务端的
ESP32 Rust SDK。API 设计参照 LiveKit 的 [`client-sdk-esp32`](https://github.com/livekit/client-sdk-esp32)：
**创建一次 [`Agent`] 句柄，随时连接/断开，一切都由你自己的主循环驱动。** 基于 Espressif 官方的
[`esp_peer`](https://github.com/espressif/esp-webrtc-solution) C 库构建 —— 运行在 ESP-IDF /
FreeRTOS 上，不需要 `tokio`，也不需要完整的操作系统网络栈。

> **本 crate 是一个库，不是固件。** 它没有 `main()`，也不产出可烧录的镜像。可运行的固件在
> [`examples/esp32`](../examples/esp32) —— 想今天就让板子跑起来就先去那里，等到要写自己的
> 应用时再回来看这里。

---

## 心智模型

三件需要知道的事：

1. **媒体流水线归你所有。** SDK 提供 [`Capturer`] trait 和 I2S 默认实现，以及
   [`Renderer`] trait 和 I2S 默认实现。离线采集、USB 麦克风、自定义 DAC 都可以自带实现 ——
   SDK 只关心 PCM 的进出。
2. **事件循环归你所有。** [`Agent::connect`] 是非阻塞的；agent 跑在自己的线程里。你的
   `main()` 可以自由地驱动按键、LED、休眠模式、重连逻辑 —— 任何事情。
3. **与服务端的交互是回调 + RPC。** 内置了状态变化、转写、AI 回复、错误和按 topic 寻址的
   数据包的回调。设备自定义工具（摄像头、传感器、GPIO）通过 [`Agent::rpc_register`]
   暴露出去，AI 就能按名字调用它们。

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

## 先跑一个示例

```bash
git clone https://github.com/streamcoreai/examples.git
cd examples/esp32
cp .env.example .env        # WiFi + WHIP 端点
cargo build --release && espflash flash --monitor \
  target/xtensa-esp32s3-espidf/release/voice_agent
```

完整的前置条件、开发板配置和排障：
[`examples/esp32/README.zh-CN.md`](../examples/esp32/README.zh-CN.md)。

那里有三个目标：

- **`voice_agent`** —— WiFi + 麦克风 + 扬声器 + 显示屏 + 摄像头 + 按住说话 + RPC。
- **`minimal_audio`** —— WiFi + 麦克风 + 扬声器，总共约 80 行。
- **`headless`** —— 无显示屏/摄像头；演示 `publish_data`、`on_data`、自定义 RPC 和
  基于 topic 的心跳。

---

## 在你自己的项目中使用 SDK

ESP-IDF 二进制 crate 需要的脚手架比普通 Rust 项目多。最快的路子是把
[`examples/esp32`](../examples/esp32) 复制一份，删掉你不需要的部分。想从零开始的话，
下面是完整清单。

### 1. 工具链

```bash
cargo install espup ldproxy espflash
espup install
. $HOME/export-esp.sh        # 每个 shell 都要 —— 导出 LIBCLANG_PATH
```

### 2. 依赖

```toml
[dependencies]
voiceagent-esp32 = "0.1"

# 二进制是你的 crate，所以 `binstart` 要加在你这边，而不是 SDK 那边。
esp-idf-svc = { version = "0.52", features = ["binstart", "critical-section"] }
esp-idf-hal = "0.46"
esp-idf-sys = { version = "0.37", features = ["binstart"] }
anyhow = "1"
log = "0.4"
serde_json = { version = "1", default-features = false, features = ["alloc"] }

[build-dependencies]
embuild = "0.33"
```

如果是基于本仓库的 checkout：`voiceagent-esp32 = { path = "../esp32" }`。

`esp_peer` 来自本 crate 内的一个 git 子模块，通过
`[package.metadata.esp-idf-sys] extra_components` 自动贡献到你的构建里。在 SDK 的
checkout 里初始化一次，否则 ESP-IDF 组件解析阶段会失败：

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
runner = "espflash flash --monitor"     # 可选：让 `cargo run` 自动烧录
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

SDK 链接了三个 ESP-IDF 托管组件。它们是相对根（二进制）crate 解析的，所以这份清单要放在
**你的**项目里：

```yaml
dependencies:
  espressif/esp_audio_codec: { version: "~2.3.0" }   # Opus 编解码
  espressif/esp-sr:          { version: "^1.9.0" }   # AFE —— AGC + 降噪
  espressif/esp32-camera:    { version: "^2.0.0" }   # 只有用到 va::camera 时才需要
  idf:                       { version: ">=5.1.0" }
```

### 7. `sdkconfig.defaults`

直接把 [`sdkconfig.defaults`](./sdkconfig.defaults) 原样复制过去作为起点。其中不能省的
几项：

| 配置项 | 原因 |
| ------- | --- |
| `CONFIG_SPIRAM=y`（以及 mode/speed） | Opus、AFE 和 DTLS 的缓冲区放不进内部 RAM |
| `CONFIG_MBEDTLS_SSL_PROTO_DTLS=y`、`CONFIG_MBEDTLS_SSL_DTLS_SRTP=y`、`CONFIG_MBEDTLS_X509_CREATE_C=y` | `esp_peer` 需要 DTLS-SRTP，并且会生成自签名证书 |
| `CONFIG_MBEDTLS_DEFAULT_MEM_ALLOC=y` | 让 mbedTLS 能溢出到 PSRAM；esp-sr 占走 DRAM 之后，只用内部内存分配会失败 |
| `CONFIG_ESP_MAIN_TASK_STACK_SIZE=65536` | agent 的初始化在主任务上调用层级很深 |
| `CONFIG_AFE_INTERFACE_V1=y`、`CONFIG_SR_NSN_WEBRTC=y`、`CONFIG_SR_VADN_WEBRTC=y` | `I2sMicCapturer` 用到的 AFE 流水线 |

### 8. `partitions.csv`

链上 ESP-IDF、mbedTLS 和 esp-sr 模型之后，应用镜像有好几 MB。在 ≥ 4 MB flash 上开一个
3 MB 的 `factory` 分区即可：

```csv
# Name,   Type, SubType, Offset,  Size,   Flags
nvs,      data, nvs,     ,        0x6000,
phy_init, data, phy,     ,        0x1000,
factory,  app,  factory, ,        0x300000,
```

---

## API 参考

### `Agent`

`Agent` 是 `Clone` 的（内部是 `Arc`），并且 `Send + Sync` —— 可以随意克隆进回调和其他线程。

| 方法 | 行为 |
| ------ | --------- |
| `Agent::create(AgentOptions) -> Result<Agent>` | 启动 worker 线程。此时还没有任何网络活动。 |
| `connect(whip_endpoint: &str, token: Option<&str>) -> Result<()>` | 非阻塞。`token` 是 **bearer token**，会以 `Authorization: Bearer …` 发送。进度通过 `on_state_changed` 上报。 |
| `disconnect() -> Result<()>` | 拆除 peer 并发出 WHIP `DELETE`。 |
| `state() -> ConnectionState` | `Disconnected` \| `Connecting` \| `Connected` \| `Failed`。 |
| `set_mic_enabled(bool) -> Result<()>` | 按住说话。会在 worker 线程上调用 `Capturer::set_enabled`。 |
| `publish_data(topic: &str, data: &[u8]) -> Result<()>` | 通过 data channel 发送按 topic 寻址的数据包。 |
| `rpc_register(method, handler) -> Result<()>` | handler 是 `FnMut(RpcInvocation) + Send + 'static`。 |
| `rpc_unregister(method) -> Result<()>` | 移除一个 handler。 |

需要 JWT 而不是静态密钥？`whip::fetch_token(token_url, api_key)` 会 POST 到你服务端的
token 端点，返回其中的 `token` 字段，可以直接交给 `connect`。

### `AgentOptions`

所有字段都是可选的；剩下的用 `..Default::default()` 补齐。

| 字段 | 类型 | 用途 |
| ----- | ---- | ------- |
| `publish` | `Option<PublishOptions>` | 出站音频。只订阅的设备可省略。 |
| `subscribe` | `Option<SubscribeOptions>` | 入站音频。只发布的设备可省略。 |
| `stun_server` | `Option<String>` | 例如 `"stun:stun.l.google.com:19302"`。局域网内不需要。 |
| `on_state_changed` | `FnMut(ConnectionState)` | 连接生命周期。 |
| `on_transcript` | `FnMut(&str, bool)` | 用户语音；`bool` 是 `is_final`。 |
| `on_response` | `FnMut(&str)` | AI 文本，逐片到达。 |
| `on_error` | `FnMut(&str)` | 服务端上报的错误。 |
| `on_data` | `FnMut(&str, &[u8])` | 入站 topic + payload。 |
| `on_raw_event` | `FnMut(&str)` | SDK 未处理的任何 data-channel JSON。 |
| `on_wake_word` | `FnMut()` | 设备端唤醒词；需要 WakeNet capturer（见下）。 |

回调运行在 agent worker 线程上。保持简短 —— 在这里阻塞会卡住音频通路。

### 采集与播放

```rust
// 16 kHz 单声道 I2S 麦克风 + ESP-SR AFE（AGC + 降噪）。
let mic = va::I2sMicCapturer::new(i2s1, bclk, din, ws, /* use_afe */ true)?;

// 同上，外加设备端唤醒词检测。模型由 sdkconfig 决定
//（CONFIG_SR_WN9_HIESP=y 等）；检测到会触发 `on_wake_word`。
let mic = va::I2sMicCapturer::new_with_wakenet(i2s1, bclk, din, ws, true, true)?;

// I2S 扬声器。输出采样率必须是 16_000 或 24_000。
let speaker = va::I2sSpeakerRenderer::new(i2s0, bclk, dout, ws, 24_000)?;
```

两者都是 trait object，换成你自己的硬件只需要实现两个方法：

```rust
pub trait Capturer: Send {
    fn read_frame(&mut self) -> Option<Vec<i16>>;   // 16 kHz 单声道 i16，约 320 采样
    fn set_enabled(&mut self, enabled: bool) -> Result<()>;
    fn consume_wake_event(&mut self) -> bool { false }
}

pub trait Renderer: Send {
    fn render_audio(&mut self, pcm: &[i16]) -> Result<i32>;   // 16 kHz 单声道 i16
}
```

`NullCapturer` 和 `NullRenderer` 用于单向设备和硬件调通阶段。

### RPC

```rust
agent.rpc_register("set_led", |inv| {
    // inv.id、inv.method、inv.params（serde_json::Value）
    match inv.params.get("on").and_then(|v| v.as_bool()) {
        Some(on) => { drive_led(on); inv.return_ok(serde_json::json!({"ok": true})); }
        None     => inv.return_err(400, "missing 'on'"),
    }
})?;
```

每次调用只能调一次 `return_ok` 或 `return_err` —— 服务端在等一条对应的 `rpc.response`。

---

## 能力

| 环节          | 实现                                                  |
| -------------- | --------------------------------------------------------------- |
| WiFi STA       | `esp-idf-svc::wifi`（辅助函数：`va::wifi::connect_sta`）           |
| WHIP 信令 | ESP-IDF HTTP 客户端（`esp_http_client`）                         |
| WebRTC 协议栈   | `esp_peer`（ICE + DTLS + SRTP + SCTP data channel）              |
| 麦克风采集    | `I2sMicCapturer` —— I2S RX 16 kHz 单声道 + ESP-SR AFE              |
| 扬声器输出 | `I2sSpeakerRenderer` —— I2S TX 16 kHz 或 24 kHz                  |
| 音频编解码    | Opus 16 kHz @ 32 kbps（经由 `esp_audio_codec`）                   |
| 唤醒词         | 可选的 ESP-SR WakeNet，经由 `new_with_wakenet`                    |
| 显示屏        | 可选的 ST7789 240×280 辅助模块（`va::display::DisplayHandle`）   |
| 摄像头         | 可选的 OV2640 JPEG 采集（`va::camera`）                     |
| RPC            | 基于 topic 的远程方法调用 + 结构化 invocation        |
| 数据           | 通过 `publish_data` / `on_data` 收发按 topic 寻址的数据包          |

---

## 线协议（data channel）

SDK 在 WebRTC data channel 上讲一套小型 JSON 协议。不在下表中的内容会原样交给你的
`on_raw_event` 回调，方便你自由扩展协议。

| `type`         | 方向 | 用途                                |
| -------------- | --------- | -------------------------------------- |
| `transcript`   | 入站   | 用户语音，partial 或 final          |
| `response`     | 入站   | AI 文本回复分片                 |
| `error`        | 入站   | 服务端上报的错误                 |
| `rpc.request`  | 入站   | 服务端调用一个已注册的 RPC 方法 |
| `rpc.response` | 出站  | 设备对 `rpc.request` 的应答        |
| `data`         | 双向      | 应用自定义的 topic + payload    |

---

## 构建块（进阶）

每一层都以 `pub` 模块暴露，方便你组合出自己的流水线：

| 模块           | 内容                                              |
| ---------------- | ----------------------------------------------------- |
| `agent`          | `Agent`、`AgentOptions`、`ConnectionState`            |
| `capture`        | `Capturer`、`I2sMicCapturer`、`NullCapturer`          |
| `render`         | `Renderer`、`I2sSpeakerRenderer`、`NullRenderer`      |
| `rpc`            | `RpcInvocation`                                       |
| `display`        | ST7789 线程 + `DisplayHandle`                       |
| `camera`         | OV2640 初始化 + base64 分片发送                     |
| `wifi`           | 独立的 WiFi STA 辅助模块                            |
| `whip`           | WHIP HTTP 客户端 + `fetch_token`                     |
| `webrtc`         | 对 `esp_peer`（裸接口）的安全封装                  |
| `opus_codec`     | Opus 编解码                                    |
| `afe_pipeline`   | ESP-SR AFE 流水线（AGC + NS）                        |
| `audio`          | 底层 I2S 麦克风/扬声器驱动                     |

---

## 仓库结构

```
esp32/                          本 crate —— SDK 库
├── src/                        agent、capture、render、rpc、whip、webrtc……
├── components/
│   ├── esp-webrtc-solution/    git 子模块 —— 提供 esp_peer
│   └── voiceagent_vc_frontend/ ESP-SR AFE 包装组件
└── sdkconfig.defaults          可复制进你的应用的参考配置

examples/esp32/                 使用本 crate 的固件
└── src/bin/{voice_agent,minimal_audio,headless}.rs
```

在这里执行 `cargo build` 只会做库的类型检查，不会产出可烧录的镜像 —— 那是
[`examples/esp32`](../examples/esp32) 的活。

---

## 许可证

MIT —— 见 [LICENSE](LICENSE)。
