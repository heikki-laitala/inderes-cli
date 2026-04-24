//! Glue between `oauth` and `storage`: ensures a valid access token, kicking
//! off a refresh (or a full re-login) when the current one is stale.

use anyhow::{bail, Context, Result};

use crate::oauth::IdpConfig;
use crate::storage::Tokens;
use crate::{oauth, storage};

const EXPIRY_SKEW_SECS: i64 = 30;

/// Return a fresh access token string. Refreshes if needed. If no tokens are
/// stored or refresh fails, returns an error instructing the user to run
/// `inderes login`.
pub async fn ensure_access_token(http: &reqwest::Client, idp: &IdpConfig) -> Result<String> {
    let tokens = storage::load()?
        .ok_or_else(|| anyhow::anyhow!("not signed in — run `inderes login` first"))?;

    if !tokens.is_expiring(EXPIRY_SKEW_SECS) {
        return Ok(tokens.access_token);
    }

    let Some(rt) = tokens.refresh_token.clone() else {
        bail!("access token expired and no refresh token stored — run `inderes login`");
    };

    let fresh = oauth::refresh(http, idp, &rt)
        .await
        .context("refreshing access token (try `inderes login` if this persists)")?;
    storage::save(&fresh)?;
    Ok(fresh.access_token)
}

/// Return stored tokens (for `inderes whoami`) without refreshing.
pub fn load_stored() -> Result<Option<Tokens>> {
    storage::load()
}
