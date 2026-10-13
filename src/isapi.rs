//! Hikvision ISAPI HTTP хүсэлт — эхлээд auth-гүй явуулж, 401 ирвэл Digest-ээр дахин явуулна.

use anyhow::{anyhow, Result};
use reqwest::{Client, Method};
use std::time::Duration;

use crate::config::CameraEntry;

pub struct IsapiResponse {
    pub status: reqwest::StatusCode,
    pub body:   String,
}

/// `path` нь query-тэй хамт (жишээ нь "/ISAPI/...?format=json").
pub async fn request(
    camera:       &CameraEntry,
    method:       Method,
    path:         &str,
    content_type: &str,
    body:         Option<String>,
) -> Result<IsapiResponse> {
    let url    = format!("http://{}:{}{}", camera.ip, camera.port, path);
    let client = Client::builder().timeout(Duration::from_secs(10)).build()?;

    let build = |auth: Option<String>| {
        let mut req = client.request(method.clone(), &url);
        if let Some(b) = &body {
            req = req.header("Content-Type", content_type).body(b.clone());
        }
        if let Some(a) = auth {
            req = req.header("Authorization", a);
        }
        req
    };

    let first = build(None).send().await?;

    let resp = if first.status() == reqwest::StatusCode::UNAUTHORIZED {
        let www_auth = first
            .headers()
            .get("www-authenticate")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| anyhow!("{}: WWW-Authenticate байхгүй", camera.ip))?
            .to_owned();

        let mut prompt = digest_auth::parse(&www_auth)?;
        let mut ctx    = digest_auth::AuthContext::new(&camera.username, &camera.password, path);
        ctx.method     = method.as_str().into();
        let auth       = prompt.respond(&ctx)?.to_header_string();

        build(Some(auth)).send().await?
    } else {
        first
    };

    let status = resp.status();
    let body   = resp.text().await.unwrap_or_default();
    Ok(IsapiResponse { status, body })
}
