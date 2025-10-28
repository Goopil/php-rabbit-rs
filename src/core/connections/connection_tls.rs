use crate::core::connections::connection_options::SslOptions;
use anyhow::Result;
use lapin::tcp::{OwnedIdentity, OwnedTLSConfig};
use std::fs;

pub fn build_tls(opts: &SslOptions) -> Result<OwnedTLSConfig> {
    let mut tls = OwnedTLSConfig::default();
    let mut pem_bundle = String::new();

    if let Some(cafile) = &opts.cafile {
        let mut s = fs::read_to_string(cafile)?;
        if !s.ends_with('\n') {
            s.push('\n');
        }
        pem_bundle.push_str(&s);
    }
    if let Some(capath) = &opts.capath {
        if capath.is_dir() {
            for entry in capath.read_dir()? {
                let p = entry?.path();
                if p.extension().and_then(|s| s.to_str()) == Some("pem") {
                    let mut s = fs::read_to_string(p)?;
                    if !s.ends_with('\n') {
                        s.push('\n');
                    }
                    pem_bundle.push_str(&s);
                }
            }
        }
    }
    if !pem_bundle.is_empty() {
        tls.cert_chain = Some(pem_bundle);
    }

    // mTLS: prefer PKCS#12, otherwise fall back to PEM (cert + key)
    match (&opts.pkcs12, &opts.certfile, &opts.keyfile) {
        (Some(p12), _, _) => {
            let der = fs::read(p12)?;
            let password = opts.passphrase.clone().unwrap_or_default();
            tls.identity = Some(OwnedIdentity::PKCS12 { der, password });
        }
        (None, Some(cert), Some(key)) => {
            let cert_pem = fs::read(cert)?;
            let key_pem = fs::read(key)?;
            tls.identity = Some(OwnedIdentity::PKCS8 {
                pem: cert_pem,
                key: key_pem,
            });
        }
        _ => {}
    }

    // Note: With this lapin/amq-protocol version there are no SNI/verify toggles
    // exposed through OwnedTLSConfig; provide the CA via cafile/capath and rely on default verification.
    Ok(tls)
}
