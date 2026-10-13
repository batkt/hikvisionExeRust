// dlls/ доторх config.toml болон vtLEDProtocol.dll-г exe-ийн хажууд (target/<profile>/) хуулна.
// Ажиллах үед config.rs / led_sdk.rs хоёулаа exe-ийн хавтаснаас хайдаг.

use std::path::PathBuf;

const FILES: &[&str] = &["config.toml", "vtLEDProtocol.dll"];

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let dlls_dir     = manifest_dir.join("dlls");

    // OUT_DIR = target/<profile>/build/<pkg>-<hash>/out → 3 түвшин дээш = target/<profile>
    let out_dir    = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let target_dir = out_dir.ancestors().nth(3).expect("target dir").to_path_buf();

    for name in FILES {
        let src = dlls_dir.join(name);
        println!("cargo:rerun-if-changed={}", src.display());
        if !src.exists() {
            println!("cargo:warning=dlls/{name} олдсонгүй — exe-ийн хажууд гараар хуулна уу");
            continue;
        }
        if let Err(e) = std::fs::copy(&src, target_dir.join(name)) {
            println!("cargo:warning=dlls/{name} хуулахад алдаа: {e}");
        }
    }
}
