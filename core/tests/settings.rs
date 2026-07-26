//! Integration test: the settings surface, including the two routes that write.
//!
//! Every test here repoints `CLAUDE_CONFIG_DIR`, `CODEX_HOME` and `ORAMA_HOME`
//! at a temp directory before touching anything, so a run can never reach the
//! real config files of whoever is running the suite. That is asserted rather
//! than assumed — a test that silently escaped its sandbox would edit the
//! developer's own harness.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use axum::Router;
use orama_core::{server::router, store::apply_schema, Config};
use serde_json::Value;
use tokio::net::TcpListener;

/// The env vars that locate config files are process-global.
static ENV: Mutex<()> = Mutex::new(());

struct Sandbox {
    _guard: MutexGuard<'static, ()>,
    dir: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Self {
        let guard = ENV.lock().unwrap_or_else(|err| err.into_inner());
        let dir = std::env::temp_dir().join(format!("orama-settings-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("CLAUDE_CONFIG_DIR", dir.join("claude"));
        std::env::set_var("CODEX_HOME", dir.join("codex"));
        std::env::set_var("ORAMA_HOME", dir.join("orama"));
        Self { _guard: guard, dir }
    }

    fn claude_settings(&self) -> PathBuf {
        self.dir.join("claude").join("settings.json")
    }

    fn codex_config(&self) -> PathBuf {
        self.dir.join("codex").join("config.toml")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        std::env::remove_var("CLAUDE_CONFIG_DIR");
        std::env::remove_var("CODEX_HOME");
        std::env::remove_var("ORAMA_HOME");
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn spawn(app: Router) -> SocketAddr {
    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

fn temp_db(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("orama-settings-{name}.sqlite"));
    let _ = std::fs::remove_file(&path);
    let conn = rusqlite::Connection::open(&path).unwrap();
    apply_schema(&conn).unwrap();
    path
}

async fn get(addr: SocketAddr, path: &str) -> (u16, Value) {
    let response = reqwest::get(format!("http://{addr}{path}")).await.unwrap();
    let status = response.status().as_u16();
    let text = response.text().await.unwrap();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

async fn post(addr: SocketAddr, path: &str) -> (u16, Value) {
    let response = reqwest::Client::new()
        .post(format!("http://{addr}{path}"))
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    let text = response.text().await.unwrap();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

#[tokio::test]
async fn settings_reports_proxy_state_and_both_upstreams() {
    let _sandbox = Sandbox::new("read");
    let db = temp_db("read");
    let config = Config::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        8787,
        "https://api.anthropic.com",
    )
    .with_openai_upstream("https://api.openai.com")
    .with_db_path(&db);
    let addr = spawn(router(config, None)).await;

    let (status, body) = get(addr, "/api/v2/settings").await;
    assert_eq!(status, 200);

    let proxy = &body["proxy"];
    assert_eq!(proxy["upstream_anthropic"], "https://api.anthropic.com");
    assert_eq!(proxy["upstream_openai"], "https://api.openai.com");
    assert_eq!(proxy["base_url"], "http://127.0.0.1:8787");
    // OpenAI SDKs append to the base, so the version segment belongs in it.
    assert_eq!(proxy["openai_base_url"], "http://127.0.0.1:8787/v1");
    // Router built with no store: relaying, not recording, and it says so.
    assert_eq!(proxy["capturing"], false);
    // Nothing captured is null, not a zero timestamp.
    assert!(proxy["last_capture_at"].is_null());

    let connectors = body["connectors"].as_array().unwrap();
    let ids: Vec<&str> = connectors
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["claude-code", "codex"]);
    assert!(connectors.iter().all(|c| c["connected"] == false));

    let guides: Vec<&str> = body["guides"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        guides,
        vec!["anthropic-api", "openai-api", "openai-compatible"]
    );
}

#[tokio::test]
async fn connecting_a_harness_is_visible_in_settings_and_reversible() {
    let sandbox = Sandbox::new("write");
    let db = temp_db("write");
    let config = Config::default().with_db_path(&db);
    let addr = spawn(router(config, None)).await;

    // The sandbox is what keeps this test off the real config files.
    assert!(sandbox.claude_settings().starts_with(std::env::temp_dir()));

    let (status, outcome) = post(addr, "/api/v2/connectors/claude-code/connect").await;
    assert_eq!(status, 200);
    assert_eq!(outcome["already"], false);
    assert_eq!(outcome["status"]["connected"], true);

    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(sandbox.claude_settings()).unwrap()).unwrap();
    assert_eq!(
        written["env"]["ANTHROPIC_BASE_URL"],
        "http://127.0.0.1:8787"
    );

    // The read endpoint agrees with the file, rather than caching a claim.
    let (_, body) = get(addr, "/api/v2/settings").await;
    let claude = body["connectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "claude-code")
        .unwrap()
        .clone();
    assert_eq!(claude["connected"], true);
    assert_eq!(claude["managed"], true);

    let (status, outcome) = post(addr, "/api/v2/connectors/claude-code/disconnect").await;
    assert_eq!(status, 200);
    assert_eq!(outcome["status"]["connected"], false);

    let (_, body) = get(addr, "/api/v2/settings").await;
    assert!(body["connectors"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c["connected"] == false));
}

#[tokio::test]
async fn codex_connector_overrides_the_base_url_without_touching_auth() {
    let sandbox = Sandbox::new("codex");
    let db = temp_db("codex");
    std::env::remove_var("OPENAI_API_KEY");
    let addr = spawn(router(Config::default().with_db_path(&db), None)).await;

    let (status, _) = post(addr, "/api/v2/connectors/codex/connect").await;
    assert_eq!(status, 200);

    let written = std::fs::read_to_string(sandbox.codex_config()).unwrap();
    // No API key on this machine, so subscription auth: a different backend
    // under a different path prefix, reached without naming any credential.
    assert!(
        written.contains("openai_base_url = \"http://127.0.0.1:8787/backend-api/codex\""),
        "{written}"
    );
    // Installing a provider would have to say where its key comes from, and an
    // env_key a subscription install cannot satisfy is what broke Codex.
    assert!(!written.contains("env_key"), "{written}");
    assert!(!written.contains("model_provider"), "{written}");
}

#[tokio::test]
async fn an_unknown_connector_names_the_ones_that_exist() {
    let _sandbox = Sandbox::new("unknown");
    let db = temp_db("unknown");
    let addr = spawn(router(Config::default().with_db_path(&db), None)).await;

    let (status, body) = post(addr, "/api/v2/connectors/opencode/connect").await;
    assert_eq!(status, 404);
    let error = body["error"].as_str().unwrap();
    // Same contract as an unknown API filter: say what would have worked.
    assert!(error.contains("opencode"), "{error}");
    assert!(
        error.contains("claude-code") && error.contains("codex"),
        "{error}"
    );
}
