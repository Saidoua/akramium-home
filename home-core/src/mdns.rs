//! Announces the daemon on the LAN so people type `akramium.local` instead of an address,
//! and so file managers list the drive under "Network". Only when listening beyond loopback.

use crate::Config;
use mdns_sd::{ServiceDaemon, ServiceInfo};

/// Keeps the announcement alive; dropping it withdraws the services.
pub struct Announced {
    daemon: ServiceDaemon,
}

impl Drop for Announced {
    fn drop(&mut self) {
        let _ = self.daemon.shutdown();
    }
}

/// `None` when disabled or when the daemon only listens on loopback.
pub fn announce(config: &Config, webdav: bool) -> Option<Announced> {
    if !config.mdns.enabled || config.listen.ip().is_loopback() {
        return None;
    }
    let host = format!("{}.local.", config.mdns.name.trim_end_matches(".local").trim_end_matches('.'));
    let port = config.listen.port();
    let started = (|| -> Result<Announced, mdns_sd::Error> {
        let daemon = ServiceDaemon::new()?;
        // No fixed addresses: the library follows the machine's interfaces as they change.
        let web = ServiceInfo::new("_http._tcp.local.", "Akramium Home", &host, "", port, &[("path", "/")][..])?.enable_addr_auto();
        daemon.register(web)?;
        if webdav {
            let dav = ServiceInfo::new("_webdav._tcp.local.", "DeKave", &host, "", port, &[("path", "/dav/")][..])?.enable_addr_auto();
            daemon.register(dav)?;
        }
        Ok(Announced { daemon })
    })();
    match started {
        Ok(a) => {
            tracing::info!("announced on the network as http://{}:{port}", host.trim_end_matches('.'));
            Some(a)
        }
        Err(e) => {
            tracing::warn!(error = %e, "could not announce on the network; the address still works");
            None
        }
    }
}
