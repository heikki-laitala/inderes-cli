//! Persistent storage for OAuth tokens.
//!
//! Primary backend is the OS keychain via the `keyring` crate (macOS Keychain,
//! Windows Credential Manager, Linux Secret Service). When the keychain is
//! unavailable (e.g. headless Linux without D-Bus) we fall back to a JSON file
//! in the platform config directory with `0600` permissions on Unix.

use std::fs;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

const KEYRING_SERVICE: &str = "inderes-cli";
const KEYRING_USER: &str = "default";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    #[serde(default)]
    pub token_type: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
}

impl Tokens {
    /// True when the access token is expired or within `skew_secs` of expiry.
    pub fn is_expiring(&self, skew_secs: i64) -> bool {
        let now = OffsetDateTime::now_utc();
        (self.expires_at - now).whole_seconds() <= skew_secs
    }
}

/// Load tokens from keyring; fall back to file on any error that looks like
/// "keyring unavailable" rather than "no entry".
pub fn load() -> Result<Option<Tokens>> {
    match load_keyring() {
        Ok(Some(t)) => Ok(Some(t)),
        Ok(None) => load_file(),
        Err(_) => load_file(),
    }
}

pub fn save(tokens: &Tokens) -> Result<()> {
    if save_keyring(tokens).is_ok() {
        // best-effort: also remove any stale file
        let _ = delete_file();
        return Ok(());
    }
    save_file(tokens)
}

pub fn clear() -> Result<()> {
    let _ = clear_keyring();
    let _ = delete_file();
    Ok(())
}

// --- keyring backend --------------------------------------------------------

fn load_keyring() -> Result<Option<Tokens>> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)?;
    match entry.get_password() {
        Ok(json) => Ok(Some(serde_json::from_str(&json)?)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn save_keyring(tokens: &Tokens) -> Result<()> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)?;
    let json = serde_json::to_string(tokens)?;
    entry.set_password(&json)?;
    Ok(())
}

fn clear_keyring() -> Result<()> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

// --- file backend -----------------------------------------------------------

fn token_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("com", "inderes", "inderes-cli")
        .context("could not determine platform config directory")?;
    Ok(dirs.config_dir().join("tokens.json"))
}

fn load_file() -> Result<Option<Tokens>> {
    let path = token_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let body = fs::read_to_string(&path)
        .with_context(|| format!("reading token file {}", path.display()))?;
    let tokens: Tokens = serde_json::from_str(&body)
        .with_context(|| format!("parsing token file {}", path.display()))?;
    Ok(Some(tokens))
}

fn save_file(tokens: &Tokens) -> Result<()> {
    let path = token_path()?;
    let parent = path.parent().context("token path has no parent")?;
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let body = serde_json::to_string_pretty(tokens)?;

    // Write atomically with a tempfile-rename dance so we never leave a
    // half-written file on crash.
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, &body).with_context(|| format!("writing {}", tmp.display()))?;
    set_file_perms_0600(&tmp)?;
    fs::rename(&tmp, &path)
        .with_context(|| format!("renaming {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

fn delete_file() -> Result<()> {
    let path = token_path()?;
    if path.exists() {
        fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_file_perms_0600(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_file_perms_0600(_path: &std::path::Path) -> Result<()> {
    // Windows: per-user %APPDATA% is already access-restricted; no chmod.
    Ok(())
}

/// Diagnostic helper used by `inderes whoami --verbose`.
pub fn backend_description() -> String {
    let path = token_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "<unknown>".into());
    format!("keyring service={KEYRING_SERVICE} user={KEYRING_USER} (fallback file: {path})")
}

#[allow(dead_code)]
pub fn token_file_path() -> Result<PathBuf> {
    token_path()
}

// Avoids dead-code warning when cross-compiling without touching the function.
#[allow(dead_code)]
fn _unused() {
    let _: fn() -> Result<()> = || bail!("unused");
}
