//! Black-box production fail-closed checks: configuration errors must exit 78
//! (EX_CONFIG) *before* the storage lock/database file is touched.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_vks-server")
}

/// Writes a temp config that passes `Config::validate` (dev defaults) and points
/// SQLite at a path that must never be created by a configuration rejection.
fn temp_config(candidate: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("vks-config-failclosed-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("create temp config dir");
    let db = dir.join("must-not-exist.db");
    let config = dir.join("server.toml");
    let body = format!(
        "server_name = \"config-failclosed\"\n\
         listen_addr = \"127.0.0.1:0\"\n\
         candidate_addr = \"{candidate}\"\n\
         allow_insecure_defaults = true\n\
         database_url = \"sqlite:{}\"\n\
         storage_allow_fallback = false\n\
         frontend_dir = \"{}\"\n",
        db.display(),
        dir.join("frontend").display()
    );
    std::fs::write(&config, body).expect("write temp config");
    (config, db)
}

fn run(config: &Path, env: &[(&str, &str)]) -> (Option<i32>, String) {
    let mut command = Command::new(bin());
    command.arg("--config").arg(config).env_clear();
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command.output().expect("spawn vks-server");
    (
        output.status.code(),
        format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr)),
    )
}

fn assert_rejected(config: &Path, db: &Path, env: &[(&str, &str)], needle: &str) {
    let (code, logs) = run(config, env);
    assert_eq!(code, Some(78), "expected EX_CONFIG=78; logs={logs}");
    assert!(logs.contains(needle), "expected {needle:?} in logs: {logs}");
    assert!(!db.exists(), "configuration rejection must happen before opening/creating the database: {}", db.display());
}

#[test]
fn missing_transport_env_exits_78_before_storage() {
    let (config, db) = temp_config("127.0.0.1");
    assert_rejected(&config, &db, &[], "VKS_WEBRTC_TRANSPORT is required");
}

#[test]
fn removed_transport_off_switch_exits_78() {
    let (config, db) = temp_config("127.0.0.1");
    for value in ["0", "false", "off", "no", "", "garbage"] {
        assert_rejected(&config, &db, &[("VKS_WEBRTC_TRANSPORT", value)], "legacy plaintext media plane was removed");
    }
}

#[test]
fn enabled_transport_with_bad_candidate_exits_78_before_storage() {
    let (config, db) = temp_config("");
    assert_rejected(&config, &db, &[("VKS_WEBRTC_TRANSPORT", "1")], "requires candidate_addr");

    // RFC 5737 TEST-NET addresses are never a real deployment.
    let (config, db) = temp_config("203.0.113.7");
    assert_rejected(&config, &db, &[("VKS_WEBRTC_TRANSPORT", "1")], "TEST-NET");
}

#[test]
fn prod_config_without_secrets_exits_78() {
    let config = concat!(env!("CARGO_MANIFEST_DIR"), "/../../configs/prod.example.toml");
    let (code, logs) = run(Path::new(config), &[("VKS_WEBRTC_TRANSPORT", "1")]);
    assert_eq!(code, Some(78), "expected EX_CONFIG=78, got {code:?}; stderr={logs}");
    assert!(logs.contains("configuration rejected"), "logs: {logs}");
}
