//! `orama` — CLI wrapper around `orama-core`.

use std::net::{IpAddr, Ipv4Addr};

use clap::{Parser, Subcommand};
use orama_core::{
    config::{DEFAULT_PORT, DEFAULT_UPSTREAM, DEFAULT_UPSTREAM_CHATGPT, DEFAULT_UPSTREAM_OPENAI},
    Config,
};

#[derive(Parser)]
#[command(
    name = "orama",
    version,
    about = "Transparent tracing proxy for Claude Code API traffic"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the tracing proxy and print the export snippet.
    Start {
        /// Port to listen on.
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,

        /// Host/interface to bind to.
        #[arg(long, default_value = "127.0.0.1")]
        host: IpAddr,

        /// Upstream Anthropic-compatible API base URL.
        #[arg(long, default_value = DEFAULT_UPSTREAM)]
        upstream: String,

        /// Upstream for OpenAI-dialect traffic — Codex, opencode, or any
        /// OpenAI-compatible host such as OpenRouter or a local vLLM.
        ///
        /// One listener serves both dialects: the relay routes each request by
        /// its wire format, so Claude Code and Codex can be traced at once.
        #[arg(long, default_value = DEFAULT_UPSTREAM_OPENAI)]
        upstream_openai: String,

        /// Upstream for `/backend-api/*` — Codex signed in through a ChatGPT
        /// subscription, which is a different backend from api.openai.com.
        #[arg(long, default_value = DEFAULT_UPSTREAM_CHATGPT)]
        upstream_chatgpt: String,

        /// Path to the SQLite capture database.
        #[arg(long, default_value = "orama.sqlite")]
        db: std::path::PathBuf,
    },

    /// Rebuild the derived analytics tables from the raw captures.
    ///
    /// Raw captures are never modified. Use this after upgrading, or with
    /// `--rebuild` to force a full re-derivation.
    Derive {
        /// Path to the SQLite capture database.
        #[arg(long, default_value = "orama.sqlite")]
        db: std::path::PathBuf,

        /// Discard existing derived rows before deriving.
        #[arg(long)]
        rebuild: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Start {
            port,
            host,
            upstream,
            upstream_openai,
            upstream_chatgpt,
            db,
        } => {
            let config = Config::new(host, port, upstream)
                .with_openai_upstream(upstream_openai)
                .with_chatgpt_upstream(upstream_chatgpt)
                .with_db_path(db);
            orama_core::serve(config).await?;
        }
        Command::Derive { db, rebuild } => {
            let report = orama_core::derive::write::run_backfill(&db, rebuild)?;
            println!(
                "derived {} call(s), {} failed, parser {}",
                report.derived,
                report.failed,
                orama_core::derive::PARSER_VERSION
            );
            if report.failed > 0 {
                eprintln!("orama: see the derive_failures table for details");
            }
        }
    }
    Ok(())
}

// Silence unused-import warning if defaults change; keep localhost handy.
#[allow(dead_code)]
const _LOCALHOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
