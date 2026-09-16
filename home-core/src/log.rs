/// Logging to stderr, filtered by `RUST_LOG` (default: info for our crates, warn elsewhere).
pub fn init() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("warn,home_core=info,dekave=info,akramium_home=info"));
    tracing_subscriber::fmt().with_env_filter(filter).with_target(false).init();
}
