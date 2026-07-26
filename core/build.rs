//! Stage the web UI bundle for embedding.
//!
//! The Rust binary serves the dashboard from bytes compiled into it, but
//! `apps/web/dist/` is a build artifact that is not tracked in git. Embedding it
//! directly made a clean clone fail to compile until someone had run
//! `npm run build` by hand.
//!
//! This script always produces the two files the crate embeds: copied from the
//! real bundle when it exists, or a placeholder page explaining how to build it
//! when it does not. Compilation therefore never depends on the frontend having
//! been built, and a missing bundle degrades to a readable message instead of a
//! build error.

use std::{env, fs, path::PathBuf};

/// Files copied out of the frontend bundle, as (source name, staged name).
const ASSETS: &[(&str, &str)] = &[("index.html", "ui_index.html"), ("main.js", "ui_main.js")];

const PLACEHOLDER_HTML: &str = r#"<!doctype html>
<meta charset="utf-8">
<title>tracer — UI not built</title>
<body style="font:14px system-ui;margin:3rem auto;max-width:40rem;line-height:1.6">
<h1>UI bundle not built</h1>
<p>The dashboard assets were not present when this binary was compiled.</p>
<pre style="background:#f4f4f5;padding:1rem;border-radius:4px">cd apps/web &amp;&amp; npm install &amp;&amp; npm run build
cargo build</pre>
<p>The JSON API under <code>/api</code> is unaffected and fully available.</p>
</body>
"#;

const PLACEHOLDER_JS: &str =
    "console.warn('tracer: UI bundle not built; run `npm run build` in apps/web');\n";

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is set by cargo"));
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo"));
    let dist = manifest_dir.join("../apps/web/dist");

    for (source_name, staged_name) in ASSETS {
        let source = dist.join(source_name);
        // Rebuild whenever the bundle changes, including when it first appears.
        println!("cargo:rerun-if-changed={}", source.display());

        let contents = fs::read(&source).unwrap_or_else(|_| {
            let placeholder = match *source_name {
                "index.html" => PLACEHOLDER_HTML,
                _ => PLACEHOLDER_JS,
            };
            println!(
                "cargo:warning=tracer: {} not found; embedding a placeholder instead",
                source.display()
            );
            placeholder.as_bytes().to_vec()
        });

        fs::write(out_dir.join(staged_name), contents).expect("staging the UI asset must succeed");
    }
}
