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
    let writer = store.clone();
    let state = RelayState::with_store(&config, store);
    let read_store = ReadStore::new(config.db_path.clone());

    let settings_state = SettingsState {
        config: std::sync::Arc::new(config),
        capturing,
        started_at: now_rfc3339(),
        store: read_store.clone(),
        writer,
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

/// A bound-but-not-yet-serving proxy.
///
/// Exists because a caller can need the port before any traffic flows. The
/// desktop shell is the case that forced it: it has to put a URL in a window,
/// and printing the port to a stdout nobody is reading is no help. Binding is
/// also the only way to learn that the port is already taken, which the shell
/// reports rather than dying silently behind a blank window.
pub struct Bound {
    listener: TcpListener,
    /// The configuration re-derived against the address actually bound, so a
    /// requested port of 0 reads back as the port the OS chose.
    config: Config,
    store: Option<StoreHandle>,
}

impl Bound {
    /// The address the proxy is listening on.
    pub fn addr(&self) -> SocketAddr {
        SocketAddr::new(self.config.host, self.config.port)
    }

    /// The configuration as bound — not as requested.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Whether captures are being recorded. False when the database could not
    /// be opened: the proxy still relays, and the UI must not pretend
    /// otherwise.
    pub fn capturing(&self) -> bool {
        self.store.is_some()
    }

    /// Serve until the process is terminated.
    pub async fn run(self) -> anyhow::Result<()> {
        let app = router(self.config, self.store);
        axum::serve(self.listener, app).await?;
        Ok(())
    }
}

/// Bind the proxy and open the capture database, without serving yet.
///
/// Failure to open the database is not failure to bind: relaying traffic is
/// the job that cannot be dropped, so capture degrades to off and the caller
/// learns which happened from [`Bound::capturing`].
pub async fn bind(config: Config) -> anyhow::Result<Bound> {
    let listener = TcpListener::bind(SocketAddr::new(config.host, config.port)).await?;
    let local = listener.local_addr()?;

    let config = Config {
        host: local.ip(),
        port: local.port(),
        ..config
    };

    // Open the DB and spawn the background writer that owns the connection.
    let store = match crate::store::spawn_writer(&config.db_path) {
        Ok(handle) => Some(handle),
        Err(err) => {
            eprintln!(
                "orama: could not open database at {}: {err} — running without capture",
                config.db_path.display()
            );
            None
        }
    };

    // Check for newer published rates in the background. Spawned rather than
    // awaited: whether models.dev is reachable has no bearing on whether this
    // proxy can relay traffic, and startup must not wait on it. A snapshot is
    // already in force by now, loaded offline by `spawn_writer`.
    if let Some(handle) = store.clone() {
        let db_path = config.db_path.clone();
        crate::catalog::fetch::spawn_refresh(db_path, move || handle.reprice());
    }

    Ok(Bound {
        listener,
        config,
        store,
    })
}

/// Bind and serve the proxy until the process is terminated.
///
/// Prints the export snippet to stdout on startup (touches no files).
pub async fn serve(config: Config) -> anyhow::Result<()> {
    let bound = bind(config).await?;
    let effective = bound.config();

    println!("Orama proxy listening on {}", bound.addr());
    println!("upstream (anthropic): {}", effective.upstream);
    println!("upstream (openai):    {}", effective.upstream_openai);
    println!("capture db: {}", effective.db_path.display());
    println!("\n# paste into the shell that runs the agent:");
    println!("{}", effective.export_snippet());
    println!(
        "\n# or configure Claude Code and Codex in one click: {}/ui/#/settings",
        effective.public_base_url()
    );

    bound.run().await
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
