use std::fs;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

use crate::registry::Snapshot;
use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{DirPerms, FilePerms, IoView, WasiCtx, WasiCtxBuilder, WasiView};

static WASM_BYTES: &[u8] = include_bytes!(env!("VPOD_WASM_PATH"));

pub struct RunConfig {
    pub version: String,
    pub snapshot: Snapshot,
}

struct State {
    wasi: WasiCtx,
    table: ResourceTable,
}

impl IoView for State {
    fn table(&mut self) -> &mut ResourceTable {
        &mut self.table
    }
}

impl WasiView for State {
    fn ctx(&mut self) -> &mut WasiCtx {
        &mut self.wasi
    }
}

#[cfg(unix)]
struct RawTerminal {
    saved: libc::termios,
}

#[cfg(unix)]
impl RawTerminal {
    fn enter() -> Option<Self> {
        if unsafe { libc::isatty(libc::STDIN_FILENO) } == 0 {
            return None;
        }

        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut t) != 0 {
                return None;
            }

            let saved = t;
            libc::cfmakeraw(&mut t);
            t.c_lflag |= libc::ISIG;
            t.c_oflag |= libc::OPOST;
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &t);

            Some(Self { saved })
        }
    }
}

#[cfg(unix)]
impl Drop for RawTerminal {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &self.saved);
        }
    }
}

#[cfg(windows)]
struct RawTerminal {
    saved_mode: u32,
}

#[cfg(windows)]
impl RawTerminal {
    fn enter() -> Option<Self> {
        use windows_sys::Win32::System::Console::*;
        unsafe {
            let handle = GetStdHandle(STD_INPUT_HANDLE);
            if handle == 0 || handle == u64::MAX as isize {
                return None;
            }
            let mut mode: u32 = 0;
            if GetConsoleMode(handle, &mut mode) == 0 {
                return None;
            }
            let raw_mode = (mode
                & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT))
                | ENABLE_VIRTUAL_TERMINAL_INPUT;
            SetConsoleMode(handle, raw_mode);
            Some(Self { saved_mode: mode })
        }
    }
}

#[cfg(windows)]
impl Drop for RawTerminal {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Console::*;
        unsafe {
            let handle = GetStdHandle(STD_INPUT_HANDLE);
            SetConsoleMode(handle, self.saved_mode);
        }
    }
}

fn cwasm_cache_path(version: &str) -> PathBuf {
    let base = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".local/share"));
    let hash = hex::encode(&Sha256::digest(WASM_BYTES)[..8]);

    base.join("vpod")
        .join(format!("component-{version}-{hash}.cwasm"))
}

fn load_component(engine: &Engine, version: &str) -> Result<Component> {
    let cache = cwasm_cache_path(version);

    if cache.exists() {
        if let Ok(c) = unsafe { Component::deserialize_file(engine, &cache) } {
            return Ok(c);
        }
        fs::remove_file(&cache).ok();
    }

    let component =
        Component::from_binary(engine, WASM_BYTES).context("failed to compile wasm component")?;

    if let Some(parent) = cache.parent() {
        fs::create_dir_all(parent).ok();
    }

    if let Ok(bytes) = component.serialize() {
        fs::write(&cache, bytes).ok();
    }

    Ok(component)
}

fn ensure_uncompressed(snap_path: &Path) -> Result<PathBuf> {
    let raw_path = snap_path.with_extension("raw");

    if raw_path.exists() {
        return Ok(raw_path);
    }

    let file = fs::File::open(snap_path)
        .with_context(|| format!("failed to open snapshot {:?}", snap_path))?;

    let mut reader = BufReader::new(GzDecoder::new(file));
    let mut data = Vec::new();
    reader
        .read_to_end(&mut data)
        .context("failed to decompress snapshot")?;

    let tmp = raw_path.with_extension("raw.tmp");
    let mut out = fs::File::create(&tmp).context("failed to create raw cache")?;
    out.write_all(&data).context("failed to write raw cache")?;

    fs::rename(&tmp, &raw_path).context("failed to rename raw cache")?;

    Ok(raw_path)
}

pub fn run(cfg: RunConfig) -> Result<()> {
    eprint!("\x1b]0;vpod ({})\x07", cfg.snapshot.display_name());
    print_header(&cfg.snapshot);

    eprint!("  \x1b[2mLoading...\x1b[0m");

    let mut config = Config::new();
    config.wasm_component_model(true);
    let engine = Engine::new(&config)?;
    let component = load_component(&engine, &cfg.version)?;

    // For clear loading message
    eprint!("\r\x1b[2K");
    let _raw = RawTerminal::enter();
    #[cfg(unix)]
    unsafe {
        libc::tcflush(libc::STDIN_FILENO, libc::TCIFLUSH);
    }
    eprint!("\r~ # ");

    let snap_path = Path::new(&cfg.snapshot.url);
    let snap_dir = snap_path.parent().unwrap_or(Path::new(".")).to_path_buf();

    let raw_path = ensure_uncompressed(snap_path)?;
    let raw_file = raw_path
        .file_name()
        .context("raw snapshot path has no filename")?
        .to_str()
        .context("raw snapshot filename is not valid UTF-8")?
        .to_string();

    let wasm_args = vec![
        "vpod-wasi-cli".to_string(),
        "--snapshot-load".to_string(),
        format!("snap/{raw_file}"),
    ];

    let mut builder = WasiCtxBuilder::new();
    builder.inherit_stdin().inherit_stdout().inherit_stderr();
    builder.args(&wasm_args);
    builder.preopened_dir(&snap_dir, "snap", DirPerms::READ, FilePerms::READ)?;
    builder.inherit_network();
    builder.allow_ip_name_lookup(true);
    builder.allow_blocking_current_thread(true);

    let state = State {
        wasi: builder.build(),
        table: ResourceTable::new(),
    };
    let mut store = Store::new(&engine, state);

    let mut linker: Linker<State> = Linker::new(&engine);
    wasmtime_wasi::add_to_linker_sync(&mut linker)?;

    let command =
        wasmtime_wasi::bindings::sync::Command::instantiate(&mut store, &component, &linker)
            .context("failed to instantiate wasm component")?;

    let result = command.wasi_cli_run().call_run(&mut store);

    handle_result(result)
}

fn print_header(snap: &Snapshot) {
    eprintln!(
        "\x1b[1mvpod\x1b[0m  \x1b[2m{} {} · {}\x1b[0m",
        snap.display_name(),
        snap.tag,
        snap.memory_label
    );
}

// struct Spinner {
//     stop: Arc<AtomicBool>,
//     handle: Option<std::thread::JoinHandle<()>>,
// }

// impl Spinner {
//     fn start(msg: &'static str) -> Self {
//         let stop = Arc::new(AtomicBool::new(false));
//         let stop_clone = stop.clone();

//         let handle = std::thread::spawn(move || {
//             const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
//             let mut i = 0;
//             while !stop_clone.load(Ordering::Relaxed) {
//                 eprint!("\r  \x1b[2m{} {msg}\x1b[0m", FRAMES[i % FRAMES.len()]);
//                 i += 1;
//                 std::thread::sleep(std::time::Duration::from_millis(80));
//             }

//             eprint!("\r\x1b[2K");
//         });

//         Self { stop, handle: Some(handle) }
//     }
// }

// impl Drop for Spinner {
//     fn drop(&mut self) {
//         self.stop.store(true, Ordering::Relaxed);
//         if let Some(h) = self.handle.take() {
//             h.join().ok();
//         }
//     }
// }

fn handle_result(result: anyhow::Result<Result<(), ()>>) -> Result<()> {
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(())) => std::process::exit(1),
        Err(e) => {
            if let Some(exit) = e.downcast_ref::<wasmtime_wasi::I32Exit>() {
                std::process::exit(exit.0);
            }
            Err(e.context("component run failed"))
        }
    }
}
