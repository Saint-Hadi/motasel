//! motasel — LAN file sharing.
//!
//! Bootstraps configuration from the environment, logs the host's local IPv4
//! addresses so the operator knows which URL to open from another device, then
//! binds `0.0.0.0:PORT` and serves the application.

use std::process::ExitCode;

use motasel::{build_app, AppState, Config};
use tokio::net::TcpListener;
use tracing_subscriber::{fmt, EnvFilter};

/// Initialize structured logging. Honors `RUST_LOG`; defaults to app+middleware
/// info level. Never logs environment variables or filesystem contents.
fn init_logging() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("motasel=info,tower_http=info,hyper=warn"));
    let _ = fmt().with_env_filter(filter).with_target(false).try_init();
}

/// Detect the host's non-loopback IPv4 addresses so we can print a ready-to-use
/// LAN URL. Shells out to `ip`, which is part of `iproute2` and present on
/// Debian by default; failure is non-fatal and simply skips the hint.
fn local_ipv4s() -> Vec<std::net::Ipv4Addr> {
    let Ok(output) = std::process::Command::new("ip")
        .args(["-4", "-o", "addr", "show"])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    let mut found = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let Some(idx) = parts.iter().position(|p| *p == "inet") else {
            continue;
        };
        let Some(cidr) = parts.get(idx + 1) else {
            continue;
        };
        if let Ok(ip) = cidr
            .split('/')
            .next()
            .unwrap_or_default()
            .parse::<std::net::Ipv4Addr>()
        {
            if !ip.is_loopback() && !found.contains(&ip) {
                found.push(ip);
            }
        }
    }
    found
}

fn log_startup_banner(config: &Config) {
    tracing::info!("motasel v{} starting", env!("CARGO_PKG_VERSION"));
    tracing::info!("listening on 0.0.0.0:{}", config.port);
    tracing::info!("max upload   {}", human_bytes(config.max_upload_bytes));

    let ips = local_ipv4s();
    if ips.is_empty() {
        tracing::warn!("could not detect a LAN IPv4 address; check the host's network config");
    } else {
        tracing::info!("open one of these from another device on the same network:");
        for ip in ips {
            tracing::info!("  http://{}:{}", ip, config.port);
        }
    }
}

/// Format a byte count for the startup log only.
fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    format!("{:.1} {}", size, UNITS[unit])
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install ctrl-c handler");
    tracing::info!("shutdown signal received, draining connections");
}

#[tokio::main]
async fn main() -> ExitCode {
    init_logging();

    let config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "invalid configuration");
            return ExitCode::FAILURE;
        }
    };

    let state = match AppState::init(config.clone()) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "failed to initialize storage");
            return ExitCode::FAILURE;
        }
    };

    log_startup_banner(&config);
    tracing::info!("storage dir  {}", state.storage_dir().display());
    tracing::info!("serving {} embedded UI asset", 1);

    let app = build_app(state);

    let listener = match TcpListener::bind(config.listen_addr()).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, addr = %config.listen_addr(), "cannot bind");
            return ExitCode::FAILURE;
        }
    };

    let bound = match listener.local_addr() {
        Ok(a) => a,
        Err(e) => {
            tracing::error!(error = %e, "cannot read bound address");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!("ready, bound to {}", bound);

    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
    {
        tracing::error!(error = %e, "server error");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}
