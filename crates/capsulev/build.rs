use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir.parent().unwrap().parent().unwrap();

    let candidates = [
        env::var("CAPSULEV_WASM").ok().map(PathBuf::from),
        Some(
            workspace_root
                .join("target")
                .join("wasm32-wasip2")
                .join("release")
                .join("wasm-component.wasm"),
        ),
        Some(workspace_root.join("dist").join("capsulev.wasm")),
    ];

    let wasm_path = candidates
        .into_iter()
        .flatten()
        .find(|p| p.exists())
        .expect(
            "wasm-component.wasm not found. Build it or set CAPSULEV_WASM=/path/to/capsulev.wasm",
        );

    println!("cargo:rustc-env=CAPSULEV_WASM_PATH={}", wasm_path.display());
    println!("cargo:rerun-if-env-changed=CAPSULEV_WASM");
    println!("cargo:rerun-if-changed={}", wasm_path.display());
}
