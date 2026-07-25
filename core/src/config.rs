//! Runtime configuration for the tracer proxy.

use std::net::{IpAddr, Ipv4Addr};

/// Default upstream Anthropic API base URL.
pub const DEFAULT_UPSTREAM: &str = "https://api.anthropic.com";

/// Default port the proxy listens on.
pub const DEFAULT_PORT: u16 = 8787;

/// Configuration for a running proxy instance.
#[derive(Debug, Clone)]
pub struct Config {
    /// Address to bind the inbound server to.
    pub host: IpAddr,
    /// Port to bind the inbound server to.
    pub port: u16,
    /// Upstream base URL every request is relayed to (no trailing slash).
    pub upstream: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: DEFAULT_PORT,
            upstream: DEFAULT_UPSTREAM.to_string(),
        }
    }
}

impl Config {
    /// Build a config, normalising the upstream URL (strip any trailing slash).
    pub fn new(host: IpAddr, port: u16, upstream: impl Into<String>) -> Self {
        let upstream = upstream.into();
        let upstream = upstream.trim_end_matches('/').to_string();
        Self {
            host,
            port,
            upstream,
        }
    }

    /// The base URL a client (Claude Code) should point `ANTHROPIC_BASE_URL` at.
    pub fn public_base_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }

    /// The shell export snippet the user pastes to route Claude Code through us.
    ///
    /// Touches no files — this is printed to stdout only.
    pub fn export_snippet(&self) -> String {
        format!(
            "export ANTHROPIC_BASE_URL={base}\n\
             export ANTHROPIC_AUTH_TOKEN=<your-anthropic-token>",
            base = self.public_base_url()
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
    fn default_upstream_and_port() {
        let cfg = Config::default();
        assert_eq!(cfg.upstream, DEFAULT_UPSTREAM);
        assert_eq!(cfg.port, DEFAULT_PORT);
    }

    #[test]
    fn export_snippet_mentions_both_vars() {
        let cfg = Config::default();
        let snippet = cfg.export_snippet();
        assert!(snippet.contains("ANTHROPIC_BASE_URL"));
        assert!(snippet.contains("ANTHROPIC_AUTH_TOKEN"));
        assert!(snippet.contains("http://127.0.0.1:8787"));
    }
}
