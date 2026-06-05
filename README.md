<div align="center">

# `Vpod`

[![CI](https://img.shields.io/github/actions/workflow/status/capsulerun/vpod/ci.yml?branch=main&label=CI)](https://github.com/capsulerun/vpod/actions/workflows/ci.yml)

[Usage](#usage) • [Contributing](#contributing) • [Issues](https://github.com/capsulerun/vpod/issues/new)

</div>

---

## Overview

A `vpod` is a lightweight, portable sandbox that gives an untrusted process an instant Linux environment. It uses the RISC‑V architecture and runs entirely inside WebAssembly.

- **Fast startup** : Boot in under a second.
- **Portable** : Runs anywhere without any setup required.
- **Isolated** : All execution state stays inside the WASM sandboxes.

## How it works

A vpod runs a RISC‑V virtual machine compiled to WebAssembly. The core implements the RV64GCV specification.

When you start a vpod, it boots from a snapshot. A snapshot is a saved VM state containing a Linux userspace, packages, and a pre-loaded filesystem. This gives you a ready environment in under a second.

The WASM component communicates with the host through WASI 0.2. Host functions provide controlled access to the filesystem, networking, and standard I/O. All execution state (CPU registers, memory, filesystem) stays isolated inside the WASM component.

### RV64GCV

**G — General-purpose extensions**
- **I** : Base 64-bit integer instruction set.
- **M** : Hardware multiply and divide, useful for hashing and cryptography.
- **A** : Atomic operations for thread-safe programs.
- **F/D** : Single and double-precision floating-point, suited for scientific computing and ML inference.

**C — Compressed instructions**
Reduces code size by roughly 30%, improving instruction fetch speed and memory efficiency. This matters when running a full Linux userspace inside a memory-constrained WASM environment.

**V — Vector extension**
Adds SIMD operations for parallel data processing. Accelerates array operations, data transformations, and numerical workloads common in AI agent execution.


## Usage

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

for s in snapshots.fetch_registry():
    print(s["name"], s["tag"])

path = snapshots.pull("alpine:latest")
```

## Limitations
TODO

## Contributing
TODO

## License

This project is licensed under the **Apache License 2.0**.
See the [LICENSE](LICENSE) file for details.
