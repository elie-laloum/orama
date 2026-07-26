//! The inbound axum server. Ticket 01 provides the skeleton (health probe +
//! bootable listener); later tickets add the catch-all relay and capture.

use std::net::SocketAddr;

use axum::{routing::get, Router};
use tokio::net::TcpListener;

use crate::api::{
    self,
    settings::{self, SettingsState},
    ReadStore,
};
use crate::config::Config;
use crate::relay::{relay, RelayState};
use crate::store::StoreHandle;
use crate::util::now_rfc3339;

/// Build the axum router: the read-only UI/API, the settings surface, a health
/// probe, and a catch-all relay to upstream.
///
/// The UI/API and health probe are registered on dedicated paths; every other
/// path/method falls through to the transparent relay. Pass a [`StoreHandle`]
/// to enable capture; `None` gives a pure pass-through proxy.
pub fn router(config: Config, store: Option<StoreHandle>) -> Router {
    let capturing = store.is_some();
    let state = RelayState::with_store(&config, store);
    let read_store = ReadStore::new(config.db_path.clone());

    let settings_state = SettingsState {
        config: std::sync::Arc::new(config),
        capturing,
        started_at: now_rfc3339(),
        store: read_store.clone(),
    };

    // The relay/health routes carry RelayState; finalise that state before
    // merging with the already-stated read-only API/UI router.
    let relay_router = Router::new()
        .route("/healthz", get(healthz))
        .fallback(relay)
        .with_state(state);

    Router::new()
        .merge(api::routes(read_store))
        .merge(settings::routes(settings_state))
        .merge(relay_router)
}

/// Health probe endpoint — returns a success response so callers can confirm
/// the proxy is up.
async fn healthz() -> &'static str {
    "ok"
}

/// Bind and serve the proxy until the process is terminated.
///
/// Prints the export snippet to stdout on startup (touches no files).
pub async fn serve(config: Config) -> anyhow::Result<()> {
    let addr = SocketAddr::new(config.host, config.port);
    let listener = TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;

    // Re-derive config against the actually-bound address (in case port 0 was
    // requested) so the printed snippet is correct.
    let effective = Config {
        host: local.ip(),
        port: local.port(),
        ..config.clone()
    };

    // Open the DB and spawn the background writer that owns the connection.
    let store = match crate::store::spawn_writer(&effective.db_path) {
        Ok(handle) => Some(handle),
        Err(err) => {
            eprintln!(
                "orama: could not open database at {}: {err} — running without capture",
                effective.db_path.display()
            );
            None
        }
    };

    println!("Orama proxy listening on {local}");
    println!("upstream (anthropic): {}", effective.upstream);
    println!("upstream (openai):    {}", effective.upstream_openai);
    println!("capture db: {}", effective.db_path.display());
    println!("\n# paste into the shell that runs the agent:");
    println!("{}", effective.export_snippet());
    println!(
        "\n# or configure Claude Code and Codex in one click: {}/ui/#/settings",
        effective.public_base_url()
    );

    let app = router(effective, store);
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[tokio::test]
    async fn health_probe_returns_ok() {
        let cfg = Config::new(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            0,
            Config::default().upstream,
        );
        let listener = TcpListener::bind(SocketAddr::new(cfg.host, cfg.port))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(cfg, None);
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let body = reqwest::get(format!("http://{addr}/healthz"))
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(body, "ok");
    }
}
