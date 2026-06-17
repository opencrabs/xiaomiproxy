//! Bundle secrets into the binary at COMPILE time, so the proxy deployed to
//! neuraprod is self-contained — no runtime key file on the server. The proxy
//! is server-side only (never distributed to users), so baking the Xiaomi key
//! into it is fine.
//!
//! Two build-time sources, in order:
//! 1. `.env` at the project root (gitignored) — used for local builds.
//! 2. A build-time environment variable of the same name — used by CI, where
//!    the value comes from a GitHub Actions secret on the (private) repo.
//!
//! If neither is present we don't fail; the binary falls back to runtime env
//! vars (see `bundled_or_runtime` in main).

use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Secrets we bake in. Keep in sync with `bundled_or_runtime` in main.rs.
const BAKED_KEYS: &[&str] = &["XIAOMI_MIMO_API_KEY", "APP_TOKEN"];

fn main() {
    println!("cargo:rerun-if-changed=.env");
    for key in BAKED_KEYS {
        println!("cargo:rerun-if-env-changed={key}");
    }

    // 1. Parse .env (if present) into a map.
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let env_path = Path::new(&manifest).join(".env");
    let mut from_env_file: HashMap<String, String> = HashMap::new();
    if let Ok(content) = fs::read_to_string(&env_path) {
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                let value = value.trim().trim_matches('"').trim_matches('\'');
                from_env_file.insert(key.trim().to_string(), value.to_string());
            }
        }
    }

    // 2. For each baked key, prefer the .env value, then a build-time env var.
    let mut any = false;
    for key in BAKED_KEYS {
        let value = from_env_file
            .get(*key)
            .cloned()
            .filter(|v| !v.is_empty())
            .or_else(|| std::env::var(key).ok().filter(|v| !v.is_empty()));
        if let Some(value) = value {
            println!("cargo:rustc-env={key}={value}");
            any = true;
        }
    }
    if !any {
        println!(
            "cargo:warning=xiaomiproxy: no .env or build-time secret found; secrets must be set at runtime"
        );
    }
}
