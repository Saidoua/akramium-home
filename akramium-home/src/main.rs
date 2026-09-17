//! Akramium Home: one process serving the household's drive (DeKave), and later docs
//! (Doks) and mail (Komail), on one port with one sign-in.

use axum::Router;
use home_core::{Config, Core};
use std::path::PathBuf;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn usage() -> ! {
    eprintln!("usage: akramium-home [--config home.toml] [--data-dir DIR] [--listen ADDR] [--version]");
    std::process::exit(2)
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
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("stopping");
        })
        .await
        .expect("server");
}

/// The same router over https, on its own port, with requests marked as secure.
async fn serve_tls(app: &App) -> Result<(), String> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let files = home_core::tls::ensure(&app.config).map_err(|e| e.to_string())?;
    let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(&files.cert, &files.key).await.map_err(|e| e.to_string())?;
    let address = app.config.tls.listen;
    let router = app.router.clone().layer(axum::Extension(home_core::headers::OverTls));
    tracing::info!("also listening on https://{address}; people trust it once with /home/ca.pem");
    tokio::spawn(async move {
        let service = router.into_make_service_with_connect_info::<std::net::SocketAddr>();
        if let Err(e) = axum_server::bind_rustls(address, config).serve(service).await {
            tracing::error!(error = %e, "the https listener stopped");
        }
    });
    Ok(())
}

pub struct App {
    pub config: std::sync::Arc<Config>,
    pub listen: std::net::SocketAddr,
    pub router: Router,
}

/// Opens the database, applies migrations, mounts the enabled modules.
pub async fn build(config: Config) -> home_core::Result<App> {
    let listen = config.listen;
    let body_limit = config.limits.body_bytes;
    let core = Core::open(config).await?;

    let mut router = home_core::routes::router();
    if core.config.modules.dekave {
        let drive = dekave::open(core.clone()).await?;
        router = router.merge(dekave::router(drive));
    }
    let router = router
        .layer(axum::middleware::from_fn_with_state(core.clone(), home_core::guard::check))
        .layer(axum::middleware::from_fn(home_core::headers::security))
        // Caps JSON and form bodies; raw upload streams are not read through this limit.
        .layer(axum::extract::DefaultBodyLimit::max(body_limit))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(core.clone());
    let config = core.config.clone();
    Ok(App { config, listen, router })
}
