//! Akramium Home as a library: what `main` runs, and what the integration tests start on a
//! free port.

use axum::Router;
use home_core::{Config, Core};

/// The same router over https, on its own port, with requests marked as secure.
pub async fn serve_tls(app: &App) -> Result<(), String> {
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
