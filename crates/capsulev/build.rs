use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir.parent().unwrap().parent().unwrap();

    let candidates = [
        // 1. Explicit env var override
        env::var("CAPSULEV_WASM").ok().map(PathBuf::from),
        // 2. Bundled WASM in the crate directory (for crates.io users)
        Some(manifest_dir.join("wasi-component.wasm")),
        // 3. Workspace build output (for local development)
        Some(
            workspace_root
                .join("target")
                .join("wasm32-wasip2")
                .join("release")
                .join("wasi-component.wasm"),
        ),
        // 4. Dist directory
        Some(workspace_root.join("dist").join("capsulev.wasm")),
    ];

    let wasm_path = candidates
        .into_iter()
        .flatten()
        .find(|p| p.exists())
        .expect(
            "wasi-component.wasm not found. Build it with:\n  \
             cargo build --release --target wasm32-wasip2 -p wasi-component\n  \
             cp target/wasm32-wasip2/release/wasi-component.wasm crates/capsulev/\n\
             Or set CAPSULEV_WASM=/path/to/wasi-component.wasm",
        );

    println!("cargo:rustc-env=CAPSULEV_WASM_PATH={}", wasm_path.display());
    println!("cargo:rerun-if-env-changed=CAPSULEV_WASM");
    println!("cargo:rerun-if-changed={}", wasm_path.display());
}
