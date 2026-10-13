// Bindings for vtLEDProtocol.dll (64-bit, StdCall == C on x86_64)
// DLL must be placed next to the exe.

use anyhow::{Context, Result};
use log::{error, info, warn};
use once_cell::sync::OnceCell;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

// ── function pointer types ────────────────────────────────────────────────────

type FnVtInitialize = unsafe extern "C" fn(u16, u16, u8, u8) -> i32;
type FnVtUninitialize = unsafe extern "C" fn() -> i32;
type FnVtAddProgram = unsafe extern "C" fn(u8) -> i32;
type FnVtAddTextAreaItem = unsafe extern "C" fn(
    u8, u8,               // program_id, area_id
    u16, u16, u16, u16,   // x, y, w, h
    *const u8, u16,        // text ptr, text_len (bytes)
    u8, u8, u8, u8, u8,   // color, action/style, font, speed, stay_time
) -> i32;
type FnVtGetProgramPack = unsafe extern "C" fn(u8, u8, *mut u8, *mut u32) -> i32;

// ── singleton ─────────────────────────────────────────────────────────────────

struct LedFuncs {
    _lib:             libloading::Library, // keep DLL loaded
    initialize:       FnVtInitialize,
    uninitialize:     FnVtUninitialize,
    add_program:      FnVtAddProgram,
    add_text_area:    FnVtAddTextAreaItem,
    get_program_pack: FnVtGetProgramPack,
}

// Safety: the DLL's global state is protected by the caller always holding
// the LED_LOCK mutex before calling into the DLL.
unsafe impl Send for LedFuncs {}
unsafe impl Sync for LedFuncs {}

static LED: OnceCell<Option<LedFuncs>> = OnceCell::new();

// We also need a mutex because the DLL uses global internal state
// (vtInitialize / vtGetProgramPack are not re-entrant).
static LED_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn get_led() -> Option<&'static LedFuncs> {
    LED.get_or_init(|| match load() {
        Ok(f)  => Some(f),
        Err(e) => { error!("vtLEDProtocol.dll ачааллахад алдаа: {}", e); None }
    }).as_ref()
}

fn load() -> Result<LedFuncs> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_default();

    let dll = exe_dir.join("vtLEDProtocol.dll");

    unsafe {
        let lib = libloading::Library::new(&dll)
            .with_context(|| format!("DLL: {}", dll.display()))?;

        let initialize:       FnVtInitialize       = *lib.get(b"vtInitialize\0")?;
        let uninitialize:     FnVtUninitialize     = *lib.get(b"vtUninitialize\0")?;
        let add_program:      FnVtAddProgram       = *lib.get(b"vtAddProgram\0")?;
        let add_text_area:    FnVtAddTextAreaItem  = *lib.get(b"vtAddTextAreaItem\0")?;
        let get_program_pack: FnVtGetProgramPack   = *lib.get(b"vtGetProgramPack\0")?;

        Ok(LedFuncs { _lib: lib, initialize, uninitialize, add_program, add_text_area, get_program_pack })
    }
}

// ── public API ────────────────────────────────────────────────────────────────

/// Send two lines of text to an LED display over TCP.
/// `led_ip` — IP of the LED controller (e.g. "192.168.1.99")
/// `line1`  — top row
/// `line2`  — bottom row
pub fn send_sambar(led_ip: &str, line1: &str, line2: &str) -> Result<()> {
    let sdk = match get_led() {
        Some(s) => s,
        None    => { warn!("LED SDK байхгүй — самбар алгасав"); return Ok(()); }
    };

    // Encode text as UTF-16 LE (same as C# Encoding.Unicode)
    let text1: Vec<u8> = line1.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
    let text2: Vec<u8> = line2.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();

    let _guard = LED_LOCK.lock().unwrap();

    let mut buf  = vec![0u8; 1024];
    let mut size = 0u32;

    unsafe {
        // 64×64 full-colour display, card type 0
        (sdk.initialize)(64, 64, 3 /*VT_FULL_COLOR*/, 0);

        let prog: u8 = 1;
        (sdk.add_program)(prog);

        // Row 1: y=0, h=16, RED, HOLD, FONT_16, speed 20, stay 5
        (sdk.add_text_area)(
            prog, 1,
            0, 0, 64, 16,
            text1.as_ptr(), text1.len() as u16,
            0x01 /*RED*/, 0x01 /*HOLD*/, 0x10 /*FONT_16*/, 20, 5,
        );

        // Row 2: y=16, h=16
        (sdk.add_text_area)(
            prog, 2,
            0, 16, 64, 16,
            text2.as_ptr(), text2.len() as u16,
            0x01, 0x01, 0x10, 20, 5,
        );

        // Send packed data via TCP (port 10000)
        let addr = format!("{}:10000", led_ip);
        let sock_addr = addr
            .to_socket_addrs()?
            .next()
            .context("LED IP задруулж чадсангүй")?;

        let mut tcp = TcpStream::connect_timeout(&sock_addr, Duration::from_secs(5))?;
        tcp.set_write_timeout(Some(Duration::from_secs(5)))?;
        tcp.set_read_timeout(Some(Duration::from_secs(5)))?;

        loop {
            let ret = (sdk.get_program_pack)(0x01, 1, buf.as_mut_ptr(), &mut size);
            if size > 0 {
                tcp.write_all(&buf[..size as usize])?;
                let mut resp = [0u8; 1024];
                let _ = tcp.read(&mut resp);
            }
            if ret <= 0 { break; }
        }

        (sdk.uninitialize)();
    }

    info!("LED самбар: '{}' / '{}'", line1, line2);
    Ok(())
}
