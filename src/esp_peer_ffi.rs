//! Raw FFI bindings to `esp_peer` (esp-webrtc-solution).

#![allow(non_camel_case_types, non_upper_case_globals, dead_code)]

use core::ffi::{c_char, c_int, c_void};

pub type esp_peer_handle_t = *mut c_void;

pub type esp_peer_state_t = c_int;
pub const ESP_PEER_STATE_CLOSED: c_int = 0;
pub const ESP_PEER_STATE_DISCONNECTED: c_int = 1;
pub const ESP_PEER_STATE_NEW_CONNECTION: c_int = 2;
pub const ESP_PEER_STATE_CANDIDATE_GATHERING: c_int = 3;
pub const ESP_PEER_STATE_PAIRING: c_int = 4;
pub const ESP_PEER_STATE_PAIRED: c_int = 5;
pub const ESP_PEER_STATE_CONNECTING: c_int = 6;
pub const ESP_PEER_STATE_CONNECTED: c_int = 7;
pub const ESP_PEER_STATE_CONNECT_FAILED: c_int = 8;
pub const ESP_PEER_STATE_DATA_CHANNEL_CONNECTED: c_int = 9;
pub const ESP_PEER_STATE_DATA_CHANNEL_OPENED: c_int = 10;
pub const ESP_PEER_STATE_DATA_CHANNEL_CLOSED: c_int = 11;
pub const ESP_PEER_STATE_DATA_CHANNEL_DISCONNECTED: c_int = 12;

pub type esp_peer_audio_codec_t = c_int;
pub const ESP_PEER_AUDIO_CODEC_NONE: c_int = 0;
pub const ESP_PEER_AUDIO_CODEC_G711A: c_int = 1;
pub const ESP_PEER_AUDIO_CODEC_G711U: c_int = 2;
pub const ESP_PEER_AUDIO_CODEC_OPUS: c_int = 3;

pub type esp_peer_video_codec_t = c_int;
pub const ESP_PEER_VIDEO_CODEC_NONE: c_int = 0;

pub type esp_peer_data_channel_type_t = c_int;
pub const ESP_PEER_DATA_CHANNEL_NONE: c_int = 0;
pub const ESP_PEER_DATA_CHANNEL_DATA: c_int = 1;
pub const ESP_PEER_DATA_CHANNEL_STRING: c_int = 2;

pub type esp_peer_data_channel_reliable_type_t = c_int;
pub const ESP_PEER_DATA_CHANNEL_RELIABLE: c_int = 0;
pub const ESP_PEER_DATA_CHANNEL_PARTIAL_RELIABLE_TIMEOUT: c_int = 1;
pub const ESP_PEER_DATA_CHANNEL_PARTIAL_RELIABLE_RETX: c_int = 2;

pub type esp_peer_media_dir_t = c_int;
pub const ESP_PEER_MEDIA_DIR_NONE: c_int = 0;
pub const ESP_PEER_MEDIA_DIR_SEND_ONLY: c_int = 1;
pub const ESP_PEER_MEDIA_DIR_RECV_ONLY: c_int = 2;
pub const ESP_PEER_MEDIA_DIR_SEND_RECV: c_int = 3;

pub type esp_peer_ice_trans_policy_t = c_int;
pub const ESP_PEER_ICE_TRANS_POLICY_ALL: c_int = 0;

pub type esp_peer_role_t = c_int;
pub const ESP_PEER_ROLE_CONTROLLING: c_int = 0;
pub const ESP_PEER_ROLE_CONTROLLED: c_int = 1;

pub type esp_peer_msg_type_t = c_int;
pub const ESP_PEER_MSG_TYPE_NONE: c_int = 0;
pub const ESP_PEER_MSG_TYPE_SDP: c_int = 1;
pub const ESP_PEER_MSG_TYPE_CANDIDATE: c_int = 2;

pub const ESP_PEER_ERR_NONE: c_int = 0;

#[repr(C)]
pub struct esp_peer_ice_server_cfg_t {
    pub stun_url: *mut c_char,
    pub user: *mut c_char,
    pub psw: *mut c_char,
}

#[repr(C)]
#[derive(Default)]
pub struct esp_peer_audio_stream_info_t {
    pub codec: esp_peer_audio_codec_t,
    pub sample_rate: u32,
    pub channel: u8,
}

#[repr(C)]
#[derive(Default)]
pub struct esp_peer_video_stream_info_t {
    pub codec: esp_peer_video_codec_t,
    pub width: c_int,
    pub height: c_int,
    pub fps: c_int,
}

#[repr(C)]
pub struct esp_peer_audio_frame_t {
    pub pts: u32,
    pub data: *mut u8,
    pub size: c_int,
}

#[repr(C)]
pub struct esp_peer_data_frame_t {
    pub r#type: esp_peer_data_channel_type_t,
    pub stream_id: u16,
    pub data: *mut u8,
    pub size: c_int,
}

#[repr(C)]
pub struct esp_peer_msg_t {
    pub r#type: esp_peer_msg_type_t,
    pub data: *mut u8,
    pub size: c_int,
}

#[repr(C)]
pub struct esp_peer_data_channel_info_t {
    pub label: *const c_char,
    pub stream_id: u16,
}

#[repr(C)]
pub struct esp_peer_data_channel_cfg_t {
    pub reliable_type: esp_peer_data_channel_reliable_type_t,
    pub ordered: bool,
    pub label: *mut c_char,
    pub max_retransmit: u16,
}

pub type on_state_fn =
    Option<unsafe extern "C" fn(state: esp_peer_state_t, ctx: *mut c_void) -> c_int>;
pub type on_msg_fn =
    Option<unsafe extern "C" fn(msg: *mut esp_peer_msg_t, ctx: *mut c_void) -> c_int>;
pub type on_audio_info_fn = Option<
    unsafe extern "C" fn(info: *mut esp_peer_audio_stream_info_t, ctx: *mut c_void) -> c_int,
>;
pub type on_video_info_fn = Option<
    unsafe extern "C" fn(info: *mut esp_peer_video_stream_info_t, ctx: *mut c_void) -> c_int,
>;
pub type on_audio_data_fn =
    Option<unsafe extern "C" fn(frame: *mut esp_peer_audio_frame_t, ctx: *mut c_void) -> c_int>;
pub type on_video_data_fn =
    Option<unsafe extern "C" fn(frame: *mut esp_peer_video_frame_t, ctx: *mut c_void) -> c_int>;
pub type on_channel_fn =
    Option<unsafe extern "C" fn(ch: *mut esp_peer_data_channel_info_t, ctx: *mut c_void) -> c_int>;
pub type on_data_fn =
    Option<unsafe extern "C" fn(frame: *mut esp_peer_data_frame_t, ctx: *mut c_void) -> c_int>;

#[repr(C)]
pub struct esp_peer_video_frame_t {
    pub pts: u32,
    pub data: *mut u8,
    pub size: c_int,
}

#[repr(C)]
pub struct esp_peer_cfg_t {
    pub server_lists: *mut esp_peer_ice_server_cfg_t,
    pub server_num: u8,
    pub role: esp_peer_role_t,
    pub ice_trans_policy: esp_peer_ice_trans_policy_t,
    pub audio_info: esp_peer_audio_stream_info_t,
    pub video_info: esp_peer_video_stream_info_t,
    pub audio_dir: esp_peer_media_dir_t,
    pub video_dir: esp_peer_media_dir_t,
    pub no_auto_reconnect: bool,
    pub enable_data_channel: bool,
    pub manual_ch_create: bool,
    pub extra_cfg: *const c_void,
    pub extra_size: c_int,
    pub ctx: *mut c_void,
    pub on_state: on_state_fn,
    pub on_msg: on_msg_fn,
    pub on_video_info: on_video_info_fn,
    pub on_audio_info: on_audio_info_fn,
    pub on_audio_data: on_audio_data_fn,
    pub on_video_data: on_video_data_fn,
    pub on_channel_open: on_channel_fn,
    pub on_data: on_data_fn,
    pub on_channel_close: on_channel_fn,
}

#[repr(C)]
#[derive(Default)]
pub struct esp_peer_default_jitter_cfg_t {
    pub cache_timeout: u16,
    pub resend_delay: u16,
    pub pli_send_interval: u16,
    pub cache_size: u32,
}

#[repr(C)]
#[derive(Default)]
pub struct esp_peer_default_rtp_cfg_t {
    pub audio_recv_jitter: esp_peer_default_jitter_cfg_t,
    pub video_recv_jitter: esp_peer_default_jitter_cfg_t,
    pub send_pool_size: u32,
    pub send_queue_num: u32,
    pub max_resend_count: u16,
}

#[repr(C)]
#[derive(Default)]
pub struct esp_peer_default_data_ch_cfg_t {
    pub cache_timeout: u16,
    pub send_cache_size: u32,
    pub recv_cache_size: u32,
}

#[repr(C)]
#[derive(Default)]
pub struct esp_peer_default_cfg_t {
    pub agent_recv_timeout: u16,
    pub data_ch_cfg: esp_peer_default_data_ch_cfg_t,
    pub rtp_cfg: esp_peer_default_rtp_cfg_t,
    pub keep_role: bool,
    pub ipv6_support: bool,
    pub max_candidates: u8,
    pub alive_binding_retries: u8,
    pub ice_use_lite_mode: bool,
}

#[repr(C)]
pub struct esp_peer_ops_t {
    _opaque: [u8; 0],
}

extern "C" {
    pub fn esp_peer_get_default_impl() -> *const esp_peer_ops_t;
    pub fn esp_peer_open(
        cfg: *const esp_peer_cfg_t,
        ops: *const esp_peer_ops_t,
        peer: *mut esp_peer_handle_t,
    ) -> c_int;
    pub fn esp_peer_new_connection(peer: esp_peer_handle_t) -> c_int;
    pub fn esp_peer_create_data_channel(
        peer: esp_peer_handle_t,
        cfg: *mut esp_peer_data_channel_cfg_t,
    ) -> c_int;
    pub fn esp_peer_send_msg(peer: esp_peer_handle_t, msg: *const esp_peer_msg_t) -> c_int;
    pub fn esp_peer_send_audio(
        peer: esp_peer_handle_t,
        frame: *mut esp_peer_audio_frame_t,
    ) -> c_int;
    pub fn esp_peer_send_data(peer: esp_peer_handle_t, frame: *mut esp_peer_data_frame_t) -> c_int;
    pub fn esp_peer_main_loop(peer: esp_peer_handle_t) -> c_int;
    pub fn esp_peer_disconnect(peer: esp_peer_handle_t) -> c_int;
    pub fn esp_peer_close(peer: esp_peer_handle_t) -> c_int;
    pub fn esp_peer_pre_generate_cert() -> c_int;
}
