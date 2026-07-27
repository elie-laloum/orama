//! Integration test: the proxy can be reached on more than one address.
//!
//! This exists for one failure that has no other test shape. A harness inside
//! WSL cannot reach a listener bound only to the Windows loopback — measured, on
//! a live machine: a Windows server bound to `127.0.0.1` refuses the guest's
//! connection outright. Bridging to it therefore means genuinely serving on a
//! second address, and "genuinely" is the part worth proving: the second
//! listener has to answer the same router, not merely be bound.
//!
//! The Windows-only half of that feature cannot run here, so this pins the half
//! that is portable — that [`Config::extra_hosts`] produces a second way in, and
//! that the config reads back describing what was actually bound rather than
//! what was asked for.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use orama_core::Config;

/// A second address is served, and reports itself as bound.
///
/// `::1` rather than a second IPv4 loopback because `127.0.0.2` is bindable on
/// Linux and not on macOS, and a test that pins the platform it was written on
/// is worse than no test.
#[tokio::test]
async fn a_bridge_address_serves_the_same_proxy() {
    let db = std::env::temp_dir().join("orama-wsl-bridge-test.sqlite");
    let _ = std::fs::remove_file(&db);

    let config = Config {
        host: IpAddr::V4(Ipv4Addr::LOCALHOST),
        port: 0,
        ..Config::default()
    }
    .with_db_path(&db)
    .with_extra_hosts(vec![IpAddr::V6(Ipv6Addr::LOCALHOST)]);

    let bound = orama_core::bind(config).await.unwrap();
    let port = bound.addr().port();
    let bridged = bound.config().extra_hosts.clone();
    let addrs = bound.addrs();

    tokio::spawn(async move {
        let _ = bound.run().await;
    });

    // Both addresses answer the health probe, which is the whole claim: a
    // listener that is bound but serving nothing would pass a bind assertion
    // and still leave the harness talking to a socket that never replies.
    for addr in [
        format!("http://127.0.0.1:{port}/healthz"),
        format!("http://[::1]:{port}/healthz"),
    ] {
        let response = reqwest::get(&addr).await.unwrap_or_else(|err| {
            panic!("{addr} should be served: {err}");
        });
        assert_eq!(response.status(), 200, "{addr}");
        assert_eq!(response.text().await.unwrap(), "ok", "{addr}");
    }

    // What was bound, not what was requested — the same reason a requested port
    // of 0 reads back as the port the OS chose.
    assert_eq!(bridged, vec![IpAddr::V6(Ipv6Addr::LOCALHOST)]);
    assert_eq!(addrs.len(), 2, "{addrs:?}");

    let _ = std::fs::remove_file(&db);
}

/// A bridge address that cannot be bound is dropped, not fatal.
///
/// The proxy the local harnesses are already pointed at must survive losing an
/// extra way in — a WSL adapter that has gone away between discovery and bind is
/// an ordinary occurrence, and it must not stop the process starting.
#[tokio::test]
async fn an_unbindable_bridge_does_not_stop_the_proxy() {
    let db = std::env::temp_dir().join("orama-wsl-bridge-unbindable.sqlite");
    let _ = std::fs::remove_file(&db);

    let config = Config {
        host: IpAddr::V4(Ipv4Addr::LOCALHOST),
        port: 0,
        ..Config::default()
    }
    .with_db_path(&db)
    // An address on no interface of this machine. This is what a stale
    // discovery looks like from the bind's point of view.
    .with_extra_hosts(vec![IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7))]);

    let bound = orama_core::bind(config).await.unwrap();
    let port = bound.addr().port();

    // Bound, and honest about it: the address that failed is not reported as
    // listening, so nothing can be pointed at it.
    assert!(
        bound.config().extra_hosts.is_empty(),
        "an unbindable bridge must not read back as bound: {:?}",
        bound.config().extra_hosts
    );
    assert_eq!(bound.addrs().len(), 1);

    tokio::spawn(async move {
        let _ = bound.run().await;
    });

    let response = reqwest::get(format!("http://127.0.0.1:{port}/healthz"))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);

    let _ = std::fs::remove_file(&db);
}

/// The address a client is told to use follows the address it can reach.
#[test]
fn base_urls_can_be_built_for_an_address_other_than_the_bound_one() {
    let config = Config {
        port: 8787,
        ..Config::default()
    };

    // What a local harness is told.
    assert_eq!(config.public_base_url(), "http://127.0.0.1:8787");
    // What a harness inside a NAT-mode distro must be told instead. The /v1 and
    // the /backend-api/codex prefix have to survive the substitution, or Codex
    // arrives at a route the upstream does not have.
    assert_eq!(
        config.base_url_on("172.17.160.1"),
        "http://172.17.160.1:8787"
    );
    assert_eq!(
        config.openai_base_url_on("172.17.160.1"),
        "http://172.17.160.1:8787/v1"
    );
    assert_eq!(
        config.chatgpt_base_url_on("172.17.160.1"),
        "http://172.17.160.1:8787/backend-api/codex"
    );
}
