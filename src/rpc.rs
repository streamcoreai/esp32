//! Remote Procedure Calls over the data channel.
//!
//! Register handlers with [`Agent::rpc_register`](crate::Agent::rpc_register)
//! and the server can invoke them by sending:
//! ```json
//! {"type":"rpc.request","id":"1","method":"capture_photo","params":{"quality":80}}
//! ```
//!
//! Your handler receives an [`RpcInvocation`] and must eventually call
//! [`RpcInvocation::return_ok`] or [`RpcInvocation::return_err`] — the
//! response is auto-sent back to the server:
//! ```json
//! {"type":"rpc.response","id":"1","ok":true,"result":{"bytes":4221}}
//! ```
//!
//! This is how voice agents expose device capabilities to the LLM (e.g.
//! `capture_photo`, `read_sensor`, `set_led`). Handlers may run on the
//! SDK's worker thread — don't block for long. For slow operations,
//! return immediately and send the result later via
//! [`Agent::publish_data`](crate::Agent::publish_data).

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};
use log::warn;
use serde::Serialize;

use crate::protocol::{RpcError, RpcResponse};

/// A handle passed to your RPC handler. Call exactly one of
/// [`return_ok`](Self::return_ok) or [`return_err`](Self::return_err) — if
/// the handler drops without doing so, the SDK returns a default error to
/// the server so the caller doesn't hang.
pub struct RpcInvocation {
    pub id: String,
    pub method: String,
    pub params: serde_json::Value,
    responder: Arc<dyn Fn(&str) + Send + Sync>,
    replied: AtomicBool,
}

impl RpcInvocation {
    pub(crate) fn new(
        id: String,
        method: String,
        params: serde_json::Value,
        responder: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> Self {
        Self {
            id,
            method,
            params,
            responder,
            replied: AtomicBool::new(false),
        }
    }

    /// Return a successful result to the caller.
    pub fn return_ok<T: Serialize>(&self, result: T) {
        if self.replied.swap(true, Ordering::SeqCst) {
            warn!("RpcInvocation::return_ok called twice for id={}", self.id);
            return;
        }
        let json_result = match serde_json::to_value(&result) {
            Ok(v) => Some(v),
            Err(e) => {
                warn!("RPC {}: result serialize failed: {e}", self.id);
                Some(serde_json::Value::Null)
            }
        };
        let msg = RpcResponse {
            type_: "rpc.response",
            id: &self.id,
            ok: true,
            result: json_result,
            error: None,
        };
        if let Ok(s) = serde_json::to_string(&msg) {
            (self.responder)(&s);
        }
    }

    /// Return an error to the caller.
    pub fn return_err(&self, code: i32, message: &str) {
        if self.replied.swap(true, Ordering::SeqCst) {
            warn!("RpcInvocation::return_err called twice for id={}", self.id);
            return;
        }
        let msg = RpcResponse {
            type_: "rpc.response",
            id: &self.id,
            ok: false,
            result: None,
            error: Some(RpcError { code, message }),
        };
        if let Ok(s) = serde_json::to_string(&msg) {
            (self.responder)(&s);
        }
    }
}

impl Drop for RpcInvocation {
    fn drop(&mut self) {
        if !self.replied.load(Ordering::SeqCst) {
            warn!(
                "RPC {} ({}) dropped without response; returning default error",
                self.id, self.method
            );
            // Mark as replied so our own return_err below doesn't log again.
            self.replied.store(true, Ordering::SeqCst);
            let msg = RpcResponse {
                type_: "rpc.response",
                id: &self.id,
                ok: false,
                result: None,
                error: Some(RpcError {
                    code: -1,
                    message: "handler dropped invocation without response",
                }),
            };
            if let Ok(s) = serde_json::to_string(&msg) {
                (self.responder)(&s);
            }
        }
    }
}

pub(crate) type RpcHandler = Box<dyn Fn(RpcInvocation) + Send + Sync>;
