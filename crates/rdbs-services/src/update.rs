//! Fetches the latest release for the update check (ADR 0013).
//!
//! Blocking HTTPS with `ureq`; the worker runs it on Tokio's blocking pool, never on the UI
//! thread.

use std::sync::Arc;
use std::time::Duration;

use rdbs_core::update::{RELEASES_API_URL, Release, latest_release};

/// The whole request (connect, TLS, response) gives up after this long.
const TIMEOUT: Duration = Duration::from_secs(15);

/// Asks GitHub for the releases list and picks the highest stable `vX.Y.Z` release.
pub(crate) fn fetch_latest_release() -> Result<Option<Release>, String> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        // GitHub rejects API requests without a User-Agent.
        .user_agent(format!("Zephix/{}", env!("CARGO_PKG_VERSION")))
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(root_certs())
                .build(),
        )
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut response = agent
        .get(RELEASES_API_URL)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .call()
        .map_err(describe_error)?;
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("Could not read the answer from GitHub: {e}"))?;
    latest_release(&body)
}

/// The OS certificate store (which also trusts a corporate TLS proxy), or the bundled
/// Mozilla roots when the store cannot be read.
fn root_certs() -> ureq::tls::RootCerts {
    let native = rustls_native_certs::load_native_certs();
    for err in &native.errors {
        tracing::debug!("skipping unreadable OS certificate: {err}");
    }
    if native.certs.is_empty() {
        return ureq::tls::RootCerts::WebPki;
    }
    let certs = native
        .certs
        .iter()
        .map(|der| ureq::tls::Certificate::from_der(der.as_ref()).to_owned())
        .collect();
    ureq::tls::RootCerts::Specific(Arc::new(certs))
}

fn describe_error(err: ureq::Error) -> String {
    match err {
        ureq::Error::StatusCode(code @ (403 | 429)) => format!(
            "GitHub refused the request (HTTP {code}). The hourly limit for anonymous \
             requests may be used up; try again later."
        ),
        ureq::Error::StatusCode(code) => format!("GitHub answered with HTTP {code}."),
        ureq::Error::Timeout(_) => "GitHub did not answer in time.".to_owned(),
        other => format!("Could not reach GitHub: {other}"),
    }
}

#[cfg(test)]
mod tests {
    /// Live: asks GitHub (needs network). Skipped without `RDBS_IT`, like the database tests.
    #[test]
    fn fetches_the_latest_release_from_github() {
        if std::env::var_os("RDBS_IT").is_none() {
            return;
        }
        let latest = super::fetch_latest_release().expect("GitHub answers");
        let latest = latest.expect("at least one stable release");
        assert!(latest.tag.starts_with('v'), "{latest:?}");
        assert!(latest.url.starts_with("https://github.com/"), "{latest:?}");
    }
}
