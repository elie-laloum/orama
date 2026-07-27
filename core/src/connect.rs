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

pub mod wsl;

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

    /// The directory this harness keeps its config in, relative to a home.
    pub fn config_leaf(self) -> &'static str {
        match self {
            Harness::ClaudeCode => ".claude",
            Harness::Codex => ".codex",
        }
    }

    /// The file inside that directory which we edit.
    pub fn config_file(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "settings.json",
            Harness::Codex => "config.toml",
        }
    }

    /// The environment variable that relocates the config directory.
    ///
    /// Named here rather than at each use because it is read on both sides of
    /// the WSL boundary: from this process's own environment for a local
    /// harness, and out of the guest's environment for one inside a distro.
    pub fn config_dir_var(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "CLAUDE_CONFIG_DIR",
            Harness::Codex => "CODEX_HOME",
        }
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
    ///
    /// Takes the mode rather than detecting it, because which environment to
    /// detect it in depends on where the harness lives — a Codex inside WSL
    /// authenticates from the guest's environment, not this process's.
    pub fn caveat(self, auth: CodexAuth) -> Option<&'static str> {
        match self {
            Harness::ClaudeCode => None,
            Harness::Codex => Some(match auth {
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
/// This is the *local* home only. A proxy running under WSL needs nothing more:
/// `HOME` is the guest home, which is where a harness installed alongside it
/// keeps its config, and both are already in the same network namespace. A proxy
/// running on Windows is the asymmetric case — the harness may be inside a
/// distro, with its config on the far side of a share and a different address
/// needed to reach us. That direction is [`wsl`]'s job, and an earlier version
/// of this comment described it as unbridgeable; it is bridgeable, but only once
/// the address the guest can actually reach is part of the answer.
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

/// Where a harness lives, relative to the process configuring it.
///
/// The distinction exists because a Windows-side proxy and a harness inside a
/// WSL distro are two environments, not one: the config file is on the far side
/// of a share, and `127.0.0.1` means something different at each end. Modelling
/// the place explicitly is what keeps those two facts from having to be
/// rediscovered at every call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Site {
    /// The same environment this proxy runs in.
    Local,
    /// A WSL distribution, seen from a proxy running on Windows.
    Wsl(String),
}

/// A harness at a place — which is what a connector row actually is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub harness: Harness,
    pub site: Site,
}

impl Target {
    pub fn local(harness: Harness) -> Self {
        Self {
            harness,
            site: Site::Local,
        }
    }

    /// Stable identifier used in URLs and in the state file.
    ///
    /// A local target keeps the bare harness id it has always had. That is not
    /// cosmetic: the state file is keyed by this string, so changing it would
    /// orphan every record of what we overwrote and turn the next disconnect
    /// into a guess.
    pub fn id(&self) -> String {
        match &self.site {
            Site::Local => self.harness.id().to_owned(),
            Site::Wsl(distro) => format!("{}@{distro}", self.harness.id()),
        }
    }

    pub fn parse(id: &str) -> Option<Target> {
        match id.split_once('@') {
            None => Harness::parse(id).map(Target::local),
            Some((harness, distro)) if !distro.is_empty() => {
                Harness::parse(harness).map(|harness| Target {
                    harness,
                    site: Site::Wsl(distro.to_owned()),
                })
            }
            Some(_) => None,
        }
    }

    pub fn label(&self) -> String {
        match &self.site {
            Site::Local => self.harness.label().to_owned(),
            Site::Wsl(distro) => format!("{} in {distro}", self.harness.label()),
        }
    }

    /// Every target worth offering on this machine.
    ///
    /// The local pair always, plus one per harness per discovered distro. A
    /// distro contributes rows whether or not the harness is installed in it:
    /// "not connected, and this is the file that would be created" is a useful
    /// answer, and deciding a harness is absent from the outside is exactly the
    /// guess that got this wrong before.
    pub fn all() -> Vec<Target> {
        let mut targets: Vec<Target> = Harness::ALL.iter().copied().map(Target::local).collect();
        for distro in wsl::distros() {
            for &harness in Harness::ALL {
                targets.push(Target {
                    harness,
                    site: Site::Wsl(distro.name.clone()),
                });
            }
        }
        targets
    }
}

/// Look up a discovered distro by name.
fn distro(name: &str) -> Result<wsl::Distro, ConnectError> {
    wsl::distros()
        .into_iter()
        .find(|distro| distro.name == name)
        .ok_or_else(|| ConnectError::NoDistro(name.to_owned()))
}

/// Where a harness keeps the config file we edit.
///
/// Both harnesses let the user relocate their config directory; honouring
/// those overrides means we edit the file the harness will actually read
/// rather than a default path it ignores. For a harness inside WSL the override
/// lives in the guest's environment and the path has to be translated onto the
/// share — see [`wsl`] for why neither can be skipped.
pub fn config_path(target: &Target) -> Result<PathBuf, ConnectError> {
    match &target.site {
        Site::Local => {
            let dir = match std::env::var_os(target.harness.config_dir_var())
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
            {
                Some(dir) => dir,
                None => home_dir()
                    .ok_or(ConnectError::NoHome)?
                    .join(target.harness.config_leaf()),
            };
            Ok(dir.join(target.harness.config_file()))
        }
        // Assembled by the distro rather than joined onto here: a Windows path
        // has to carry Windows separators whatever platform is doing the
        // joining.
        Site::Wsl(name) => Ok(distro(name)?.windows_config_file(target.harness)),
    }
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
    #[error("no WSL distribution named `{0}` was found")]
    NoDistro(String),
    /// The distro is there but there is no address it could reach us on. Its own
    /// error rather than a silent fallback to `127.0.0.1`: that value parses,
    /// writes, and reads back as connected while failing every request.
    #[error(
        "{distro} is behind a NAT and no gateway to Windows could be found, so there is no \
         address it could reach this proxy on. Start the distribution and re-read, or set \
         networkingMode=mirrored in .wslconfig to reach it on 127.0.0.1."
    )]
    NoRoute { distro: String },
    /// The address exists but this proxy is not on it. Refused rather than
    /// written, because the config would read back as connected while every
    /// request from the guest went nowhere.
    #[error(
        "this proxy is not listening on {gateway}, the only address {distro} could reach it on. \
         Restart Orama with the distribution running, or set networkingMode=mirrored in \
         .wslconfig to reach it on 127.0.0.1."
    )]
    NotBridged {
        distro: String,
        gateway: std::net::IpAddr,
    },
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

    fn get(&self, target: &Target) -> Option<&Value> {
        self.entries.get(&target.id())
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
    pub id: String,
    pub label: String,
    /// `local`, or `wsl` for a harness inside a distribution.
    pub site: &'static str,
    /// The distribution this row is about, when it is about one.
    pub distro: Option<String>,
    /// The file we would read and write. Shown so the user can check it, and
    /// so a wrong-home diagnosis takes one glance rather than a support thread.
    pub config_path: String,
    pub config_exists: bool,
    /// Whether the config points at this proxy's base URL specifically.
    pub connected: bool,
    /// Whatever it currently points at, ours or not. `None` means the harness
    /// is using its own default and talking to the provider directly.
    pub base_url: Option<String>,
    /// What connecting would write. Reported rather than left for the client to
    /// reassemble: it depends on how Codex authenticates *and* on which side of
    /// a WSL boundary the harness sits, and a second implementation of that in
    /// the dashboard is a second thing to drift.
    pub expected_base_url: Option<String>,
    /// True when we wrote the current value, which is what makes disconnect
    /// safe to offer. A base URL someone else set is left alone.
    pub managed: bool,
    pub effect: &'static str,
    /// A prerequisite the connector cannot meet for you. Shown before the
    /// button, not after the harness stops working.
    pub caveat: Option<String>,
    pub restart_required: bool,
    /// Set when the config could not be read or parsed. Connecting is refused
    /// rather than risking a clobber of a file we do not understand.
    pub error: Option<String>,
}

/// Read the current state of every harness. Never fails as a whole: a harness
/// whose config is unreadable reports its own error and the rest still work.
pub fn status_all(config: &Config) -> Vec<HarnessStatus> {
    let state = ConnectionState::load();
    Target::all()
        .iter()
        .map(|target| status(target, config, &state))
        .collect()
}

/// What connecting this target would write, and why that might be impossible.
///
/// Fallible where the single-site version was not, because a WSL target can be
/// perfectly locatable on disk and still have no address the guest could reach
/// us on. Reporting that is the whole point: the alternative is a config that
/// looks connected and fails every request.
pub fn expected_base_url(target: &Target, config: &Config) -> Result<String, ConnectError> {
    let host = match &target.site {
        Site::Local => config.host.to_string(),
        Site::Wsl(name) => {
            let address = distro(name)?
                .host_address(wsl::networking())
                .ok_or_else(|| ConnectError::NoRoute {
                    distro: name.clone(),
                })?;
            // Reachable in principle is not reachable in fact. Anything but the
            // address we are already serving has to be one we successfully
            // bound, or the config we write points at nothing.
            if address != config.host && !config.extra_hosts.contains(&address) {
                return Err(ConnectError::NotBridged {
                    distro: name.clone(),
                    gateway: address,
                });
            }
            address.to_string()
        }
    };

    Ok(match target.harness {
        Harness::ClaudeCode => config.base_url_on(&host),
        Harness::Codex => match codex_auth_for(target) {
            CodexAuth::ChatGpt => config.chatgpt_base_url_on(&host),
            CodexAuth::ApiKey => config.openai_base_url_on(&host),
        },
    })
}

/// Everything the connector inferred, in one line, before the button is pressed.
///
/// The WSL half is not decoration. Under NAT the address written into the guest
/// is the gateway of a virtual adapter, and that gateway is reassigned when WSL
/// restarts — at which point a config that was correct becomes a config that
/// captures nothing. Saying so is the difference between a limitation and a
/// mystery.
fn caveat_for(target: &Target, config: &Config) -> Option<String> {
    let harness = target
        .harness
        .caveat(codex_auth_for(target))
        .map(str::to_owned);

    let Site::Wsl(name) = &target.site else {
        return harness;
    };

    let bridging = match distro(name).ok() {
        None => Some(format!(
            "{name} was not reachable when this page was read, so its paths could not be resolved."
        )),
        Some(distro) if !distro.is_wsl2() => None,
        Some(distro) => match wsl::networking() {
            wsl::Networking::Mirrored => None,
            wsl::Networking::Nat => Some(match distro.gateway {
                // Whether we are *actually* listening there is read off the
                // bound config rather than assumed from having wanted to. A
                // bridge that failed to bind is exactly the case where claiming
                // otherwise sends someone hunting through their harness for a
                // problem that is on this side.
                Some(gateway) if config.extra_hosts.contains(&gateway) => format!(
                    "{name} is behind a NAT, so it reaches this proxy at {gateway} rather than \
                     127.0.0.1, and the proxy is listening there as well. That address is \
                     reassigned when WSL restarts — reconnect if capture stops. Setting \
                     networkingMode=mirrored in .wslconfig makes it 127.0.0.1 for good."
                ),
                Some(gateway) => format!(
                    "{name} is behind a NAT and would reach this proxy at {gateway}, but the proxy \
                     could not listen on that address, so connecting would not capture anything. \
                     Restart Orama once the distribution is running, or set \
                     networkingMode=mirrored in .wslconfig to reach it on 127.0.0.1 instead."
                ),
                None => format!("No route from {name} to Windows could be found."),
            }),
        },
    };

    // A relocated config directory is worth stating because it is the surprising
    // part of the answer, and because it is the one thing here we inferred by
    // running a shell inside someone else's distro.
    let relocated = distro(name).ok().and_then(|distro| {
        distro
            .config_dir_is_overridden(target.harness)
            .then(|| format!("{} is set in {name}.", target.harness.config_dir_var()))
    });

    let joined: Vec<String> = [harness, bridging, relocated]
        .into_iter()
        .flatten()
        .collect();
    (!joined.is_empty()).then(|| joined.join(" "))
}

fn status(target: &Target, config: &Config, state: &ConnectionState) -> HarnessStatus {
    let (site, distro_name) = match &target.site {
        Site::Local => ("local", None),
        Site::Wsl(name) => ("wsl", Some(name.clone())),
    };

    let blank = |error: String| HarnessStatus {
        id: target.id(),
        label: target.label(),
        site,
        distro: distro_name.clone(),
        config_path: String::new(),
        config_exists: false,
        connected: false,
        base_url: None,
        expected_base_url: None,
        managed: false,
        effect: target.harness.effect(),
        caveat: caveat_for(target, config),
        restart_required: target.harness.restart_required(),
        error: Some(error),
    };

    let path = match config_path(target) {
        Ok(path) => path,
        Err(err) => return blank(err.to_string()),
    };

    // An unresolvable base URL is reported without hiding the rest: the path is
    // still worth showing, and so is whatever the config currently says.
    let (expected, route_error) = match expected_base_url(target, config) {
        Ok(url) => (Some(url), None),
        Err(err) => (None, Some(err.to_string())),
    };

    let (base_url, read_error) = match read_base_url(target.harness, &path) {
        Ok(url) => (url, None),
        Err(err) => (None, Some(err.to_string())),
    };

    HarnessStatus {
        id: target.id(),
        label: target.label(),
        site,
        distro: distro_name,
        config_path: path.display().to_string(),
        config_exists: path.exists(),
        connected: expected.is_some() && base_url == expected,
        base_url,
        expected_base_url: expected,
        managed: state.get(target).is_some(),
        effect: target.harness.effect(),
        caveat: caveat_for(target, config),
        restart_required: target.harness.restart_required(),
        error: read_error.or(route_error),
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
    codex_auth_for(&Target::local(Harness::Codex))
}

/// How the Codex at this particular target authenticates.
///
/// Which environment to look in is part of the question, not an implementation
/// detail. A Codex inside WSL reads the guest's `OPENAI_API_KEY`, and inspecting
/// this process's instead would write the wrong provider shape — the mistake
/// that stops a subscription install from starting at all.
pub fn codex_auth_for(target: &Target) -> CodexAuth {
    if target.harness != Harness::Codex {
        // Meaningless for anything else, and the caller only uses it to pick
        // Codex's caveat and base URL.
        return CodexAuth::ChatGpt;
    }

    let from_env = match &target.site {
        Site::Local => std::env::var_os("OPENAI_API_KEY").is_some_and(|key| !key.is_empty()),
        Site::Wsl(name) => distro(name)
            .ok()
            .and_then(|distro| distro.env_var("OPENAI_API_KEY").map(str::to_owned))
            .is_some_and(|key| !key.is_empty()),
    };
    if from_env {
        return CodexAuth::ApiKey;
    }

    // Older installs keep a key in auth.json rather than the environment. Its
    // path is asked of the distro for a WSL target rather than derived from the
    // config path, because `parent` and `join` on a Windows path are only
    // meaningful when Windows is what is running.
    let auth_path = match &target.site {
        Site::Local => config_path(target)
            .ok()
            .and_then(|path| path.parent().map(|dir| dir.join("auth.json"))),
        Site::Wsl(name) => distro(name)
            .ok()
            .map(|distro| distro.windows_config_sibling(target.harness, "auth.json")),
    };

    let has_key_file = auth_path
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
    /// The target id — the bare harness id locally, `harness@distro` in WSL.
    /// Named `harness` still because it is what the dashboard has always read.
    pub harness: String,
    pub config_path: String,
    /// Where the untouched original was copied, if there was one to copy.
    pub backup_path: Option<String>,
    /// True when the config already said what we were about to write.
    pub already: bool,
    pub status: HarnessStatus,
}

/// Point a harness at this proxy.
pub fn connect(target: &Target, config: &Config) -> Result<ConnectOutcome, ConnectError> {
    let harness = target.harness;

    // Take the discovery again before writing. Under NAT the address a guest
    // reaches us on is reassigned when WSL restarts, and a cached one would be
    // written into a config that then captures nothing.
    if let Site::Wsl(name) = &target.site {
        if wsl::probe_named(name).is_none() {
            return Err(ConnectError::NoDistro(name.clone()));
        }
    }

    let path = config_path(target)?;
    let base = expected_base_url(target, config)?;

    // Refuse to write over a file we could not parse: replacing a config we do
    // not understand would destroy settings we never read.
    let previous = read_base_url(harness, &path)?;
    if previous.as_deref() == Some(base.as_str()) && is_current_shape(harness, &path) {
        let state = ConnectionState::load();
        return Ok(ConnectOutcome {
            harness: target.id(),
            config_path: path.display().to_string(),
            backup_path: None,
            already: true,
            status: status(target, config, &state),
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
            match codex_auth_for(target) {
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
    state.entries.insert(target.id(), record);
    state.save()?;

    Ok(ConnectOutcome {
        harness: target.id(),
        config_path: path.display().to_string(),
        backup_path: backup.map(|p| p.display().to_string()),
        already: false,
        status: status(target, config, &state),
    })
}

/// Put a harness back the way it was.
///
/// Only touches a value that points at us. A base URL someone else configured
/// is left exactly as found, and the call reports that nothing changed.
pub fn disconnect(target: &Target, config: &Config) -> Result<ConnectOutcome, ConnectError> {
    let harness = target.harness;
    let path = config_path(target)?;
    let current = read_base_url(harness, &path)?;

    let mut state = ConnectionState::load();
    let record = state.get(target).cloned();

    // What we would write now is normally what we wrote before, but under NAT
    // the address can have moved since. So the value recorded at connect time is
    // the primary test of "is this ours", with the current expectation as a
    // fallback for a state file that was lost — otherwise a WSL restart would
    // strand a config we wrote and are no longer willing to remove.
    let ours: Vec<String> = [
        record
            .as_ref()
            .and_then(|entry| entry.get("base_url"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        expected_base_url(target, config).ok(),
    ]
    .into_iter()
    .flatten()
    .collect();

    if !current.as_ref().is_some_and(|url| ours.contains(url)) {
        // Nothing of ours in the file; drop our bookkeeping and report it.
        state.entries.remove(&target.id());
        let _ = state.save();
        return Ok(ConnectOutcome {
            harness: target.id(),
            config_path: path.display().to_string(),
            backup_path: None,
            already: true,
            status: status(target, config, &state),
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

    state.entries.remove(&target.id());
    state.save()?;

    Ok(ConnectOutcome {
        harness: target.id(),
        config_path: path.display().to_string(),
        backup_path: None,
        already: false,
        status: status(target, config, &state),
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
            // Discovery spawns `wsl.exe`. On a Windows dev machine these tests
            // would start every installed distro and run a shell inside it, so
            // the whole suite stays on the local site.
            std::env::set_var("ORAMA_WSL", "0");
            Self { _guard: guard, dir }
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            std::env::remove_var("CLAUDE_CONFIG_DIR");
            std::env::remove_var("CODEX_HOME");
            std::env::remove_var("ORAMA_HOME");
            std::env::remove_var("ORAMA_WSL");
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn config() -> Config {
        Config::default()
    }

    fn cc() -> Target {
        Target::local(Harness::ClaudeCode)
    }

    fn cx() -> Target {
        Target::local(Harness::Codex)
    }

    #[test]
    fn claude_code_connect_then_disconnect_leaves_no_trace() {
        let _sandbox = Sandbox::new("cc-roundtrip");
        let path = config_path(&cc()).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"theme":"dark","permissions":{"allow":["Bash"]}}"#,
        )
        .unwrap();

        connect(&cc(), &config()).unwrap();
        let after: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(after["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:8787");
        // Unrelated settings survive the edit.
        assert_eq!(after["theme"], "dark");
        assert_eq!(after["permissions"]["allow"][0], "Bash");

        disconnect(&cc(), &config()).unwrap();
        let restored: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(restored["theme"], "dark");
        // The env block existed only to hold our key, so it goes too.
        assert!(restored.get("env").is_none(), "{restored}");
    }

    #[test]
    fn claude_code_disconnect_restores_a_previous_base_url() {
        let _sandbox = Sandbox::new("cc-restore");
        let path = config_path(&cc()).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"env":{"ANTHROPIC_BASE_URL":"https://gateway.internal"}}"#,
        )
        .unwrap();

        connect(&cc(), &config()).unwrap();
        disconnect(&cc(), &config()).unwrap();

        let restored: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            restored["env"]["ANTHROPIC_BASE_URL"], "https://gateway.internal",
            "a base URL we replaced must come back, not be removed"
        );
    }

    #[test]
    fn connecting_creates_a_config_that_did_not_exist() {
        let _sandbox = Sandbox::new("cc-fresh");
        let path = config_path(&cc()).unwrap();
        assert!(!path.exists());

        let outcome = connect(&cc(), &config()).unwrap();
        assert!(path.exists());
        assert!(outcome.status.connected);
        // Nothing existed to back up.
        assert!(outcome.backup_path.is_none());
    }

    #[test]
    fn codex_connect_leaves_auth_and_providers_untouched() {
        let _sandbox = Sandbox::new("codex-preserve");
        let path = config_path(&cx()).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "# my notes\nmodel = \"gpt-5\"\nmodel_provider = \"mine\"\n\n\
             [model_providers.mine]\nname = \"Mine\"\nbase_url = \"https://mine.test/v1\"\n",
        )
        .unwrap();

        connect(&cx(), &config()).unwrap();
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

        disconnect(&cx(), &config()).unwrap();
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

        connect(&cx(), &config()).unwrap();
        let written = fs::read_to_string(config_path(&cx()).unwrap()).unwrap();
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
        let path = config_path(&cx()).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        // What an earlier build of this connector wrote.
        fs::write(
            &path,
            "model = \"gpt-5\"\nopenai_base_url = \"http://127.0.0.1:8787/backend-api/codex\"\n",
        )
        .unwrap();

        // It still reads as connected, rather than looking like a stranger's.
        let status = status(&cx(), &config(), &ConnectionState::load());
        assert!(status.connected, "{status:?}");

        // Re-connecting replaces it instead of leaving two settings that
        // disagree about where traffic goes.
        connect(&cx(), &config()).unwrap();
        let after = fs::read_to_string(&path).unwrap();
        assert!(!after.contains("openai_base_url"), "{after}");
        assert!(after.contains("[model_providers.orama]"), "{after}");

        disconnect(&cx(), &config()).unwrap();
        let restored = fs::read_to_string(&path).unwrap();
        assert!(!restored.contains("openai_base_url"), "{restored}");
        assert!(!restored.contains("orama"), "{restored}");
        assert!(restored.contains("model = \"gpt-5\""), "{restored}");
    }

    #[test]
    fn a_base_url_the_user_set_themselves_is_left_alone() {
        let _sandbox = Sandbox::new("codex-foreign-base");
        let path = config_path(&cx()).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "openai_base_url = \"https://gateway.internal/v1\"\n").unwrap();

        connect(&cx(), &config()).unwrap();
        disconnect(&cx(), &config()).unwrap();
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

        let written = connect(&cx(), &config()).and_then(|_| {
            fs::read_to_string(config_path(&cx()).unwrap())
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
        let path = config_path(&cc()).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"theme":"light"}"#).unwrap();

        let first = connect(&cc(), &config()).unwrap();
        assert!(!first.already);
        let backup = first.backup_path.clone().unwrap();
        assert!(fs::read_to_string(&backup).unwrap().contains("light"));

        // Re-connecting while already connected does not reach the backup step
        // at all, so our own output can never become the "original".
        let second = connect(&cc(), &config()).unwrap();
        assert!(second.already);
        assert!(!fs::read_to_string(&backup)
            .unwrap()
            .contains("ANTHROPIC_BASE_URL"));

        // After a genuine round trip the backup refreshes to whatever the user
        // has now, rather than pinning the first file we ever saw.
        disconnect(&cc(), &config()).unwrap();
        fs::write(&path, r#"{"theme":"dark"}"#).unwrap();
        connect(&cc(), &config()).unwrap();
        let saved = fs::read_to_string(&backup).unwrap();
        assert!(saved.contains("dark"), "{saved}");
        assert!(!saved.contains("ANTHROPIC_BASE_URL"), "{saved}");
    }

    #[test]
    fn a_malformed_config_is_refused_rather_than_overwritten() {
        let _sandbox = Sandbox::new("cc-malformed");
        let path = config_path(&cc()).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{ not json at all").unwrap();

        let result = connect(&cc(), &config());
        assert!(matches!(result, Err(ConnectError::Malformed { .. })));
        // The file is exactly as the user left it.
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ not json at all");
    }

    #[test]
    fn disconnecting_a_base_url_we_did_not_set_changes_nothing() {
        let _sandbox = Sandbox::new("cc-foreign");
        let path = config_path(&cc()).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = r#"{"env":{"ANTHROPIC_BASE_URL":"https://someone-elses-proxy.test"}}"#;
        fs::write(&path, original).unwrap();

        let outcome = disconnect(&cc(), &config()).unwrap();
        assert!(outcome.already);
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn a_local_target_keeps_the_bare_harness_id() {
        // The state file is keyed by this string. Changing it for a local
        // harness would orphan every record of what we overwrote, so a config
        // connected by an earlier build could no longer be put back.
        assert_eq!(cc().id(), "claude-code");
        assert_eq!(cx().id(), "codex");
        assert_eq!(Target::parse("claude-code"), Some(cc()));
        assert_eq!(Target::parse("codex"), Some(cx()));
    }

    #[test]
    fn a_wsl_target_round_trips_through_its_id() {
        let target = Target {
            harness: Harness::ClaudeCode,
            site: Site::Wsl("Ubuntu".into()),
        };
        assert_eq!(target.id(), "claude-code@Ubuntu");
        assert_eq!(Target::parse("claude-code@Ubuntu"), Some(target));
        assert_eq!(Target::parse("claude-code@"), None);
        assert_eq!(Target::parse("nonesuch@Ubuntu"), None);
        // A distro whose name contains a dash or a dot is still one segment.
        assert_eq!(
            Target::parse("codex@Ubuntu-24.04").map(|t| t.id()),
            Some("codex@Ubuntu-24.04".to_owned())
        );
    }

    #[test]
    fn disconnect_restores_a_config_whose_address_has_since_moved() {
        // The NAT case, reduced to something testable without a distro: we
        // wrote an address, and by the time the user disconnects the address we
        // *would* write has changed. Comparing only against the current
        // expectation would decide the file was not ours and strand a setting
        // that points at a proxy about to stop.
        let _sandbox = Sandbox::new("cc-moved");
        let path = config_path(&cc()).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"env":{"ANTHROPIC_BASE_URL":"https://gateway.internal"}}"#,
        )
        .unwrap();

        connect(&cc(), &config()).unwrap();

        // Same proxy, different address — as if WSL had been restarted under it.
        let moved = Config {
            port: 9999,
            ..config()
        };
        let outcome = disconnect(&cc(), &moved).unwrap();
        assert!(
            !outcome.already,
            "a value we wrote is ours to remove even at an address that has moved"
        );

        let restored: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            restored["env"]["ANTHROPIC_BASE_URL"], "https://gateway.internal",
            "the user's own base URL must come back: {restored}"
        );
    }

    #[test]
    fn a_stranger_at_a_moved_address_is_still_left_alone() {
        // The other side of the same coin: widening what counts as "ours" must
        // not start claiming values we never wrote.
        let _sandbox = Sandbox::new("cc-moved-foreign");
        let path = config_path(&cc()).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = r#"{"env":{"ANTHROPIC_BASE_URL":"https://someone-elses-proxy.test"}}"#;
        fs::write(&path, original).unwrap();

        let outcome = disconnect(&cc(), &config()).unwrap();
        assert!(outcome.already);
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    }

    /// A NAT-mode WSL 2 distro with a relocated config directory — this
    /// machine's real shape, as recorded by the live probe.
    fn seeded_ubuntu() {
        std::env::set_var("ORAMA_WSL", "1");
        let distro = wsl::distro_from_probe(
            "Ubuntu",
            &[
                ("orama.home", "/home/elie"),
                ("orama.kernel", "6.18.33.2-microsoft-standard-WSL2"),
                ("orama.gateway", "172.17.160.1"),
                ("CLAUDE_CONFIG_DIR", "/home/elie/.ai/claude"),
            ]
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        )
        .unwrap();
        wsl::seed_cache(vec![distro]);
    }

    fn gateway() -> std::net::IpAddr {
        "172.17.160.1".parse().unwrap()
    }

    #[test]
    fn a_wsl_harness_is_pointed_at_the_address_it_can_actually_reach() {
        let _sandbox = Sandbox::new("wsl-address");
        seeded_ubuntu();
        let target = Target {
            harness: Harness::ClaudeCode,
            site: Site::Wsl("Ubuntu".into()),
        };

        // Not bridged: refused outright. 127.0.0.1 would parse, write, and read
        // back as connected while every request from the guest went nowhere.
        let err = expected_base_url(&target, &config()).unwrap_err();
        assert!(
            matches!(err, ConnectError::NotBridged { .. }),
            "{err:?} should refuse an address we are not listening on"
        );

        // Bridged: the gateway, not the loopback.
        let bridged = Config {
            extra_hosts: vec![gateway()],
            ..config()
        };
        assert_eq!(
            expected_base_url(&target, &bridged).unwrap(),
            "http://172.17.160.1:8787"
        );
        // Codex keeps its backend prefix across the substitution, or it arrives
        // at a route the upstream does not have.
        let codex = Target {
            harness: Harness::Codex,
            site: Site::Wsl("Ubuntu".into()),
        };
        assert_eq!(
            expected_base_url(&codex, &bridged).unwrap(),
            "http://172.17.160.1:8787/backend-api/codex"
        );

        // A local harness is unaffected by any of it.
        assert_eq!(
            expected_base_url(&cc(), &bridged).unwrap(),
            "http://127.0.0.1:8787"
        );
    }

    #[test]
    fn wsl_rows_report_the_path_inside_the_distro() {
        let _sandbox = Sandbox::new("wsl-rows");
        seeded_ubuntu();
        let bridged = Config {
            extra_hosts: vec![gateway()],
            ..config()
        };

        let all = status_all(&bridged);
        let row = all
            .iter()
            .find(|s| s.id == "claude-code@Ubuntu")
            .expect("a discovered distro contributes a row per harness");

        assert_eq!(row.site, "wsl");
        assert_eq!(row.distro.as_deref(), Some("Ubuntu"));
        // The override the guest exports, translated onto the share — not the
        // default path, and not a Windows-side path.
        assert_eq!(
            row.config_path,
            r"\\wsl.localhost\Ubuntu\home\elie\.ai\claude\settings.json"
        );
        assert_eq!(
            row.expected_base_url.as_deref(),
            Some("http://172.17.160.1:8787")
        );
        // The two facts that make a surprising path and a moving address
        // explicable rather than mysterious.
        let caveat = row.caveat.clone().unwrap_or_default();
        assert!(caveat.contains("reassigned when WSL restarts"), "{caveat}");
        assert!(caveat.contains("CLAUDE_CONFIG_DIR is set"), "{caveat}");

        // The local rows are still there, and still first.
        assert_eq!(all[0].id, "claude-code");
        assert_eq!(all[0].site, "local");
    }

    #[test]
    fn status_reports_where_a_harness_points_when_it_is_not_us() {
        let _sandbox = Sandbox::new("cc-elsewhere");
        let path = config_path(&cc()).unwrap();
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
