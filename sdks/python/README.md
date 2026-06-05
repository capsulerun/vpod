# vpod Python SDK

Secure code execution sandbox powered by RISC-V and WebAssembly.

## Installation

```bash
pip install vpod
```

## Usage

### Shell commands (stateless)

Each call gets a fresh VM — no shared state:

```python
from vpod import Sandbox

sbx = Sandbox.create()
result = sbx.commands.run("echo hello")
print(result.stdout)    # hello
print(result.exit_code) # 0
```

### Persistent session

All calls share the same running VM:

```python
from vpod import Sandbox

with Sandbox.create() as sbx:
    sbx.commands.run("export Foo=Bar")
    sbx.commands.run("touch /tmp/data.csv")

    result = sbx.commands.run("echo $Foo")
    print(result.stdout)  # Bar
```

### Code execution

Run interpreted code directly inside a session:

```python
from vpod import Sandbox

with Sandbox.create() as sbx:
    result = sbx.code.run("print(2 + 2)")
    print(result.text)  # 4

    result = sbx.code.run("[x**2 for x in range(5)]")
    print(result.text)  # [0, 1, 4, 9, 16]
```

### Snapshots

The first call to `Sandbox.create()` downloads the VM snapshot (~50MB) and caches it locally at `~/.local/share/vpod/snapshots/`. Subsequent calls use the cache instantly.

To pre-download (e.g. in a Dockerfile or CI setup):

```python
from vpod import snapshots

for s in snapshots.fetch_registry():
    print(s["name"], s["tag"])

path = snapshots.pull("alpine:latest")
```

## Requirements

- Python 3.10+
- wasmtime-py 25.0+
- platformdirs 4.0+
