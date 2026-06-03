use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir.parent().unwrap().parent().unwrap();

    let candidates = [
        env::var("VPOD_WASM").ok().map(PathBuf::from),
        Some(manifest_dir.join("vpod-wasi-cli.wasm")),
        Some(
            workspace_root
                .join("target")
                .join("wasm32-wasip2")
                .join("release")
                .join("vpod-wasi-cli.wasm"),
        ),
        Some(workspace_root.join("dist").join("vpod.wasm")),
    ];

    let wasm_path = candidates
        .into_iter()
        .flatten()
        .find(|p| p.exists())
        .expect(
            "vpod-wasi-cli.wasm not found. Build it with:\n  \
             cargo build --release --target wasm32-wasip2 -p wasi-component --bin vpod-wasi-cli\n\
             Or set VPOD_WASM=/path/to/vpod-wasi-cli.wasm",
        );

    println!("cargo:rustc-env=VPOD_WASM_PATH={}", wasm_path.display());
    println!("cargo:rerun-if-env-changed=VPOD_WASM");
    println!("cargo:rerun-if-changed={}", wasm_path.display());
}
