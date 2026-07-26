//! Wiring a coding-agent harness to this proxy by editing its own config file.
//!
//! Every other module in this crate reads. This one writes, and only ever to
//! files that belong to a harness — never to the capture database, which stays
//! append-only truth derived from nothing but captured traffic.
//!
//! Three rules make the writes safe to hand to a button:
//!
//! - **Surgical.** Connecting sets one key and leaves the rest of the file
//!   alone, including comments and key order in TOML. A harness config holds
//!   the user's own settings; we are a guest in it.
//! - **Reversible.** What we overwrote is recorded in our own state file, so
//!   disconnecting restores the previous value rather than guessing a default.
//!   A key we did not set is never removed.
//! - **Atomic.** Writes land in a sibling temp file and are renamed over the
//!   target, so an interrupted write cannot leave a half-written config that
//!   stops the harness from starting.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::config::Config;
use crate::util::now_rfc3339;

/// The `model_providers` entry the Codex connector installs.
///
/// A provider entry rather than a bare `openai_base_url` override because it is
/// the only place `supports_websockets` can be set. Codex otherwise opens every
/// session by probing a WebSocket transport that Orama cannot proxy, retries it
/// five times, and falls back to HTTP several seconds later.
///
/// What made an earlier version of this break subscription installs was not the
/// entry itself but `env_key`: naming a variable a ChatGPT plan has no value for
/// makes Codex refuse to start. `requires_openai_auth` is the field that hands
/// the request to Codex's own credentials instead, and with it a subscription
/// authenticates through Orama normally.
const CODEX_PROVIDER: &str = "orama";

/// Top-level key an earlier build of this connector used.
///
/// Still read and still cleaned up, so a config connected by that version
/// reports honestly and disconnects completely rather than being left with two
/// settings that disagree about where traffic goes.
const CODEX_LEGACY_BASE_KEY: &str = "openai_base_url";

/// A harness Orama knows how to configure without the user editing anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Harness {
    ClaudeCode,
    Codex,
}

impl Harness {
    pub const ALL: &'static [Harness] = &[Harness::ClaudeCode, Harness::Codex];

    /// Stable identifier used in URLs and in the state file.
    pub fn id(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "claude-code",
            Harness::Codex => "codex",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "Claude Code",
            Harness::Codex => "Codex",
        }
    }

    pub fn parse(id: &str) -> Option<Harness> {
        Harness::ALL.iter().copied().find(|h| h.id() == id)
    }

    /// What connecting actually changes, in one line, for the UI to show
    /// *before* the button is pressed.
    pub fn effect(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "Sets env.ANTHROPIC_BASE_URL in settings.json.",
            Harness::Codex => {
                "Adds a model_providers.orama entry and selects it as model_provider."
            }
        }
    }

    /// What the connector inferred about this machine, shown before the button
    /// rather than discovered afterwards.
    ///
    /// The Codex one is load-bearing: the two auth modes need different
    /// backends and different path prefixes, so the mode is inferred rather
    /// than assumed, and stating which one was picked is what makes a wrong
    /// guess correctable instead of mysterious.
    pub fn caveat(self) -> Option<&'static str> {
        match self {
            Harness::ClaudeCode => None,
            Harness::Codex => Some(match codex_auth() {
                CodexAuth::ChatGpt => {
                    "No OPENAI_API_KEY found, so this assumes ChatGPT sign-in \
                     and routes to the ChatGPT Codex backend, using Codex's own \
                     credentials. Run `codex login` first if you are not \
                     signed in."
                }
                CodexAuth::ApiKey => {
                    "OPENAI_API_KEY is set, so this routes to api.openai.com \
                     for API-key billing."
                }
            }),
        }
    }

    /// Whether the harness must be restarted for the change to take effect.
    ///
    /// Both read their config at startup, so a running session keeps talking
    /// straight to the provider — saying so avoids the "why is nothing being
    /// captured" confusion.
    pub fn restart_required(self) -> bool {
        true
    }
}

/// The home directory, across the platforms a local dev tool actually runs on.
///
/// WSL is Linux and needs nothing special: `HOME` is the WSL home, which is
/// where the harness installed under WSL keeps its config. A Windows-side
/// install is a separate environment with its own `USERPROFILE`, and the two
/// are deliberately not bridged — writing across the boundary would configure
/// a harness the proxy's own `127.0.0.1` may not even resolve to.
fn home_dir() -> Option<PathBuf> {
    let from = |key: &str| {
        std::env::var_os(key)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    from("HOME").or_else(|| from("USERPROFILE")).or_else(|| {
        // Windows without USERPROFILE: the classic drive + path pair.
        let drive = std::env::var_os("HOMEDRIVE")?;
        let path = std::env::var_os("HOMEPATH")?;
        let mut joined = PathBuf::from(drive);
        joined.push(PathBuf::from(path));
        Some(joined)
    })
}

/// Where a harness keeps the config file we edit.
///
/// Both harnesses let the user relocate their config directory; honouring
/// those overrides means we edit the file the harness will actually read
/// rather than a default path it ignores.
pub fn config_path(harness: Harness) -> Result<PathBuf, ConnectError> {
    let explicit = match harness {
        Harness::ClaudeCode => std::env::var_os("CLAUDE_CONFIG_DIR"),
        Harness::Codex => std::env::var_os("CODEX_HOME"),
    }
    .filter(|value| !value.is_empty())
    .map(PathBuf::from);

    let dir = match explicit {
        Some(dir) => dir,
        None => {
            let home = home_dir().ok_or(ConnectError::NoHome)?;
            match harness {
                Harness::ClaudeCode => home.join(".claude"),
                Harness::Codex => home.join(".codex"),
            }
        }
    };

    Ok(match harness {
        Harness::ClaudeCode => dir.join("settings.json"),
        Harness::Codex => dir.join("config.toml"),
    })
}

/// The directory Orama keeps its own files in — `$ORAMA_HOME`, or `~/.orama`.
///
/// Public because the desktop shell needs it for the same reason this module
/// does: a windowed app is launched from a desktop entry, not a shell, so its
/// working directory is arbitrary and a relative default would scatter a
/// database wherever the launcher happened to start. Both callers resolving it
/// here means `ORAMA_HOME` moves everything at once, which is what makes the
/// connector tests safe to sandbox.
pub fn orama_home() -> Result<PathBuf, ConnectError> {
    match std::env::var_os("ORAMA_HOME").filter(|value| !value.is_empty()) {
        Some(dir) => Ok(PathBuf::from(dir)),
        None => Ok(home_dir().ok_or(ConnectError::NoHome)?.join(".orama")),
    }
}

/// Where Orama records what it changed, so a disconnect can put it back.
fn state_path() -> Result<PathBuf, ConnectError> {
    Ok(orama_home()?.join("connections.json"))
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("could not locate a home directory (set HOME, or ORAMA_HOME)")]
    NoHome,
    #[error("{path}: {source}")]
    Io {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("{path} is not valid {format}: {detail}")]
    Malformed {
        path: String,
        format: &'static str,
        detail: String,
    },
}

fn io_err(path: &Path, source: io::Error) -> ConnectError {
    ConnectError::Io {
        path: path.display().to_string(),
        source,
    }
}

/// What we overwrote when connecting, per harness.
///
/// Absent means we never connected this harness; `None` inside means the key
/// did not exist before, which is different from it having been empty — the
/// first is removed on disconnect, the second is restored.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct ConnectionState {
    #[serde(default)]
    entries: Map<String, Value>,
}

impl ConnectionState {
    fn load() -> Self {
        let Ok(path) = state_path() else {
            return Self::default();
        };
        // A missing or unreadable state file is not an error: it only means we
        // cannot restore a previous value, and disconnect falls back to
        // removing the key we recognise as ours.
        fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn save(&self) -> Result<(), ConnectError> {
        let path = state_path()?;
        let text = serde_json::to_string_pretty(&self).unwrap_or_else(|_| "{}".into());
        write_atomic(&path, &text)
    }

    fn get(&self, harness: Harness) -> Option<&Value> {
        self.entries.get(harness.id())
    }
}

/// Replace a file's contents without ever exposing a partial write.
fn write_atomic(path: &Path, contents: &str) -> Result<(), ConnectError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| io_err(parent, err))?;
    }
    let temp = path.with_extension(format!(
        "{}orama-tmp",
        path.extension()
            .map(|ext| format!("{}.", ext.to_string_lossy()))
            .unwrap_or_default()
    ));
    fs::write(&temp, contents).map_err(|err| io_err(&temp, err))?;
    // `rename` replaces the destination on every platform we target, so the
    // config is either entirely the old file or entirely the new one.
    match fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = fs::remove_file(&temp);
            Err(io_err(path, err))
        }
    }
}

/// A backup of the file as it was before the first connect that touched it.
fn backup_path(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
    path.with_file_name(format!(
        "{}.orama.bak",
        name.unwrap_or_else(|| "config".into())
    ))
}

/// Copy the file aside before editing it.
///
/// Safe to repeat: `connect` returns early when the config already points at
/// us, so whenever this runs the file on disk is the user's, never our own
/// output. Refreshing it each time means the backup reflects what they had
/// most recently, rather than whatever existed the first time they ever
/// clicked Connect.
fn back_up(path: &Path) -> Option<PathBuf> {
    if !path.exists() {
        return None;
    }
    let backup = backup_path(path);
    fs::copy(path, &backup).ok().map(|_| backup)
}

/* ── status ──────────────────────────────────────────────────────────────── */

/// What a harness is currently pointed at, and whether that is us.
#[derive(Debug, Clone, Serialize)]
pub struct HarnessStatus {
    pub id: &'static str,
    pub label: &'static str,
    /// The file we would read and write. Shown so the user can check it, and
    /// so a wrong-home diagnosis takes one glance rather than a support thread.
    pub config_path: String,
    pub config_exists: bool,
    /// Whether the config points at this proxy's base URL specifically.
    pub connected: bool,
    /// Whatever it currently points at, ours or not. `None` means the harness
    /// is using its own default and talking to the provider directly.
    pub base_url: Option<String>,
    /// True when we wrote the current value, which is what makes disconnect
    /// safe to offer. A base URL someone else set is left alone.
    pub managed: bool,
    pub effect: &'static str,
    /// A prerequisite the connector cannot meet for you. Shown before the
    /// button, not after the harness stops working.
    pub caveat: Option<&'static str>,
    pub restart_required: bool,
    /// Set when the config could not be read or parsed. Connecting is refused
    /// rather than risking a clobber of a file we do not understand.
    pub error: Option<String>,
}

/// Read the current state of every harness. Never fails as a whole: a harness
/// whose config is unreadable reports its own error and the rest still work.
pub fn status_all(config: &Config) -> Vec<HarnessStatus> {
    let state = ConnectionState::load();
    Harness::ALL
        .iter()
        .map(|&harness| status(harness, config, &state))
        .collect()
}

fn status(harness: Harness, config: &Config, state: &ConnectionState) -> HarnessStatus {
    let path = match config_path(harness) {
        Ok(path) => path,
        Err(err) => {
            return HarnessStatus {
                id: harness.id(),
                label: harness.label(),
                config_path: String::new(),
                config_exists: false,
                connected: false,
                base_url: None,
                managed: false,
                effect: harness.effect(),
                caveat: harness.caveat(),
                restart_required: harness.restart_required(),
                error: Some(err.to_string()),
            }
        }
    };

    let expected = expected_base_url(harness, config);
    let (base_url, error) = match read_base_url(harness, &path) {
        Ok(url) => (url, None),
        Err(err) => (None, Some(err.to_string())),
    };

    HarnessStatus {
        id: harness.id(),
        label: harness.label(),
        config_path: path.display().to_string(),
        config_exists: path.exists(),
        connected: base_url.as_deref() == Some(expected.as_str()),
        base_url,
        managed: state.get(harness).is_some(),
        effect: harness.effect(),
        caveat: harness.caveat(),
        restart_required: harness.restart_required(),
        error,
    }
}

/// How Codex proves who it is, which decides everything else about its config.
///
/// The two modes are not variations on a theme — they use different backends,
/// different path prefixes, and different auth plumbing. Writing the wrong one
/// does not degrade, it breaks: a subscription install handed an `env_key`
/// provider fails on startup with an unset `OPENAI_API_KEY`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodexAuth {
    /// Signed in with a ChatGPT plan. Talks to `chatgpt.com/backend-api/codex`
    /// and carries its own token, so no key can or should be configured.
    ChatGpt,
    /// Billing against an API key read from the environment.
    ApiKey,
}

/// Is the config already in the shape this version writes?
///
/// Distinct from "points at us": a config connected by an older build points at
/// us through a key this one no longer uses, and treating that as done would
/// leave it permanently un-migrated — still working, but still probing a
/// WebSocket on every session.
fn is_current_shape(harness: Harness, path: &Path) -> bool {
    match harness {
        Harness::ClaudeCode => true,
        Harness::Codex => fs::read_to_string(path)
            .ok()
            .and_then(|text| text.parse::<toml_edit::DocumentMut>().ok())
            .is_some_and(|document| {
                document
                    .get("model_provider")
                    .and_then(|item| item.as_str())
                    == Some(CODEX_PROVIDER)
            }),
    }
}

/// Does this URL point at a local Orama rather than a real provider?
///
/// Used only to decide whether a stale setting is ours to remove. Deliberately
/// shape-based rather than an exact match against the running port: the config
/// may have been written by an Orama started on a different one.
fn is_our_base_url(url: &str) -> bool {
    let host = url
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    (host.starts_with("127.0.0.1") || host.starts_with("localhost") || host.starts_with("[::1]"))
        && (url.contains("/backend-api/codex") || url.ends_with("/v1"))
}

/// Detect how this machine's Codex authenticates.
///
/// An API key present in the environment is the only positive evidence
/// available — subscription tokens live in the OS secret store on current
/// builds, so their *absence* is what identifies the common case. Guessing
/// wrong is cheap to correct and visible on the settings page either way.
pub fn codex_auth() -> CodexAuth {
    if std::env::var_os("OPENAI_API_KEY").is_some_and(|key| !key.is_empty()) {
        return CodexAuth::ApiKey;
    }
    // Older installs keep a key in auth.json rather than the environment.
    let has_key_file = config_path(Harness::Codex)
        .ok()
        .and_then(|path| path.parent().map(|dir| dir.join("auth.json")))
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .is_some_and(|auth| {
            auth.get("OPENAI_API_KEY")
                .and_then(Value::as_str)
                .is_some_and(|key| !key.is_empty())
        });
    if has_key_file {
        CodexAuth::ApiKey
    } else {
        CodexAuth::ChatGpt
    }
}

/// The base URL this harness should be pointed at to reach us.
pub fn expected_base_url(harness: Harness, config: &Config) -> String {
    match harness {
        Harness::ClaudeCode => config.public_base_url(),
        Harness::Codex => match codex_auth() {
            CodexAuth::ChatGpt => config.chatgpt_base_url(),
            CodexAuth::ApiKey => config.openai_base_url(),
        },
    }
}

/// Read whichever key the harness uses to name its provider endpoint.
fn read_base_url(harness: Harness, path: &Path) -> Result<Option<String>, ConnectError> {
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(path).map_err(|err| io_err(path, err))?;
    match harness {
        Harness::ClaudeCode => {
            let settings = parse_json(path, &text)?;
            Ok(settings
                .get("env")
                .and_then(Value::as_object)
                .and_then(|env| env.get("ANTHROPIC_BASE_URL"))
                .and_then(Value::as_str)
                .map(|url| url.trim_end_matches('/').to_owned()))
        }
        Harness::Codex => {
            let document = parse_toml(path, &text)?;
            // The selected provider decides where traffic goes; a provider
            // block that exists but is not selected is inert, and reporting it
            // as connected would be a confident wrong answer.
            let selected = document
                .get("model_provider")
                .and_then(|item| item.as_str())
                .and_then(|name| {
                    document
                        .get("model_providers")
                        .and_then(|item| item.as_table_like())
                        .and_then(|providers| providers.get(name))
                        .and_then(|provider| provider.as_table_like())
                        .and_then(|provider| provider.get("base_url"))
                        .and_then(|value| value.as_str())
                });
            Ok(selected
                // Fall back to the key the previous version wrote, so a config
                // connected by it is still recognised as ours.
                .or_else(|| {
                    document
                        .get(CODEX_LEGACY_BASE_KEY)
                        .and_then(|item| item.as_str())
                })
                .map(|url| url.trim_end_matches('/').to_owned()))
        }
    }
}

fn parse_json(path: &Path, text: &str) -> Result<Value, ConnectError> {
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(text).map_err(|err| ConnectError::Malformed {
        path: path.display().to_string(),
        format: "JSON",
        detail: err.to_string(),
    })
}

fn parse_toml(path: &Path, text: &str) -> Result<toml_edit::DocumentMut, ConnectError> {
    text.parse::<toml_edit::DocumentMut>()
        .map_err(|err| ConnectError::Malformed {
            path: path.display().to_string(),
            format: "TOML",
            detail: err.to_string(),
        })
}

/* ── connect / disconnect ────────────────────────────────────────────────── */

/// The result of a write, detailed enough for the UI to say what happened.
#[derive(Debug, Clone, Serialize)]
pub struct ConnectOutcome {
    pub harness: &'static str,
    pub config_path: String,
    /// Where the untouched original was copied, if there was one to copy.
    pub backup_path: Option<String>,
    /// True when the config already said what we were about to write.
    pub already: bool,
    pub status: HarnessStatus,
}

/// Point a harness at this proxy.
pub fn connect(harness: Harness, config: &Config) -> Result<ConnectOutcome, ConnectError> {
    let path = config_path(harness)?;
    let base = expected_base_url(harness, config);

    // Refuse to write over a file we could not parse: replacing a config we do
    // not understand would destroy settings we never read.
    let previous = read_base_url(harness, &path)?;
    if previous.as_deref() == Some(base.as_str()) && is_current_shape(harness, &path) {
        let state = ConnectionState::load();
        return Ok(ConnectOutcome {
            harness: harness.id(),
            config_path: path.display().to_string(),
            backup_path: None,
            already: true,
            status: status(harness, config, &state),
        });
    }

    let backup = back_up(&path);
    let mut record = json!({
        "connected_at": now_rfc3339(),
        "base_url": base,
        "config_path": path.display().to_string(),
    });

    match harness {
        Harness::ClaudeCode => {
            let text = if path.exists() {
                fs::read_to_string(&path).map_err(|err| io_err(&path, err))?
            } else {
                String::new()
            };
            let mut settings = parse_json(&path, &text)?;
            if !settings.is_object() {
                return Err(ConnectError::Malformed {
                    path: path.display().to_string(),
                    format: "JSON",
                    detail: "expected an object at the top level".into(),
                });
            }
            record["previous"] = previous.clone().map(Value::from).unwrap_or(Value::Null);

            let object = settings.as_object_mut().expect("checked above");
            let env = object
                .entry("env")
                .or_insert_with(|| json!({}))
                .as_object_mut()
                .ok_or_else(|| ConnectError::Malformed {
                    path: path.display().to_string(),
                    format: "JSON",
                    detail: "`env` exists but is not an object".into(),
                })?;
            env.insert("ANTHROPIC_BASE_URL".into(), Value::from(base.clone()));

            write_atomic(
                &path,
                &format!("{}\n", serde_json::to_string_pretty(&settings).unwrap()),
            )?;
        }
        Harness::Codex => {
            let text = if path.exists() {
                fs::read_to_string(&path).map_err(|err| io_err(&path, err))?
            } else {
                String::new()
            };
            let mut document = parse_toml(&path, &text)?;
            record["previous_model_provider"] = document
                .get("model_provider")
                .and_then(|item| item.as_str())
                .map(Value::from)
                .unwrap_or(Value::Null);
            record["previous_openai_base_url"] = document
                .get(CODEX_LEGACY_BASE_KEY)
                .and_then(|item| item.as_str())
                .map(Value::from)
                .unwrap_or(Value::Null);

            // A base URL this connector set previously would now compete with
            // the provider entry for the same job. Only ours is removed; one
            // the user set themselves is left to be restored on disconnect.
            if document
                .get(CODEX_LEGACY_BASE_KEY)
                .and_then(|item| item.as_str())
                .is_some_and(is_our_base_url)
            {
                document.remove(CODEX_LEGACY_BASE_KEY);
                record["previous_openai_base_url"] = Value::Null;
            }

            document["model_provider"] = toml_edit::value(CODEX_PROVIDER);
            let providers = document
                .entry("model_providers")
                .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
            let providers =
                providers
                    .as_table_like_mut()
                    .ok_or_else(|| ConnectError::Malformed {
                        path: path.display().to_string(),
                        format: "TOML",
                        detail: "`model_providers` exists but is not a table".into(),
                    })?;

            let mut entry = toml_edit::Table::new();
            entry["name"] = toml_edit::value("Orama");
            entry["base_url"] = toml_edit::value(base.clone());
            // Named explicitly so the entry cannot change meaning if Codex's
            // own default ever moves.
            entry["wire_api"] = toml_edit::value("responses");
            // The reason this is a provider entry at all: without it every
            // session opens with a WebSocket probe Orama cannot proxy.
            entry["supports_websockets"] = toml_edit::value(false);
            match codex_auth() {
                // Hand the request to Codex's own credentials. Naming an
                // `env_key` here is what broke subscription installs.
                CodexAuth::ChatGpt => entry["requires_openai_auth"] = toml_edit::value(true),
                CodexAuth::ApiKey => entry["env_key"] = toml_edit::value("OPENAI_API_KEY"),
            }
            providers.insert(CODEX_PROVIDER, toml_edit::Item::Table(entry));

            write_atomic(&path, &document.to_string())?;
        }
    }

    let mut state = ConnectionState::load();
    state.entries.insert(harness.id().to_owned(), record);
    state.save()?;

    Ok(ConnectOutcome {
        harness: harness.id(),
        config_path: path.display().to_string(),
        backup_path: backup.map(|p| p.display().to_string()),
        already: false,
        status: status(harness, config, &state),
    })
}

/// Put a harness back the way it was.
///
/// Only touches a value that points at us. A base URL someone else configured
/// is left exactly as found, and the call reports that nothing changed.
pub fn disconnect(harness: Harness, config: &Config) -> Result<ConnectOutcome, ConnectError> {
    let path = config_path(harness)?;
    let expected = expected_base_url(harness, config);
    let current = read_base_url(harness, &path)?;

    let mut state = ConnectionState::load();
    let record = state.get(harness).cloned();

    if current.as_deref() != Some(expected.as_str()) {
        // Nothing of ours in the file; drop our bookkeeping and report it.
        state.entries.remove(harness.id());
        let _ = state.save();
        return Ok(ConnectOutcome {
            harness: harness.id(),
            config_path: path.display().to_string(),
            backup_path: None,
            already: true,
            status: status(harness, config, &state),
        });
    }

    let text = fs::read_to_string(&path).map_err(|err| io_err(&path, err))?;

    match harness {
        Harness::ClaudeCode => {
            let mut settings = parse_json(&path, &text)?;
            let previous = record
                .as_ref()
                .and_then(|entry| entry.get("previous"))
                .and_then(Value::as_str)
                .map(str::to_owned);

            if let Some(object) = settings.as_object_mut() {
                if let Some(env) = object.get_mut("env").and_then(Value::as_object_mut) {
                    match previous {
                        Some(url) => {
                            env.insert("ANTHROPIC_BASE_URL".into(), Value::from(url));
                        }
                        None => {
                            env.remove("ANTHROPIC_BASE_URL");
                        }
                    }
                    // An `env` block that only ever held our key goes with it,
                    // leaving the file as we found it rather than as we left it.
                    if env.is_empty() {
                        object.remove("env");
                    }
                }
            }
            write_atomic(
                &path,
                &format!("{}\n", serde_json::to_string_pretty(&settings).unwrap()),
            )?;
        }
        Harness::Codex => {
            let mut document = parse_toml(&path, &text)?;
            let recorded = |key: &str| {
                record
                    .as_ref()
                    .and_then(|entry| entry.get(key))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            };

            match recorded("previous_model_provider") {
                Some(name) => document["model_provider"] = toml_edit::value(name),
                None => {
                    document.remove("model_provider");
                }
            }
            match recorded("previous_openai_base_url") {
                Some(url) => document[CODEX_LEGACY_BASE_KEY] = toml_edit::value(url),
                None => {
                    // Only ever remove a base URL that points at us; one the
                    // user set themselves is not ours to delete.
                    if document
                        .get(CODEX_LEGACY_BASE_KEY)
                        .and_then(|item| item.as_str())
                        .is_some_and(is_our_base_url)
                    {
                        document.remove(CODEX_LEGACY_BASE_KEY);
                    }
                }
            }
            if let Some(providers) = document
                .get_mut("model_providers")
                .and_then(|item| item.as_table_like_mut())
            {
                providers.remove(CODEX_PROVIDER);
                if providers.is_empty() {
                    document.remove("model_providers");
                }
            }
            write_atomic(&path, &document.to_string())?;
        }
    }

    state.entries.remove(harness.id());
    state.save()?;

    Ok(ConnectOutcome {
        harness: harness.id(),
        config_path: path.display().to_string(),
        backup_path: None,
        already: false,
        status: status(harness, config, &state),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    /// The env vars that locate config files are process-global, so the tests
    /// that repoint them take a lock rather than racing each other.
    static ENV: Mutex<()> = Mutex::new(());

    struct Sandbox {
        _guard: MutexGuard<'static, ()>,
        dir: PathBuf,
    }

    impl Sandbox {
        fn new(name: &str) -> Self {
            let guard = ENV.lock().unwrap_or_else(|err| err.into_inner());
            let dir = std::env::temp_dir().join(format!("orama-connect-{name}"));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            std::env::set_var("CLAUDE_CONFIG_DIR", dir.join("claude"));
            std::env::set_var("CODEX_HOME", dir.join("codex"));
            std::env::set_var("ORAMA_HOME", dir.join("orama"));
            Self { _guard: guard, dir }
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            std::env::remove_var("CLAUDE_CONFIG_DIR");
            std::env::remove_var("CODEX_HOME");
            std::env::remove_var("ORAMA_HOME");
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn config() -> Config {
        Config::default()
    }

    #[test]
    fn claude_code_connect_then_disconnect_leaves_no_trace() {
        let _sandbox = Sandbox::new("cc-roundtrip");
        let path = config_path(Harness::ClaudeCode).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"theme":"dark","permissions":{"allow":["Bash"]}}"#,
        )
        .unwrap();

        connect(Harness::ClaudeCode, &config()).unwrap();
        let after: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(after["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:8787");
        // Unrelated settings survive the edit.
        assert_eq!(after["theme"], "dark");
        assert_eq!(after["permissions"]["allow"][0], "Bash");

        disconnect(Harness::ClaudeCode, &config()).unwrap();
        let restored: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(restored["theme"], "dark");
        // The env block existed only to hold our key, so it goes too.
        assert!(restored.get("env").is_none(), "{restored}");
    }

    #[test]
    fn claude_code_disconnect_restores_a_previous_base_url() {
        let _sandbox = Sandbox::new("cc-restore");
        let path = config_path(Harness::ClaudeCode).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"env":{"ANTHROPIC_BASE_URL":"https://gateway.internal"}}"#,
        )
        .unwrap();

        connect(Harness::ClaudeCode, &config()).unwrap();
        disconnect(Harness::ClaudeCode, &config()).unwrap();

        let restored: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            restored["env"]["ANTHROPIC_BASE_URL"], "https://gateway.internal",
            "a base URL we replaced must come back, not be removed"
        );
    }

    #[test]
    fn connecting_creates_a_config_that_did_not_exist() {
        let _sandbox = Sandbox::new("cc-fresh");
        let path = config_path(Harness::ClaudeCode).unwrap();
        assert!(!path.exists());

        let outcome = connect(Harness::ClaudeCode, &config()).unwrap();
        assert!(path.exists());
        assert!(outcome.status.connected);
        // Nothing existed to back up.
        assert!(outcome.backup_path.is_none());
    }

    #[test]
    fn codex_connect_leaves_auth_and_providers_untouched() {
        let _sandbox = Sandbox::new("codex-preserve");
        let path = config_path(Harness::Codex).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "# my notes\nmodel = \"gpt-5\"\nmodel_provider = \"mine\"\n\n\
             [model_providers.mine]\nname = \"Mine\"\nbase_url = \"https://mine.test/v1\"\n",
        )
        .unwrap();

        connect(Harness::Codex, &config()).unwrap();
        let after = fs::read_to_string(&path).unwrap();
        assert!(
            after.contains("# my notes"),
            "comments must survive: {after}"
        );
        assert!(after.contains("model_provider = \"orama\""), "{after}");
        assert!(after.contains("[model_providers.orama]"), "{after}");
        // Without an API key, credentials come from Codex itself. Naming an
        // env_key is what made a subscription install refuse to start.
        assert!(after.contains("requires_openai_auth = true"), "{after}");
        assert!(!after.contains("env_key"), "{after}");
        // The reason this is a provider entry rather than a base-URL override.
        assert!(after.contains("supports_websockets = false"), "{after}");
        // The user's own provider is left intact alongside ours.
        assert!(after.contains("[model_providers.mine]"), "{after}");

        disconnect(Harness::Codex, &config()).unwrap();
        let restored = fs::read_to_string(&path).unwrap();
        assert!(restored.contains("model_provider = \"mine\""), "{restored}");
        assert!(restored.contains("[model_providers.mine]"), "{restored}");
        assert!(!restored.contains("model_providers.orama"), "{restored}");
        assert!(restored.contains("# my notes"));
    }

    #[test]
    fn codex_without_an_api_key_is_pointed_at_the_chatgpt_backend() {
        let _sandbox = Sandbox::new("codex-chatgpt");
        std::env::remove_var("OPENAI_API_KEY");
        assert_eq!(codex_auth(), CodexAuth::ChatGpt);

        connect(Harness::Codex, &config()).unwrap();
        let written = fs::read_to_string(config_path(Harness::Codex).unwrap()).unwrap();
        // Subscription auth talks to a different backend under a different
        // path prefix; api.openai.com/v1 would 404 it however well it parsed.
        // That prefix is also what routes it back out again.
        assert!(
            written.contains("base_url = \"http://127.0.0.1:8787/backend-api/codex\""),
            "{written}"
        );
        assert!(written.contains("requires_openai_auth = true"), "{written}");
    }

    #[test]
    fn a_config_connected_by_the_old_base_url_scheme_disconnects_cleanly() {
        let _sandbox = Sandbox::new("codex-legacy");
        std::env::remove_var("OPENAI_API_KEY");
        let path = config_path(Harness::Codex).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        // What an earlier build of this connector wrote.
        fs::write(
            &path,
            "model = \"gpt-5\"\nopenai_base_url = \"http://127.0.0.1:8787/backend-api/codex\"\n",
        )
        .unwrap();

        // It still reads as connected, rather than looking like a stranger's.
        let status = status(Harness::Codex, &config(), &ConnectionState::load());
        assert!(status.connected, "{status:?}");

        // Re-connecting replaces it instead of leaving two settings that
        // disagree about where traffic goes.
        connect(Harness::Codex, &config()).unwrap();
        let after = fs::read_to_string(&path).unwrap();
        assert!(!after.contains("openai_base_url"), "{after}");
        assert!(after.contains("[model_providers.orama]"), "{after}");

        disconnect(Harness::Codex, &config()).unwrap();
        let restored = fs::read_to_string(&path).unwrap();
        assert!(!restored.contains("openai_base_url"), "{restored}");
        assert!(!restored.contains("orama"), "{restored}");
        assert!(restored.contains("model = \"gpt-5\""), "{restored}");
    }

    #[test]
    fn a_base_url_the_user_set_themselves_is_left_alone() {
        let _sandbox = Sandbox::new("codex-foreign-base");
        let path = config_path(Harness::Codex).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "openai_base_url = \"https://gateway.internal/v1\"\n").unwrap();

        connect(Harness::Codex, &config()).unwrap();
        disconnect(Harness::Codex, &config()).unwrap();
        let restored = fs::read_to_string(&path).unwrap();
        assert!(
            restored.contains("openai_base_url = \"https://gateway.internal/v1\""),
            "a setting we did not make is not ours to remove: {restored}"
        );
    }

    #[test]
    fn codex_with_an_api_key_is_pointed_at_the_v1_api() {
        let _sandbox = Sandbox::new("codex-apikey");
        std::env::set_var("OPENAI_API_KEY", "sk-test");
        assert_eq!(codex_auth(), CodexAuth::ApiKey);

        let written = connect(Harness::Codex, &config()).and_then(|_| {
            fs::read_to_string(config_path(Harness::Codex).unwrap())
                .map_err(|err| io_err(Path::new("codex"), err))
        });
        std::env::remove_var("OPENAI_API_KEY");
        let written = written.unwrap();
        assert!(
            written.contains("base_url = \"http://127.0.0.1:8787/v1\""),
            "{written}"
        );
        // With a key present, that is what Codex should authenticate with.
        assert!(
            written.contains("env_key = \"OPENAI_API_KEY\""),
            "{written}"
        );
        assert!(!written.contains("requires_openai_auth"), "{written}");
    }

    #[test]
    fn a_backup_never_captures_our_own_output() {
        let _sandbox = Sandbox::new("cc-twice");
        let path = config_path(Harness::ClaudeCode).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"theme":"light"}"#).unwrap();

        let first = connect(Harness::ClaudeCode, &config()).unwrap();
        assert!(!first.already);
        let backup = first.backup_path.clone().unwrap();
        assert!(fs::read_to_string(&backup).unwrap().contains("light"));

        // Re-connecting while already connected does not reach the backup step
        // at all, so our own output can never become the "original".
        let second = connect(Harness::ClaudeCode, &config()).unwrap();
        assert!(second.already);
        assert!(!fs::read_to_string(&backup)
            .unwrap()
            .contains("ANTHROPIC_BASE_URL"));

        // After a genuine round trip the backup refreshes to whatever the user
        // has now, rather than pinning the first file we ever saw.
        disconnect(Harness::ClaudeCode, &config()).unwrap();
        fs::write(&path, r#"{"theme":"dark"}"#).unwrap();
        connect(Harness::ClaudeCode, &config()).unwrap();
        let saved = fs::read_to_string(&backup).unwrap();
        assert!(saved.contains("dark"), "{saved}");
        assert!(!saved.contains("ANTHROPIC_BASE_URL"), "{saved}");
    }

    #[test]
    fn a_malformed_config_is_refused_rather_than_overwritten() {
        let _sandbox = Sandbox::new("cc-malformed");
        let path = config_path(Harness::ClaudeCode).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{ not json at all").unwrap();

        let result = connect(Harness::ClaudeCode, &config());
        assert!(matches!(result, Err(ConnectError::Malformed { .. })));
        // The file is exactly as the user left it.
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ not json at all");
    }

    #[test]
    fn disconnecting_a_base_url_we_did_not_set_changes_nothing() {
        let _sandbox = Sandbox::new("cc-foreign");
        let path = config_path(Harness::ClaudeCode).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = r#"{"env":{"ANTHROPIC_BASE_URL":"https://someone-elses-proxy.test"}}"#;
        fs::write(&path, original).unwrap();

        let outcome = disconnect(Harness::ClaudeCode, &config()).unwrap();
        assert!(outcome.already);
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn status_reports_where_a_harness_points_when_it_is_not_us() {
        let _sandbox = Sandbox::new("cc-elsewhere");
        let path = config_path(Harness::ClaudeCode).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"env":{"ANTHROPIC_BASE_URL":"https://elsewhere.test"}}"#,
        )
        .unwrap();

        let all = status_all(&config());
        let claude = all.iter().find(|s| s.id == "claude-code").unwrap();
        assert!(!claude.connected);
        assert_eq!(claude.base_url.as_deref(), Some("https://elsewhere.test"));
        assert!(!claude.managed);
    }
}
