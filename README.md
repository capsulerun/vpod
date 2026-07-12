

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

A `vpod` runs a RISC‑V virtual machine compiled to WebAssembly, implementing the RV64GC specification. When you start a `vpod`, it boots from a snapshot, a saved VM state ready in under a second.

The WASM component communicates with the host through WASI 0.2, providing controlled access to filesystem, networking, and standard I/O while keeping all execution state (CPU registers, memory, filesystem) isolated inside the sandbox.

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

> <details>
> <summary>Or install via cargo</summary>
>
> ```bash
> cargo install vpod
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

For more details, see the [full documentation](https://docs.vpod.sh/quickstart).

## Limitations
- **Emulation overhead**: No hardware acceleration in the WASM component. CPU-intensive workloads may run slower than native.
- **No GPU access**: CUDA, Metal, and hardware ML accelerators are not yet available. Support may be added in the future with wasi-nn.
- **Env vars don't cross between shell and Python**: `sandbox.commands.run("export FOO=bar")` is not visible in `sandbox.code.run(...)`. Use the filesystem to share data between the two.

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
pytest sdks/python/tests/ -v -m integration  # Integration tests (requires WASM build)
```

**Building snapshots**

The project uses pre-built Alpine snapshots from `registry.vpod.sh`. To build a custom snapshot:

```bash
./scripts/build-default-snapshot.sh
```

This creates `dist/alpine-3.23.0-256mb.snap`.

> [!IMPORTANT]
> To use it locally, uncomment lines in `resolve_snapshot()` in `crates/vpod/src/main.rs`.

## License

This project is licensed under the **Apache License 2.0**.
See the [LICENSE](LICENSE) file for details.
