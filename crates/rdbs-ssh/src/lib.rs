//! SSH tunnels for database connections (local port forwarding over `direct-tcpip`).
//!
//! [`open`] logs in to the SSH server, verifies that the target is reachable from it, and
//! listens on an ephemeral `127.0.0.1` port. Every TCP connection to that port is forwarded to
//! `target_host:target_port` as seen from the SSH server. Dropping the [`Tunnel`] stops it.
//!
//! Host keys are trusted on first use and recorded in an application-specific `known_hosts`
//! file; a changed key is refused.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rdbs_core::{DbError, DbErrorKind, SshAuth, SshTunnel};
use russh::client::{self, Handle};
use russh::keys::known_hosts::learn_known_hosts_path;
use russh::keys::{PrivateKeyWithHashAlg, check_known_hosts_path, load_secret_key};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// A running tunnel. Dropping it closes the listener and the SSH session.
pub struct Tunnel {
    local_port: u16,
    accept_task: JoinHandle<()>,
    session: Arc<Handle<HostKeyCheck>>,
}

impl Tunnel {
    /// Port on `127.0.0.1` that forwards to the target.
    pub fn local_port(&self) -> u16 {
        self.local_port
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.accept_task.abort();
        let session = self.session.clone();
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                let _ = session
                    .disconnect(russh::Disconnect::ByApplication, "", "en")
                    .await;
            });
        }
    }
}

/// Trust-on-first-use host key verification against `known_hosts`.
struct HostKeyCheck {
    host: String,
    port: u16,
    known_hosts: PathBuf,
    rejection: Arc<Mutex<Option<String>>>,
}

impl HostKeyCheck {
    fn reject(&self, reason: String) -> Result<bool, russh::Error> {
        *self
            .rejection
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(reason);
        Ok(false)
    }
}

impl client::Handler for HostKeyCheck {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let russh::keys::PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            return self.reject("SSH host certificates are not supported yet".into());
        };
        let fingerprint = key.fingerprint(Default::default());
        match check_known_hosts_path(&self.host, self.port, key, &self.known_hosts) {
            Ok(true) => Ok(true),
            Err(russh::keys::Error::KeyChanged { line }) => self.reject(format!(
                "the host key of {}:{} changed (known_hosts line {line}, now {fingerprint}). \
                 This can mean a man-in-the-middle attack; remove the old entry from {} \
                 only if the change is expected.",
                self.host,
                self.port,
                self.known_hosts.display()
            )),
            // Unknown host (or no known_hosts file yet): trust and remember it.
            Ok(false) | Err(_) => {
                if let Err(e) =
                    learn_known_hosts_path(&self.host, self.port, key, &self.known_hosts)
                {
                    tracing::warn!("could not record SSH host key: {e}");
                }
                tracing::info!(
                    "trusting new SSH host key {fingerprint} for {}:{}",
                    self.host,
                    self.port
                );
                Ok(true)
            }
        }
    }
}

fn connection_error(message: impl Into<String>) -> DbError {
    DbError::new(DbErrorKind::Connection, message)
}

/// Opens a tunnel through `ssh` to `target_host:target_port`.
///
/// `secret` is the SSH password or the private key passphrase.
pub async fn open(
    ssh: &SshTunnel,
    secret: Option<&str>,
    target_host: &str,
    target_port: u16,
    known_hosts: &Path,
) -> Result<Tunnel, DbError> {
    let rejection = Arc::new(Mutex::new(None));
    let handler = HostKeyCheck {
        host: ssh.host.clone(),
        port: ssh.port,
        known_hosts: known_hosts.to_owned(),
        rejection: rejection.clone(),
    };
    let config = Arc::new(client::Config {
        keepalive_interval: Some(Duration::from_secs(30)),
        ..client::Config::default()
    });
    let address = format!("{}:{}", ssh.host, ssh.port);
    let connect = client::connect(config, (ssh.host.as_str(), ssh.port), handler);
    let mut session = match tokio::time::timeout(CONNECT_TIMEOUT, connect).await {
        Err(_) => {
            return Err(DbError::new(
                DbErrorKind::Timeout,
                format!("SSH connection to {address} timed out"),
            ));
        }
        Ok(Err(e)) => {
            let reason = rejection
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            return Err(match reason {
                Some(reason) => connection_error(format!("SSH host key rejected: {reason}")),
                None => connection_error(format!("SSH connection to {address} failed: {e}"))
                    .with_detail(
                        "Check the SSH host and port in the SSH section of the connection.",
                    ),
            });
        }
        Ok(Ok(session)) => session,
    };

    let auth = match &ssh.auth {
        SshAuth::Password => {
            session
                .authenticate_password(&ssh.username, secret.unwrap_or_default())
                .await
        }
        SshAuth::PrivateKey { path } => {
            let key = load_secret_key(path, secret).map_err(|e| {
                DbError::new(
                    DbErrorKind::Authentication,
                    format!("could not load SSH private key {path}: {e}"),
                )
            })?;
            let hash = session
                .best_supported_rsa_hash()
                .await
                .ok()
                .flatten()
                .flatten();
            session
                .authenticate_publickey(
                    &ssh.username,
                    PrivateKeyWithHashAlg::new(Arc::new(key), hash),
                )
                .await
        }
    }
    .map_err(|e| connection_error(format!("SSH authentication with {address} failed: {e}")))?;
    if !auth.success() {
        return Err(DbError::new(
            DbErrorKind::Authentication,
            format!("SSH authentication failed for {}@{address}", ssh.username),
        ));
    }

    // Probe: fails fast if forwarding is disabled or the target is unreachable from the server.
    let probe = session
        .channel_open_direct_tcpip(target_host, u32::from(target_port), "127.0.0.1", 0)
        .await
        .map_err(|e| {
            connection_error(format!(
                "the SSH server could not open a tunnel to {target_host}:{target_port}: {e}"
            ))
            .with_detail("Check that TCP forwarding is allowed on the SSH server and that the database host/port are reachable from it.")
        })?;
    let _ = probe.close().await;

    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| connection_error(format!("could not open a local tunnel port: {e}")))?;
    let local_port = listener
        .local_addr()
        .map_err(|e| connection_error(e.to_string()))?
        .port();

    let session = Arc::new(session);
    let forwarder = session.clone();
    let target = target_host.to_owned();
    let accept_task = tokio::spawn(async move {
        loop {
            let (mut socket, peer) = match listener.accept().await {
                Ok(accepted) => accepted,
                Err(e) => {
                    tracing::warn!("SSH tunnel listener stopped: {e}");
                    break;
                }
            };
            let session = forwarder.clone();
            let target = target.clone();
            tokio::spawn(async move {
                let channel = session
                    .channel_open_direct_tcpip(
                        target.as_str(),
                        u32::from(target_port),
                        peer.ip().to_string(),
                        u32::from(peer.port()),
                    )
                    .await;
                match channel {
                    Ok(channel) => {
                        let mut stream = channel.into_stream();
                        if let Err(e) =
                            tokio::io::copy_bidirectional(&mut socket, &mut stream).await
                        {
                            tracing::debug!("SSH tunnel stream closed: {e}");
                        }
                    }
                    Err(e) => tracing::warn!("SSH channel to {target}:{target_port} failed: {e}"),
                }
            });
        }
    });

    Ok(Tunnel {
        local_port,
        accept_task,
        session,
    })
}
