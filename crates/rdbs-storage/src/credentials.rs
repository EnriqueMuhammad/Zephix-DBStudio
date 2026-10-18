//! OS keychain credential store (Windows Credential Manager, macOS Keychain, Secret Service).

use rdbs_core::{CredentialError, CredentialStore, ProfileId, SecretKind};

/// Service name under which secrets are stored. It keeps the original project name so
/// passwords saved by earlier builds stay readable.
pub const KEYRING_SERVICE: &str = "RustDB Studio";

/// [`CredentialStore`] backed by the platform keychain via the `keyring` crate.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyringCredentials;

/// Keychain account: the profile id for the database password (unchanged since M2),
/// `<id>:ssh` for the tunnel secret.
fn entry(id: ProfileId, kind: SecretKind) -> Result<keyring::Entry, CredentialError> {
    let account = match kind {
        SecretKind::Database => id.to_string(),
        SecretKind::Ssh => format!("{id}:ssh"),
    };
    keyring::Entry::new(KEYRING_SERVICE, &account).map_err(|e| CredentialError(e.to_string()))
}

impl CredentialStore for KeyringCredentials {
    fn get(
        &self,
        profile_id: ProfileId,
        kind: SecretKind,
    ) -> Result<Option<String>, CredentialError> {
        match entry(profile_id, kind)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(CredentialError(e.to_string())),
        }
    }

    fn set(
        &self,
        profile_id: ProfileId,
        kind: SecretKind,
        secret: &str,
    ) -> Result<(), CredentialError> {
        entry(profile_id, kind)?
            .set_password(secret)
            .map_err(|e| CredentialError(e.to_string()))
    }

    fn delete(&self, profile_id: ProfileId, kind: SecretKind) -> Result<(), CredentialError> {
        match entry(profile_id, kind)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(CredentialError(e.to_string())),
        }
    }
}
