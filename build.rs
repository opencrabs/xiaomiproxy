//! Bundle secrets from `.env` (project root) into the binary at COMPILE time,
//! so the proxy deployed to neuraprod is self-contained — no runtime key file
//! on the server. The proxy is server-side only (never distributed to users),
//! so baking the Xiaomi key into it is fine.
//!
//! `.env` is gitignored. If it is absent at build (e.g. CI), we don't fail —
//! the binary falls back to runtime env vars (see `bundled_or_runtime` in main).

use std::fs;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=.env");

    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let env_path = Path::new(&manifest).join(".env");

    let content = match fs::read_to_string(&env_path) {
        Ok(c) => c,
        Err(_) => {
            println!(
                "cargo:warning=xiaomiproxy: no .env found at build; secrets must be set at runtime"
            );
            return;
        }
    };

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim().trim_matches('"').trim_matches('\'');
        // Only bake the secrets the binary actually consumes.
        if matches!(key, "XIAOMI_MIMO_API_KEY" | "APP_TOKEN") && !value.is_empty() {
            println!("cargo:rustc-env={key}={value}");
        }
    }
}
