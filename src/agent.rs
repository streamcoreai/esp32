//! The main voice-agent handle.
//!
//! Design mirrors LiveKit's `livekit_room_*` C API: create once (no network),
//! then call [`Agent::connect`] / [`Agent::disconnect`] as many times as you
//! like. A single worker thread owns the WebRTC peer, the capture pipeline,
//! the render pipeline, and the RPC registry; all external interaction is
//! message-passed through an internal command channel, so the caller never
//! blocks.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use core::time::Duration;
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::Mutex;
use std::thread::JoinHandle;

use anyhow::Result;
use log::{error, info, warn};

use crate::capture::{Capturer, FRAME_SAMPLES};
use crate::opus_codec::{OpusDecoder, OpusEncoder};
use crate::protocol::{DataMsg, RpcError, RpcResponse};
use crate::render::Renderer;
use crate::rpc::{RpcHandler, RpcInvocation};
use crate::webrtc::{PeerCallbacks, PeerConnection};
use crate::whip;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Connection lifecycle states, delivered via
/// [`AgentOptions::on_state_changed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// Not connected and no connect in progress.
    Disconnected,
    /// Connect in progress (WHIP offer + ICE/DTLS handshake).
    Connecting,
    /// WebRTC peer is up; audio + data are flowing.
    Connected,
    /// Last connect attempt failed.
    Failed,
}

/// Audio publish configuration.
pub struct PublishOptions {
    pub capturer: Box<dyn Capturer>,
}

/// Audio subscribe configuration.
pub struct SubscribeOptions {
    pub renderer: Box<dyn Renderer>,
}

/// Signature for the state-change callback.
pub type StateCb = Box<dyn FnMut(ConnectionState) + Send>;

/// Signature for the transcript callback: `(text, is_final)`.
pub type TranscriptCb = Box<dyn FnMut(&str, bool) + Send>;

/// Signature for a plain text callback (response, error).
pub type TextCb = Box<dyn FnMut(&str) + Send>;

/// Signature for the raw-data-channel callback.
pub type RawCb = Box<dyn FnMut(&str) + Send>;

/// Signature for topic-addressed data packets: `(topic, payload)`.
pub type DataCb = Box<dyn FnMut(&str, &[u8]) + Send>;

/// Wake-word callback — fired on the agent worker thread once per wake
/// event detected by the on-device WakeNet model. The callback runs
/// inline; keep it short (e.g. flip a flag, update a display).
pub type WakeCb = Box<dyn FnMut() + Send>;

/// Everything needed to create an [`Agent`]. Most fields are optional.
pub struct AgentOptions {
    /// Publish side — omit for subscribe-only devices (e.g. TTS kiosk).
    pub publish: Option<PublishOptions>,
    /// Subscribe side — omit for publish-only devices.
    pub subscribe: Option<SubscribeOptions>,
    /// Optional STUN server, e.g. `"stun:stun.l.google.com:19302"`.
    pub stun_server: Option<String>,

    pub on_state_changed: Option<StateCb>,
    pub on_transcript: Option<TranscriptCb>,
    pub on_response: Option<TextCb>,
    pub on_error: Option<TextCb>,
    pub on_data: Option<DataCb>,
    pub on_raw_event: Option<RawCb>,
    /// Fired once per on-device wake-word detection. Requires a
    /// `Capturer` whose `consume_wake_event` returns true on detection
    /// (the bundled `I2sMicCapturer::new_with_wakenet(...)` does).
    pub on_wake_word: Option<WakeCb>,
}

impl Default for AgentOptions {
    fn default() -> Self {
        Self {
            publish: None,
            subscribe: None,
            stun_server: None,
            on_state_changed: None,
            on_transcript: None,
            on_response: None,
            on_error: None,
            on_data: None,
            on_raw_event: None,
            on_wake_word: None,
        }
    }
}

/// Opaque handle for a voice-agent session. `Agent` is `Send + Sync`; clone
/// it cheaply (it's an `Arc` under the hood) to call from multiple tasks.
#[derive(Clone)]
pub struct Agent {
    inner: Arc<AgentInner>,
}

struct AgentInner {
    state: Arc<Mutex<ConnectionState>>,
    shutdown: Arc<AtomicBool>,
    // Wrapped in Mutex so `Agent: Sync` regardless of mpsc::Sender's Sync
    // status across toolchain versions. The lock is uncontended in normal
    // use — commands are bursty, not high-rate.
    command_tx: Mutex<mpsc::Sender<Command>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl AgentInner {
    fn send(&self, cmd: Command) -> Result<()> {
        self.command_tx
            .lock()
            .map_err(|_| anyhow::anyhow!("agent command lock poisoned"))?
            .send(cmd)
            .map_err(|_| anyhow::anyhow!("agent worker is gone"))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Command protocol (external → worker)
// ---------------------------------------------------------------------------

enum Command {
    Connect {
        endpoint: String,
        token: Option<String>,
    },
    Disconnect,
    SetMicEnabled(bool),
    /// Raw text to send over the data channel — used internally by
    /// `publish_data` and by RPC responders.
    SendRaw(String),
    RpcRegister {
        method: String,
        handler: RpcHandler,
    },
    RpcUnregister(String),
    Shutdown,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

impl Agent {
    /// Create the agent handle and spawn its worker thread. No network
    /// activity happens until [`connect`](Self::connect) is called.
    pub fn create(options: AgentOptions) -> Result<Self> {
        let (command_tx, command_rx) = mpsc::channel::<Command>();
        let state = Arc::new(Mutex::new(ConnectionState::Disconnected));
        let shutdown = Arc::new(AtomicBool::new(false));

        let cmd_tx_for_worker = command_tx.clone();
        let state_for_worker = state.clone();
        let shutdown_for_worker = shutdown.clone();

        let handle = std::thread::Builder::new()
            .name("voiceagent-worker".into())
            .stack_size(32 * 1024)
            .spawn(move || {
                worker_main(
                    options,
                    command_rx,
                    cmd_tx_for_worker,
                    state_for_worker,
                    shutdown_for_worker,
                );
            })?;

        Ok(Self {
            inner: Arc::new(AgentInner {
                state,
                shutdown,
                command_tx: Mutex::new(command_tx),
                worker: Mutex::new(Some(handle)),
            }),
        })
    }

    /// Initiate a connect attempt. Returns immediately; progress is reported
    /// via [`AgentOptions::on_state_changed`].
    pub fn connect(&self, whip_endpoint: &str, token: Option<&str>) -> Result<()> {
        self.inner.send(Command::Connect {
            endpoint: whip_endpoint.to_string(),
            token: token.map(|s| s.to_string()),
        })
    }

    /// Gracefully tear down the current connection. No-op if already
    /// disconnected. Returns immediately.
    pub fn disconnect(&self) -> Result<()> {
        self.inner.send(Command::Disconnect)
    }

    /// Current connection state (caller's view, may briefly lag the worker).
    pub fn state(&self) -> ConnectionState {
        *self.inner.state.lock().unwrap()
    }

    /// Mute or unmute the capturer. When muted, no audio is published.
    pub fn set_mic_enabled(&self, enabled: bool) -> Result<()> {
        self.inner.send(Command::SetMicEnabled(enabled))
    }

    /// Publish a topic-addressed data packet to the server.
    ///
    /// The wire format is `{"type":"data","topic":"<t>","payload":"<base64>"}`.
    pub fn publish_data(&self, topic: &str, data: &[u8]) -> Result<()> {
        let payload = base64_encode(data);
        let msg = DataMsg {
            type_: "data",
            topic,
            payload: &payload,
        };
        let serialized = serde_json::to_string(&msg)?;
        self.inner.send(Command::SendRaw(serialized))
    }

    /// Register an RPC handler that the server can invoke by name.
    ///
    /// Overwrites any existing handler with the same method name.
    pub fn rpc_register<F>(&self, method: impl Into<String>, handler: F) -> Result<()>
    where
        F: Fn(RpcInvocation) + Send + Sync + 'static,
    {
        self.inner.send(Command::RpcRegister {
            method: method.into(),
            handler: Box::new(handler),
        })
    }

    /// Remove a previously-registered RPC handler.
    pub fn rpc_unregister(&self, method: impl Into<String>) -> Result<()> {
        self.inner.send(Command::RpcUnregister(method.into()))
    }
}

impl Drop for AgentInner {
    fn drop(&mut self) {
        // Flip the shared flag first so any in-flight connection setup
        // aborts even if our Shutdown command is consumed by a pump.
        self.shutdown.store(true, Ordering::SeqCst);
        if let Ok(tx) = self.command_tx.lock() {
            let _ = tx.send(Command::Shutdown);
        }
        if let Some(handle) = self.worker.lock().unwrap().take() {
            let _ = handle.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Worker thread
// ---------------------------------------------------------------------------

struct Callbacks {
    on_state_changed: Option<StateCb>,
    on_transcript: Option<TranscriptCb>,
    on_response: Option<TextCb>,
    on_error: Option<TextCb>,
    on_data: Option<DataCb>,
    on_raw_event: Option<RawCb>,
    on_wake_word: Option<WakeCb>,
}

fn worker_main(
    options: AgentOptions,
    cmd_rx: mpsc::Receiver<Command>,
    cmd_tx: mpsc::Sender<Command>,
    state_arc: Arc<Mutex<ConnectionState>>,
    shutdown: Arc<AtomicBool>,
) {
    let AgentOptions {
        publish,
        subscribe,
        stun_server,
        on_state_changed,
        on_transcript,
        on_response,
        on_error,
        on_data,
        on_raw_event,
        on_wake_word,
    } = options;

    let mut callbacks = Callbacks {
        on_state_changed,
        on_transcript,
        on_response,
        on_error,
        on_data,
        on_raw_event,
        on_wake_word,
    };

    let mut capturer: Option<Box<dyn Capturer>> = publish.map(|p| p.capturer);
    let mut rpc_handlers: HashMap<String, RpcHandler> = HashMap::new();

    // Shared playback queue: peer's on_audio callback pushes Opus frames,
    // the render thread pops, decodes, and writes to the speaker. Sized
    // for ~1 s of TTS audio.
    const MAX_OPUS_FRAMES: usize = 50;
    let playback_buf: Arc<spin::Mutex<VecDeque<Vec<u8>>>> = Arc::new(spin::Mutex::new(
        VecDeque::with_capacity(MAX_OPUS_FRAMES),
    ));

    // Half-duplex signal: each time the render thread finishes playing a
    // TTS frame it bumps this counter to PLAYBACK_TAIL. The mic side
    // decrements per encoded frame and drops the frame while >0.
    // Without this, the worker burns CPU re-encoding the speaker's own
    // output picked up by the mic — which on this hardware is enough to
    // skip the speaker DMA between TTS frames.
    let playback_tail = Arc::new(AtomicU32::new(0));

    // Render thread runs for the lifetime of the agent, completely
    // independent of mic capture. The previous design did playback in
    // the same loop as mic encoding, so any time we were encoding 400 ms
    // of mic audio the speaker was being starved and TTS broke up.
    if let Some(renderer) = subscribe.map(|s| s.renderer) {
        let pb = playback_buf.clone();
        let sd = shutdown.clone();
        let pt = playback_tail.clone();
        let _ = std::thread::Builder::new()
            .name("voiceagent-render".into())
            .stack_size(16 * 1024)
            .spawn(move || render_thread(renderer, pb, sd, pt, MAX_OPUS_FRAMES));
    }

    while !shutdown.load(Ordering::SeqCst) {
        // Idle — block until we get a Connect or Shutdown.
        match cmd_rx.recv() {
            Ok(Command::Connect { endpoint, token }) => {
                notify_state(&state_arc, &mut callbacks, ConnectionState::Connecting);
                match run_connection(
                    &endpoint,
                    token.as_deref(),
                    stun_server.as_deref(),
                    capturer.as_deref_mut(),
                    playback_buf.clone(),
                    playback_tail.clone(),
                    &mut rpc_handlers,
                    &mut callbacks,
                    &cmd_rx,
                    &cmd_tx,
                    &state_arc,
                    &shutdown,
                ) {
                    Ok(()) => {
                        notify_state(&state_arc, &mut callbacks, ConnectionState::Disconnected);
                    }
                    Err(e) => {
                        error!("voiceagent-worker: connection failed: {e:#}");
                        if let Some(ref mut cb) = callbacks.on_error {
                            let msg = alloc::format!("{e:#}");
                            cb(&msg);
                        }
                        notify_state(&state_arc, &mut callbacks, ConnectionState::Failed);
                    }
                }
            }
            Ok(Command::Shutdown) => {
                info!("voiceagent-worker: shutdown");
                return;
            }
            Ok(Command::RpcRegister { method, handler }) => {
                rpc_handlers.insert(method, handler);
            }
            Ok(Command::RpcUnregister(m)) => {
                rpc_handlers.remove(&m);
            }
            Ok(_) => {
                // Other commands only make sense while connected.
            }
            Err(_) => return, // channel closed = agent dropped
        }
    }
    info!("voiceagent-worker: exiting (shutdown flag set)");
}

#[allow(clippy::too_many_arguments)]
fn run_connection(
    whip_endpoint: &str,
    token: Option<&str>,
    stun: Option<&str>,
    mut capturer: Option<&mut (dyn Capturer + 'static)>,
    playback_buf: Arc<spin::Mutex<VecDeque<Vec<u8>>>>,
    playback_tail: Arc<AtomicU32>,
    rpc_handlers: &mut HashMap<String, RpcHandler>,
    callbacks: &mut Callbacks,
    cmd_rx: &mpsc::Receiver<Command>,
    cmd_tx: &mpsc::Sender<Command>,
    state_arc: &Arc<Mutex<ConnectionState>>,
    shutdown: &AtomicBool,
) -> Result<()> {
    info!("voiceagent-worker: connecting to {whip_endpoint}");

    // Cap matches the worker-level playback_buf creation.
    const MAX_OPUS_FRAMES: usize = 50;
    let pb_writer = playback_buf.clone();

    // Inbound-event queue: peer data-channel callback pushes, audio loop
    // processes. Keeps the callback non-blocking and keeps all user-facing
    // callback invocations on this thread.
    let inbound_queue: Arc<spin::Mutex<VecDeque<String>>> =
        Arc::new(spin::Mutex::new(VecDeque::new()));
    let ib_writer = inbound_queue.clone();

    let peer_callbacks = PeerCallbacks {
        on_connected: Some(Box::new(|| info!("WebRTC peer connected"))),
        on_disconnected: Some(Box::new(|| warn!("WebRTC peer disconnected"))),
        on_audio: Some(Box::new({
            let counter = alloc::sync::Arc::new(core::sync::atomic::AtomicU32::new(0));
            let drops = alloc::sync::Arc::new(core::sync::atomic::AtomicU32::new(0));
            move |audio_data| {
                let mut buf = pb_writer.lock();
                let mut dropped_now = 0u32;
                while buf.len() >= MAX_OPUS_FRAMES {
                    buf.pop_front();
                    dropped_now += 1;
                }
                buf.push_back(audio_data.to_vec());
                let total = counter
                    .fetch_add(1, core::sync::atomic::Ordering::Relaxed)
                    + 1;
                if dropped_now > 0 {
                    let total_drops = drops.fetch_add(
                        dropped_now,
                        core::sync::atomic::Ordering::Relaxed,
                    ) + dropped_now;
                    warn!(
                        "playback: queue overflow, dropping {dropped_now} \
                         frame(s) (total dropped={total_drops}, received={total})"
                    );
                } else if total % 100 == 0 {
                    info!(
                        "playback: rx={total} buf_len={} (cap={MAX_OPUS_FRAMES})",
                        buf.len()
                    );
                }
            }
        })),
        on_data: Some(Box::new(move |text: &str| {
            ib_writer.lock().push_back(text.to_string());
        })),
    };

    let peer = PeerConnection::new(stun.unwrap_or(""), peer_callbacks)?;

    // --- SDP offer / WHIP exchange -----------------------------------------
    peer.start_connection()?;
    let mut local_sdp = None;
    let sdp_start = std::time::Instant::now();
    while local_sdp.is_none() {
        let _ = peer.run_loop();
        local_sdp = peer.take_local_sdp();
        if sdp_start.elapsed() > Duration::from_secs(10) {
            anyhow::bail!("timed out waiting for local SDP");
        }
        std::thread::sleep(Duration::from_millis(20));
        pump_control_commands(cmd_rx, rpc_handlers, &mut capturer, shutdown)?;
    }
    let offer_sdp = local_sdp
        .unwrap()
        .replace("a=setup:passive", "a=setup:actpass");

    let whip_result = whip::whip_offer(whip_endpoint, &offer_sdp, token)?;
    peer.set_remote_sdp(&whip_result.answer_sdp)?;

    // --- Wait for DTLS + data channel layer --------------------------------
    let wait_start = std::time::Instant::now();
    while !peer.is_connected() {
        let _ = peer.run_loop();
        if wait_start.elapsed() > Duration::from_secs(15) {
            whip::whip_delete(&whip_result.session_url, token);
            anyhow::bail!("timed out waiting for WebRTC connection");
        }
        std::thread::sleep(Duration::from_millis(20));
        pump_control_commands(cmd_rx, rpc_handlers, &mut capturer, shutdown)?;
    }

    let dc_start = std::time::Instant::now();
    while !peer.is_data_channel_connected() {
        let _ = peer.run_loop();
        if dc_start.elapsed() > Duration::from_secs(10) {
            warn!("voiceagent-worker: data channel layer not ready, trying anyway");
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
        pump_control_commands(cmd_rx, rpc_handlers, &mut capturer, shutdown)?;
    }
    peer.create_data_channel("events")?;
    info!("voiceagent-worker: connected, data channel ready");
    notify_state(state_arc, callbacks, ConnectionState::Connected);

    // --- Opus codecs + audio loop state ------------------------------------
    // The decoder lives in the render thread (spawned by worker_main). Here
    // we only need the encoder for outbound mic audio.
    let mut opus_enc = OpusEncoder::new(16_000, 1, 32_000)?;
    const PTS_INCREMENT: u32 = 960;
    let mut pts: u32 = 0;
    let silence = alloc::vec![0u8; opus_enc.in_frame_size];

    // Bootstrap RTP so the server starts streaming the greeting.
    for _ in 0..5 {
        if let Ok(op) = opus_enc.encode(&silence) {
            let _ = peer.send_audio(op, pts);
            pts = pts.wrapping_add(PTS_INCREMENT);
        }
    }

    let rpc_responder: Arc<dyn Fn(&str) + Send + Sync> = {
        let tx = cmd_tx.clone();
        Arc::new(move |text: &str| {
            let _ = tx.send(Command::SendRaw(text.to_string()));
        })
    };

    // --- Main audio / command loop -----------------------------------------
    'main_loop: loop {
        if shutdown.load(Ordering::SeqCst) {
            info!("voiceagent-worker: shutdown flag set, exiting main loop");
            break;
        }
        if !peer.is_connected() {
            warn!("voiceagent-worker: connection lost");
            break;
        }
        let _ = peer.run_loop();

        // 1. Drain control commands.
        loop {
            match cmd_rx.try_recv() {
                Ok(Command::Disconnect) | Ok(Command::Shutdown) => {
                    info!("voiceagent-worker: disconnect requested");
                    break 'main_loop;
                }
                Ok(Command::SetMicEnabled(on)) => {
                    if let Some(ref mut cap) = capturer {
                        if let Err(e) = cap.set_enabled(on) {
                            warn!("set_mic_enabled failed: {e}");
                        }
                    }
                }
                Ok(Command::SendRaw(text)) => {
                    let _ = peer.send_data_channel(&text);
                }
                Ok(Command::RpcRegister { method, handler }) => {
                    rpc_handlers.insert(method, handler);
                }
                Ok(Command::RpcUnregister(m)) => {
                    rpc_handlers.remove(&m);
                }
                Ok(Command::Connect { .. }) => {
                    warn!("connect() called while already connected — ignoring");
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break 'main_loop,
            }
        }

        // 2. Process inbound data-channel events.
        while let Some(text) = inbound_queue.lock().pop_front() {
            dispatch_inbound(&text, rpc_handlers, &rpc_responder, callbacks);
        }

        // 3. Capture → encode → send.
        //
        // Drain UP TO `MAX_SEND_PER_ITER` 20 ms frames per worker pass so
        // STT keeps up with real-time, then yield to the playback stage
        // below. Previous behaviour (`break` after one frame) starved STT;
        // unbounded draining starves playback when the user is talking,
        // causing the inbound TTS queue to overflow at MAX_OPUS_FRAMES.
        // 20 frames = 400 ms of audio is the same bound the original
        // streamcoreai/esp32 main loop used for its mic-feed phase.
        // Poll on-device wake-word detection. Cheap (atomic read +
        // clear); when the model fired, surface it to the app callback.
        if let Some(ref mut cap) = capturer {
            if cap.consume_wake_event() {
                if let Some(ref mut cb) = callbacks.on_wake_word {
                    cb();
                }
            }
        }

        const MAX_SEND_PER_ITER: usize = 20;
        let mut sent_any = false;
        let mut sent_this_iter = 0usize;
        if let Some(ref mut cap) = capturer {
            'outer: while let Some(frame) = cap.read_frame() {
                let mut offset = 0;
                while offset + FRAME_SAMPLES <= frame.len() {
                    // Half-duplex: if the speaker is mid-playback, skip
                    // encoding this mic frame. Keeps the agent worker
                    // idle so the render thread can run the speaker DMA
                    // without contention, AND prevents the server from
                    // hearing its own TTS echoed back via the mic.
                    if playback_tail.load(Ordering::Relaxed) > 0 {
                        playback_tail.fetch_sub(1, Ordering::Relaxed);
                        offset += FRAME_SAMPLES;
                        continue;
                    }

                    let chunk = &frame[offset..offset + FRAME_SAMPLES];
                    let pcm_bytes: &[u8] = unsafe {
                        core::slice::from_raw_parts(chunk.as_ptr() as *const u8, FRAME_SAMPLES * 2)
                    };
                    match opus_enc.encode(pcm_bytes) {
                        Ok(op) => {
                            let _ = peer.send_audio(op, pts);
                            pts = pts.wrapping_add(PTS_INCREMENT);
                            sent_any = true;
                            sent_this_iter += 1;
                        }
                        Err(e) => warn!("opus encode: {e}"),
                    }
                    offset += FRAME_SAMPLES;
                    if sent_this_iter >= MAX_SEND_PER_ITER {
                        break 'outer;
                    }
                }
            }
        }

        // Playback decode + render lives in its own thread now (see
        // worker_main → render_thread). This worker only handles outbound
        // mic audio + protocol events, so the speaker can never get
        // starved while we're encoding a burst of mic frames.

        if !sent_any {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    // --- Shutdown ----------------------------------------------------------
    if let Some(ref mut cap) = capturer {
        let _ = cap.set_enabled(false);
    }
    whip::whip_delete(&whip_result.session_url, token);
    drop(peer);
    info!("voiceagent-worker: cleanly disconnected");
    Ok(())
}

// Pumps control commands during the connect phase (before the main audio
// loop starts). Returns Err to abort the connection if Shutdown/Disconnect
// was requested or the channel closed. Sets the shared shutdown flag so
// the outer worker_main loop also exits.
fn pump_control_commands(
    cmd_rx: &mpsc::Receiver<Command>,
    rpc_handlers: &mut HashMap<String, RpcHandler>,
    capturer: &mut Option<&mut (dyn Capturer + 'static)>,
    shutdown: &AtomicBool,
) -> Result<()> {
    if shutdown.load(Ordering::SeqCst) {
        anyhow::bail!("cancelled (shutdown)");
    }
    loop {
        match cmd_rx.try_recv() {
            Ok(Command::Disconnect) => anyhow::bail!("cancelled (disconnect)"),
            Ok(Command::Shutdown) => {
                shutdown.store(true, Ordering::SeqCst);
                anyhow::bail!("cancelled (shutdown)");
            }
            Ok(Command::SetMicEnabled(on)) => {
                if let Some(ref mut cap) = capturer {
                    let _ = cap.set_enabled(on);
                }
            }
            Ok(Command::RpcRegister { method, handler }) => {
                rpc_handlers.insert(method, handler);
            }
            Ok(Command::RpcUnregister(m)) => {
                rpc_handlers.remove(&m);
            }
            Ok(_) => {}
            Err(mpsc::TryRecvError::Empty) => return Ok(()),
            Err(mpsc::TryRecvError::Disconnected) => anyhow::bail!("agent dropped"),
        }
    }
}

// ---------------------------------------------------------------------------
// Inbound dispatcher
// ---------------------------------------------------------------------------

fn dispatch_inbound(
    text: &str,
    rpc_handlers: &HashMap<String, RpcHandler>,
    rpc_responder: &Arc<dyn Fn(&str) + Send + Sync>,
    callbacks: &mut Callbacks,
) {
    if let Some(ref mut cb) = callbacks.on_raw_event {
        cb(text);
    }

    let v: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return,
    };
    let msg_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match msg_type {
        "transcript" => {
            let t = v.get("text").and_then(|t| t.as_str()).unwrap_or("");
            let is_final = v.get("final").and_then(|b| b.as_bool()).unwrap_or(false);
            if !t.is_empty() {
                if let Some(ref mut cb) = callbacks.on_transcript {
                    cb(t, is_final);
                }
            }
        }
        "response" => {
            let t = v.get("text").and_then(|t| t.as_str()).unwrap_or("");
            if !t.is_empty() {
                if let Some(ref mut cb) = callbacks.on_response {
                    cb(t);
                }
            }
        }
        "error" => {
            let m = v
                .get("message")
                .and_then(|t| t.as_str())
                .unwrap_or("(unspecified server error)");
            if let Some(ref mut cb) = callbacks.on_error {
                cb(m);
            }
        }
        "data" => {
            let topic = v.get("topic").and_then(|t| t.as_str()).unwrap_or("");
            let payload = v.get("payload").and_then(|t| t.as_str()).unwrap_or("");
            let bytes = base64_decode(payload).unwrap_or_default();
            if let Some(ref mut cb) = callbacks.on_data {
                cb(topic, &bytes);
            }
        }
        "rpc.request" => {
            let id = v
                .get("id")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();
            let method = v
                .get("method")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();
            let params = v.get("params").cloned().unwrap_or(serde_json::Value::Null);

            if id.is_empty() || method.is_empty() {
                warn!("rpc.request missing id/method, dropping");
                return;
            }

            if let Some(handler) = rpc_handlers.get(&method) {
                let inv = RpcInvocation::new(id.clone(), method, params, rpc_responder.clone());
                handler(inv);
            } else {
                warn!("RPC method '{method}' not registered; returning error");
                let resp = RpcResponse {
                    type_: "rpc.response",
                    id: &id,
                    ok: false,
                    result: None,
                    error: Some(RpcError {
                        code: -32601,
                        message: "method not found",
                    }),
                };
                if let Ok(s) = serde_json::to_string(&resp) {
                    rpc_responder(&s);
                }
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// State notifier
// ---------------------------------------------------------------------------

fn notify_state(
    state_arc: &Arc<Mutex<ConnectionState>>,
    cbs: &mut Callbacks,
    new_state: ConnectionState,
) {
    *state_arc.lock().unwrap() = new_state;
    if let Some(ref mut cb) = cbs.on_state_changed {
        cb(new_state);
    }
}

// ---------------------------------------------------------------------------
// Base64 helpers (no_std friendly)
// ---------------------------------------------------------------------------

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

fn base64_decode(input: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u8> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => return None,
            _ => return None,
        })
    }
    let bytes: Vec<u8> = input.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if bytes.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let b0 = val(chunk[0])?;
        let b1 = val(chunk[1])?;
        let b2 = val(chunk[2]);
        let b3 = val(chunk[3]);
        let v = ((b0 as u32) << 18)
            | ((b1 as u32) << 12)
            | (b2.map(|b| b as u32).unwrap_or(0) << 6)
            | b3.map(|b| b as u32).unwrap_or(0);
        out.push(((v >> 16) & 0xFF) as u8);
        if b2.is_some() {
            out.push(((v >> 8) & 0xFF) as u8);
        }
        if b3.is_some() {
            out.push((v & 0xFF) as u8);
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Render thread — decodes Opus audio frames from the WebRTC peer and writes
// PCM to the renderer (typically the speaker I2S output). Runs in its own
// FreeRTOS task so it can never be starved by the mic-capture / encode work
// the agent worker is doing.
// ---------------------------------------------------------------------------
fn render_thread(
    mut renderer: Box<dyn Renderer>,
    playback_buf: Arc<spin::Mutex<VecDeque<Vec<u8>>>>,
    shutdown: Arc<AtomicBool>,
    playback_tail: Arc<AtomicU32>,
    _capacity_hint: usize,
) {
    let mut opus_dec = match OpusDecoder::new(16_000, 1) {
        Ok(d) => d,
        Err(e) => {
            error!("render-thread: opus_dec init failed: {e:#}");
            return;
        }
    };
    info!("render-thread: ready");

    // Each successful render bumps the half-duplex tail back to this
    // many mic frames. Matches the original streamcoreai value.
    const PLAYBACK_TAIL: u32 = 15;

    while !shutdown.load(Ordering::SeqCst) {
        let frame = { playback_buf.lock().pop_front() };
        match frame {
            Some(op) => match opus_dec.decode(&op) {
                Ok(pcm_bytes) => {
                    let samples: &[i16] = unsafe {
                        core::slice::from_raw_parts(
                            pcm_bytes.as_ptr() as *const i16,
                            pcm_bytes.len() / 2,
                        )
                    };
                    match renderer.render_audio(samples) {
                        Ok(_) => {
                            playback_tail.store(PLAYBACK_TAIL, Ordering::Relaxed);
                        }
                        Err(e) => warn!("render-thread: renderer: {e}"),
                    }
                }
                Err(e) => warn!("render-thread: opus decode: {e}"),
            },
            None => std::thread::sleep(Duration::from_millis(2)),
        }
    }
    info!("render-thread: exiting");
}
