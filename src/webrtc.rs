//! Safe Rust wrapper around the `esp_peer` C library.
//!
//! `new()` → `start_connection()` → WHIP exchange → `set_remote_sdp()` →
//! poll `run_loop()` every ~20 ms → `send_audio()` / `send_data_channel()`.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::{c_int, c_void};
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use anyhow::{bail, Result};
use log::{info, warn};

use crate::esp_peer_ffi::*;

struct CallbackCtx {
    connected: AtomicBool,
    state: AtomicI32,
    local_sdp: spin::Mutex<Option<String>>,
    on_audio: Option<Box<dyn Fn(&[u8]) + Send>>,
    on_data: Option<Box<dyn Fn(&str) + Send>>,
    on_connected: Option<Box<dyn Fn() + Send>>,
    on_disconnected: Option<Box<dyn Fn() + Send>>,
}

unsafe extern "C" fn cb_on_state(state: esp_peer_state_t, ctx: *mut c_void) -> c_int {
    let cx = &*(ctx as *const CallbackCtx);
    cx.state.store(state, Ordering::Relaxed);
    match state {
        ESP_PEER_STATE_CONNECTED => {
            info!("esp_peer: CONNECTED");
            cx.connected.store(true, Ordering::SeqCst);
            if let Some(ref cb) = cx.on_connected {
                cb();
            }
        }
        ESP_PEER_STATE_DISCONNECTED | ESP_PEER_STATE_CONNECT_FAILED => {
            info!("esp_peer: DISCONNECTED/FAILED (state={state})");
            cx.connected.store(false, Ordering::SeqCst);
            if let Some(ref cb) = cx.on_disconnected {
                cb();
            }
        }
        ESP_PEER_STATE_DATA_CHANNEL_OPENED => {
            info!("esp_peer: data channel opened");
        }
        _ => {
            info!("esp_peer: state={state}");
        }
    }
    0
}

unsafe extern "C" fn cb_on_msg(msg: *mut esp_peer_msg_t, ctx: *mut c_void) -> c_int {
    if msg.is_null() {
        return -1;
    }
    let m = &*msg;
    let cx = &*(ctx as *const CallbackCtx);
    if m.r#type == ESP_PEER_MSG_TYPE_SDP && !m.data.is_null() && m.size > 0 {
        let slice = core::slice::from_raw_parts(m.data, m.size as usize);
        if let Ok(sdp) = core::str::from_utf8(slice) {
            info!("esp_peer: received local SDP ({} bytes)", sdp.len());
            *cx.local_sdp.lock() = Some(String::from(sdp));
        }
    }
    0
}

unsafe extern "C" fn cb_on_audio_data(
    frame: *mut esp_peer_audio_frame_t,
    ctx: *mut c_void,
) -> c_int {
    if frame.is_null() {
        return -1;
    }
    let f = &*frame;
    let cx = &*(ctx as *const CallbackCtx);
    if f.pts % 3000 == 0 {
        info!("RX audio: size={} pts={}", f.size, f.pts);
    }
    if let Some(ref cb) = cx.on_audio {
        if !f.data.is_null() && f.size > 0 {
            let slice = core::slice::from_raw_parts(f.data, f.size as usize);
            cb(slice);
        }
    }
    0
}

unsafe extern "C" fn cb_on_data(frame: *mut esp_peer_data_frame_t, ctx: *mut c_void) -> c_int {
    if frame.is_null() {
        return -1;
    }
    let f = &*frame;
    let cx = &*(ctx as *const CallbackCtx);
    info!("RX data frame: size={} type={}", f.size, f.r#type);
    if let Some(ref cb) = cx.on_data {
        if !f.data.is_null() && f.size > 0 {
            let slice = core::slice::from_raw_parts(f.data, f.size as usize);
            if let Ok(text) = core::str::from_utf8(slice) {
                cb(text);
            }
        }
    }
    0
}

unsafe extern "C" fn cb_noop_audio_info(
    _: *mut esp_peer_audio_stream_info_t,
    _: *mut c_void,
) -> c_int {
    0
}
unsafe extern "C" fn cb_noop_video_info(
    _: *mut esp_peer_video_stream_info_t,
    _: *mut c_void,
) -> c_int {
    0
}
unsafe extern "C" fn cb_noop_video_data(_: *mut esp_peer_video_frame_t, _: *mut c_void) -> c_int {
    0
}
unsafe extern "C" fn cb_noop_channel(
    _: *mut esp_peer_data_channel_info_t,
    _: *mut c_void,
) -> c_int {
    0
}

pub struct PeerCallbacks {
    pub on_connected: Option<Box<dyn Fn() + Send>>,
    pub on_disconnected: Option<Box<dyn Fn() + Send>>,
    pub on_audio: Option<Box<dyn Fn(&[u8]) + Send>>,
    pub on_data: Option<Box<dyn Fn(&str) + Send>>,
}

impl Default for PeerCallbacks {
    fn default() -> Self {
        Self {
            on_connected: None,
            on_disconnected: None,
            on_audio: None,
            on_data: None,
        }
    }
}

pub struct PeerConnection {
    handle: esp_peer_handle_t,
    ctx: Box<CallbackCtx>,
    _ice_server_strs: Vec<alloc::ffi::CString>,
}

unsafe impl Send for PeerConnection {}
unsafe impl Sync for PeerConnection {}

impl PeerConnection {
    pub fn new(stun_url: &str, callbacks: PeerCallbacks) -> Result<Self> {
        let stun_c = if stun_url.is_empty() {
            None
        } else {
            Some(alloc::ffi::CString::new(stun_url)?)
        };

        let ctx = Box::new(CallbackCtx {
            connected: AtomicBool::new(false),
            state: AtomicI32::new(ESP_PEER_STATE_CLOSED),
            local_sdp: spin::Mutex::new(None),
            on_audio: callbacks.on_audio,
            on_data: callbacks.on_data,
            on_connected: callbacks.on_connected,
            on_disconnected: callbacks.on_disconnected,
        });
        let ctx_ptr = &*ctx as *const CallbackCtx as *mut c_void;

        let mut ice_server = esp_peer_ice_server_cfg_t {
            stun_url: stun_c
                .as_ref()
                .map_or(ptr::null_mut(), |c| c.as_ptr() as *mut _),
            user: ptr::null_mut(),
            psw: ptr::null_mut(),
        };

        let (server_lists, server_num) = if stun_c.is_some() {
            (&mut ice_server as *mut _, 1u8)
        } else {
            (ptr::null_mut(), 0u8)
        };

        let peer_default_cfg = esp_peer_default_cfg_t::default();

        let cfg = esp_peer_cfg_t {
            server_lists,
            server_num,
            role: ESP_PEER_ROLE_CONTROLLING,
            ice_trans_policy: ESP_PEER_ICE_TRANS_POLICY_ALL,
            audio_info: esp_peer_audio_stream_info_t {
                codec: ESP_PEER_AUDIO_CODEC_OPUS,
                sample_rate: 48000,
                channel: 1,
            },
            video_info: esp_peer_video_stream_info_t::default(),
            audio_dir: ESP_PEER_MEDIA_DIR_SEND_RECV,
            video_dir: ESP_PEER_MEDIA_DIR_NONE,
            no_auto_reconnect: true,
            enable_data_channel: true,
            manual_ch_create: false,
            extra_cfg: &peer_default_cfg as *const _ as *const c_void,
            extra_size: core::mem::size_of::<esp_peer_default_cfg_t>() as c_int,
            ctx: ctx_ptr,
            on_state: Some(cb_on_state),
            on_msg: Some(cb_on_msg),
            on_video_info: Some(cb_noop_video_info),
            on_audio_info: Some(cb_noop_audio_info),
            on_audio_data: Some(cb_on_audio_data),
            on_video_data: Some(cb_noop_video_data),
            on_channel_open: Some(cb_noop_channel),
            on_data: Some(cb_on_data),
            on_channel_close: Some(cb_noop_channel),
        };

        let mut handle: esp_peer_handle_t = ptr::null_mut();
        let ret = unsafe { esp_peer_open(&cfg, esp_peer_get_default_impl(), &mut handle) };
        if ret != ESP_PEER_ERR_NONE || handle.is_null() {
            bail!("esp_peer_open failed: {ret}");
        }

        info!("esp_peer: opened");
        Ok(Self {
            handle,
            ctx,
            _ice_server_strs: stun_c.into_iter().collect(),
        })
    }

    pub fn start_connection(&self) -> Result<()> {
        let ret = unsafe { esp_peer_new_connection(self.handle) };
        if ret != ESP_PEER_ERR_NONE {
            bail!("esp_peer_new_connection failed: {ret}");
        }
        info!("esp_peer: new_connection started");
        Ok(())
    }

    pub fn take_local_sdp(&self) -> Option<String> {
        self.ctx.local_sdp.lock().take()
    }

    pub fn set_remote_sdp(&self, sdp: &str) -> Result<()> {
        let mut msg = esp_peer_msg_t {
            r#type: ESP_PEER_MSG_TYPE_SDP,
            data: sdp.as_ptr() as *mut u8,
            size: sdp.len() as c_int,
        };
        let ret = unsafe { esp_peer_send_msg(self.handle, &mut msg) };
        if ret != ESP_PEER_ERR_NONE {
            bail!("esp_peer_send_msg (remote SDP) failed: {ret}");
        }
        info!("esp_peer: remote SDP set ({} bytes)", sdp.len());
        Ok(())
    }

    pub fn run_loop(&self) -> Result<()> {
        let ret = unsafe { esp_peer_main_loop(self.handle) };
        if ret != ESP_PEER_ERR_NONE {
            bail!("esp_peer_main_loop failed: {ret}");
        }
        Ok(())
    }

    pub fn send_audio(&self, data: &[u8], pts: u32) -> Result<()> {
        let mut frame = esp_peer_audio_frame_t {
            pts,
            data: data.as_ptr() as *mut u8,
            size: data.len() as c_int,
        };
        let ret = unsafe { esp_peer_send_audio(self.handle, &mut frame) };
        if ret != ESP_PEER_ERR_NONE {
            warn!("esp_peer_send_audio failed: {ret}");
            bail!("esp_peer_send_audio failed: {ret}");
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub fn send_data_channel(&self, text: &str) -> Result<()> {
        let mut frame = esp_peer_data_frame_t {
            r#type: ESP_PEER_DATA_CHANNEL_STRING,
            stream_id: 0,
            data: text.as_ptr() as *mut u8,
            size: text.len() as c_int,
        };
        let ret = unsafe { esp_peer_send_data(self.handle, &mut frame) };
        if ret != ESP_PEER_ERR_NONE {
            bail!("esp_peer_send_data failed: {ret}");
        }
        Ok(())
    }

    pub fn create_data_channel(&self, label: &str) -> Result<()> {
        let label_c = alloc::ffi::CString::new(label)?;
        let mut cfg = esp_peer_data_channel_cfg_t {
            reliable_type: ESP_PEER_DATA_CHANNEL_RELIABLE,
            ordered: true,
            label: label_c.as_ptr() as *mut _,
            max_retransmit: 0,
        };
        let ret = unsafe { esp_peer_create_data_channel(self.handle, &mut cfg) };
        if ret != ESP_PEER_ERR_NONE {
            bail!("esp_peer_create_data_channel failed: {ret}");
        }
        info!("esp_peer: data channel '{}' created", label);
        Ok(())
    }

    pub fn is_connected(&self) -> bool {
        self.ctx.connected.load(Ordering::SeqCst)
    }

    pub fn state(&self) -> esp_peer_state_t {
        self.ctx.state.load(Ordering::Relaxed)
    }

    pub fn is_data_channel_connected(&self) -> bool {
        let state = self.ctx.state.load(Ordering::Relaxed);
        state >= ESP_PEER_STATE_DATA_CHANNEL_CONNECTED
    }
}

impl Drop for PeerConnection {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            info!("esp_peer: closing");
            unsafe {
                esp_peer_close(self.handle);
            }
        }
    }
}
