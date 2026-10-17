//! Сборка .proto через prost-build.
//!
//! Использует `protobuf-src`: компилирует собственный `protoc` в `OUT_DIR`
//! при первой сборке, чтобы НЕ требовать системного `protoc`
//! (кросс-платформенно: Linux/macOS/CI).
//!
//! Основание: docs/DEVELOPMENT_PLAN.md Фаза 0 (build.rs — prost-build).

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_dir = "proto";
    let protos = ["media.proto", "signaling.proto", "cluster.proto", "admin.proto"];

    #[cfg(feature = "vendored-protoc")]
    let (protoc, include) = (protobuf_src::protoc(), protobuf_src::include());

    #[cfg(not(feature = "vendored-protoc"))]
    let (protoc, include) = {
        let protoc = std::env::var("PROTOC")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from("protoc"));
        let include = std::env::var("PROTOC_INCLUDE").map(std::path::PathBuf::from).unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            let cargo_inc = std::path::PathBuf::from(home).join(".cargo/include");
            if cargo_inc.exists() {
                cargo_inc
            } else {
                std::path::PathBuf::from("proto")
            }
        });
        (protoc, include)
    };

    let mut config = prost_build::Config::new();
    config.protoc_executable(&protoc);
    config.protoc_arg("--experimental_allow_proto3_optional");
    config.compile_well_known_types();

    let out = std::env::var("OUT_DIR").unwrap();
    config.out_dir(&out);

    // Компилируем все .proto (import'ят друг друга: signaling→media, admin→media).
    // `includes` — директории, доступные для `import` (тут только proto/).
    config.compile_protos(&protos, &[proto_dir])?;

    // Передаём путь к protoc + include-директории (для debug/дебага).
    println!("cargo:rustc-env=PROTOC_PATH={}", protoc.display());
    println!("cargo:rustc-env=PROTOC_INCLUDE={}", include.display());
    // Re-run при изменении .proto.
    println!("cargo:rerun-if-changed=proto");

    Ok(())
}
