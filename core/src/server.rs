//! The inbound axum server. Ticket 01 provides the skeleton (health probe +
//! bootable listener); later tickets add the catch-all relay and capture.

use std::net::SocketAddr;

use axum::{routing::get, Router};
use tokio::net::TcpListener;

use crate::config::Config;
use crate::relay::{relay, RelayState};

/// Build the axum router: a health probe plus a catch-all relay to upstream.
///
/// The health probe is registered on a dedicated path; every other path/method
/// falls through to the transparent relay.
pub fn router(config: Config) -> Router {
    let state = RelayState::new(config.upstream.clone());
    Router::new()
        .route("/healthz", get(healthz))
        .fallback(relay)
        .with_state(state)
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
        upstream: config.upstream.clone(),
    };

    println!("tracer proxy listening on {local}");
    println!("upstream: {}", effective.upstream);
    println!("\n# paste into the shell that runs Claude Code:");
    println!("{}", effective.export_snippet());
    println!("\n# open the UI at {}/ui", effective.public_base_url());

    let app = router(effective);
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[tokio::test]
    async fn health_probe_returns_ok() {
        let cfg = Config::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0, Config::default().upstream);
        let listener = TcpListener::bind(SocketAddr::new(cfg.host, cfg.port))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(cfg);
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
