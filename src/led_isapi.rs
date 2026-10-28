//! Камерын RS-485-д холбогдсон LED самбарыг ISAPI-аар удирдана
//! (Configuration → Entrance and Exit → LED Display Settings, Control Mode = By Platform).
//!
//! Камер өөрөө 3 сцентэй: 1 = түр машин орох, 2 = idle (машингүй), 3 = бүртгэлтэй машин.
//! Самбарт текст гаргахдаа гурвуулангийн мөрүүдийг солино — эс тэгвээс машин орох үед
//! камер "plate" токеноор өөрийн (заримдаа хуучин) дугаарыг харуулдаг.
//! `hold` хугацааны дараа бүх сценийг idle мөрүүд рүү буцаана.

use anyhow::{anyhow, Context, Result};
use log::{error, info};
use once_cell::sync::Lazy;
use reqwest::Method;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use crate::config::CameraEntry;
use crate::isapi;

const SCENE_IDS: [u32; 3] = [1, 2, 3];
/// Камерын өөрөө орлуулдаг токенууд — эдгээрийг `value`, бусдыг `customValue` болгож илгээнэ
const TOKENS: [&str; 4] = ["plate", "YYYY-MM-DD", "hh-mm-ss", "expireRemind"];
const MAX_CUSTOM_LEN: usize = 40;
/// capabilities: ShowInfo id maxSize = 4
const MAX_LINES: usize = 4;

/// Үүнээс урт мөрийг гүйдэг ("left") горимоор харуулна. `instant` горимд самбарын өргөнөөс
/// урт текстийг контроллер хуудаслаж харуулдаг тул нөгөө мөр заримдаа хоосон харагддаг.
static SCROLL_CHARS: AtomicUsize = AtomicUsize::new(8);

pub fn set_scroll_chars(n: usize) {
    SCROLL_CHARS.store(n, Ordering::Relaxed);
}

/// IP бүрийн сүүлийн хүсэлтийн дугаар — шинэ текст ирвэл өмнөх сэргээлтийг цуцална
static GENERATION: Lazy<Mutex<HashMap<String, u64>>> = Lazy::new(|| Mutex::new(HashMap::new()));
/// Нэг камерт PUT-ууд давхцахгүй байх
static CAMERA_LOCKS: Lazy<Mutex<HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

fn camera_lock(ip: &str) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    CAMERA_LOCKS.lock().unwrap().entry(ip.to_string()).or_default().clone()
}

/// `lines`-г самбарт гаргаад, `hold` дараа `idle_lines` руу буцаана.
pub async fn show(camera: &CameraEntry, lines: Vec<String>, idle_lines: Vec<String>, hold: Duration) -> Result<()> {
    let gen = {
        let mut g = GENERATION.lock().unwrap();
        let e = g.entry(camera.ip.clone()).or_insert(0);
        *e += 1;
        *e
    };

    set_all_scenes(camera, &lines).await?;
    info!("LED самбар {}: {:?}", camera.ip, lines);

    let camera = camera.clone();
    tokio::spawn(async move {
        tokio::time::sleep(hold).await;
        if GENERATION.lock().unwrap().get(&camera.ip) != Some(&gen) {
            return; // энэ хооронд шинэ текст ирсэн
        }
        match set_all_scenes(&camera, &idle_lines).await {
            Ok(_)  => info!("LED самбар {}: idle руу буцлаа", camera.ip),
            Err(e) => error!("LED самбар {} idle руу буцаахад алдаа: {}", camera.ip, e),
        }
    });

    Ok(())
}

/// Эхлэх үед config-ийн idle мөрүүдийг камерт бичнэ (config өөрчлөгдсөн бол шууд хүчинтэй болно).
/// Камер унтарсан бол 30с тутам дахин оролдоно.
pub async fn apply_idle(camera: CameraEntry, idle_lines: Vec<String>) {
    let mut logged = false;
    loop {
        match set_all_scenes(&camera, &idle_lines).await {
            Ok(_) => {
                info!("LED самбар {}: idle {:?}", camera.ip, idle_lines);
                return;
            }
            Err(e) => {
                // Камер унтарсан үед error.log-г дүүргэхгүйн тулд зөвхөн эхний алдааг бичнэ
                if !logged {
                    error!("LED самбар {} idle тохируулахад алдаа: {:#} — камер асах хүртэл 30с тутам дахин оролдоно", camera.ip, e);
                    logged = true;
                }
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        }
    }
}

async fn set_all_scenes(camera: &CameraEntry, lines: &[String]) -> Result<()> {
    let lock  = camera_lock(&camera.ip);
    let _held = lock.lock().await;

    // Idle (2)-г эхэлж — машингүй үед шууд харагддаг сцен
    for id in [2].into_iter().chain(SCENE_IDS.into_iter().filter(|&i| i != 2)) {
        set_scene(camera, id, lines)
            .await
            .with_context(|| format!("{}: LED сцен {}", camera.ip, id))?;
    }
    Ok(())
}

async fn set_scene(camera: &CameraEntry, id: u32, lines: &[String]) -> Result<()> {
    let path = format!("/ISAPI/Parking/channels/1/LEDConfigurations/multiScene/{id}?format=json");

    let cur = isapi::request(camera, Method::GET, &path, "application/json", None).await?;
    if !cur.status.is_success() {
        return Err(anyhow!("GET {} — {}", cur.status, cur.body));
    }
    let mut cfg: Value = serde_json::from_str(&cur.body).context("LED JSON задлахад алдаа")?;

    apply_lines(&mut cfg, lines)?;

    let resp = isapi::request(
        camera, Method::PUT, &path, "application/json; charset=utf-8", Some(cfg.to_string()),
    ).await?;
    if !resp.status.is_success() || !resp.body.contains("\"statusCode\":1") {
        return Err(anyhow!("PUT {} — {}", resp.status, resp.body));
    }
    Ok(())
}

/// Сцений ShowInfoList-г `lines`-аар солино. Өнгө, хурд зэрэг тохиргоог эхний мөрөөс хуулна.
fn apply_lines(cfg: &mut Value, lines: &[String]) -> Result<()> {
    let show_list = cfg
        .pointer_mut("/SingleSceneLEDConfigurations/LEDConfigurationList/0/LEDConfiguration/ShowInfoList")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| anyhow!("ShowInfoList олдсонгүй"))?;

    let template = show_list
        .first()
        .and_then(|s| s.get("ShowInfo"))
        .cloned()
        .unwrap_or_else(|| json!({
            "fontSize": 16, "fontColor": "red", "speedType": "medium", "displayMode": "instant",
        }));

    let new_list: Vec<Value> = lines
        .iter()
        .take(MAX_LINES)
        .enumerate()
        .map(|(i, line)| {
            let mut info = show_list
                .get(i)
                .and_then(|s| s.get("ShowInfo"))
                .cloned()
                .unwrap_or_else(|| template.clone());
            info["id"]           = json!(i + 1);
            info["displayMode"]  = json!(display_mode(line));
            info["LineInfoList"] = json!([{ "LineInfo": line_info(line) }]);
            json!({ "ShowInfo": info })
        })
        .collect();

    *show_list = new_list;
    Ok(())
}

/// Самбарт харагдах тэмдэгтийн тоогоор гүйлгэх эсэхийг шийднэ
fn display_mode(line: &str) -> &'static str {
    let line = line.trim();
    let shown = match line {
        "YYYY-MM-DD"   => 10,
        "hh-mm-ss"     => 8,
        "plate"        => 8,
        "expireRemind" => usize::MAX,
        _              => line.chars().count(),
    };
    if shown > SCROLL_CHARS.load(Ordering::Relaxed) { "left" } else { "instant" }
}

fn line_info(line: &str) -> Value {
    let line = line.trim();
    if TOKENS.contains(&line) {
        return json!({ "id": 1, "value": line });
    }
    // customValue хамгийн багадаа 1 тэмдэгт, ихдээ 40
    let text: String = if line.is_empty() { " ".into() } else { line.chars().take(MAX_CUSTOM_LEN).collect() };
    json!({ "id": 1, "customValue": text })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_lines_keeping_style() {
        let mut cfg: Value = serde_json::from_str(r#"{"SingleSceneLEDConfigurations":{"sid":0,"mode":"noVehicle","LEDConfigurationList":[{"LEDConfiguration":{"id":0,"enabled":true,"ShowInfoList":[{"ShowInfo":{"id":1,"fontSize":16,"fontColor":"green","speedType":"medium","displayMode":"instant","LineInfoList":[{"LineInfo":{"id":1,"customValue":"Welcome"}}]}},{"ShowInfo":{"id":2,"fontSize":16,"fontColor":"red","speedType":"medium","displayMode":"instant","LineInfoList":[{"LineInfo":{"id":1,"value":"YYYY-MM-DD"}}]}}]}}]}}"#).unwrap();

        apply_lines(&mut cfg, &["9117УБӨ".into(), "plate".into(), "3 дахь".into()]).unwrap();

        let list = cfg.pointer("/SingleSceneLEDConfigurations/LEDConfigurationList/0/LEDConfiguration/ShowInfoList").unwrap();
        assert_eq!(list.as_array().unwrap().len(), 3);
        assert_eq!(list[0]["ShowInfo"]["LineInfoList"][0]["LineInfo"]["customValue"], "9117УБӨ");
        assert_eq!(list[1]["ShowInfo"]["LineInfoList"][0]["LineInfo"]["value"], "plate");
        assert_eq!(list[1]["ShowInfo"]["fontColor"], "red");
        assert_eq!(list[2]["ShowInfo"]["fontColor"], "green"); // эхний мөрийн загвар
        assert_eq!(list[2]["ShowInfo"]["id"], 3);
        assert_eq!(cfg["SingleSceneLEDConfigurations"]["mode"], "noVehicle");
        assert_eq!(list[0]["ShowInfo"]["displayMode"], "instant"); // 9117УБӨ = 7 тэмдэгт
    }

    #[test]
    fn long_lines_scroll() {
        assert_eq!(display_mode("Тавтай морил"), "left");
        assert_eq!(display_mode("YYYY-MM-DD"), "left");
        assert_eq!(display_mode("55T"), "instant");
        assert_eq!(display_mode("hh-mm-ss"), "instant");
    }
}
