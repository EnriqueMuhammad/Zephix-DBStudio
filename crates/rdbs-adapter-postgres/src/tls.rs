//! rustls configuration for the PostgreSQL connector.

use std::sync::Arc;

use rdbs_core::{DbError, DbErrorKind, SslMode};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use tokio_postgres::config::SslMode as PgSslMode;
use tokio_postgres_rustls::MakeRustlsConnect;

/// Builds the libpq-style SSL mode and the connector for a profile's [`SslMode`].
///
/// `Prefer` and `Require` encrypt without verifying the certificate (libpq semantics);
/// `VerifyFull` verifies the chain against the OS trust store and the host name.
pub fn connector(mode: SslMode) -> Result<(PgSslMode, MakeRustlsConnect), DbError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(tls_error)?;
    let config = match mode {
        SslMode::VerifyFull => {
            let mut roots = RootCertStore::empty();
            let native = rustls_native_certs::load_native_certs();
            for err in &native.errors {
                tracing::warn!("skipping unreadable OS certificate: {err}");
            }
            roots.add_parsable_certificates(native.certs);
            builder.with_root_certificates(roots).with_no_client_auth()
        }
        SslMode::Disable | SslMode::Prefer | SslMode::Require => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyCert(provider)))
            .with_no_client_auth(),
    };
    let pg_mode = match mode {
        SslMode::Disable => PgSslMode::Disable,
        SslMode::Prefer => PgSslMode::Prefer,
        SslMode::Require | SslMode::VerifyFull => PgSslMode::Require,
    };
    Ok((pg_mode, MakeRustlsConnect::new(config)))
}

fn tls_error(e: rustls::Error) -> DbError {
    DbError::new(
        DbErrorKind::Connection,
        format!("TLS configuration error: {e}"),
    )
}

/// Encrypts without authenticating the server (libpq `sslmode=require`).
#[derive(Debug)]
struct AcceptAnyCert(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}
