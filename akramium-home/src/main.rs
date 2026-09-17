//! Akramium Home: one process serving the household's drive (DeKave), and later docs
//! (Doks) and mail (Komail), on one port with one sign-in.

use akramium_home::{build, serve_tls};
use home_core::Config;
use std::path::PathBuf;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn usage() -> ! {
    eprintln!("usage: akramium-home [--config home.toml] [--data-dir DIR] [--listen ADDR] [--version]");
    std::process::exit(2)
}

/// Ctrl-C, or the SIGTERM that init systems and containers send. Stopping on it (instead of
/// being killed by it) lets the network announcement say goodbye, so the name stops resolving
/// at once rather than lingering in other machines' caches.
async fn stop_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    tracing::info!("stopping");
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let mut config_path: Option<PathBuf> = None;
    let mut data_dir: Option<PathBuf> = None;
    let mut listen: Option<String> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => config_path = Some(args.next().map(PathBuf::from).unwrap_or_else(|| usage())),
            "--data-dir" => data_dir = Some(args.next().map(PathBuf::from).unwrap_or_else(|| usage())),
            "--listen" => listen = Some(args.next().unwrap_or_else(|| usage())),
            "--version" | "-V" => {
                println!("Akramium Home {VERSION}");
                return;
            }
            _ => usage(),
        }
    }

    home_core::log::init();
    let mut config = match Config::load(config_path.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("config: {e}");
            std::process::exit(1);
        }
    };
    if let Some(d) = data_dir {
        config.data_dir = d;
    }
    if let Some(l) = listen {
        config.listen = l.parse().unwrap_or_else(|e| {
            eprintln!("--listen: {e}");
            std::process::exit(1)
        });
    }

    let app = match build(config).await {
        Ok(app) => app,
        Err(e) => {
            eprintln!("start: {e}");
            std::process::exit(1);
        }
    };
    let listener = match tokio::net::TcpListener::bind(app.listen).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot listen on {}: {e}", app.listen);
            std::process::exit(1);
        }
    };
    tracing::info!("Akramium Home {VERSION} listening on http://{}", app.listen);
    let _announced = home_core::mdns::announce(&app.config, app.config.modules.dekave);
    if app.config.tls.enabled
        && let Err(e) = serve_tls(&app).await
    {
        eprintln!("https: {e}");
        std::process::exit(1);
    }
    axum::serve(listener, app.router.clone().into_make_service_with_connect_info::<std::net::SocketAddr>())
        .with_graceful_shutdown(stop_signal())
        .await
        .expect("server");
}
