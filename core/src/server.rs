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
    /// Additional listeners on [`Config::extra_hosts`], serving the identical
    /// router. Their addresses are recorded on the config as bound, so one that
    /// could not be bound is absent from both rather than reported as working.
    extra: Vec<TcpListener>,
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

    /// Every address being listened on, primary first.
    pub fn addrs(&self) -> Vec<SocketAddr> {
        let mut all = vec![self.addr()];
        all.extend(self.extra.iter().filter_map(|l| l.local_addr().ok()));
        all
    }

    /// Serve until the process is terminated.
    ///
    /// One router, cloned per listener rather than rebuilt: a second router
    /// would be a second store handle and a second set of state, and the two
    /// could answer the same question differently.
    pub async fn run(self) -> anyhow::Result<()> {
        let app = router(self.config, self.store);
        let mut serving = tokio::task::JoinSet::new();
        for listener in std::iter::once(self.listener).chain(self.extra) {
            let app = app.clone();
            serving.spawn(async move { axum::serve(listener, app).await });
        }
        // Any listener failing takes the process down with it. A half-serving
        // proxy is the state where a harness is configured to reach an address
        // that has stopped answering, which is worse than stopping loudly.
        while let Some(joined) = serving.join_next().await {
            joined??;
        }
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

    // Bind the bridge addresses on the port we actually got, which matters when
    // the caller asked for port 0. A bridge that cannot be bound is reported and
    // dropped rather than fatal: it is an extra way in, and losing it must not
    // stop the proxy the local harnesses are already using.
    let mut extra = Vec::new();
    let mut bound_extra = Vec::new();
    for host in config
        .extra_hosts
        .iter()
        .copied()
        .filter(|host| *host != local.ip())
    {
        match TcpListener::bind(SocketAddr::new(host, local.port())).await {
            Ok(listener) => {
                extra.push(listener);
                bound_extra.push(host);
            }
            Err(err) => eprintln!(
                "orama: could not also listen on {host}:{}: {err}",
                local.port()
            ),
        }
    }

    let config = Config {
        host: local.ip(),
        port: local.port(),
        extra_hosts: bound_extra,
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
        extra,
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

    // A guest cannot use the snippet above: its 127.0.0.1 is its own. Printing
    // the one that works there is the difference between "the proxy is running"
    // and "the proxy is reachable from where the agent actually runs".
    for host in &effective.extra_hosts {
        let host = host.to_string();
        println!(
            "\n# from inside WSL, where 127.0.0.1 is the guest's own loopback:\n\
             export ANTHROPIC_BASE_URL={}\n\
             export OPENAI_BASE_URL={}",
            effective.base_url_on(&host),
            effective.openai_base_url_on(&host),
        );
    }
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
