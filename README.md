

<h1 align="center"> <code>Vpod</code> </h1>

<div align="center">
  <a href="https://github.com/capsulerun/vpod/actions/workflows/ci.yml" target="_blank">
    <img src="https://img.shields.io/github/actions/workflow/status/capsulerun/vpod/ci.yml?branch=main&label=CI&logo=github" alt="CI">
  </a>

  <a href="https://riscv.org/specifications/ratified/"><img src="https://img.shields.io/badge/RISCV-RV64GC-orange?logo=RISCV" alt="Risc-V"></a>
  <a href="https://wasi.dev/"><img src="https://img.shields.io/badge/Wasm%2FWASI-0.2.0-654FF0?logo=webassembly&logoColor=white" alt="Wasm/WASI 0.2 Sandbox"></a>

[Getting Started](#getting-started) • [Documentation](https://docs.vpod.sh/quickstart) • [Issues](https://github.com/capsulerun/vpod/issues/new) • [Contributing](#contributing)

![demo](assets/demo.gif)
</div>

## What is a `vpod` ?

A `vpod` is a lightweight, portable sandbox that gives an untrusted process an instant Linux environment. It uses a RISC‑V architecture and runs entirely inside WebAssembly.

- **Fast startup** : Boot in under a second.
- **Portable** : Runs anywhere without any setup required.
- **Isolated** : All execution state stays inside the WASM sandboxes.

## How it works

A `vpod` runs a full RISC‑V virtual machine (RV64GC, single vCPU) compiled to WebAssembly. Inside it boots a real Linux kernel with a real userspace, so `apk`, `pip`, shells and daemons all behave like they would on actual hardware.

Three pieces make that practical:

**Snapshots.** Instead of booting Linux from scratch, a `vpod` restores a snapshot: a saved VM state (CPU registers, RAM, filesystem) captured right after boot. Restoring one takes well under a second. Suspend works the same way in reverse, only dirty memory pages are written back to disk, so you can pause a sandbox and resume it later, even from another process.

**Ahead-of-time translation.** Pure instruction-by-instruction emulation is slow, and WebAssembly rules out a runtime JIT. So at snapshot build time, the hottest guest code paths are translated from RISC‑V into native code that gets compiled into the WASM module itself. At runtime the VM dispatches into these translated blocks when the guest code matches, and falls back to the interpreter when it doesn't. This is worth roughly 5x on CPU-bound work, with zero effect on isolation: translated code goes through the same MMU and memory checks as interpreted code.

**The WASI boundary.** The WASM component talks to the host exclusively through WASI 0.2. The guest never sees host file descriptors, sockets, or memory: filesystem access goes through explicitly mounted directories, and networking goes through a user-mode network stack inside the component that only ever asks the host for plain outbound sockets. Everything else (guest kernel, processes, memory) lives inside the WASM linear memory and dies with it.

### RV64GC Specification

**G (General-purpose extensions)**
- **I** : Base 64-bit integer instruction set.
- **M** : Hardware multiply and divide, useful for hashing and cryptography.
- **A** : Atomic operations for thread-safe programs.
- **F/D** : Single and double-precision floating-point, suited for scientific computing and ML inference.

**C (Compressed instructions)**
Reduces code size by 30%, improving instruction fetch speed and memory efficiency. This matters when running a full Linux userspace inside our memory-constrained WASM environment.

> [!NOTE]
> The V (vector) extension is not implemented. RVV instructions would execute as emulated RISC-V; there is no SIMD passthrough to the host CPU. Adding V would increase emulation overhead without any performance benefit for vectorized workloads.

## Getting started

### CLI

```bash
curl -fsSL https://install.vpod.sh | sh
```

> <details>
> <summary>Or install via PowerShell (windows)</summary>
>
> ```bash
> irm https://install.vpod.sh | iex
> ```
>
> </details>

```bash
# Pull a snapshot
vpod pull alpine:latest

# Start an interactive shell
vpod
```

### Python SDK

```bash
pip install vpod
```

```python
from vpod import Sandbox

# Run a command
sandbox = Sandbox.create()
result = sandbox.commands.run("whoami")
print(result.stdout)  # root
sandbox.close()

# Persistent session — state preserved across calls
with Sandbox.create() as sandbox:
    sandbox.commands.run("export API_KEY=secret")
    result = sandbox.commands.run("echo $API_KEY")
    print(result.stdout)  # secret

# Python REPL — variables persist
with Sandbox.create() as sandbox:
    sandbox.code.run("import requests")
    sandbox.code.run("data = [1, 2, 3]")
    result = sandbox.code.run("print(sum(data))")
    print(result.text)  # 6
```

> [!IMPORTANT]
> The first call to `Sandbox.create()` downloads the default snapshot (`alpine`) and caches it locally if not already present.

## Documentation
Visit [Vpod documentation](https://docs.vpod.sh/quickstart).

## Limitations
- **Emulation overhead**: There is no hardware virtualization inside WebAssembly, so all guest code is emulated. The overhead depends entirely on the workload: I/O-bound and network-bound work runs close to native speed, while tight CPU-bound loops can be 10x or more slower even with AOT translation. If your workload is mostly "run a tool, read a file, call an API", you won't notice. If it's "crunch numbers for an hour", a vpod is the wrong tool.
- **riscv64 guest**: The sandbox is a RISC‑V machine. Precompiled x86/ARM binaries won't run inside it. Alpine's `apk` packages and pure-Python `pip` packages work out of the box; Python packages with native extensions need riscv64 wheels, or use a snapshot that ships them pre-installed (like `vsnap-data` with numpy/pandas/scipy).
- **Single vCPU**: The VM exposes one core. Multi-process and multi-threaded guest code runs fine, but it is time-sliced, not parallel.
- **No GPU access**: CUDA, Metal, and hardware ML accelerators are not available. Support may be added in the future with wasi-nn.
- **No SIMD passthrough**: The V (vector) extension is not implemented, so vectorized code runs scalar (see the note above).
- **Fixed memory**: Guest RAM is fixed by the snapshot (256 MB for `alpine`, 512 MB for `vsnap-data`). There is no ballooning or dynamic growth.

## Contributing

Contributions are welcome, from bug reports to new device support. Open an [issue](https://github.com/capsulerun/vpod/issues/new) to discuss anything substantial before building it.

### Repository layout

| Path | What it is |
|:---|:---|
| `crates/riscv-core` | RV64GC decoder and executor, MMU, and the AOT block runtime |
| `crates/machine` | The virtual machine: RAM (copy-on-write), UART, PLIC/CLINT, virtio devices, snapshot save/restore |
| `crates/wasi-component` | The WASM component (WASI 0.2) that wraps the machine for sandboxed use |
| `crates/vpod` | The host CLI (`vpod`), which runs the WASM component |
| `crates/native-cli` | A native (non-WASM) build of the VM, used for development and debugging |
| `crates/vpod-translate` | The AOT translator: turns traced hot RISC‑V code into Rust at snapshot build time |
| `sdks/python` | The Python SDK (`pip install vpod`) |
| `scripts/` | Build scripts for the WASM component, snapshots, and AOT translation |

### Prerequisites

- **Rust** (latest stable) with the `wasm32-wasip2` target: `rustup target add wasm32-wasip2`
- **Python 3.10+** for the SDK
- **Zig** (0.14) and **bsdtar**, only needed if you build snapshots yourself

### Development setup

```bash
# One-time: generate the AOT stub (a fresh clone has no translated blocks)
./scripts/aot-stub.sh

# Build the WASM component (library + CLI)
./scripts/build-wasm.sh

# Install the host CLI
cargo install --path crates/vpod

# Install the Python SDK in dev mode
pip install -e "sdks/python[dev]"
```

### Running tests

CI runs these on every PR, so run them before pushing:

```bash
cargo fmt --all -- --check                        # formatting
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all                                  # Rust tests

# Python SDK integration tests (needs the WASM library in place)
cp target/wasm32-wasip2/release/vpod_wasi_lib.wasm sdks/python/vpod/
pytest sdks/python/tests/ -v -m integration
```

### Building snapshots

The project uses pre-built Alpine snapshots from `registry.vpod.sh`, so you normally don't need this. To build one locally:

```bash
./scripts/build-default-snapshot.sh   # dist/alpine-3.23.0-256mb.snap
./scripts/build-data-snapshot.sh      # 512 MB variant with numpy/pandas/scipy
```

> [!IMPORTANT]
> To use a locally built snapshot, uncomment the lines in `resolve_snapshot()` in `crates/vpod/src/main.rs`.

Snapshot builds can also run the AOT pass (`scripts/aot-snapshot.sh <snapshot>`), which traces a representative workload, translates the hot blocks, and rebuilds the VM with them baked in. It takes a while; the stub from `aot-stub.sh` is fine for everyday development, everything works the same, just slower.

### Pull requests

- Keep PRs focused: one change per PR.
- `fmt`, `clippy` and the test suite must pass (CI enforces all three).
- If you touch the emulator's execution or memory paths, say how you validated correctness (the test suite at minimum; for subtle changes a boot plus a real workload in the guest is a good sanity check).

## License

This project is licensed under the **Apache License 2.0**.
See the [LICENSE](LICENSE) file for details.
