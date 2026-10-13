use anyhow::{anyhow, Result};
use futures_util::TryStreamExt;
use log::{error, info, warn};
use reqwest::Client;
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tokio_util::io::StreamReader;

use crate::config::CameraEntry;
use crate::plate_service::PlateEvent;

pub async fn run_plate_listener(camera: CameraEntry, barilgiin_id: Option<String>, tx: mpsc::Sender<PlateEvent>) {
    let client = Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .expect("HTTP client үүсгэхэд алдаа");

    let mut delay = 3u64;
    let mut device_error_logged = false;
    loop {
        match connect_and_read(&client, &camera, &barilgiin_id, &tx).await {
            Ok(_) => {
                warn!("{} stream дууссан", camera.ip);
                delay = 3;
                device_error_logged = false;
            }
            Err(e) if e.to_string().contains("deviceError") => {
                // Arming холболт эзлэгдсэн (ихэвчлэн вэб UI-ийн Real-Time Capture) —
                // нэг удаа анхааруулаад 1с тутам чимээгүй дахин оролдоно
                if !device_error_logged {
                    warn!("{}: deviceError — камерын вэб Live View / Real-Time Capture нээлттэй бол хаана уу. Дахин оролдсоор байна...", camera.ip);
                    device_error_logged = true;
                }
                tokio::time::sleep(Duration::from_millis(1000)).await;
                continue;
            }
            Err(e) => {
                error!("{} алдаа: {}", camera.ip, e);
                info!("{} — {}с дараа дахиж холбогдоно", camera.ip, delay);
                tokio::time::sleep(Duration::from_secs(delay)).await;
                delay = (delay * 2).min(60);
            }
        }
    }
}

async fn connect_and_read(client: &Client, camera: &CameraEntry, barilgiin_id: &Option<String>, tx: &mpsc::Sender<PlateEvent>) -> Result<()> {
    let url = format!(
        "http://{}:{}/ISAPI/Event/notification/alertStream",
        camera.ip, camera.port
    );

    // Step 1: unauthenticated request to get the auth challenge
    let first = client
        .get(&url)
        .timeout(Duration::from_secs(15))
        .send()
        .await?;

    let resp = if first.status() == 401 {
        let www_auth = first
            .headers()
            .get("www-authenticate")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();

        if www_auth.to_lowercase().starts_with("digest") {
            let mut prompt = digest_auth::parse(&www_auth)?;
            let ctx = digest_auth::AuthContext::new(
                &camera.username,
                &camera.password,
                "/ISAPI/Event/notification/alertStream",
            );
            let auth = prompt.respond(&ctx)?.to_header_string();
            client.get(&url).header("Authorization", auth).send().await?
        } else {
            // Basic auth fallback
            client
                .get(&url)
                .basic_auth(&camera.username, Some(&camera.password))
                .send()
                .await?
        }
    } else {
        first
    };

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("{}: HTTP {} хариу — {}", camera.ip, status, body));
    }

    info!("{}: stream нээгдлээ, дугаар хүлээж байна...", camera.ip);

    // Step 2: read the stream line by line
    let stream = resp
        .bytes_stream()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e));
    read_plates(BufReader::new(StreamReader::new(stream)), &camera.ip, barilgiin_id, tx).await
}

/// multipart alertStream-ээс `<licensePlate>` утгуудыг уншиж emit хийнэ.
/// Урсгалд ANPR зургууд (JPEG) холилдож ирдэг тул буруу UTF-8 мөрүүдийг алгасна.
async fn read_plates<R: AsyncBufRead + Unpin>(
    reader:       R,
    camera_ip:    &str,
    barilgiin_id: &Option<String>,
    tx:           &mpsc::Sender<PlateEvent>,
) -> Result<()> {
    let mut lines = reader.lines();

    const OPEN:  &str = "<licensePlate>";
    const CLOSE: &str = "</licensePlate>";

    // Зарим Hikvision firmware pretty-print хийж <licensePlate>, утга, </licensePlate>-ийг
    // тус тусдаа мөрөнд илгээдэг тул нэг мөрөнд хоёр tag-г хайх нь дугаарыг дутуу таслав.
    // Иймд tag хоорондын утгыг мөр дамжуулан цуглуулна.
    let mut pending: Option<String> = None;

    loop {
        let line = match lines.next_line().await {
            Ok(Some(l)) => l,
            Ok(None) => break,
            // Stream дунд буруу UTF-8 байт (жишээ нь зурган хавсралт, тасархай chunk) орж
            // ирвэл өмнөх коммит бүх холболтыг таслаж, дараагийн дугаарууд алгасагддаг байсан.
            // Үүний оронд зөвхөн тухайн мөрийг алгасаж, холболтыг үргэлжлүүлнэ.
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                pending = None;
                continue;
            }
            Err(e) => return Err(e.into()),
        };

        // Нэг мөрөнд хэд хэдэн <licensePlate>...</licensePlate> хос дараалан ирж болох тул
        // мөрийн үлдсэн хэсгээс олдох хүртэл дугаарыг цуглуулна — эхнийхээр зогсохгүй.
        let mut rest = line.as_str();
        loop {
            if let Some(buf) = pending.as_mut() {
                if let Some(end) = rest.find(CLOSE) {
                    buf.push_str(rest[..end].trim());
                    let plate = pending.take().unwrap();
                    if !emit_plate(plate, camera_ip, barilgiin_id, tx).await {
                        return Ok(()); // channel closed, stop
                    }
                    rest = &rest[end + CLOSE.len()..];
                    continue;
                } else {
                    buf.push_str(rest.trim());
                    break;
                }
            }

            if let Some(start) = rest.find(OPEN) {
                let after = &rest[start + OPEN.len()..];
                if let Some(end) = after.find(CLOSE) {
                    let plate = after[..end].trim().to_string();
                    if !emit_plate(plate, camera_ip, barilgiin_id, tx).await {
                        return Ok(()); // channel closed, stop
                    }
                    rest = &after[end + CLOSE.len()..];
                } else {
                    pending = Some(after.trim().to_string());
                    break;
                }
            } else {
                break;
            }
        }
    }

    Ok(())
}

/// Уншсан дугаарыг log-лоод plate_service рүү явуулна. Суваг хаагдсан бол `false` буцаана.
async fn emit_plate(
    plate:        String,
    camera_ip:    &str,
    barilgiin_id: &Option<String>,
    tx:           &mpsc::Sender<PlateEvent>,
) -> bool {
    let plate = plate.trim().to_string();

    if plate.is_empty() {
        return true;
    }

    // Хэрэв mismatched tag эсвэл тасархай byte-с болж parse гажуудвал утга дотор нь
    // "<" / ">" үлдэнэ — ийм үед дутуу/буруу дугаарыг серверт явуулахгүй.
    if plate.contains('<') || plate.contains('>') {
        warn!("{}: XML tag-тай холилдсон дугаарыг алгаслаа: {:?}", camera_ip, plate);
        return true;
    }

    // Log raw bytes to debug encoding mismatches
    let hex = plate
        .as_bytes()
        .iter()
        .map(|b| format!("{:02X}", b))
        .collect::<Vec<_>>()
        .join("-");
    info!("{}: дугаар={} RAW={}", camera_ip, plate, hex);

    tx.send(PlateEvent {
        plate,
        camera_ip: camera_ip.to_string(),
        barilgiin_id: barilgiin_id.clone(),
    })
    .await
    .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn collect(data: Vec<u8>) -> Vec<String> {
        let (tx, mut rx) = mpsc::channel(16);
        read_plates(BufReader::new(&data[..]), "192.168.1.64", &None, &tx).await.unwrap();
        drop(tx);
        let mut out = Vec::new();
        while let Some(ev) = rx.recv().await { out.push(ev.plate); }
        out
    }

    /// DS-TMC200-E1 (V5.5.0)-ийн alertStream-тэй ижил бүтэц: XML + JPEG хавсралт
    #[tokio::test]
    async fn parses_anpr_multipart_with_images() {
        let mut data = Vec::new();
        data.extend_from_slice(b"---------------------------7e13971310878\r\nContent-Type: application/xml; charset=\"UTF-8\"\r\n\r\n");
        data.extend_from_slice(b"<eventType>heartBeat</eventType>\r\n");
        data.extend_from_slice(b"---------------------------7e13971310878\r\nContent-Type: application/xml\r\n\r\n");
        data.extend_from_slice("<ANPR>\r\n<country>76</country>\r\n<licensePlate>5103УКУ</licensePlate>\r\n<line>1</line>\r\n</ANPR>\r\n".as_bytes());
        data.extend_from_slice(b"---------------------------7e13971310878\r\nContent-Type: image/jpeg\r\n\r\n");
        data.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE0, 0x80, 0x81, 0xFE, b'\n', 0xC3, 0x28, 0xFF, 0xD9, b'\r', b'\n']);
        data.extend_from_slice("<licensePlate>\r\n9117УБӨ\r\n</licensePlate>\r\n".as_bytes());

        assert_eq!(collect(data).await, vec!["5103УКУ", "9117УБӨ"]);
    }

    /// Камераас бичиж авсан бодит урсгал: HIK_STREAM_FILE=path cargo test -- --ignored
    #[tokio::test]
    #[ignore]
    async fn parses_captured_stream() {
        let path = std::env::var("HIK_STREAM_FILE").expect("HIK_STREAM_FILE");
        let plates = collect(std::fs::read(path).unwrap()).await;
        println!("plates: {plates:?}");
        assert!(!plates.is_empty());
    }
}
