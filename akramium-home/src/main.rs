//! Akramium Home: one process serving the household's drive (DeKave), and later docs
//! (Doks) and mail (Komail), on one port with one sign-in.

use akramium_home::{build, serve_tls};
use home_core::Config;
use std::path::PathBuf;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn usage() -> ! {
    eprintln!("usage: akramium-home [--config home.toml] [--data-dir DIR] [--listen ADDR] [--health] [--version]");
    std::process::exit(2)
}

/// For a container's health check, where there is no curl: is a daemon answering on the
/// configured port with its sign-in page?
fn healthy(listen: std::net::SocketAddr) -> bool {
    use std::io::{Read, Write};
    let target = if listen.ip().is_unspecified() { std::net::SocketAddr::from(([127, 0, 0, 1], listen.port())) } else { listen };
    let timeout = std::time::Duration::from_secs(3);
    let Ok(mut stream) = std::net::TcpStream::connect_timeout(&target, timeout) else { return false };
    let _ = stream.set_read_timeout(Some(timeout));
    if stream.write_all(b"GET /login HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\n\r\n").is_err() {
        return false;
    }
    let mut head = [0u8; 64];
    let n = stream.read(&mut head).unwrap_or(0);
    head[..n].starts_with(b"HTTP/1.1 200")
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
    let mut health = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => config_path = Some(args.next().map(PathBuf::from).unwrap_or_else(|| usage())),
            "--data-dir" => data_dir = Some(args.next().map(PathBuf::from).unwrap_or_else(|| usage())),
            "--listen" => listen = Some(args.next().unwrap_or_else(|| usage())),
            "--health" => health = true,
            "--version" | "-V" => {
                println!("Akramium Home {VERSION}");
                return;
            }
            _ => usage(),
        }
    }

    if !health {
        home_core::log::init();
    }
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

    if health {
        std::process::exit(if healthy(config.listen) { 0 } else { 1 });
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
