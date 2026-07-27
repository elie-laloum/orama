//! Runtime configuration for the Orama proxy.

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

/// Default upstream Anthropic API base URL.
pub const DEFAULT_UPSTREAM: &str = "https://api.anthropic.com";

/// Default upstream for OpenAI-dialect traffic — Codex, opencode, and anything
/// else speaking Chat Completions or Responses.
///
/// Separate from [`DEFAULT_UPSTREAM`] because one listener serves both dialects:
/// the relay picks a destination per request rather than per process, so a
/// single `orama start` can trace Claude Code and Codex at the same time.
pub const DEFAULT_UPSTREAM_OPENAI: &str = "https://api.openai.com";

/// Default upstream for Codex signed in through a ChatGPT subscription.
///
/// A third destination because it is genuinely a third backend: subscription
/// auth does not talk to `api.openai.com` at all, and its routes live under
/// `/backend-api/codex` rather than `/v1`. Traffic reaches it by path prefix,
/// so no flag has to be set for the common case.
pub const DEFAULT_UPSTREAM_CHATGPT: &str = "https://chatgpt.com";

/// Path prefix that marks the ChatGPT Codex backend.
///
/// Codex appends its route to the configured base, so pointing it at
/// `http://127.0.0.1:8787/backend-api/codex` makes the request arrive here
/// carrying its own destination — the relay swaps the origin and the upstream
/// sees the exact path it expects.
pub const CHATGPT_PREFIX: &str = "/backend-api/";

/// Default port the proxy listens on.
pub const DEFAULT_PORT: u16 = 8787;

/// Default SQLite database filename.
pub const DEFAULT_DB: &str = "orama.sqlite";

/// Configuration for a running proxy instance.
#[derive(Debug, Clone)]
pub struct Config {
    /// Address to bind the inbound server to.
    pub host: IpAddr,
    /// Port to bind the inbound server to.
    pub port: u16,
    /// Where Anthropic-dialect requests are relayed (no trailing slash).
    pub upstream: String,
    /// Where OpenAI-dialect requests are relayed (no trailing slash).
    ///
    /// Point this at any OpenAI-compatible host to trace a third-party
    /// provider; the wire format is what the parsers key off, not the vendor.
    pub upstream_openai: String,
    /// Where `/backend-api/*` requests go — Codex on a ChatGPT subscription.
    pub upstream_chatgpt: String,
    /// Path to the SQLite database file capture is written to.
    pub db_path: PathBuf,
    /// Extra addresses to listen on, beyond [`Config::host`].
    ///
    /// Exists for one reason: a harness inside WSL cannot reach a listener bound
    /// only to the Windows loopback, so bridging to it means also listening on
    /// the virtual adapter the guest routes through. Deliberately a list of
    /// specific addresses rather than a "bind everything" flag — the dashboard
    /// is served by this same listener, so `0.0.0.0` would publish every
    /// captured prompt and response to the local network unauthenticated.
    pub extra_hosts: Vec<IpAddr>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: DEFAULT_PORT,
            upstream: DEFAULT_UPSTREAM.to_string(),
            upstream_openai: DEFAULT_UPSTREAM_OPENAI.to_string(),
            upstream_chatgpt: DEFAULT_UPSTREAM_CHATGPT.to_string(),
            db_path: PathBuf::from(DEFAULT_DB),
            extra_hosts: Vec::new(),
        }
    }
}

/// Strip any trailing slash so joining a path never produces a double slash.
fn normalize(url: impl Into<String>) -> String {
    url.into().trim_end_matches('/').to_string()
}

impl Config {
    /// Build a config, normalising the upstream URL (strip any trailing slash).
    pub fn new(host: IpAddr, port: u16, upstream: impl Into<String>) -> Self {
        Self {
            host,
            port,
            upstream: normalize(upstream),
            ..Self::default()
        }
    }

    /// Override the SQLite database path (builder style).
    pub fn with_db_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.db_path = path.into();
        self
    }

    /// Override where Anthropic-dialect traffic is relayed (builder style).
    ///
    /// The sibling of [`Config::new`] for callers that start from
    /// [`Config::default`] and only want to move one upstream — a windowed
    /// process has no argv to build a config from.
    pub fn with_upstream(mut self, upstream: impl Into<String>) -> Self {
        self.upstream = normalize(upstream);
        self
    }

    /// Override where OpenAI-dialect traffic is relayed (builder style).
    pub fn with_openai_upstream(mut self, upstream: impl Into<String>) -> Self {
        self.upstream_openai = normalize(upstream);
        self
    }

    /// Override where ChatGPT-backend traffic is relayed (builder style).
    pub fn with_chatgpt_upstream(mut self, upstream: impl Into<String>) -> Self {
        self.upstream_chatgpt = normalize(upstream);
        self
    }

    /// Override the addresses to listen on beyond [`Config::host`].
    pub fn with_extra_hosts(mut self, hosts: Vec<IpAddr>) -> Self {
        self.extra_hosts = hosts;
        self
    }

    /// The base URL a client should be pointed at.
    pub fn public_base_url(&self) -> String {
        self.base_url_on(&self.host.to_string())
    }

    /// The same, for a client that reaches this proxy on some other address.
    ///
    /// Parameterised because "where are you" has more than one answer once a
    /// client can be on the far side of a WSL boundary: the address a harness
    /// inside a distro must use is not the one the dashboard is opened on, and
    /// writing the wrong one produces a config that fails every request.
    pub fn base_url_on(&self, host: &str) -> String {
        format!("http://{host}:{}", self.port)
    }

    /// The base URL an OpenAI-dialect client should be pointed at.
    ///
    /// OpenAI SDKs append paths to the base without a version segment, so the
    /// `/v1` belongs here — the relay forwards the path verbatim and the
    /// upstream sees exactly the route the client asked for.
    pub fn openai_base_url(&self) -> String {
        self.openai_base_url_on(&self.host.to_string())
    }

    pub fn openai_base_url_on(&self, host: &str) -> String {
        format!("{}/v1", self.base_url_on(host))
    }

    /// The base URL Codex should be pointed at when it authenticates through a
    /// ChatGPT subscription.
    ///
    /// Carries the upstream's own path prefix so the request arrives here
    /// self-describing: the relay only has to swap the origin.
    pub fn chatgpt_base_url(&self) -> String {
        self.chatgpt_base_url_on(&self.host.to_string())
    }

    pub fn chatgpt_base_url_on(&self, host: &str) -> String {
        format!("{}{CHATGPT_PREFIX}codex", self.base_url_on(host))
    }

    /// The shell export snippet the user pastes to route a harness through us.
    ///
    /// Both dialects are listed because one listener serves both: exporting
    /// only the Anthropic pair would leave Codex talking straight to its
    /// provider with nothing captured. Touches no files — printed to stdout
    /// only; the settings page writes config for real.
    pub fn export_snippet(&self) -> String {
        format!(
            "# Claude Code / Anthropic SDK\n\
             export ANTHROPIC_BASE_URL={base}\n\
             \n\
             # Codex / OpenAI SDK\n\
             export OPENAI_BASE_URL={openai}\n\
             \n\
             # Existing credentials pass through untouched; auth headers are\n\
             # redacted before anything is written to disk.",
            base = self.public_base_url(),
            openai = self.openai_base_url(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_trailing_slash_is_stripped() {
        let cfg = Config::new(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            9000,
            "https://api.anthropic.com/",
        );
        assert_eq!(cfg.upstream, "https://api.anthropic.com");
    }

    #[test]
    fn openai_upstream_trailing_slash_is_stripped() {
        let cfg = Config::default().with_openai_upstream("https://openrouter.ai/api/");
        assert_eq!(cfg.upstream_openai, "https://openrouter.ai/api");
    }

    #[test]
    fn default_upstreams_and_port() {
        let cfg = Config::default();
        assert_eq!(cfg.upstream, DEFAULT_UPSTREAM);
        assert_eq!(cfg.upstream_openai, DEFAULT_UPSTREAM_OPENAI);
        assert_eq!(cfg.port, DEFAULT_PORT);
    }

    #[test]
    fn new_keeps_the_openai_default() {
        // Setting the Anthropic upstream must not silently drop the other one,
        // or OpenAI traffic would be relayed to Anthropic.
        let cfg = Config::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1, "https://example.test");
        assert_eq!(cfg.upstream_openai, DEFAULT_UPSTREAM_OPENAI);
    }

    #[test]
    fn openai_base_url_carries_the_version_segment() {
        let cfg = Config::default();
        assert_eq!(cfg.openai_base_url(), "http://127.0.0.1:8787/v1");
    }

    #[test]
    fn export_snippet_covers_both_dialects() {
        let cfg = Config::default();
        let snippet = cfg.export_snippet();
        assert!(snippet.contains("ANTHROPIC_BASE_URL=http://127.0.0.1:8787"));
        assert!(snippet.contains("OPENAI_BASE_URL=http://127.0.0.1:8787/v1"));
    }
}
