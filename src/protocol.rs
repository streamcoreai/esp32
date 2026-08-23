//! JSON wire format used over the WebRTC data channel.
//!
//! Every frame is a JSON object with a `type` field. The SDK handles the
//! following built-in types; anything else is delivered verbatim to the
//! user via [`AgentOptions::on_raw_event`](crate::AgentOptions::on_raw_event).
//!
//! | `type`         | Direction | Purpose                                   |
//! | -------------- | --------- | ----------------------------------------- |
//! | `transcript`   | in        | User speech (partial or final)            |
//! | `response`     | in        | Assistant text response chunk             |
//! | `error`        | in        | Server-reported error                     |
//! | `rpc.request`  | in        | Server invokes a registered RPC method    |
//! | `rpc.response` | out       | Device answers an `rpc.request`           |
//! | `data`         | both      | App-defined topic-based payload           |

use serde::Serialize;

/// Device → server: answer to an RPC invocation.
#[derive(Serialize)]
pub(crate) struct RpcResponse<'a> {
    #[serde(rename = "type")]
    pub type_: &'static str, // always "rpc.response"
    pub id: &'a str,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError<'a>>,
}

#[derive(Serialize)]
pub(crate) struct RpcError<'a> {
    pub code: i32,
    pub message: &'a str,
}

/// Device → server: topic-based data packet (payload base64-encoded).
#[derive(Serialize)]
pub(crate) struct DataMsg<'a> {
    #[serde(rename = "type")]
    pub type_: &'static str, // always "data"
    pub topic: &'a str,
    pub payload: &'a str,
}
