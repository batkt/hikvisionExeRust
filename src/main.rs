use anyhow::Result;
use log::{error, info};
use std::ffi::OsString;
use std::time::Duration;
use tokio::sync::mpsc;
use windows_service::{
    define_windows_service,
    service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode,
        ServiceState, ServiceStatus, ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher,
    service_manager::{ServiceManager, ServiceManagerAccess},
    service::{ServiceAccess, ServiceStartType, ServiceErrorControl, ServiceInfo},
};

mod api;
mod config;
mod isapi;
mod led_isapi;
mod led_sdk;
mod plate_listener;
mod plate_service;

const SERVICE_NAME:    &str = "zevHikvision";
const SERVICE_DISPLAY: &str = "zevHikvision";
const SERVICE_DESC:    &str = "Parkease hikvision";

// ── Windows Service boilerplate ───────────────────────────────────────────────

define_windows_service!(ffi_service_main, service_main);

fn service_main(args: Vec<OsString>) {
    if let Err(e) = run_service(args) {
        error!("Service fatal error: {e}");
    }
}

fn run_service(_args: Vec<OsString>) -> Result<()> {
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();

    let event_handler = move |control: ServiceControl| -> ServiceControlHandlerResult {
        match control {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                let _ = stop_tx.send(());
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };

    let status_handle = service_control_handler::register(SERVICE_NAME, event_handler)?;

    status_handle.set_service_status(ServiceStatus {
        service_type:      ServiceType::OWN_PROCESS,
        current_state:     ServiceState::StartPending,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code:         ServiceExitCode::Win32(0),
        checkpoint:        0,
        wait_hint:         Duration::from_secs(15),
        process_id:        None,
    })?;

    init_logging()?;

    let cfg = match config::load_config() {
        Ok(c) => c,
        Err(e) => {
            error!("Config load failed: {e}");
            status_handle.set_service_status(ServiceStatus {
                service_type:      ServiceType::OWN_PROCESS,
                current_state:     ServiceState::Stopped,
                controls_accepted: ServiceControlAccept::empty(),
                exit_code:         ServiceExitCode::ServiceSpecific(1),
                checkpoint: 0, wait_hint: Duration::ZERO, process_id: None,
            })?;
            return Ok(());
        }
    };

    status_handle.set_service_status(ServiceStatus {
        service_type:      ServiceType::OWN_PROCESS,
        current_state:     ServiceState::Running,
        controls_accepted: ServiceControlAccept::STOP,
        exit_code:         ServiceExitCode::Win32(0),
        checkpoint: 0, wait_hint: Duration::ZERO, process_id: None,
    })?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Tokio runtime үүсгэхэд алдаа");

    rt.block_on(async {
        tokio::select! {
            result = run_app(cfg) => {
                if let Err(e) = result { error!("App error: {e}"); }
            }
            _ = tokio::task::spawn_blocking(move || { let _ = stop_rx.recv(); }) => {
                info!("Stop signal received");
            }
        }
    });

    status_handle.set_service_status(ServiceStatus {
        service_type:      ServiceType::OWN_PROCESS,
        current_state:     ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code:         ServiceExitCode::Win32(0),
        checkpoint: 0, wait_hint: Duration::ZERO, process_id: None,
    })?;

    Ok(())
}

// ── Main application logic ────────────────────────────────────────────────────

async fn run_app(cfg: config::Config) -> Result<()> {
    info!("Hikvision Service эхэлж байна...");
    info!("{} камер тохируулагдсан", cfg.cameras.len());

    led_isapi::set_scroll_chars(cfg.sambar_scroll_chars);

    let (tx, rx) = mpsc::channel::<plate_service::PlateEvent>(200);

    for camera in cfg.cameras.clone() {
        let tx2 = tx.clone();
        // Per-camera barilgiin_id overrides the global one
        let bid = camera.barilgiin_id.clone().or(cfg.barilgiin_id.clone());
        info!("Камер {} ({})", camera.ip, camera.role.as_deref().unwrap_or("-"));
        if camera.led_isapi && camera.led_ip.is_none() {
            tokio::spawn(led_isapi::apply_idle(camera.clone(), cfg.sambar_idle.clone()));
        }
        tokio::spawn(async move {
            plate_listener::run_plate_listener(camera, bid, tx2).await;
        });
    }
    drop(tx);

    let server_cfg = cfg.server.clone();
    tokio::spawn(async move {
        plate_service::run_plate_service(server_cfg, rx).await;
    });

    let port = cfg.api_port;
    let router = api::build_router(cfg);
    let addr = format!("0.0.0.0:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!("API сервер: http://{}", addr);
    axum::serve(listener, router).await?;

    Ok(())
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("install")   => install_service(),
        Some("uninstall") => uninstall_service(),
        Some("run")       => run_interactive(),
        _ => {
            service_dispatcher::start(SERVICE_NAME, ffi_service_main)
                .expect("Service dispatcher failed — run as Windows service or use 'run'");
        }
    }
}

fn run_interactive() {
    init_logging().expect("Logging init failed");
    println!("Running interactively (not as Windows service)");

    let cfg = config::load_config().expect("Cannot load config.toml");

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            if let Err(e) = run_app(cfg).await {
                error!("Error: {e}");
            }
        });
}

// ── Service install / uninstall ───────────────────────────────────────────────

fn install_service() {
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    ).expect("SCM нээхэд алдаа — Administrator эрхтэй ажиллуулна уу");

    let exe = std::env::current_exe().expect("Exe замыг авч чадсангүй");
    let info = ServiceInfo {
        name:             OsString::from(SERVICE_NAME),
        display_name:     OsString::from(SERVICE_DISPLAY),
        service_type:     ServiceType::OWN_PROCESS,
        start_type:       ServiceStartType::AutoStart,
        error_control:    ServiceErrorControl::Normal,
        executable_path:  exe,
        launch_arguments: vec![],
        dependencies:     vec![],
        account_name:     None,
        account_password: None,
    };

    match manager.create_service(&info, ServiceAccess::CHANGE_CONFIG) {
        Ok(svc) => {
            svc.set_description(SERVICE_DESC).ok();
            println!("✓ Service '{SERVICE_NAME}' суулгагдлаа.");
            println!("  Эхлүүлэх: net start {SERVICE_NAME}");
        }
        Err(e) => { eprintln!("✗ Суулгахад алдаа: {e}"); std::process::exit(1); }
    }
}

fn uninstall_service() {
    let manager = ServiceManager::local_computer(
        None::<&str>, ServiceManagerAccess::CONNECT,
    ).expect("SCM нээхэд алдаа");

    let svc = manager.open_service(
        SERVICE_NAME, ServiceAccess::DELETE | ServiceAccess::STOP,
    ).expect("Service олдсонгүй");

    let _ = svc.stop();
    std::thread::sleep(Duration::from_secs(2));

    match svc.delete() {
        Ok(_)  => println!("✓ Service '{SERVICE_NAME}' устгагдлаа."),
        Err(e) => { eprintln!("✗ Устгахад алдаа: {e}"); std::process::exit(1); }
    }
}

fn init_logging() -> Result<()> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    let fmt = |out: fern::FormatCallback, msg: &std::fmt::Arguments, rec: &log::Record| {
        out.finish(format_args!(
            "[{}][{}] {}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
            rec.level(),
            msg
        ))
    };

    let error_log = fern::Dispatch::new()
        .format(fmt)
        .level(log::LevelFilter::Error)
        .chain(fern::log_file(exe_dir.join("error.log"))?);

    let main_log = fern::Dispatch::new()
        .format(fmt)
        .level(log::LevelFilter::Info)
        .chain(fern::log_file(exe_dir.join("hikvision-service.log"))?);

    fern::Dispatch::new()
        .level(log::LevelFilter::Info)
        .level_for("hyper",           log::LevelFilter::Warn)
        .level_for("hyper_util",      log::LevelFilter::Warn)
        .level_for("reqwest",         log::LevelFilter::Warn)
        .level_for("h2",              log::LevelFilter::Warn)
        .level_for("tower",           log::LevelFilter::Warn)
        .chain(error_log)
        .chain(main_log)
        .chain(
            fern::Dispatch::new()
                .format(fmt)
                .chain(std::io::stdout()),
        )
        .apply()?;

    Ok(())
}
