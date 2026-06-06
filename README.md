

<h1 align="center"> <code>Vpod</code> </h1>

<div align="center">
  <a href="https://github.com/capsulerun/vpod/actions/workflows/ci.yml" target="_blank">
    <img src="https://img.shields.io/github/actions/workflow/status/capsulerun/vpod/ci.yml?branch=main&label=Build" alt="Build">
  </a>

  <a href="https://riscv.org/specifications/ratified/"><img src="https://img.shields.io/badge/RISCV-RV64GCV-blue" alt="Risc-V"></a>
  <a href="https://wasi.dev/"><img src="https://img.shields.io/badge/Wasm%2FWASI-0.2.0-654FF0?logo=webassembly&logoColor=white" alt="Wasm/WASI 0.2 Sandbox"></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/Rust-2024_edition-orange" alt="Rust"></a>
</div>

<div align="center">
  <a href="#getting-started">Getting started</a>
  <span>&nbsp;&nbsp;•&nbsp;&nbsp;</span>
  <a href="#documentation">Documentation</a>
  <span>&nbsp;&nbsp;•&nbsp;&nbsp;</span>
  <a href="#contributing">Contributing</a>
</div>


## What is a vpod ?

A `vpod` is a lightweight, portable sandbox that gives an untrusted process an instant Linux environment. It uses the RISC‑V architecture and runs entirely inside WebAssembly.

- **Fast startup** : Boot in under a second.
- **Portable** : Runs anywhere without any setup required.
- **Isolated** : All execution state stays inside the WASM sandboxes.

## How it works

A vpod runs a RISC‑V virtual machine compiled to WebAssembly. The core implements the RV64GCV specification. When you start a vpod, it boots from a snapshot, a saved VM state ready in under a second.

The WASM component communicates with the host through WASI 0.2, providing controlled access to filesystem, networking, and standard I/O while keeping all execution state (CPU registers, memory, filesystem) isolated inside the sandbox.

### `RV64GCV` Specification

**G (General-purpose extensions)**
- **I** : Base 64-bit integer instruction set.
- **M** : Hardware multiply and divide, useful for hashing and cryptography.
- **A** : Atomic operations for thread-safe programs.
- **F/D** : Single and double-precision floating-point, suited for scientific computing and ML inference.

**C (Compressed instructions)**
Reduces code size by 30%, improving instruction fetch speed and memory efficiency. This matters when running a full Linux userspace inside our memory-constrained WASM environment.

**V (Vector extension)**
Adds SIMD operations for parallel data processing. Accelerates array operations, data transformations, and numerical workloads common in AI agent execution.

## Getting started

### CLI

```bash
cargo install vpod
```

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

> [!NOTE]
> The first call to `Sandbox.create()` downloads the default snapshot (`alpine`) and caches it locally.

## Documentation

Full reference for the CLI and Python SDK.

### CLI commands

| Command | Description |
|:---|:---|
| `vpod` | Start an interactive shell with default snapshot |
| `vpod start <snapshot>` | Start an interactive shell with a specific snapshot |
| `vpod pull <snapshot>` | Pull a snapshot |
| `vpod list` | List available snapshots |

### Python SDK

#### Sandbox

| Method | Description |
|:---|:---|
| `Sandbox.create()` | Create a new sandbox |
| `sandbox.commands.run(cmd)` | Run a command |
| `sandbox.code.run(code)` | Run Python code |


#### Snapshots

| Method | Description | Return type |
|:---|:---|:---|
| `snapshots.fetch_registry()` | Fetch available snapshots | list[dict] |
| `snapshots.pull(name)` | Pull a snapshot | str |

**Example**

```python
from vpod import snapshots

for snap in snapshots.fetch_registry():
    print(snap["name"], snap["tag"])

path = snapshots.pull("alpine:latest")
```

## Limitations
- **Emulation overhead** — No hardware acceleration in WASM. CPU-intensive workloads run slower than native.
- **No GPU access** — CUDA, Metal, and hardware ML accelerators are not yet available. Support may be added in the future with WASI‑nn.

## Contributing

**Prerequisites**
- Rust (latest stable)
- Python 3.10+

**Development setup**
```bash
# Build WASM component
./scripts/build-wasm.sh

# Install CLI
cargo install --path crates/vpod

# Install Python SDK in dev mode
pip install -e sdks/python[dev]

# Run tests
cargo test                              # Rust tests
pytest sdks/python/tests/ -v            # Python unit tests
pytest sdks/python/tests/ -v -m integration  # Integration tests (requires WASM build)
```

**Building snapshots**

The project uses pre-built Alpine snapshots from `registry.vpod.sh`. To build a custom snapshot:

```bash
./scripts/build-default-snapshot.sh
```

This creates `dist/alpine-3.23.0-256mb.snap`. To use it locally, uncomment lines in `resolve_snapshot()` in `crates/vpod/src/main.rs`.

## License

This project is licensed under the **Apache License 2.0**.
See the [LICENSE](LICENSE) file for details.
