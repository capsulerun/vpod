# Python SDK Architecture - Component Model Implementation

## Overview

This document describes the architecture for adding Python SDK support to Capsulev using the WASM Component Model. This approach allows the same Rust codebase to be used both as a CLI tool and as a Python library without code duplication.

## Why Component Model?

The WASM Component Model provides:
- **Multiple interfaces** from the same codebase
- **Type-safe bindings** automatically generated for Python
- **Better separation** between CLI and library concerns
- **Future-proof** - standard approach for WASM interop
- **No breaking changes** - CLI continues to work exactly as before

## Architecture Diagram

```
┌─────────────────────────────────────────────────────────────┐
│                    Capsulev Rust Codebase                   │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  ┌──────────────────┐         ┌──────────────────┐        │
│  │  CLI Interface   │         │ Library Interface│        │
│  │  (world command) │         │  (world library) │        │
│  │                  │         │                  │        │
│  │  - main()        │         │  - session_start │        │
│  │  - run_interactive│        │  - session_exec  │        │
│  │  - run_worker    │         │  - session_close │        │
│  │  - run_session   │         │  - execute       │        │
│  └────────┬─────────┘         └────────┬─────────┘        │
│           │                            │                   │
│           └────────┬───────────────────┘                   │
│                    │                                       │
│         ┌──────────▼──────────┐                           │
│         │  Shared Core Logic  │                           │
│         │                     │                           │
│         │  - MachineBus       │                           │
│         │  - Hart (RISC-V)    │                           │
│         │  - Snapshot         │                           │
│         │  - VirtIO           │                           │
│         └─────────────────────┘                           │
└─────────────────────────────────────────────────────────────┘
            │                           │
            ▼                           ▼
    ┌───────────────┐         ┌──────────────────┐
    │ wasmtime CLI  │         │  wasmtime-py     │
    │ (Terminal)    │         │  (Python SDK)    │
    └───────────────┘         └──────────────────┘
```

## Implementation Plan

### Phase 1: WIT Interface Definition

**File**: `wit/vpod.wit` (new)

Define the component interfaces using WASM Interface Types:

```wit
package vpod:sandbox@0.1.0;

/// Command-line interface for terminal usage
world command {
    import wasi:cli/environment@0.2.0;
    import wasi:cli/exit@0.2.0;
    import wasi:cli/stdin@0.2.0;
    import wasi:cli/stdout@0.2.0;
    import wasi:cli/stderr@0.2.0;
    import wasi:filesystem/types@0.2.0;
    import wasi:filesystem/preopens@0.2.0;
    import wasi:sockets/network@0.2.0;
    import wasi:sockets/tcp@0.2.0;
    import wasi:sockets/udp@0.2.0;
    import wasi:clocks/wall-clock@0.2.0;
}

/// Library interface for Python SDK
world library {
    import wasi:filesystem/types@0.2.0;
    import wasi:filesystem/preopens@0.2.0;
    import wasi:sockets/network@0.2.0;
    import wasi:sockets/tcp@0.2.0;
    import wasi:sockets/udp@0.2.0;
    import wasi:clocks/wall-clock@0.2.0;

    export executor;
}

/// Core execution interface
interface executor {
    /// Result of a code execution
    record execution-result {
        stdout: string,
        stderr: string,
        exit-code: u32,
    }

    /// Execute code once (stateless)
    execute: func(snapshot-path: string, code: string) -> result<execution-result, string>;

    /// Start a persistent Python REPL session
    session-start: func(snapshot-path: string) -> result<session-handle, string>;

    /// Execute code in an existing session (stateful)
    session-exec: func(handle: session-handle, code: string) -> result<string, string>;

    /// Close a session and free resources
    session-close: func(handle: session-handle);

    /// Opaque session handle
    type session-handle = u64;
}
```

### Phase 2: Cargo Configuration

**File**: `crates/wasi-component/Cargo.toml`

Update to support component model:

```toml
[package]
name = "wasi-component"
version = "0.1.0"
edition = "2021"

[dependencies]
machine = { path = "../machine" }
riscv-core = { path = "../riscv-core" }
flate2 = "1.0"
log = "0.4"

# Component model support
wit-bindgen = "0.32"
# For library: thread-safe session storage
lazy_static = "1.4"
parking_lot = "0.12"

[lib]
crate-type = ["cdylib"]
name = "vpod-lib-wasm"

[[bin]]
name = "vpod-wasi-cli"
path = "src/main.rs"

[package.metadata.component]
package = "vpod:sandbox"

[package.metadata.component.target]
path = "wit"

[package.metadata.component.dependencies]
```

### Phase 3: Restructure Source Code

#### 3.1 Create Library Entry Point

**File**: `crates/wasi-component/src/lib.rs` (new)

```rust
//! Library interface for Python SDK using WASM Component Model

mod api;
mod logger;
mod run_interactive;
mod run_worker;
mod run_session;

// Export the component interface
wit_bindgen::generate!({
    world: "library",
    path: "../../wit",
    exports: {
        "vpod:sandbox/executor": api::Executor,
    }
});

// Component exports are handled by wit_bindgen
```

#### 3.2 Implement API Layer

**File**: `crates/wasi-component/src/api/mod.rs` (new)

```rust
pub mod executor;
pub mod session;

pub use executor::Executor;
```

**File**: `crates/wasi-component/src/api/executor.rs` (new)

```rust
use crate::api::session::{SessionManager, SESSION_MANAGER};
use crate::exports::vpod::sandbox::executor::{
    ExecutionResult, Guest, GuestSessionHandle, SessionHandle,
};
use machine::machine_bus::MachineBus;
use machine::snapshot;
use riscv_core::Hart;
use std::io::BufReader;
use flate2::read::GzDecoder;

pub struct Executor;

impl Guest for Executor {
    /// One-shot execution (stateless)
    fn execute(snapshot_path: String, code: String) -> Result<ExecutionResult, String> {
        // Create fresh VM
        let mut bus = MachineBus::new(256 * 1024 * 1024);
        bus.attach_net();
        let mut hart = Hart::new(0x1000);

        // Load snapshot
        let f = std::fs::File::open(&snapshot_path)
            .map_err(|e| format!("Failed to open snapshot: {}", e))?;

        snapshot::restore(
            &mut bus,
            &mut hart,
            &mut BufReader::new(GzDecoder::new(f))
        ).map_err(|e| format!("Failed to restore snapshot: {}", e))?;

        // Execute code via worker mode
        let result = crate::run_worker::execute_code(&mut bus, &mut hart, &code);

        Ok(ExecutionResult {
            stdout: result.stdout,
            stderr: result.stderr,
            exit_code: result.exit_code,
        })
    }

    /// Start a persistent REPL session
    fn session_start(snapshot_path: String) -> Result<SessionHandle, String> {
        SESSION_MANAGER.start_session(snapshot_path)
    }

    /// Execute code in a session (stateful)
    fn session_exec(handle: SessionHandle, code: String) -> Result<String, String> {
        SESSION_MANAGER.exec_code(handle, code)
    }

    /// Close a session
    fn session_close(handle: SessionHandle) {
        SESSION_MANAGER.close_session(handle);
    }
}

impl GuestSessionHandle for SessionHandle {
    // Handle implementation (opaque type)
}
```

**File**: `crates/wasi-component/src/api/session.rs` (new)

```rust
use machine::machine_bus::MachineBus;
use machine::snapshot;
use riscv_core::Hart;
use lazy_static::lazy_static;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::io::BufReader;
use flate2::read::GzDecoder;
use std::sync::atomic::{AtomicU64, Ordering};

pub struct Session {
    pub bus: MachineBus,
    pub hart: Hart,
    pub python_ready: bool,
}

pub struct SessionManager {
    sessions: Mutex<HashMap<u64, Session>>,
    next_id: AtomicU64,
}

lazy_static! {
    pub static ref SESSION_MANAGER: SessionManager = SessionManager {
        sessions: Mutex::new(HashMap::new()),
        next_id: AtomicU64::new(1),
    };
}

impl SessionManager {
    pub fn start_session(&self, snapshot_path: String) -> Result<u64, String> {
        // Create VM
        let mut bus = MachineBus::new(256 * 1024 * 1024);
        bus.attach_net();
        let mut hart = Hart::new(0x1000);

        // Load snapshot
        let f = std::fs::File::open(&snapshot_path)
            .map_err(|e| format!("Failed to open snapshot: {}", e))?;

        snapshot::restore(
            &mut bus,
            &mut hart,
            &mut BufReader::new(GzDecoder::new(f))
        ).map_err(|e| format!("Failed to restore snapshot: {}", e))?;

        // Start Python interpreter
        for b in b"python3 -u -i 2>&1\n" {
            bus.uart.push_rx(*b);
        }

        // Wait for Python to be ready
        crate::run_session::wait_for_python_ready(&mut bus, &mut hart)
            .map_err(|e| format!("Failed to start Python: {}", e))?;

        // Store session
        let session_id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let session = Session {
            bus,
            hart,
            python_ready: true,
        };

        self.sessions.lock().insert(session_id, session);

        Ok(session_id)
    }

    pub fn exec_code(&self, handle: u64, code: String) -> Result<String, String> {
        let mut sessions = self.sessions.lock();
        let session = sessions.get_mut(&handle)
            .ok_or("Invalid session handle")?;

        // Send code to Python REPL
        for b in code.bytes() {
            session.bus.uart.push_rx(b);
        }
        session.bus.uart.push_rx(b'\n');

        // Capture output
        let output = crate::run_session::capture_output_until_prompt(
            &mut session.bus,
            &mut session.hart
        ).map_err(|e| format!("Execution failed: {}", e))?;

        Ok(output)
    }

    pub fn close_session(&self, handle: u64) {
        self.sessions.lock().remove(&handle);
    }
}
```

#### 3.3 Refactor run_worker for Library Use

**File**: `crates/wasi-component/src/run_worker.rs`

Add public function for library use:

```rust
// Existing code stays...

// NEW: Public API for library use
pub fn execute_code(bus: &mut MachineBus, hart: &mut Hart, code: &str) -> ExecutionOutput {
    // Push code to VM
    for b in code.bytes() {
        bus.uart.push_rx(b);
    }
    if !code.ends_with('\n') {
        bus.uart.push_rx(b'\n');
    }

    // Add EOF marker
    for b in b"VPOD_EOF\n" {
        bus.uart.push_rx(*b);
    }

    // Execute and capture output
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut exit_code = 0;

    // ... execution loop similar to existing run() ...

    ExecutionOutput {
        stdout: String::from_utf8_lossy(&stdout).to_string(),
        stderr: String::from_utf8_lossy(&stderr).to_string(),
        exit_code,
    }
}

pub struct ExecutionOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: u32,
}
```

#### 3.4 Refactor run_session for Library Use

**File**: `crates/wasi-component/src/run_session.rs`

Make helper functions public:

```rust
// Change from `fn` to `pub fn`
pub fn wait_for_python_ready(bus: &mut MachineBus, hart: &mut Hart) -> Result<(), String> {
    // Existing implementation...
}

pub fn capture_output_until_prompt(bus: &mut MachineBus, hart: &mut Hart) -> Result<String, String> {
    // Existing implementation...
}

// Keep existing run() for CLI usage
pub fn run(bus: &mut MachineBus, hart: &mut Hart) {
    // Existing implementation...
}
```

#### 3.5 Keep CLI Unchanged

**File**: `crates/wasi-component/src/main.rs`

No changes needed - stays exactly as is!

```rust
// Existing code works unchanged
mod logger;
mod run_interactive;
mod run_worker;
mod run_session;

fn main() {
    // ... existing CLI logic ...
}
```

### Phase 4: Build Configuration

**File**: `.cargo/config.toml` (update or create)

```toml
[build]
target = "wasm32-wasip2"

[target.wasm32-wasip2]
rustflags = ["-C", "target-feature=+bulk-memory,+mutable-globals"]
```

### Phase 5: Build Scripts

**File**: `scripts/build-python-sdk.sh` (new)

```bash
#!/bin/bash
set -e

echo "Building Capsulev Python SDK components..."

# Build the library component
echo "→ Building library component..."
cargo component build --target wasm32-wasip2 --release --lib

# Build the CLI component (existing)
echo "→ Building CLI component..."
cargo build --target wasm32-wasip2 --release --bin vpod-wasi-cli

echo "✓ Build complete!"
echo ""
echo "Library: target/wasm32-wasip2/release/vpod-wasi-lib.wasm"
echo "CLI:     target/wasm32-wasip2/release/vpod-wasi-cli.wasm"
```

### Phase 6: Python SDK Implementation

**File**: `sdks/python/pyproject.toml` (new)

```toml
[build-system]
requires = ["setuptools>=61.0", "wheel"]
build-backend = "setuptools.build_meta"

[project]
name = "vpod"
version = "0.1.0"
description = "Secure Python code execution sandbox powered by RISC-V and WASM"
readme = "README.md"
requires-python = ">=3.8"
license = {text = "Apache-2.0"}
authors = [
    {name = "Capsulev Team"}
]
dependencies = [
    "wasmtime>=25.0.0",
]

[project.optional-dependencies]
dev = [
    "pytest>=7.0",
    "black>=23.0",
    "mypy>=1.0",
]

[tool.setuptools.packages.find]
where = ["."]
include = ["vpod*"]
```

**File**: `sdks/python/vpod/__init__.py`

```python
"""Capsulev Python SDK - Secure code execution sandbox."""

from .sandbox import Sandbox
from .execution import Execution

__version__ = "0.1.0"
__all__ = ["Sandbox", "Execution"]
```

**File**: `sdks/python/vpod/execution.py`

```python
"""Execution result for code run in sandbox."""

from dataclasses import dataclass
from typing import Optional


@dataclass
class Execution:
    """Result of code execution in the sandbox."""

    text: str
    """The output text (stdout) from the execution."""

    error: Optional[str] = None
    """Error output (stderr) if any."""

    exit_code: int = 0
    """Exit code of the execution (0 = success)."""

    @property
    def success(self) -> bool:
        """Whether the execution succeeded (exit code 0)."""
        return self.exit_code == 0
```

**File**: `sdks/python/vpod/sandbox.py`

```python
"""Capsulev sandbox for secure Python code execution."""

from pathlib import Path
from typing import Optional
from wasmtime import Config, Engine, Store, Module, Linker
import wasmtime.loader  # Component model support

from .execution import Execution


class Sandbox:
    """
    Capsulev sandbox for secure code execution.

    Example:
        # Session REPL (stateful)
        with Sandbox.create() as sandbox:
            sandbox.run_code("x = 1")
            result = sandbox.run_code("x += 1; x")
            print(result.text)  # Output: 2

        # One-shot execution (stateless)
        sandbox = Sandbox.create()
        result = sandbox.exec("python3 -c 'print(2+2)'")
        print(result.text)  # Output: 4
    """

    def __init__(self, snapshot_name: str = "alpine-3.23.0-256mb"):
        self.snapshot_name = snapshot_name

        # Find paths
        root = Path(__file__).parent.parent.parent.parent
        self.snapshot_path = str(root / "dist" / f"{snapshot_name}.snap")
        wasm_path = root / "target" / "wasm32-wasip2" / "release" / "vpod_lib.wasm"

        if not wasm_path.exists():
            raise RuntimeError(
                f"WASM library not found at {wasm_path}. "
                "Run: ./scripts/build-python-sdk.sh"
            )

        # Initialize wasmtime with component model support
        config = Config()
        config.wasm_component_model = True

        self._engine = Engine(config)
        self._store = Store(self._engine)

        # Load component
        self._component = wasmtime.loader.Component.from_file(self._engine, str(wasm_path))
        self._linker = Linker(self._engine)
        self._linker.define_wasi()

        # Instantiate
        self._instance = self._linker.instantiate(self._store, self._component)

        # Get exported functions
        self._execute = self._instance.exports(self._store)["execute"]
        self._session_start = self._instance.exports(self._store)["session-start"]
        self._session_exec = self._instance.exports(self._store)["session-exec"]
        self._session_close = self._instance.exports(self._store)["session-close"]

        self._session_id: Optional[int] = None

    @classmethod
    def create(cls, snapshot_name: str = "alpine-3.23.0-256mb") -> "Sandbox":
        """Create a new sandbox instance."""
        return cls(snapshot_name)

    def __enter__(self):
        """Start a REPL session when entering context."""
        result = self._session_start(self._store, self.snapshot_path)

        if result["is_err"]:
            raise RuntimeError(f"Failed to start session: {result['err']}")

        self._session_id = result["ok"]
        return self

    def __exit__(self, *args):
        """Close the REPL session when exiting context."""
        if self._session_id is not None:
            self._session_close(self._store, self._session_id)
            self._session_id = None

    def run_code(self, code: str, timeout: Optional[float] = None) -> Execution:
        """
        Execute Python code in the sandbox session.
        State persists between calls within the same 'with' block.

        Args:
            code: Python code to execute
            timeout: Execution timeout in seconds (not yet implemented)

        Returns:
            Execution result with output text

        Raises:
            RuntimeError: If session not started or execution fails
        """
        if self._session_id is None:
            raise RuntimeError(
                "Session not started. Use 'with Sandbox.create() as sandbox:'"
            )

        result = self._session_exec(self._store, self._session_id, code)

        if result["is_err"]:
            return Execution(text="", error=result["err"], exit_code=1)

        output = result["ok"]
        return Execution(text=output, exit_code=0)

    def exec(self, command: str, timeout: Optional[float] = None) -> Execution:
        """
        Execute a one-shot command (stateless).
        Each call is independent - no state preserved between calls.

        Args:
            command: Shell command to execute
            timeout: Execution timeout in seconds (not yet implemented)

        Returns:
            Execution result with stdout, stderr, and exit code
        """
        result = self._execute(self._store, self.snapshot_path, command)

        if result["is_err"]:
            return Execution(text="", error=result["err"], exit_code=1)

        exec_result = result["ok"]
        return Execution(
            text=exec_result["stdout"],
            error=exec_result["stderr"] or None,
            exit_code=exec_result["exit_code"]
        )
```

**File**: `sdks/python/README.md` (new)

```markdown
# Capsulev Python SDK

Secure Python code execution sandbox powered by RISC-V and WebAssembly.

## Installation

```bash
pip install vpod
```

## Usage

### Session REPL (Stateful)

State persists between `run_code()` calls within a context:

```python
from vpod import Sandbox

with Sandbox.create() as sandbox:
    sandbox.run_code("x = 1")
    result = sandbox.run_code("x += 1; x")
    print(result.text)  # Output: 2
```

### One-shot Execution (Stateless)

Each call is independent:

```python
from vpod import Sandbox

sandbox = Sandbox.create()
result = sandbox.exec("python3 -c 'print(2+2)'")
print(result.text)  # Output: 4
```

## Requirements

- Python 3.8+
- wasmtime-py 25.0+
```

### Phase 7: Testing

**File**: `sdks/python/tests/test_sandbox.py` (new)

```python
"""Tests for Capsulev Python SDK."""

import pytest
from vpod import Sandbox, Execution


def test_session_repl_state_persistence():
    """Test that state persists in session REPL."""
    with Sandbox.create() as sandbox:
        sandbox.run_code("x = 1")
        result = sandbox.run_code("x += 1; x")
        assert result.success
        assert "2" in result.text


def test_session_repl_multiple_variables():
    """Test multiple variables in session."""
    with Sandbox.create() as sandbox:
        sandbox.run_code("a = 10")
        sandbox.run_code("b = 20")
        result = sandbox.run_code("print(a + b)")
        assert result.success
        assert "30" in result.text


def test_one_shot_execution():
    """Test stateless one-shot execution."""
    sandbox = Sandbox.create()
    result = sandbox.exec("echo 'hello world'")
    assert result.success
    assert "hello world" in result.text


def test_one_shot_no_state():
    """Test that one-shot calls don't share state."""
    sandbox = Sandbox.create()

    result1 = sandbox.exec("python3 -c 'x = 1; print(x)'")
    assert "1" in result1.text

    # x should not exist in second call
    result2 = sandbox.exec("python3 -c 'print(x)'")
    assert not result2.success


def test_session_error_handling():
    """Test error handling in session REPL."""
    with Sandbox.create() as sandbox:
        result = sandbox.run_code("1 / 0")
        assert not result.success
        assert result.error or "ZeroDivisionError" in result.text


def test_session_requires_context():
    """Test that run_code requires context manager."""
    sandbox = Sandbox.create()
    with pytest.raises(RuntimeError, match="Session not started"):
        sandbox.run_code("x = 1")


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
```

## Summary of Changes

### New Files

1. `wit/vpod.wit` - WASM Component Model interface definition
2. `crates/wasi-component/src/lib.rs` - Library entry point
3. `crates/wasi-component/src/api/mod.rs` - API module
4. `crates/wasi-component/src/api/executor.rs` - Executor implementation
5. `crates/wasi-component/src/api/session.rs` - Session manager
6. `scripts/build-python-sdk.sh` - Build script for SDK
7. `sdks/python/pyproject.toml` - Python package configuration
8. `sdks/python/vpod/__init__.py` - Package entry point
9. `sdks/python/vpod/execution.py` - Execution result class
10. `sdks/python/vpod/sandbox.py` - Main Sandbox class
11. `sdks/python/vpod/README.md` - Python SDK documentation
12. `sdks/python/tests/test_sandbox.py` - Test suite

### Modified Files

1. `crates/wasi-component/Cargo.toml` - Add component model support
2. `crates/wasi-component/src/run_worker.rs` - Add public `execute_code()` function
3. `crates/wasi-component/src/run_session.rs` - Make helper functions public
4. `.cargo/config.toml` - Add WASM build configuration

### Unchanged Files

- `crates/wasi-component/src/main.rs` - CLI continues to work as-is
- `crates/wasi-component/src/run_interactive.rs` - No changes
- All other existing files

## Build & Test Commands

```bash
# Build both CLI and library
./scripts/build-python-sdk.sh

# Install Python SDK in dev mode
cd sdks/python
pip install -e .[dev]

# Run tests
pytest tests/ -v

# Use in Python
python -c "
from vpod import Sandbox
with Sandbox.create() as s:
    result = s.run_code('print(2+2)')
    print(result.text)
"
```

## Benefits of This Approach

1. **Zero Code Duplication** - CLI and library share all core logic
2. **Type Safety** - WIT interface ensures correct Python bindings
3. **Clean Separation** - Library and CLI concerns separated by worlds
4. **Future Proof** - Standard WASM Component Model approach
5. **No Breaking Changes** - Existing CLI works exactly as before
6. **Better Testing** - Can test both interfaces independently
7. **Maintainability** - Single codebase, multiple interfaces

## Migration Path

1. Implement WIT interface and library world
2. Test library build works
3. Implement Python SDK
4. Test Python SDK
5. Document both interfaces
6. Release both CLI and SDK together

The CLI continues to work unchanged throughout this entire process!
