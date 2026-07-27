//! Proxy state, and the endpoints that wire a harness to it.
//!
//! The rest of the API is strictly read-only over the capture database, and
//! that stays true here: these handlers never touch `calls` or anything derived
//! from it. What they write is harness config — files that belong to Claude
//! Code and Codex — and only through [`crate::connect`], which backs up before
//! it edits and records enough to put things back.
//!
//! The read endpoint answers one question the dashboard could not answer
//! before: *is anything actually pointed at me right now?* A proxy with no
//! traffic looks identical to a proxy nobody configured, and the difference
//! matters enough to show rather than leave the user to guess.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};

use crate::config::Config;
use crate::connect::{self, Target};
use crate::derive::PARSER_VERSION;
use crate::detect::POLICY_VERSION;
use crate::pricing::pricing_version;

use super::ReadStore;

/// What the settings endpoints need that the read API does not: the running
/// configuration, and whether capture is actually on.
#[derive(Clone)]
pub struct SettingsState {
    pub config: Arc<Config>,
    /// False when the database could not be opened at startup — the proxy still
    /// relays, but nothing is being recorded, which the UI must not hide.
    pub capturing: bool,
    pub started_at: String,
    pub store: ReadStore,
    /// The single writer, when capture is on. Needed because a catalogue
    /// refresh has to re-price, and re-pricing is a write — which goes through
    /// the one task that owns the connection, never a second one.
    pub writer: Option<crate::store::StoreHandle>,
}

pub fn routes(state: SettingsState) -> Router {
    Router::new()
        .route("/api/v2/settings", get(settings))
        .route("/api/v2/connectors/:id/connect", post(connect_handler))
        .route(
            "/api/v2/connectors/:id/disconnect",
            post(disconnect_handler),
        )
        .route("/api/v2/catalog/refresh", post(refresh_catalog))
        .with_state(state)
}

/// POST /api/v2/catalog/refresh — fetch the published rates now.
///
/// The daily background check is the normal path; this exists because "my
/// prices look stale" should be answerable without restarting the proxy.
async fn refresh_catalog(State(state): State<SettingsState>) -> Response {
    if !crate::catalog::fetch::refresh_enabled() {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": "catalogue refresh is disabled by ORAMA_CATALOG_REFRESH" })),
        )
            .into_response();
    }

    match crate::catalog::fetch::refresh(&state.config.db_path).await {
        Ok(outcome) => {
            let changed = matches!(outcome, crate::catalog::fetch::Refreshed::Installed { .. });
            // Rates that moved leave every priced row disagreeing with them, so
            // the re-price is part of the refresh rather than a later surprise.
            if changed {
                if let Some(writer) = &state.writer {
                    writer.reprice();
                }
            }
            Json(json!({ "changed": changed, "catalog": crate::api::v2::catalog_meta() }))
                .into_response()
        }
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": format!("could not reach models.dev: {err}") })),
        )
            .into_response(),
    }
}

/// GET /api/v2/settings — everything the settings screen renders.
async fn settings(State(state): State<SettingsState>) -> Response {
    let config = state.config.as_ref();
    Json(json!({
        "proxy": proxy_state(&state),
        "connectors": connect::status_all(config),
        "guides": guides(config),
    }))
    .into_response()
}

fn proxy_state(state: &SettingsState) -> Value {
    let config = state.config.as_ref();
    let (db_bytes, last_capture) = capture_stats(&state.store);
    json!({
        "listening_on": format!("{}:{}", config.host, config.port),
        // The bridge addresses, which is where a WSL harness reaches us. Listed
        // separately from `listening_on` because they are not interchangeable:
        // one is where the dashboard is, the other is only reachable from a
        // guest.
        "bridged_on": config
            .extra_hosts
            .iter()
            .map(|host| format!("{host}:{}", config.port))
            .collect::<Vec<_>>(),
        "base_url": config.public_base_url(),
        "openai_base_url": config.openai_base_url(),
        "upstream_anthropic": config.upstream,
        "upstream_openai": config.upstream_openai,
        "upstream_chatgpt": config.upstream_chatgpt,
        "db_path": config.db_path.display().to_string(),
        "db_bytes": db_bytes,
        "capturing": state.capturing,
        "started_at": state.started_at,
        "last_capture_at": last_capture,
        "parser_version": PARSER_VERSION,
        "pricing_version": pricing_version(),
        "policy_version": POLICY_VERSION,
        "catalog": crate::api::v2::catalog_meta(),
    })
}

/// Size on disk and the most recent capture time.
///
/// Both are `None` rather than `0` when unavailable: a database that cannot be
/// read is not an empty one, and "last capture: never" is a different fact from
/// "last capture: unknown".
fn capture_stats(store: &ReadStore) -> (Option<u64>, Option<String>) {
    let Ok(conn) = store.open() else {
        return (None, None);
    };
    let last = conn
        .query_row("SELECT MAX(timestamp_start) FROM calls", [], |row| {
            row.get::<_, Option<String>>(0)
        })
        .ok()
        .flatten();
    let bytes = conn
        .query_row(
            "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
            [],
            |row| row.get::<_, i64>(0),
        )
        .ok()
        .and_then(|n| u64::try_from(n).ok());
    (bytes, last)
}

/// Clients Orama cannot configure for the user, and what to set by hand.
///
/// These are SDKs and third-party endpoints rather than harnesses with a config
/// file at a known path — there is nothing to click, so the honest thing is to
/// hand over the exact line to paste.
fn guides(config: &Config) -> Value {
    let base = config.public_base_url();
    let openai = config.openai_base_url();
    json!([
        {
            "id": "anthropic-api",
            "label": "Anthropic API",
            "summary": "Any client speaking the Messages API — the Anthropic SDKs, or your own code.",
            "env": [{ "name": "ANTHROPIC_BASE_URL", "value": base }],
            "snippets": [
                { "language": "shell", "label": "Environment", "code": format!("export ANTHROPIC_BASE_URL={base}") },
                { "language": "python", "label": "Python SDK", "code": format!("from anthropic import Anthropic\n\nclient = Anthropic(base_url=\"{base}\")") },
                { "language": "typescript", "label": "TypeScript SDK", "code": format!("import Anthropic from \"@anthropic-ai/sdk\";\n\nconst client = new Anthropic({{ baseURL: \"{base}\" }});") }
            ],
            "note": "Your API key is sent as usual and forwarded untouched. Auth headers are redacted before anything is written to disk."
        },
        {
            "id": "openai-api",
            "label": "OpenAI API",
            "summary": "Chat Completions or Responses, from the OpenAI SDKs or anything that speaks them.",
            "env": [{ "name": "OPENAI_BASE_URL", "value": openai }],
            "snippets": [
                { "language": "shell", "label": "Environment", "code": format!("export OPENAI_BASE_URL={openai}") },
                { "language": "python", "label": "Python SDK", "code": format!("from openai import OpenAI\n\nclient = OpenAI(base_url=\"{openai}\")") },
                { "language": "typescript", "label": "TypeScript SDK", "code": format!("import OpenAI from \"openai\";\n\nconst client = new OpenAI({{ baseURL: \"{openai}\" }});") }
            ],
            "note": "The /v1 belongs in the base URL: OpenAI SDKs append paths to it, and the relay forwards the path verbatim."
        },
        {
            "id": "openai-compatible",
            "label": "OpenAI-compatible endpoint",
            "summary": "A third-party host speaking OpenAI's wire format — OpenRouter, Together, vLLM, Ollama.",
            "env": [{ "name": "OPENAI_BASE_URL", "value": openai }],
            "snippets": [
                { "language": "shell", "label": "Start the proxy against their host", "code": format!("orama start --upstream-openai https://openrouter.ai/api\n\n# then point the client at us as usual\nexport OPENAI_BASE_URL={openai}") }
            ],
            "note": format!("Unlike the others this needs a flag: OpenAI-dialect traffic is relayed to {}, so the destination has to be named at startup. The wire format is what the parsers key off, not the vendor.", config.upstream_openai)
        }
    ])
}

/// POST /api/v2/connectors/:id/connect — point a harness at this proxy.
async fn connect_handler(State(state): State<SettingsState>, Path(id): Path<String>) -> Response {
    apply(&state, &id, connect::connect)
}

/// POST /api/v2/connectors/:id/disconnect — put it back the way it was.
async fn disconnect_handler(
    State(state): State<SettingsState>,
    Path(id): Path<String>,
) -> Response {
    apply(&state, &id, connect::disconnect)
}

fn apply(
    state: &SettingsState,
    id: &str,
    action: fn(&Target, &Config) -> Result<connect::ConnectOutcome, connect::ConnectError>,
) -> Response {
    let Some(target) = Target::parse(id) else {
        // An unknown connector names the accepted set, the same as an unknown
        // API filter does — a 404 with no detail is a debugging dead end.
        let known: Vec<String> = Target::all().iter().map(Target::id).collect();
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("unknown connector `{id}`; known: {}", known.join(", ")) })),
        )
            .into_response();
    };

    match action(&target, state.config.as_ref()) {
        Ok(outcome) => Json(outcome).into_response(),
        Err(err) => {
            eprintln!("orama: {} connector failed: {err}", target.id());
            // The message says which file and why, which is the whole of what
            // the user needs to fix it by hand.
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({ "error": err.to_string() })),
            )
                .into_response()
        }
    }
}
