//! Embed the web UI bundle into the binary.
//!
//! `apps/web/dist/` is a build artifact and is not tracked, so embedding it
//! directly once made a clean clone fail to compile until someone had run
//! `npm run build` by hand. This script always emits an asset table: the real
//! bundle when it exists, or a single placeholder page explaining how to build
//! it when it does not. Compilation therefore never depends on the frontend
//! having been built.
//!
//! The whole directory is walked rather than a fixed file list, because the
//! bundler emits a stylesheet, a source map and a dozen font files alongside
//! the entry script — naming them individually meant every other asset would
//! silently 404 at runtime.

use std::{env, fs, path::Path, path::PathBuf};

const PLACEHOLDER_HTML: &str = r#"<!doctype html>
<meta charset="utf-8">
<title>orama — UI not built</title>
<body style="font:14px system-ui;margin:3rem auto;max-width:40rem;line-height:1.6">
<h1>UI bundle not built</h1>
<p>The dashboard assets were not present when this binary was compiled.</p>
<pre style="background:#f4f4f5;padding:1rem;border-radius:4px">npm --prefix apps/web install
npm --prefix apps/web run build
cargo build</pre>
<p>The JSON API under <code>/api</code> is unaffected and fully available.</p>
</body>
"#;

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is set by cargo"));
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo"));
    let dist = manifest_dir.join("../apps/web/dist");

    println!("cargo:rerun-if-changed={}", dist.display());

    let mut assets: Vec<(String, PathBuf)> = Vec::new();
    collect(&dist, &dist, &mut assets);
    assets.sort();

    let staged = out_dir.join("ui");
    let _ = fs::remove_dir_all(&staged);
    fs::create_dir_all(&staged).expect("staging directory must be creatable");

    let mut table = String::from(
        "/// (path, mime, bytes) for every file in the bundle.\n\
         pub static UI_ASSETS: &[(&str, &str, &[u8])] = &[\n",
    );

    if assets.is_empty() {
        println!(
            "cargo:warning=orama: {} not found; embedding a placeholder page instead",
            dist.display()
        );
        fs::write(staged.join("index.html"), PLACEHOLDER_HTML)
            .expect("staging the placeholder must succeed");
        table.push_str(
            "    (\"index.html\", \"text/html; charset=utf-8\", \
             include_bytes!(concat!(env!(\"OUT_DIR\"), \"/ui/index.html\"))),\n",
        );
    } else {
        for (name, source) in &assets {
            // Flatten into one directory: the bundle has no nesting, and flat
            // names keep the generated include_bytes! paths simple.
            let flat = name.replace('/', "_");
            fs::copy(source, staged.join(&flat)).expect("staging a UI asset must succeed");
            table.push_str(&format!(
                "    ({name:?}, {:?}, include_bytes!(concat!(env!(\"OUT_DIR\"), \"/ui/{flat}\"))),\n",
                mime_of(name)
            ));
        }
    }
    table.push_str("];\n");

    fs::write(out_dir.join("ui_assets.rs"), table).expect("writing the asset table must succeed");
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, out);
        } else if let Ok(relative) = path.strip_prefix(root) {
            out.push((relative.to_string_lossy().replace('\\', "/"), path.clone()));
        }
    }
}

fn mime_of(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    }
}
