use anyhow::Result;
use axum::{
    extract::{Path, State},
    http::{HeaderValue, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
use log::{error, info};
use reqwest::Method;
use std::{sync::Arc, time::Duration};
use tower_http::cors::{Any, CorsLayer};

use crate::{
    config::{CameraEntry, Config},
    isapi,
    led_isapi,
    led_sdk,
};

const ALLOWED_ORIGIN: &str = "https://turees.zevtabs.mn";

#[derive(Clone)]
pub struct AppState {
    config: Arc<Config>,
}

pub fn build_router(config: Config) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(ALLOWED_ORIGIN.parse::<HeaderValue>().expect("valid origin"))
        .allow_methods(Any)
        .allow_headers(Any)
        .allow_private_network(true);

    Router::new()
        .route("/api/health",                                   get(health))
        .route("/api/neeye/:ip",                               get(neeye))
        .route("/api/sambar/:ip/:text/:dun",                   get(sambar))
        .route("/api/sambarOgnootoi/:ip/:text/:dun/:start/:end", get(sambar_ognootoi))
        .layer(cors)
        .with_state(AppState { config: Arc::new(config) })
}

// ── health ───────────────────────────────────────────────────────────────────

async fn health() -> impl IntoResponse {
    "hikvision-service OK"
}

// ── neeye (open gate) ────────────────────────────────────────────────────────

async fn neeye(
    State(st): State<AppState>,
    Path(ip):  Path<String>,
) -> (StatusCode, String) {
    info!("Хаалга нээх: {}", ip);

    let Some(camera) = st.config.cameras.iter().find(|c| c.ip == ip) else {
        return (StatusCode::BAD_REQUEST, format!("Мэдэгдэхгүй IP: {}", ip));
    };

    match open_hikvision_gate(camera).await {
        Ok(_)  => (StatusCode::OK, "Amjilttai".into()),
        Err(e) => {
            error!("Hikvision хаалга нээхэд алдаа {}: {}", ip, e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        }
    }
}

async fn open_hikvision_gate(camera: &CameraEntry) -> Result<()> {
    let xml  = r#"<BarrierGate><ctrlMode>open</ctrlMode></BarrierGate>"#.to_string();
    let resp = isapi::request(
        camera, Method::PUT, "/ISAPI/Parking/channels/1/barrierGate", "text/xml", Some(xml),
    ).await?;

    info!("Hikvision хаалга хариу {} — {} {}", camera.ip, resp.status, resp.body);

    if !resp.status.is_success() {
        anyhow::bail!("{}: хаалга нээгдсэнгүй {} — {}", camera.ip, resp.status, resp.body);
    }
    Ok(())
}

// ── sambar (LED display) ─────────────────────────────────────────────────────

async fn sambar(
    State(st):             State<AppState>,
    Path((ip, text, dun)): Path<(String, String, String)>,
) -> (StatusCode, String) {
    info!("Самбар: {} / {} / {}", ip, text, dun);
    show_on_led(&st, &ip, vec![text, dun]).await
}

async fn sambar_ognootoi(
    State(st): State<AppState>,
    Path((ip, text, dun, start, end)): Path<(String, String, String, String, String)>,
) -> (StatusCode, String) {
    info!("СамбарОгноотой: {} / {} / {} / {} / {}", ip, text, dun, start, end);
    show_on_led(&st, &ip, vec![text, dun, start, end]).await
}

/// `led_ip` тохируулсан бол тусдаа LED контроллер (vtLEDProtocol.dll), үгүй бол
/// камерын RS-485 самбарыг ISAPI-аар удирдана.
async fn show_on_led(st: &AppState, ip: &str, lines: Vec<String>) -> (StatusCode, String) {
    let Some(camera) = st.config.cameras.iter().find(|c| c.ip == ip) else {
        return (StatusCode::BAD_REQUEST, format!("Мэдэгдэхгүй IP: {}", ip));
    };

    if let Some(led_ip) = camera.led_ip.clone() {
        let line1 = lines.first().cloned().unwrap_or_default();
        let line2 = lines.get(1).cloned().unwrap_or_default();
        return match tokio::task::spawn_blocking(move || led_sdk::send_sambar(&led_ip, &line1, &line2)).await {
            Ok(Ok(_))  => (StatusCode::OK,                   "Amjilttai".into()),
            Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
            Err(e)     => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        };
    }

    // Самбаргүй камер — алгасаад амжилттай гэж хариулна
    if !camera.led_isapi {
        return (StatusCode::OK, "Amjilttai".into());
    }

    let idle = st.config.sambar_idle.clone();
    let hold = Duration::from_secs(st.config.sambar_hold_secs);
    match led_isapi::show(camera, lines, idle, hold).await {
        Ok(_)  => (StatusCode::OK, "Amjilttai".into()),
        Err(e) => {
            error!("LED самбар {} алдаа: {:#}", ip, e);
            (StatusCode::INTERNAL_SERVER_ERROR, format!("{:#}", e))
        }
    }
}
