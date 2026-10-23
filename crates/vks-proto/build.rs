//! Сборка .proto через prost-build.
//!
//! protoc берётся из крейта `protoc-bin-vendored`: готовый бинарь лежит в реестре
//! cargo, а не в `$OUT_DIR` воркспейса, поэтому сборка не зависит от пробелов в
//! пути (`protobuf-src`/autotools/libtool зависели: libtool режет `$PWD` по пробелу).
//!
//! Системный protoc переопределяется переменной `PROTOC` (соглашение prost-build):
//! `PROTOC=/usr/bin/protoc cargo build`.
//!
//! Основание: docs/DEVELOPMENT_PLAN.md Фаза 0 (build.rs — prost-build); ADR-0031.

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    const PROTO_DIR: &str = "proto";
    const PROTOS: [&str; 4] = ["media.proto", "signaling.proto", "cluster.proto", "admin.proto"];

    // Системный protoc — если задан; иначе герметичный из крейта.
    let protoc: PathBuf = match std::env::var_os("PROTOC") {
        Some(p) => p.into(),
        None => protoc_bin_vendored::protoc_bin_path()?,
    };

    // Include с well-known типами нужен, только если .proto будут импортировать
    // google/protobuf/*; prost-build читает его только из переменной окружения.
    // NB: в edition 2024 `set_var` станет unsafe — тогда обернуть в unsafe-блок.
    if let Ok(include) = protoc_bin_vendored::include_path() {
        std::env::set_var("PROTOC_INCLUDE", include);
    }

    let mut config = prost_build::Config::new();
    config.protoc_executable(&protoc);
    config.compile_well_known_types();

    let out = std::env::var("OUT_DIR")?;
    config.out_dir(&out);
    // Компилируем все .proto (import'ят друг друга: signaling→media, admin→media).
    config.compile_protos(&PROTOS, &[PROTO_DIR])?;

    println!("cargo:rustc-env=PROTOC_PATH={}", protoc.display());
    println!("cargo:rerun-if-changed={PROTO_DIR}");

    Ok(())
}
