//! Black-box проверка production fail-closed: prod-конфиг без секретов
//! обязан завершиться с кодом 78 (EX_CONFIG) и не занять порт.

use std::process::Command;

#[test]
fn prod_config_without_secrets_exits_78() {
    let bin = env!("CARGO_BIN_EXE_vks-server");
    let config = concat!(env!("CARGO_MANIFEST_DIR"), "/../../configs/prod.example.toml");
    let output = Command::new(bin).arg("--config").arg(config).env_clear().output().expect("spawn vks-server");
    assert_eq!(
        output.status.code(),
        Some(78),
        "expected EX_CONFIG=78, got {:?}; stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let logs = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(logs.contains("configuration rejected"), "logs: {logs}");
}
