//! Keeping the catalogue fresh.
//!
//! Every failure here is survivable and none of it may touch the relay. A
//! refresh that cannot reach models.dev leaves the cached snapshot in force and
//! says so on stderr — the same best-effort posture capture itself has.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use super::{Catalog, Source};

/// Where the catalogue is published.
pub const CATALOG_URL: &str = "https://models.dev/api.json";

/// How often a long-running proxy re-checks. The request is conditional, so a
/// day between checks costs a 304 and a few hundred bytes.
const REFRESH_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// A refresh that fetches nothing is not a failure, so the outcome is explicit.
#[derive(Debug, Clone, PartialEq)]
pub enum Refreshed {
    /// The published catalogue is the one already cached.
    Unchanged,
    /// A new snapshot was stored and installed.
    Installed { digest: String, models: usize },
}

/// Set `ORAMA_CATALOG_REFRESH=0` to never reach the network.
///
/// Tests set it so a build machine's connectivity cannot change what they
/// assert; it doubles as the escape hatch for an air-gapped run.
pub fn refresh_enabled() -> bool {
    !matches!(
        std::env::var("ORAMA_CATALOG_REFRESH").as_deref(),
        Ok("0") | Ok("false") | Ok("no")
    )
}

/// Fetch the catalogue, store it, and install it — unless the published copy is
/// the one already held.
pub async fn refresh(db_path: &Path) -> anyhow::Result<Refreshed> {
    let conn = rusqlite::Connection::open(db_path)?;
    let cached = super::cache::current_etag(&conn)?;
    let etag = cached.as_ref().and_then(|(_, etag)| etag.clone());

    let mut request = reqwest::Client::new()
        .get(CATALOG_URL)
        .timeout(Duration::from_secs(30));
    if let Some(etag) = &etag {
        request = request.header(reqwest::header::IF_NONE_MATCH, etag);
    }

    let response = request.send().await?;
    if response.status() == reqwest::StatusCode::NOT_MODIFIED {
        super::cache::touch(&conn)?;
        return Ok(Refreshed::Unchanged);
    }
    if !response.status().is_success() {
        anyhow::bail!("models.dev returned {}", response.status());
    }

    let fresh_etag = response
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let payload = response.bytes().await?;

    // Parse before storing. A truncated or reshaped document must never replace
    // a snapshot that works — an unparseable catalogue would price nothing.
    let catalog = super::parse(&payload, fresh_etag.clone(), Source::Network)?;
    validate(&catalog)?;

    let digest = catalog.snapshot.digest.clone();
    // Compare against what is actually in force, not only against the cache. On
    // a first run the cache is empty but the bundled snapshot is loaded, and it
    // is routinely the same bytes the server just sent — re-pricing every row to
    // arrive at identical numbers is work nobody asked for.
    let in_force = super::current().map(|held| held.snapshot.digest.clone());
    let already_held = cached.as_ref().is_some_and(|(held, _)| held == &digest)
        || in_force.as_deref() == Some(digest.as_str());
    if already_held {
        // Still worth persisting: the cache is what survives a restart, and
        // holding the ETag is what makes the next check a 304.
        super::cache::write(&conn, &digest, fresh_etag.as_deref(), &payload)?;
        return Ok(Refreshed::Unchanged);
    }

    let models = catalog.model_count();
    super::cache::write(&conn, &digest, fresh_etag.as_deref(), &payload)?;
    super::install(Arc::new(catalog));
    Ok(Refreshed::Installed { digest, models })
}

/// Refuse a payload that parses but is obviously not the catalogue.
///
/// models.dev serving an error page as JSON, or a partial regeneration, would
/// otherwise silently un-price traffic that was priced a moment ago.
fn validate(catalog: &Catalog) -> anyhow::Result<()> {
    for provider in ["anthropic", "openai"] {
        if catalog
            .providers
            .get(provider)
            .is_none_or(HashMap::is_empty)
        {
            anyhow::bail!("catalogue is missing the {provider} provider");
        }
    }
    Ok(())
}

/// Refresh now, then once a day, installing new snapshots as they land.
///
/// Spawned rather than awaited: the proxy must serve traffic whether or not
/// models.dev is reachable.
pub fn spawn_refresh(db_path: PathBuf, on_change: impl Fn() + Send + 'static) {
    if !refresh_enabled() {
        return;
    }
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(REFRESH_INTERVAL);
        loop {
            ticker.tick().await;
            match refresh(&db_path).await {
                Ok(Refreshed::Installed { digest, models }) => {
                    eprintln!(
                        "orama: model catalogue updated — {models} models, snapshot {}",
                        &digest[..digest.len().min(12)]
                    );
                    // Rates moved, so what is already derived is priced at rates
                    // nobody is using any more.
                    on_change();
                }
                Ok(Refreshed::Unchanged) => {}
                Err(err) => eprintln!("orama: could not refresh the model catalogue: {err}"),
            }
        }
    });
}
