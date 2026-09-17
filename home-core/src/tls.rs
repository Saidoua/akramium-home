//! Opt-in https for the LAN. On first use the install makes its own certificate authority
//! and signs a certificate for the names and addresses it is reached by. People trust the
//! authority once (it is downloadable at `/home/ca.pem`), and every later certificate this
//! install issues is trusted with it. Nothing leaves the machine; no outside authority can
//! issue for `akramium.local`.

use crate::{Config, Error, Result};
use rcgen::{BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use time::{Duration, OffsetDateTime};

const CA_DAYS: i64 = 3650;
/// Apple's limit for certificates from authorities people add themselves is 825 days.
const LEAF_DAYS: i64 = 397;
const RENEW_BEFORE_DAYS: i64 = 30;

pub struct Files {
    pub cert: PathBuf,
    pub key: PathBuf,
    pub ca: PathBuf,
}

fn bad(e: rcgen::Error) -> Error {
    Error::Internal(format!("certificate: {e}"))
}

/// Every name the certificate must cover: configured names, `localhost`, and the addresses
/// of this machine right now.
pub fn subject_names(config: &Config) -> Vec<String> {
    let mut names: Vec<String> = config.host_names.iter().map(|n| n.trim_end_matches('.').to_lowercase()).collect();
    names.push("localhost".into());
    let mut addresses: Vec<IpAddr> = vec!["127.0.0.1".parse().unwrap(), "::1".parse().unwrap()];
    if let Ok(interfaces) = if_addrs::get_if_addrs() {
        addresses.extend(interfaces.into_iter().map(|i| i.ip()).filter(|ip| !ip.is_loopback()));
    }
    names.extend(addresses.iter().map(IpAddr::to_string));
    names.sort();
    names.dedup();
    names
}

fn write_private(path: &Path, text: &str) -> Result<()> {
    std::fs::write(path, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Makes what is missing or stale under `data_dir/tls/` and returns the paths to serve with.
pub fn ensure(config: &Config) -> Result<Files> {
    ensure_at(&config.data_dir.join("tls"), &subject_names(config), OffsetDateTime::now_utc())
}

fn ensure_at(dir: &Path, names: &[String], now: OffsetDateTime) -> Result<Files> {
    std::fs::create_dir_all(dir)?;
    let files = Files { cert: dir.join("leaf.pem"), key: dir.join("leaf.key"), ca: dir.join("ca.pem") };
    let ca_key_path = dir.join("ca.key");
    let stamp_path = dir.join("leaf.names");

    if !files.ca.exists() || !ca_key_path.exists() {
        let key = KeyPair::generate().map_err(bad)?;
        let mut params = CertificateParams::new(Vec::<String>::new()).map_err(bad)?;
        params.distinguished_name.push(DnType::CommonName, "Akramium Home local authority");
        params.distinguished_name.push(DnType::OrganizationName, "Akramium Home");
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        params.not_before = now - Duration::days(1);
        params.not_after = now + Duration::days(CA_DAYS);
        let cert = params.self_signed(&key).map_err(bad)?;
        std::fs::write(&files.ca, cert.pem())?;
        write_private(&ca_key_path, &key.serialize_pem())?;
        let _ = std::fs::remove_file(&stamp_path);
        tracing::info!("made this install's certificate authority: {}", files.ca.display());
    }

    // The stamp records what the current certificate covers and until when.
    let wanted = names.join("\n");
    let fresh = std::fs::read_to_string(&stamp_path).ok().is_some_and(|stamp| {
        let (expiry, covered) = stamp.split_once('\n').unwrap_or(("0", ""));
        let expires: i64 = expiry.trim().parse().unwrap_or(0);
        covered == wanted && expires - now.unix_timestamp() > RENEW_BEFORE_DAYS * 86_400 && files.cert.exists() && files.key.exists()
    });
    if !fresh {
        let ca_key = KeyPair::from_pem(&std::fs::read_to_string(&ca_key_path)?).map_err(bad)?;
        let issuer = Issuer::from_ca_cert_pem(&std::fs::read_to_string(&files.ca)?, ca_key).map_err(bad)?;
        let key = KeyPair::generate().map_err(bad)?;
        let mut params = CertificateParams::new(names.to_vec()).map_err(bad)?;
        params.distinguished_name.push(DnType::CommonName, "Akramium Home");
        params.is_ca = IsCa::NoCa;
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params.not_before = now - Duration::days(1);
        let not_after = now + Duration::days(LEAF_DAYS);
        params.not_after = not_after;
        let cert = params.signed_by(&key, &issuer).map_err(bad)?;
        std::fs::write(&files.cert, cert.pem())?;
        write_private(&files.key, &key.serialize_pem())?;
        std::fs::write(&stamp_path, format!("{}\n{wanted}", not_after.unix_timestamp()))?;
        tracing::info!(names = names.len(), "issued a certificate for this install's names and addresses");
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issues_once_and_reissues_when_needed() {
        let dir = tempfile::tempdir().unwrap();
        let now = OffsetDateTime::now_utc();
        let names = vec!["akramium.local".to_string(), "192.168.1.20".to_string()];

        let files = ensure_at(dir.path(), &names, now).unwrap();
        let ca = std::fs::read_to_string(&files.ca).unwrap();
        let leaf = std::fs::read_to_string(&files.cert).unwrap();
        assert!(ca.starts_with("-----BEGIN CERTIFICATE-----"));
        assert_ne!(ca, leaf);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&files.key).unwrap().permissions().mode() & 0o777, 0o600);
        }

        // Same names, same day: nothing changes.
        ensure_at(dir.path(), &names, now).unwrap();
        assert_eq!(std::fs::read_to_string(&files.cert).unwrap(), leaf);
        assert_eq!(std::fs::read_to_string(&files.ca).unwrap(), ca);

        // A new address: a new certificate from the same authority.
        let more = vec!["akramium.local".to_string(), "192.168.1.20".to_string(), "192.168.1.99".to_string()];
        ensure_at(dir.path(), &more, now).unwrap();
        let second = std::fs::read_to_string(&files.cert).unwrap();
        assert_ne!(second, leaf);
        assert_eq!(std::fs::read_to_string(&files.ca).unwrap(), ca, "the authority people trusted stays");

        // Close to expiry: renewed.
        ensure_at(dir.path(), &more, now + Duration::days(LEAF_DAYS - 10)).unwrap();
        assert_ne!(std::fs::read_to_string(&files.cert).unwrap(), second);
    }
}
