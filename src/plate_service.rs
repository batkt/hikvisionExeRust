use anyhow::Result;
use log::{error, info};
use reqwest::Client;
use serde::Serialize;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::config::ServerConfig;

pub struct PlateEvent {
    pub plate:        String,
    pub camera_ip:    String,
    pub barilgiin_id: Option<String>,
}

#[derive(Serialize)]
struct Payload {
    #[serde(rename = "mashiniiDugaar")]
    mashini_dugaar: String,
    #[serde(rename = "CAMERA_IP")]
    camera_ip: String,
    #[serde(rename = "barilgiinId", skip_serializing_if = "Option::is_none")]
    barilgiin_id: Option<String>,
}

pub async fn run_plate_service(server: ServerConfig, mut rx: mpsc::Receiver<PlateEvent>) {
    while let Some(event) = rx.recv().await {
        if let Err(e) = send_plate(&server, &event).await {
            error!("Явуулахад алдаа ({}): {}", event.plate, e);
        }
    }
}

async fn send_plate(server: &ServerConfig, event: &PlateEvent) -> Result<()> {
    let client = Client::builder()
        .timeout(Duration::from_secs(server.timeout_secs))
        .danger_accept_invalid_certs(true)
        .build()?;

    let url = server.url.clone();

    let resp = client
        .post(&url)
        .bearer_auth(&server.token)
        .json(&Payload {
            mashini_dugaar: event.plate.clone(),
            camera_ip:      event.camera_ip.clone(),
            barilgiin_id:   event.barilgiin_id.clone(),
        })
        .send()
        .await?;

    let status = resp.status();
    let body   = resp.text().await.unwrap_or_default();

    if status.is_success() {
        info!("Амжилттай явуулсан: {} → {}", event.plate, body);
    } else {
        error!("Сервер алдаа {} дугаарт: {} — {}", event.plate, status, body);
    }

    Ok(())
}
