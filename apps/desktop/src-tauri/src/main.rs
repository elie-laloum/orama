//! Orama as a desktop app.
//!
//! The shell owns a window and a process; it owns no logic. Everything it
//! shows is the same axum server the `orama` CLI runs, bound in-process, and
//! the window is pointed at that server's own `/ui`. Nothing is re-implemented
//! for the desktop: no second copy of the dashboard, no second API client, no
//! IPC bridge. A bug fixed in the browser is fixed here, because it is the
//! same page served over loopback.
//!
//! Two things the CLI never had to answer, a window does.
//!
//! **Where does the database live?** A CLI inherits a working directory the
//! user chose; an app launched from a dock or a Start menu does not. The
//! default therefore resolves to `$ORAMA_HOME` (or `~/.orama`) — the directory
//! the connector already keeps its state in — instead of dropping a database
//! wherever the launcher happened to start.
//!
//! **What happens when the window closes?** Quitting stops the proxy, and any
//! harness still pointed at it would fail its next request with a connection
//! refused. So quitting puts the harness configs back first. That cannot save
//! an agent session already running — both harnesses read their config at
//! startup, so a live session keeps dialling a port that is no longer
//! listening — but it does mean the next session started after quitting talks
//! straight to the provider instead of a dead socket. Harnesses inside WSL are
//! restored the same way, with one more limit that is real: the restore edits a
//! file over the distribution's share, so a distribution shut down before the
//! app quits keeps a config pointing at a proxy that has stopped. The failure is
//! reported on stderr and quitting continues, because refusing to exit over it
//! would strand the window.
//!
//! **Which addresses does it listen on?** Loopback, plus the WSL virtual adapter
//! when a NAT-mode distribution is present — resolved before binding, because
//! that is the only moment a listener can be added. See `connect::wsl` for why a
//! guest cannot use loopback at all, and why the extra bind is one specific
//! address and never `0.0.0.0`.

#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use std::io::ErrorKind;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use orama_core::config::{Config, DEFAULT_PORT};
use orama_core::connect::{self, Target};
use tauri::{Manager, RunEvent, WebviewUrl, WebviewWindowBuilder};

/// Marker state: this process started the proxy, so this process is
/// responsible for putting the harness configs back when it exits.
///
/// Absent when the window attached to an `orama start` that was already
/// running. Disconnecting harnesses on the way out would be wrong there — the
/// server they point at outlives us, and capture would silently stop for a
/// proxy that is still perfectly alive.
struct OwnedServer(Config);

fn main() {
    #[cfg(target_os = "linux")]
    soften_webkit_rendering();

    let context = tauri::generate_context!();

    tauri::Builder::default()
        .setup(|app| {
            open_window(app.handle())?;
            Ok(())
        })
        .build(context)
        .expect("the Tauri context is generated at compile time and must be valid")
        .run(|app, event| {
            // Fires once, after the last window is gone and before the process
            // ends — the only point where the configs are still ours to
            // restore and nothing is left that could rewrite them.
            if let RunEvent::Exit = event {
                if let Some(owned) = app.try_state::<OwnedServer>() {
                    restore_harnesses(&owned.0);
                }
            }
        });
}

/// Bind the proxy and show it, or explain why we could not.
fn open_window(app: &tauri::AppHandle) -> anyhow::Result<()> {
    let config = desktop_config();

    match tauri::async_runtime::block_on(orama_core::bind(config.clone())) {
        Ok(bound) => {
            let url = format!("http://{}/ui/", bound.addr());
            app.manage(OwnedServer(bound.config().clone()));

            if !bound.capturing() {
                // Relaying is the job that cannot be dropped, so a database
                // that will not open degrades to pass-through rather than
                // refusing to start. Say so on stderr; the dashboard reports
                // it too, and neither should pretend capture is happening.
                eprintln!(
                    "orama: capture is off — {} could not be opened",
                    config.db_path.display()
                );
            }

            tauri::async_runtime::spawn(async move {
                if let Err(err) = bound.run().await {
                    eprintln!("orama: the proxy stopped: {err}");
                }
            });

            window(app, WebviewUrl::External(url.parse()?), None)
        }

        // The port is taken. If it is taken by another Orama, showing that one
        // is better than refusing to open: the user asked to see their
        // dashboard, and there it is. We just do not own it, so we will not
        // disconnect anything on the way out.
        Err(err) if is_addr_in_use(&err) => {
            let addr = SocketAddr::new(config.host, config.port);
            if is_orama(addr) {
                window(
                    app,
                    WebviewUrl::External(format!("http://{addr}/ui/").parse()?),
                    None,
                )
            } else {
                window(
                    app,
                    WebviewUrl::App(PathBuf::from("index.html")),
                    Some(&format!(
                    "Port {} is already in use by another program. Close it, or start Orama on a \
                     different port with ORAMA_PORT.",
                    config.port
                )),
                )
            }
        }

        Err(err) => window(
            app,
            WebviewUrl::App(PathBuf::from("index.html")),
            Some(&format!("The proxy could not start: {err}")),
        ),
    }
}

/// Create the one window, either on the live dashboard or on the bundled page
/// that explains why there is no dashboard to show.
fn window(app: &tauri::AppHandle, url: WebviewUrl, error: Option<&str>) -> anyhow::Result<()> {
    let mut builder = WebviewWindowBuilder::new(app, "main", url)
        .title("Orama")
        .inner_size(1440.0, 900.0)
        .min_inner_size(960.0, 600.0)
        .center();

    if let Some(message) = error {
        // Injected before the document runs so the page never flashes a
        // "starting…" state it will not leave.
        builder = builder.initialization_script(format!(
            "window.__ORAMA_ERROR__ = {};",
            json_string(message)
        ));
    }

    builder.build()?;
    Ok(())
}

/// The configuration a windowed process should run with.
///
/// Environment overrides exist so the desktop build is never strictly less
/// capable than the CLI — pointing it at a copy of a capture, or at an
/// OpenAI-compatible host, should not require rebuilding it.
fn desktop_config() -> Config {
    let port = std::env::var("ORAMA_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_PORT);

    let mut config = Config {
        port,
        db_path: database_path(),
        ..Config::default()
    };

    if let Some(upstream) = env_value("ORAMA_UPSTREAM") {
        config = config.with_upstream(upstream);
    }
    if let Some(upstream) = env_value("ORAMA_UPSTREAM_OPENAI") {
        config = config.with_openai_upstream(upstream);
    }

    // A harness inside WSL cannot reach a listener bound only to the Windows
    // loopback — so on a machine with a NAT-mode distro, also listen on the
    // virtual adapter it routes through. Resolved before binding because that is
    // the only moment a listener can be added, and empty everywhere else: off
    // Windows, under mirrored networking, and on WSL 1, loopback already works.
    let bridges = connect::wsl::bridge_hosts();
    if !bridges.is_empty() {
        eprintln!(
            "orama: bridging to WSL on {}",
            bridges
                .iter()
                .map(|host| host.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        config = config.with_extra_hosts(bridges);
    }
    config
}

/// Where the capture database lives for a windowed process.
///
/// Falls back to the CLI's relative default only if no home directory can be
/// resolved at all, which is the one case where there is nowhere better.
fn database_path() -> PathBuf {
    if let Some(explicit) = env_value("ORAMA_DB") {
        return PathBuf::from(explicit);
    }
    match connect::orama_home() {
        Ok(dir) => {
            // spawn_writer opens the file, not the directory; without this the
            // first launch on a clean machine fails with "unable to open
            // database file" and no hint that a parent is missing.
            if let Err(err) = std::fs::create_dir_all(&dir) {
                eprintln!("orama: could not create {}: {err}", dir.display());
            }
            dir.join(orama_core::config::DEFAULT_DB)
        }
        Err(err) => {
            eprintln!("orama: {err} — falling back to the working directory");
            PathBuf::from(orama_core::config::DEFAULT_DB)
        }
    }
}

/// Put back every harness config that points at the proxy we are about to
/// stop.
///
/// `disconnect` only touches a value it wrote itself, so a base URL the user
/// set by hand survives untouched. Failures are reported and then swallowed:
/// this runs during exit, and refusing to quit over it would strand the
/// window.
fn restore_harnesses(config: &Config) {
    for status in connect::status_all(config) {
        if !status.connected {
            continue;
        }
        let Some(target) = Target::parse(&status.id) else {
            continue;
        };
        match connect::disconnect(&target, config) {
            Ok(_) => eprintln!("orama: disconnected {} on exit", status.label),
            Err(err) => eprintln!(
                "orama: could not restore {} ({}): {err}",
                status.label, status.config_path
            ),
        }
    }
}

/// Whether a bind failure was "someone already has this port".
fn is_addr_in_use(err: &anyhow::Error) -> bool {
    err.downcast_ref::<std::io::Error>()
        .is_some_and(|err| err.kind() == ErrorKind::AddrInUse)
}

/// Whether the thing holding the port is an Orama proxy.
///
/// Hand-rolled rather than pulling in an HTTP client: this is one request to
/// loopback with a fixed shape, and the answer only decides which URL the
/// window opens. Anything unexpected — a timeout, a stranger, a refusal —
/// reads as "not Orama", which is the safe direction to be wrong in.
fn is_orama(addr: SocketAddr) -> bool {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));

    let request = format!("GET /healthz HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }

    let mut response = String::new();
    // Cap the read: a stranger on this port could stream forever, and we only
    // need enough bytes to recognise a very short reply.
    let mut buffer = [0u8; 512];
    while let Ok(read) = stream.read(&mut buffer) {
        if read == 0 {
            break;
        }
        response.push_str(&String::from_utf8_lossy(&buffer[..read]));
        if response.len() > 4096 {
            break;
        }
    }

    response.starts_with("HTTP/1.1 200") && response.trim_end().ends_with("ok")
}

fn env_value(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.is_empty())
}

/// Minimal JSON string literal, so injecting a message cannot break the script
/// it is injected into.
fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// WebKitGTK's DMA-BUF renderer needs a GPU path that WSLg does not provide,
/// and without this the window comes up blank — no error, just white.
///
/// Only under WSL, and only when the user has not already chosen: a normal
/// Linux desktop should keep the accelerated path.
#[cfg(target_os = "linux")]
fn soften_webkit_rendering() {
    let under_wsl = std::env::var_os("WSL_DISTRO_NAME").is_some()
        || std::fs::read_to_string("/proc/version")
            .map(|version| version.to_lowercase().contains("microsoft"))
            .unwrap_or(false);

    if under_wsl && std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Repoints every path the connector resolves at a temp directory, so a
    /// run can never reach the real `~/.claude` of whoever is running the
    /// suite. Asserted rather than assumed: quitting is the one moment this
    /// binary writes to files it did not create.
    struct Sandbox(PathBuf);

    impl Sandbox {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("orama-desktop-{name}"));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("claude")).unwrap();
            std::env::set_var("CLAUDE_CONFIG_DIR", dir.join("claude"));
            std::env::set_var("CODEX_HOME", dir.join("codex"));
            std::env::set_var("ORAMA_HOME", dir.join("orama"));
            // No distro discovery: a test must not spawn wsl.exe, and on a
            // Windows dev machine it otherwise would.
            std::env::set_var("ORAMA_WSL", "0");
            Self(dir)
        }

        fn claude_settings(&self) -> PathBuf {
            self.0.join("claude").join("settings.json")
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            std::env::remove_var("CLAUDE_CONFIG_DIR");
            std::env::remove_var("CODEX_HOME");
            std::env::remove_var("ORAMA_HOME");
            std::env::remove_var("ORAMA_WSL");
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Quitting has to leave the harness the way it found it, including a base
    /// URL that was already there. Restoring to a default instead of to the
    /// previous value would silently repoint someone's gateway at Anthropic.
    #[test]
    fn quitting_restores_what_was_there_before() {
        let sandbox = Sandbox::new("restores");
        let config = Config::default();

        std::fs::write(
            sandbox.claude_settings(),
            r#"{"env":{"ANTHROPIC_BASE_URL":"https://gateway.example.invalid"},"theme":"dark"}"#,
        )
        .unwrap();

        connect::connect(
            &Target::local(orama_core::connect::Harness::ClaudeCode),
            &config,
        )
        .unwrap();
        let connected = std::fs::read_to_string(sandbox.claude_settings()).unwrap();
        assert!(
            connected.contains(&config.public_base_url()),
            "connect should point the harness at us: {connected}"
        );

        restore_harnesses(&config);

        let restored = std::fs::read_to_string(sandbox.claude_settings()).unwrap();
        assert!(
            restored.contains("https://gateway.example.invalid"),
            "the previous base URL must come back, not a default: {restored}"
        );
        assert!(
            !restored.contains(&config.public_base_url()),
            "nothing should still point at the proxy we are about to stop: {restored}"
        );
        assert!(
            restored.contains("\"theme\""),
            "unrelated settings must survive: {restored}"
        );
    }

    /// A harness pointed somewhere by hand is not ours to put back. Quitting
    /// must not treat "not connected to us" as "needs fixing".
    #[test]
    fn quitting_leaves_a_harness_we_never_touched_alone() {
        let sandbox = Sandbox::new("untouched");
        let config = Config::default();

        let original = r#"{"env":{"ANTHROPIC_BASE_URL":"https://someone-elses-proxy.invalid"}}"#;
        std::fs::write(sandbox.claude_settings(), original).unwrap();

        restore_harnesses(&config);

        assert_eq!(
            std::fs::read_to_string(sandbox.claude_settings()).unwrap(),
            original,
            "a config we did not write must be byte-identical after a quit"
        );
    }
}
