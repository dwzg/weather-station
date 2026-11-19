//! ESP-IDF environment, the firmware version and the secrets.
//!
//! Secrets come from `secrets.env` (git-ignored, written by
//! tools/make-secrets.py). Without it the build fails, unless
//! `WS_EXAMPLE_SECRETS=1` asks for `secrets.example.env` (CI): such an image
//! cannot reach the network, and its OTA key is all zeros.

use std::path::Path;
use std::process::Command;

const KEYS: [&str; 5] = ["WIFI_SSID", "WIFI_PASS", "API_URL", "API_KEY", "OTA_KEY"];

fn main() {
    embuild::espidf::sysenv::output();

    println!("cargo:rerun-if-env-changed=WS_EXAMPLE_SECRETS");
    let file = if Path::new("secrets.env").exists() {
        "secrets.env"
    } else if std::env::var("WS_EXAMPLE_SECRETS").as_deref() == Ok("1") {
        println!("cargo:warning=building with secrets.example.env: no network access, OTA key all zeros");
        "secrets.example.env"
    } else {
        panic!(
            "secrets.env missing: run tools/make-secrets.py (or set WS_EXAMPLE_SECRETS=1 \
             for a build that cannot reach the network and has an all-zero OTA key)"
        );
    };
    println!("cargo:rerun-if-changed=secrets.env");
    println!("cargo:rerun-if-changed=secrets.example.env");
    let text = std::fs::read_to_string(file).expect("read secrets");
    for key in KEYS {
        let value = text
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .filter_map(|l| l.split_once('='))
            .find(|(k, _)| k.trim() == key)
            .map(|(_, v)| v.trim().to_string())
            .unwrap_or_else(|| panic!("{file}: {key}= missing"));
        // ESP-IDF's limits; a longer value would silently become an empty one.
        let max = match key {
            "WIFI_SSID" => 32,
            "WIFI_PASS" => 64,
            _ => usize::MAX,
        };
        assert!(value.len() <= max, "{file}: {key} is {} bytes, at most {max} fit", value.len());
        if key == "OTA_KEY" {
            assert!(
                value.len() == 64 && value.chars().all(|c| c.is_ascii_hexdigit()),
                "{file}: OTA_KEY must be 64 hex digits"
            );
        }
        println!("cargo:rustc-env=WS_{key}={value}");
    }

    let version = Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=FW_VERSION={version}");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");
}
