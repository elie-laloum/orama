//! Reaching a harness that lives inside WSL from a proxy running on Windows.
//!
//! An earlier version of [`super`] deliberately refused to cross this boundary,
//! and the reason it gave was sound: writing a Windows-side `127.0.0.1` into a
//! config the guest reads would point the harness at an address that does not
//! resolve to the proxy. What was missing was the other half — the address that
//! *does*. With it, crossing is safe, and refusing to cross means a Windows app
//! offers to configure a harness the user does not run while the one they do
//! run stays invisible.
//!
//! Three facts decide everything here, and all three were measured on a live
//! machine rather than inferred:
//!
//! - **A guest cannot reach a loopback-bound Windows listener.** Under the
//!   default NAT networking, a Windows server bound to `127.0.0.1` refuses the
//!   guest's connection; the same server bound so the vEthernet address is
//!   included answers it. So the base URL written into a guest config is the
//!   gateway of the guest's default route — which is the Windows side of that
//!   virtual adapter — and the proxy has to be listening there too.
//! - **The guest's own `127.0.0.1` works only in mirrored mode.** With
//!   `networkingMode=mirrored`, Windows' interfaces are mirrored into the guest
//!   and loopback reaches Windows, so nothing needs binding or rewriting. WSL 1
//!   shares the host's network stack outright and behaves the same way.
//! - **A harness's config directory cannot be guessed from the path.**
//!   `CLAUDE_CONFIG_DIR` and `CODEX_HOME` move it, and they are typically
//!   exported from an *interactive* shell rc. `sh -lc` and even `zsh -lc` do
//!   not read one; `zsh -lic` does, but only with a tty, which a windowed
//!   Windows process does not hand its children. Hence the pty in [`PROBE`] —
//!   without it the probe reports "not set" for a variable that is set, and we
//!   would write to a file the harness never opens.
//!
//! Everything in this module that makes a decision is a pure function over
//! bytes or text, and is unit-tested on any platform. Only the four small
//! wrappers that actually spawn `wsl.exe` are Windows-only, and they are
//! Windows-only at *runtime* via `cfg!` rather than compiled away, so the whole
//! module keeps type-checking on the machines it does not run on.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::Harness;

/// Where Windows sees a distro's filesystem.
///
/// `\\wsl$` is the older alias for the same share and still resolves. Only one
/// spelling is ever used, so a path we record and a path we later compare are
/// the same string.
const UNC_ROOT: &str = r"\\wsl.localhost";

/// How long a discovery result stands before it is taken again.
///
/// The expensive part of discovery is the guest probe, which can start a
/// stopped distro. Config directories are effectively immutable, so a couple of
/// minutes of staleness costs nothing — with one exception that matters: under
/// NAT the gateway address moves when WSL restarts. That is why [`connect`]
/// re-probes rather than trusting this cache; being current matters most at the
/// instant we write an address down.
///
/// [`connect`]: super::connect
const TTL: Duration = Duration::from_secs(120);

/// How long any single `wsl.exe` call is given before it is abandoned.
///
/// `std::process` has no timeout, and a hung child would hang the settings
/// endpoint behind it for as long as it hung. Discovery is a convenience; the
/// proxy relaying traffic is not, and one must never be able to stall the other.
const DEADLINE: Duration = Duration::from_secs(20);

/// What the guest is asked about itself, in one round trip.
///
/// Written as a script rather than a series of calls because each call can
/// start the VM. The `orama.` prefix separates our answers from whatever an
/// interactive rc decides to print — a prompt, a banner, a version-manager
/// notice — which is also why the parser ignores anything it does not
/// recognise instead of trusting line positions.
///
/// The pty is the load-bearing part. `script -qec` allocates one, which is what
/// makes the login shell genuinely interactive and therefore what makes it read
/// the rc file that exports `CLAUDE_CONFIG_DIR`. Where `script` is missing the
/// fallback sources the rc directly: less faithful, since an rc written for an
/// interactive shell may not survive being sourced by a non-interactive one, but
/// better than reporting a variable as unset because nothing ever read it.
///
/// `awk` over `/etc/passwd` rather than `getent`, because `getent passwd` was
/// measured returning nothing under `wsl.exe --exec` on a distro whose
/// `/etc/passwd` plainly contains the user.
const PROBE: &str = r#"
SH=$(awk -F: -v u="$(id -u)" '$3==u{print $7; exit}' /etc/passwd)
[ -x "$SH" ] || SH=/bin/sh
printf 'orama.home=%s\n' "$HOME"
printf 'orama.shell=%s\n' "$SH"
printf 'orama.gateway=%s\n' "$(ip route show default 2>/dev/null | awk '{print $3; exit}')"
printf 'orama.kernel=%s\n' "$(cat /proc/sys/kernel/osrelease 2>/dev/null)"
if command -v script >/dev/null 2>&1; then
  script -qec "$SH -lic printenv" /dev/null 2>/dev/null
else
  case "$SH" in
    *zsh) "$SH" -c '[ -f "$HOME/.zshrc" ] && . "$HOME/.zshrc" >/dev/null 2>&1; printenv' 2>/dev/null ;;
    *bash) "$SH" -c '[ -f "$HOME/.bashrc" ] && . "$HOME/.bashrc" >/dev/null 2>&1; printenv' 2>/dev/null ;;
    *) "$SH" -lc printenv 2>/dev/null ;;
  esac
fi
"#;

/// The networking mode in force for the WSL VM.
///
/// Not a detail: it decides the only address a guest can reach the proxy on, and
/// whether the proxy has to bind anything beyond loopback to be reachable at
/// all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Networking {
    /// The default. The guest sits behind a NAT and reaches Windows at its
    /// default gateway. Its `127.0.0.1` is its own.
    Nat,
    /// `networkingMode=mirrored` in `.wslconfig`, on Windows 11 22H2 and up.
    /// Windows' interfaces are mirrored into the guest, so `127.0.0.1` reaches
    /// a Windows server directly.
    Mirrored,
}

/// One WSL distribution, as far as configuring a harness inside it requires.
#[derive(Debug, Clone, Serialize)]
pub struct Distro {
    pub name: String,
    /// `$HOME` inside the guest, as a guest path.
    pub home: String,
    /// The guest's default-route gateway — the Windows side of the virtual
    /// adapter, and so the address the guest reaches this proxy on under NAT.
    /// `None` when the probe could not determine it, which is reported rather
    /// than guessed around.
    pub gateway: Option<IpAddr>,
    /// `/proc/sys/kernel/osrelease`, the one place that distinguishes a WSL 2
    /// guest from a WSL 1 one.
    pub kernel: String,
    /// The login shell the probe used, kept because it explains the answer:
    /// which rc file was read is the difference between finding an override and
    /// missing it.
    pub shell: String,
    /// The guest environment as a login+interactive shell presents it. Holds
    /// the whole of `printenv` rather than the two keys we read today, so the
    /// next harness with a relocatable config costs no second round trip.
    #[serde(skip)]
    env: BTreeMap<String, String>,
}

impl Distro {
    /// Whether this is a WSL 2 guest, which is the case that has its own
    /// network namespace and therefore its own loopback.
    ///
    /// A WSL 1 distro shares the host's network stack, so `127.0.0.1` inside it
    /// already is the Windows loopback and no bridging applies.
    pub fn is_wsl2(&self) -> bool {
        self.kernel.to_ascii_uppercase().contains("WSL2")
    }

    /// The address a harness inside this distro must be pointed at to reach a
    /// proxy running on Windows.
    ///
    /// `None` means we could not work one out — under NAT with no gateway
    /// discovered there is no address that would work, and inventing one would
    /// produce a config that fails every request.
    pub fn host_address(&self, networking: Networking) -> Option<IpAddr> {
        if !self.is_wsl2() || networking == Networking::Mirrored {
            // Loopback genuinely reaches Windows in both of these.
            return Some(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
        }
        self.gateway
    }

    /// Where this harness keeps its config, as a guest path.
    ///
    /// The environment override wins, because it is what the harness itself
    /// honours. Falling back to the default directory under a `$HOME` we read
    /// from the guest rather than assembling `/home/<name>` keeps a distro whose
    /// user has an unusual home working.
    pub fn guest_config_dir(&self, harness: Harness) -> String {
        if let Some(dir) = self
            .env
            .get(harness.config_dir_var())
            .filter(|value| !value.is_empty())
        {
            return dir.clone();
        }
        format!(
            "{}/{}",
            self.home.trim_end_matches('/'),
            harness.config_leaf()
        )
    }

    /// One variable out of the guest's environment.
    ///
    /// The guest's, specifically — reading this process's would answer a
    /// question about Windows when the question was about the distro.
    pub fn env_var(&self, key: &str) -> Option<&str> {
        self.env.get(key).map(String::as_str)
    }

    /// Whether the config directory came from an environment override, which is
    /// worth saying out loud: it is the fact that makes the path surprising.
    pub fn config_dir_is_overridden(&self, harness: Harness) -> bool {
        self.env
            .get(harness.config_dir_var())
            .is_some_and(|value| !value.is_empty())
    }

    /// The path a Windows process opens to reach that config directory.
    pub fn windows_config_dir(&self, harness: Harness) -> PathBuf {
        guest_to_windows(&self.name, &self.guest_config_dir(harness))
    }

    /// A file inside the harness's config directory, as Windows must open it.
    ///
    /// Built here rather than by joining onto [`Self::windows_config_dir`] for
    /// the same reason that path is assembled as a string: `join` would append
    /// the host platform's separator, and on a Linux host that produces a path
    /// that is neither valid Windows nor valid anything.
    pub fn windows_config_sibling(&self, harness: Harness, file: &str) -> PathBuf {
        guest_to_windows(
            &self.name,
            &format!(
                "{}/{file}",
                self.guest_config_dir(harness).trim_end_matches('/')
            ),
        )
    }

    /// The config file this connector edits, as Windows must open it.
    pub fn windows_config_file(&self, harness: Harness) -> PathBuf {
        self.windows_config_sibling(harness, harness.config_file())
    }
}

/* ── pure translation and parsing ─────────────────────────────────────────── */

/// Translate an absolute guest path into the path a Windows process must open.
///
/// Two cases, and getting the second one wrong edits a file nobody reads.
/// `/mnt/c/...` is a Windows drive mounted into the guest, so it maps back to
/// `C:\...` — reaching it through the share would go Windows → 9p → Windows for
/// a file that was local all along. Everything else genuinely lives inside the
/// distro and is reached over `\\wsl.localhost\<distro>`.
///
/// Assembled as a string with explicit separators rather than with
/// `PathBuf::push`, which inserts the separator of whichever platform is
/// *running* — on a Linux host that yields `\\wsl.localhost\Ubuntu/home/elie`.
/// Windows accepts either separator, so being explicit costs nothing there and
/// is what lets this be tested anywhere.
pub fn guest_to_windows(distro: &str, guest: &str) -> PathBuf {
    let parts: Vec<&str> = guest
        .trim_start_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();

    // A drive mount: /mnt/<letter>/rest. The letter must be exactly one ASCII
    // character, or `/mnt/wsl/...` and `/mnt/storage/...` would be mistaken for
    // drives and rewritten into nonsense.
    if parts.len() >= 2 && parts[0] == "mnt" && parts[1].len() == 1 {
        let letter = parts[1].chars().next().expect("length checked");
        if letter.is_ascii_alphabetic() {
            return PathBuf::from(format!(
                "{}:\\{}",
                letter.to_ascii_uppercase(),
                parts[2..].join("\\")
            ));
        }
    }

    PathBuf::from(format!(r"{UNC_ROOT}\{distro}\{}", parts.join("\\")))
}

/// Decode what `wsl.exe` prints about itself.
///
/// Its own output is UTF-16LE — measured, not assumed: `wsl -l -q` emits
/// `U\0b\0u\0n\0t\0u\0\r\0\n\0`. The stdout of a command *run inside* a distro
/// is the guest process's own bytes and stays UTF-8, so only listings come
/// through here. A stray odd trailing byte is dropped rather than failing the
/// decode; losing one character beats losing the distro list.
pub fn decode_utf16le(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// Distro names out of `wsl --list --quiet`.
///
/// `--quiet` rather than `--verbose` on purpose: the verbose table's header is
/// localised — this was found on a machine whose `wsl --version` prints
/// "Version WSL" — so parsing it means parsing the user's display language.
/// The quiet form is one name per line in any locale.
pub fn parse_distro_list(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| line.trim().trim_end_matches('\0').trim())
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Split the probe's output into the answers we asked for.
///
/// Deliberately tolerant. An interactive rc can print anything at all before
/// our lines, and does; anything that is not `KEY=VALUE` is skipped rather than
/// allowed to shift the meaning of the lines around it. Keys are taken
/// first-wins so that `orama.home`, printed before the shell rc runs, cannot be
/// overwritten by a later duplicate.
pub fn parse_probe(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim_end_matches(['\r', '\n']);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        // A key with whitespace or shell punctuation in it is prompt noise that
        // happens to contain an `=`, not an environment variable.
        if key.is_empty()
            || !key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
        {
            continue;
        }
        out.entry(key.to_owned())
            .or_insert_with(|| value.to_owned());
    }
    out
}

/// Read `networkingMode` out of a `.wslconfig`.
///
/// INI-shaped, and the key only counts inside `[wsl2]` — the same name under
/// another section heading is not this setting. Anything unrecognised, absent,
/// or unparseable means the documented default, which is NAT. Being wrong in
/// that direction is the safe one: NAT is the mode that needs an explicit
/// address, so assuming it produces a config that works under mirrored mode
/// too, while assuming mirrored produces one that works under neither.
pub fn parse_networking(text: &str) -> Networking {
    let mut in_wsl2 = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(section) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            in_wsl2 = section.trim().eq_ignore_ascii_case("wsl2");
            continue;
        }
        if !in_wsl2 {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            if key.trim().eq_ignore_ascii_case("networkingMode")
                && value.trim().eq_ignore_ascii_case("mirrored")
            {
                return Networking::Mirrored;
            }
        }
    }
    Networking::Nat
}

/// Build a [`Distro`] from a name and the probe's answers.
///
/// Separated from the spawning so the shape of a probe result can be tested
/// without a WSL install. Returns `None` when the probe told us nothing usable:
/// without `$HOME` there is no path to fall back to, and a distro we cannot
/// locate a config in is better omitted than listed with a path we invented.
pub fn distro_from_probe(name: &str, answers: &BTreeMap<String, String>) -> Option<Distro> {
    let home = answers
        .get("orama.home")
        .or_else(|| answers.get("HOME"))
        .filter(|value| !value.is_empty())?
        .clone();

    let env = answers
        .iter()
        .filter(|(key, _)| !key.starts_with("orama."))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();

    Some(Distro {
        name: name.to_owned(),
        home,
        gateway: answers
            .get("orama.gateway")
            .and_then(|value| value.trim().parse().ok()),
        kernel: answers.get("orama.kernel").cloned().unwrap_or_default(),
        shell: answers.get("orama.shell").cloned().unwrap_or_default(),
        env,
    })
}

/* ── discovery ────────────────────────────────────────────────────────────── */

/// Whether to look for distros at all.
///
/// Off on anything but Windows by default, because a WSL-side install needs none
/// of this — `HOME` there already is the guest home, and the proxy is already
/// inside the same network namespace as the harness.
///
/// `ORAMA_WSL` overrides in both directions. `0` is an escape hatch for a
/// machine where probing misbehaves, and is what keeps the connector tests from
/// spawning `wsl.exe`. `1` forces discovery on where it would be off, which
/// exists so the probe can be exercised against a real WSL install from inside
/// one — the paths it yields are Windows paths and are useless to a Linux
/// process, so this is a diagnostic, never a way to connect anything.
fn enabled() -> bool {
    match std::env::var("ORAMA_WSL").ok().as_deref() {
        Some("0") => false,
        Some("1") => true,
        _ => cfg!(windows),
    }
}

/// Run a command with a deadline, returning its stdout.
///
/// The work happens on a thread so a child that never exits cannot hold the
/// caller. When the deadline passes the thread is abandoned rather than killed:
/// it is blocked in `wait`, it owns nothing the rest of the process needs, and
/// leaving it is preferable to the alternative of hanging every settings request
/// behind it.
fn run_with_deadline(args: Vec<String>) -> Option<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let output = std::process::Command::new("wsl.exe").args(&args).output();
        let _ = tx.send(output);
    });

    match rx.recv_timeout(DEADLINE) {
        Ok(Ok(output)) if output.status.success() => Some(output.stdout),
        Ok(Ok(output)) => {
            eprintln!(
                "orama: wsl.exe exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
            None
        }
        Ok(Err(err)) => {
            // Not on Windows, or WSL is not installed. Neither is worth
            // reporting on every poll.
            if cfg!(windows) {
                eprintln!("orama: could not run wsl.exe: {err}");
            }
            None
        }
        Err(_) => {
            eprintln!("orama: wsl.exe did not answer within {DEADLINE:?}");
            None
        }
    }
}

/// The networking mode this machine's WSL is running.
pub fn networking() -> Networking {
    let Some(home) = super::home_dir() else {
        return Networking::Nat;
    };
    match std::fs::read_to_string(home.join(".wslconfig")) {
        Ok(text) => parse_networking(&text),
        // No file is the default configuration, which is NAT.
        Err(_) => Networking::Nat,
    }
}

/// Ask one distro about itself.
fn probe(name: &str) -> Option<Distro> {
    let args = vec![
        "--distribution".to_owned(),
        name.to_owned(),
        "--exec".to_owned(),
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        PROBE.to_owned(),
    ];
    let stdout = run_with_deadline(args)?;
    // A guest process's stdout is its own bytes, so UTF-8 — not the UTF-16 that
    // wsl.exe uses for its own messages.
    let text = String::from_utf8_lossy(&stdout);
    distro_from_probe(name, &parse_probe(&text))
}

/// Every distro installed on this machine, probed.
fn discover() -> Vec<Distro> {
    if !enabled() {
        return Vec::new();
    }
    let Some(listing) = run_with_deadline(vec!["--list".into(), "--quiet".into()]) else {
        return Vec::new();
    };
    parse_distro_list(&decode_utf16le(&listing))
        .iter()
        .filter_map(|name| probe(name))
        .collect()
}

static CACHE: Mutex<Option<(Instant, Vec<Distro>)>> = Mutex::new(None);

/// The known distros, taken from cache when it is fresh enough.
///
/// Cached because the settings endpoint is polled and a probe can start a
/// stopped VM; a page refresh must not cost that every time.
pub fn distros() -> Vec<Distro> {
    if !enabled() {
        return Vec::new();
    }
    {
        let guard = CACHE.lock().unwrap_or_else(|err| err.into_inner());
        if let Some((taken, cached)) = guard.as_ref() {
            if taken.elapsed() < TTL {
                return cached.clone();
            }
        }
    }
    refresh()
}

/// Discover again, discarding whatever was cached.
///
/// Called before a write. Under NAT the gateway moves when WSL restarts, so the
/// address in a stale cache is exactly the one that would produce a config that
/// silently captures nothing.
pub fn refresh() -> Vec<Distro> {
    let found = discover();
    let mut guard = CACHE.lock().unwrap_or_else(|err| err.into_inner());
    *guard = Some((Instant::now(), found.clone()));
    found
}

/// Place a known set of distros in the cache, for tests.
///
/// A seam rather than a mock: the alternative is that none of the WSL decision
/// logic — which path, which address, whether to refuse — can be tested at all
/// off Windows, and it is exactly that logic which cannot be checked by running
/// the thing here. Test-only, so it adds no production surface.
#[cfg(test)]
pub(crate) fn seed_cache(distros: Vec<Distro>) {
    let mut guard = CACHE.lock().unwrap_or_else(|err| err.into_inner());
    *guard = Some((Instant::now(), distros));
}

/// Find one distro by name, re-probing rather than trusting the cache.
pub fn probe_named(name: &str) -> Option<Distro> {
    if !enabled() {
        return None;
    }
    refresh().into_iter().find(|distro| distro.name == name)
}

/// Every address the proxy must listen on for a WSL guest to reach it.
///
/// Empty under mirrored mode and on WSL 1, where loopback already works, and
/// empty off Windows. Deliberately never `0.0.0.0`: the dashboard is served by
/// the same listener as the relay, so binding every interface would publish
/// every captured prompt and response to the local network with no
/// authentication. A vEthernet address reaches the WSL VM and nothing else.
pub fn bridge_hosts() -> Vec<IpAddr> {
    let networking = networking();
    let mut hosts: Vec<IpAddr> = distros()
        .iter()
        .filter(|distro| distro.is_wsl2())
        .filter_map(|distro| distro.gateway)
        .filter(|_| networking == Networking::Nat)
        .collect();
    hosts.sort();
    hosts.dedup();
    hosts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answers(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn a_guest_path_becomes_a_unc_path_into_the_distro() {
        assert_eq!(
            guest_to_windows("Ubuntu", "/home/elie/.ai/claude"),
            PathBuf::from(r"\\wsl.localhost\Ubuntu\home\elie\.ai\claude")
        );
    }

    #[test]
    fn a_drive_mount_maps_back_to_the_drive() {
        // Reaching a Windows file through the guest's share would be a round
        // trip out and back, and would record a path that means the same file
        // under a different name.
        assert_eq!(
            guest_to_windows("Ubuntu", "/mnt/c/Users/elie/.claude"),
            PathBuf::from(r"C:\Users\elie\.claude")
        );
    }

    #[test]
    fn a_multi_letter_mnt_entry_is_not_a_drive() {
        // /mnt/wsl is real and is not drive "wsl".
        assert_eq!(
            guest_to_windows("Ubuntu", "/mnt/wsl/docker"),
            PathBuf::from(r"\\wsl.localhost\Ubuntu\mnt\wsl\docker")
        );
    }

    #[test]
    fn the_distro_listing_is_utf16() {
        // What wsl.exe actually emits, byte for byte.
        let bytes = b"U\0b\0u\0n\0t\0u\0\r\0\n\0D\0e\0b\0i\0a\0n\0\r\0\n\0";
        assert_eq!(
            parse_distro_list(&decode_utf16le(bytes)),
            vec!["Ubuntu".to_owned(), "Debian".to_owned()]
        );
    }

    #[test]
    fn probe_output_survives_an_rc_that_prints() {
        // An interactive shell rc emits banners and prompts, and one of them
        // containing an `=` must not be mistaken for an environment variable.
        let text = "orama.home=/home/elie\n\
                    nvm: using v20\n\
                    \x1b[32m➜\x1b[0m  ~ = ready\n\
                    CLAUDE_CONFIG_DIR=/home/elie/.ai/claude\n\
                    CODEX_HOME=/home/elie/.ai/codex\n";
        let parsed = parse_probe(text);
        assert_eq!(
            parsed.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some("/home/elie/.ai/claude")
        );
        assert_eq!(
            parsed.get("orama.home").map(String::as_str),
            Some("/home/elie")
        );
        assert!(!parsed.contains_key("nvm: using v20"));
    }

    #[test]
    fn an_env_override_decides_the_config_directory() {
        // The case this module exists for: the default path is empty and the
        // real one is somewhere else entirely.
        let distro = distro_from_probe(
            "Ubuntu",
            &answers(&[
                ("orama.home", "/home/elie"),
                ("orama.kernel", "6.18.33.2-microsoft-standard-WSL2"),
                ("CLAUDE_CONFIG_DIR", "/home/elie/.ai/claude"),
            ]),
        )
        .unwrap();

        assert_eq!(
            distro.guest_config_dir(Harness::ClaudeCode),
            "/home/elie/.ai/claude"
        );
        assert!(distro.config_dir_is_overridden(Harness::ClaudeCode));
        // Codex was not overridden, so it falls back under the guest's own home.
        assert_eq!(distro.guest_config_dir(Harness::Codex), "/home/elie/.codex");
        assert!(!distro.config_dir_is_overridden(Harness::Codex));
        assert_eq!(
            distro.windows_config_dir(Harness::ClaudeCode),
            PathBuf::from(r"\\wsl.localhost\Ubuntu\home\elie\.ai\claude")
        );
    }

    #[test]
    fn a_probe_that_found_no_home_yields_no_distro() {
        assert!(distro_from_probe("Ubuntu", &answers(&[("orama.kernel", "x")])).is_none());
    }

    #[test]
    fn nat_points_the_guest_at_the_gateway_and_mirrored_at_loopback() {
        let distro = distro_from_probe(
            "Ubuntu",
            &answers(&[
                ("orama.home", "/home/elie"),
                ("orama.kernel", "6.18.33.2-microsoft-standard-WSL2"),
                ("orama.gateway", "172.17.160.1"),
            ]),
        )
        .unwrap();

        assert!(distro.is_wsl2());
        // Measured on a live machine: this is the address that answers, and
        // 127.0.0.1 is the one that does not.
        assert_eq!(
            distro
                .host_address(Networking::Nat)
                .map(|ip| ip.to_string()),
            Some("172.17.160.1".to_owned())
        );
        assert_eq!(
            distro
                .host_address(Networking::Mirrored)
                .map(|ip| ip.to_string()),
            Some("127.0.0.1".to_owned())
        );
    }

    #[test]
    fn wsl1_reaches_windows_on_loopback_even_under_nat() {
        // WSL 1 shares the host network stack, so there is nothing to bridge.
        let distro = distro_from_probe(
            "Legacy",
            &answers(&[
                ("orama.home", "/home/elie"),
                ("orama.kernel", "4.4.0-19041-Microsoft"),
            ]),
        )
        .unwrap();
        assert!(!distro.is_wsl2());
        assert_eq!(
            distro
                .host_address(Networking::Nat)
                .map(|ip| ip.to_string()),
            Some("127.0.0.1".to_owned())
        );
    }

    #[test]
    fn nat_with_no_gateway_has_no_usable_address() {
        // Better to report that we do not know than to write 127.0.0.1 and
        // leave every request failing with a connection refused.
        let distro = distro_from_probe(
            "Ubuntu",
            &answers(&[
                ("orama.home", "/home/elie"),
                ("orama.kernel", "microsoft-standard-WSL2"),
            ]),
        )
        .unwrap();
        assert!(distro.host_address(Networking::Nat).is_none());
    }

    #[test]
    fn networking_mode_is_read_only_from_the_wsl2_section() {
        assert_eq!(
            parse_networking("[wsl2]\nnetworkingMode=mirrored\n"),
            Networking::Mirrored
        );
        assert_eq!(
            parse_networking("[wsl2]\nmemory=17097031680\n"),
            Networking::Nat
        );
        // The same key under another heading is a different setting.
        assert_eq!(
            parse_networking("[experimental]\nnetworkingMode=mirrored\n"),
            Networking::Nat
        );
        // Absent, commented, and unparseable all mean the documented default.
        assert_eq!(parse_networking(""), Networking::Nat);
        assert_eq!(
            parse_networking("[wsl2]\n# networkingMode=mirrored\n"),
            Networking::Nat
        );
        // Case and spacing are not significant in the file, so not here either.
        assert_eq!(
            parse_networking("[WSL2]\n  NetworkingMode = Mirrored  \n"),
            Networking::Mirrored
        );
    }

    /// Not a unit test — a diagnostic that runs the real thing.
    ///
    /// Everything else here is a pure function over recorded bytes, which proves
    /// the parsing and proves nothing about whether `wsl.exe` still answers the
    /// way it did. Ignored by default because it needs a WSL install and starts
    /// a distro; run it deliberately:
    ///
    /// ```sh
    /// ORAMA_WSL=1 cargo test -p orama-core wsl -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a real WSL install; starts a distribution"]
    fn probe_a_real_machine() {
        let found = refresh();
        println!("networking: {:?}", networking());
        println!("bridge hosts: {:?}", bridge_hosts());
        assert!(!found.is_empty(), "no distribution was discovered");
        for distro in &found {
            println!(
                "\n{} (wsl2={}, shell={}, kernel={})\n  home     {}\n  gateway  {:?}\n  claude   {}\n  codex    {}",
                distro.name,
                distro.is_wsl2(),
                distro.shell,
                distro.kernel,
                distro.home,
                distro.gateway,
                distro.windows_config_file(Harness::ClaudeCode).display(),
                distro.windows_config_file(Harness::Codex).display(),
            );
            assert!(
                !distro.home.is_empty(),
                "a probed distro must report a home"
            );
        }
    }

    #[test]
    fn discovery_is_off_when_disabled() {
        // The guard the connector tests rely on to stay hermetic.
        std::env::set_var("ORAMA_WSL", "0");
        assert!(!enabled());
        assert!(distros().is_empty());
        std::env::remove_var("ORAMA_WSL");
    }
}
