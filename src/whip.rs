//! WHIP signaling client (RFC 9725) using ESP-IDF's HTTP client.

use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{bail, Result};
use embedded_svc::http::client::Client as HttpClient;
use esp_idf_svc::http::client::{Configuration as HttpConfig, EspHttpConnection};
use esp_idf_svc::http::Method;
use log::info;

pub struct WhipResult {
    pub answer_sdp: String,
    pub session_url: String,
}

pub fn whip_offer(endpoint: &str, offer_sdp: &str, token: Option<&str>) -> Result<WhipResult> {
    let http_cfg = HttpConfig {
        buffer_size: Some(4096),
        buffer_size_tx: Some(4096),
        timeout: Some(core::time::Duration::from_secs(15)),
        ..Default::default()
    };

    let mut client = HttpClient::wrap(EspHttpConnection::new(&http_cfg)?);

    let content_type = "application/sdp";
    let content_len = offer_sdp.len().to_string();
    let auth_value = token.map(|t| alloc::format!("Bearer {}", t));
    let mut headers_vec: Vec<(&str, &str)> = alloc::vec![
        ("Content-Type", content_type),
        ("Content-Length", content_len.as_str()),
    ];
    if let Some(ref auth) = auth_value {
        headers_vec.push(("Authorization", auth.as_str()));
    }

    let mut request = client.request(Method::Post, endpoint, &headers_vec)?;
    request.write(offer_sdp.as_bytes())?;
    let mut response = request.submit()?;

    let status = response.status();
    if status != 201 {
        let mut body = Vec::new();
        let mut buf = [0u8; 512];
        loop {
            let n = response.read(&mut buf)?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&buf[..n]);
        }
        let body_str = String::from_utf8_lossy(&body);
        bail!("WHIP offer failed: HTTP {status}: {body_str}");
    }

    let location = response.header("location").unwrap_or("").to_string();

    let session_url = if location.starts_with("http") {
        location
    } else {
        if let Some(idx) = endpoint.find("://") {
            if let Some(slash) = endpoint[idx + 3..].find('/') {
                let base = &endpoint[..idx + 3 + slash];
                alloc::format!("{}{}", base, location)
            } else {
                alloc::format!("{}{}", endpoint, location)
            }
        } else {
            location
        }
    };

    let mut answer_buf = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        let n = response.read(&mut buf)?;
        if n == 0 {
            break;
        }
        answer_buf.extend_from_slice(&buf[..n]);
    }
    let answer_sdp = String::from_utf8(answer_buf)
        .map_err(|e| anyhow::anyhow!("invalid UTF-8 in SDP answer: {e}"))?;

    info!("WHIP offer accepted, session: {}", session_url);
    Ok(WhipResult {
        answer_sdp,
        session_url,
    })
}

pub fn whip_delete(session_url: &str, token: Option<&str>) {
    if session_url.is_empty() {
        return;
    }

    let http_cfg = HttpConfig {
        timeout: Some(core::time::Duration::from_secs(5)),
        ..Default::default()
    };

    let auth_value = token.map(|t| alloc::format!("Bearer {}", t));
    let mut headers_vec: Vec<(&str, &str)> = Vec::new();
    if let Some(ref auth) = auth_value {
        headers_vec.push(("Authorization", auth.as_str()));
    }

    match HttpClient::wrap(EspHttpConnection::new(&http_cfg).unwrap()).request(
        Method::Delete,
        session_url,
        &headers_vec,
    ) {
        Ok(request) => {
            let _ = request.submit();
            info!("WHIP session deleted: {}", session_url);
        }
        Err(e) => {
            log::warn!("WHIP delete failed: {e}");
        }
    }
}

pub fn fetch_token(token_url: &str, api_key: Option<&str>) -> Result<String> {
    let http_cfg = HttpConfig {
        buffer_size: Some(2048),
        buffer_size_tx: Some(1024),
        timeout: Some(core::time::Duration::from_secs(10)),
        ..Default::default()
    };

    let mut client = HttpClient::wrap(EspHttpConnection::new(&http_cfg)?);

    let auth_value = api_key.map(|k| alloc::format!("Bearer {}", k));
    let mut headers_vec: Vec<(&str, &str)> = alloc::vec![("Content-Length", "0")];
    if let Some(ref auth) = auth_value {
        headers_vec.push(("Authorization", auth.as_str()));
    }

    let request = client.request(Method::Post, token_url, &headers_vec)?;
    let mut response = request.submit()?;

    let status = response.status();
    if status != 200 {
        let mut body = Vec::new();
        let mut buf = [0u8; 512];
        loop {
            let n = response.read(&mut buf)?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&buf[..n]);
        }
        let body_str = String::from_utf8_lossy(&body);
        bail!("Token fetch failed: HTTP {status}: {body_str}");
    }

    let mut body = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        let n = response.read(&mut buf)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&buf[..n]);
    }

    let json: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| anyhow::anyhow!("invalid JSON in token response: {e}"))?;
    let token = json
        .get("token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing 'token' field in response"))?;

    info!("Fetched JWT token ({} bytes)", token.len());
    Ok(String::from(token))
}
