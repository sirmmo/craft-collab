//! TLS for `mqtts://` brokers: rustls with the ring provider (no OpenSSL, so the static musl
//! build works), the system's certificates, or Mozilla's when the system has none.

use std::sync::Arc;

use rustls::{ClientConfig, RootCertStore};

/// A client configuration that checks the broker's certificate.
pub fn config() -> Result<Arc<ClientConfig>, String> {
    let mut roots = RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs().certs {
        let _ = roots.add(cert);
    }
    if roots.is_empty() {
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cfg = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS: {e}"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(cfg))
}

#[cfg(test)]
mod tests {
    #[test]
    fn builds() {
        assert!(super::config().is_ok());
    }
}
