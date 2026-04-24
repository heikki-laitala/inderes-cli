//! Persistent storage for OAuth tokens.
//!
//! Tokens are stored as a JSON file in the platform config directory
//! (`directories::ProjectDirs`), written atomically via tempfile-rename and
//! chmod'd to `0600` on Unix. Windows relies on per-user `%APPDATA%` ACLs.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

pub fn load() -> Result<Option<Tokens>> {
    load_from(&token_path()?)
}

pub fn save(tokens: &Tokens) -> Result<()> {
    save_to(&token_path()?, tokens)
}

pub fn clear() -> Result<()> {
    remove_at(&token_path()?)
}

/// Diagnostic helper used by `inderes whoami --verbose`.
pub fn backend_description() -> String {
    token_path()
        .map(|p| format!("file: {}", p.display()))
        .unwrap_or_else(|e| format!("file: <unresolvable: {e:#}>"))
}

pub fn token_path() -> Result<PathBuf> {
    // Explicit override is handy both for power users (shared creds across
    // machines) and for integration tests that need to pre-seed or inspect
    // the token file without touching a real user profile.
    if let Ok(explicit) = std::env::var("INDERES_TOKEN_PATH") {
        if !explicit.is_empty() {
            return Ok(PathBuf::from(explicit));
        }
    }
    let dirs = ProjectDirs::from("com", "inderes", "inderes-cli")
        .context("could not determine platform config directory")?;
    Ok(dirs.config_dir().join("tokens.json"))
}

fn load_from(path: &Path) -> Result<Option<Tokens>> {
    if !path.exists() {
        return Ok(None);
    }
    let body = fs::read_to_string(path)
        .with_context(|| format!("reading token file {}", path.display()))?;
    let tokens: Tokens = serde_json::from_str(&body)
        .with_context(|| format!("parsing token file {}", path.display()))?;
    Ok(Some(tokens))
}

fn save_to(path: &Path, tokens: &Tokens) -> Result<()> {
    let parent = path.parent().context("token path has no parent")?;
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let body = serde_json::to_string_pretty(tokens)?;

    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, &body).with_context(|| format!("writing {}", tmp.display()))?;
    set_file_perms_0600(&tmp)?;
    fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

fn remove_at(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_file(path).with_context(|| format!("removing {}", path.display()))?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_file_perms_0600(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_file_perms_0600(_path: &Path) -> Result<()> {
    // Windows: per-user %APPDATA% is already access-restricted; no chmod.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use time::Duration;

    fn sample() -> Tokens {
        Tokens {
            access_token: "access-abc".into(),
            refresh_token: Some("refresh-xyz".into()),
            expires_at: OffsetDateTime::now_utc() + Duration::minutes(5),
            token_type: Some("Bearer".into()),
            scope: Some("openid offline_access".into()),
        }
    }

    #[test]
    fn load_missing_returns_none() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("tokens.json");
        assert!(load_from(&path).unwrap().is_none());
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("sub").join("tokens.json");
        let tokens = sample();
        save_to(&path, &tokens).unwrap();
        let loaded = load_from(&path).unwrap().expect("tokens should be present");
        assert_eq!(loaded, tokens);
    }

    #[test]
    fn clear_removes_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("tokens.json");
        save_to(&path, &sample()).unwrap();
        assert!(path.exists());
        remove_at(&path).unwrap();
        assert!(!path.exists());
        remove_at(&path).unwrap(); // idempotent
    }

    #[cfg(unix)]
    #[test]
    fn saved_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("tokens.json");
        save_to(&path, &sample()).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn is_expiring_detects_skew() {
        let mut t = sample();
        t.expires_at = OffsetDateTime::now_utc() + Duration::seconds(10);
        assert!(t.is_expiring(30));
        t.expires_at = OffsetDateTime::now_utc() + Duration::seconds(120);
        assert!(!t.is_expiring(30));
    }
}
