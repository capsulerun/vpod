import hashlib
import os
import subprocess
import sys
import threading
from pathlib import Path

from platformdirs import user_data_dir
from wasmtime import Config, Engine, Store, WasiConfig
from wasmtime._bindings import (
    wasi_config_allow_ip_name_lookup,
    wasi_config_inherit_network,
    wasmtime_component_linker_add_wasi_http,
    wasmtime_context_set_wasi_http,
)
from wasmtime.component import Component, Linker


_PKG_DIR = Path(__file__).parent
_TARGET_DIR = Path(__file__).parents[4] / "target" / "wasm32-wasip2" / "release"

_AOT_CANDIDATES = (
    _PKG_DIR / "vpod_wasi_lib_aot.wasm",
    _TARGET_DIR / "vpod_wasi_lib_aot.wasm",
)
_BASE_CANDIDATES = (_PKG_DIR / "vpod_wasi_lib.wasm", _TARGET_DIR / "vpod_wasi_lib.wasm")

try:
    from importlib.metadata import version
    _VERSION = version("vpod")
except Exception:
    _VERSION = "0.0.0"

_engine = None
_component = None
_linker = None
_instance_cache = {}
_precompile_started = set()
_thread_compile_started = set()
_cwasm_path_cache = {}
_active_tier = None
_load_lock = threading.RLock()


def _first_existing(candidates) -> Path | None:
    for candidate in candidates:
        if candidate.exists():
            return candidate
    return None


def locate_wasm() -> Path:
    """The best engine module available: AOT if we have it, else interpreter-only."""
    found = _first_existing(_AOT_CANDIDATES) or _first_existing(_BASE_CANDIDATES)
    if found is not None:
        return found

    raise FileNotFoundError(
        f"WASM library not found. Either:\n"
        f"  - Run: ./scripts/build-wasm.sh\n"
        f"  - Or copy vpod_wasi_lib.wasm to {_PKG_DIR}"
    )


def _tier_of(wasm_path: Path) -> str:
    return "aot" if wasm_path.name.endswith("_aot.wasm") else "base"


def _cwasm_cache_path(wasm_path: Path) -> Path:
    """Cache file for a module, named so the directory explains itself."""
    cached = _cwasm_path_cache.get(wasm_path)
    if cached is not None:
        return cached

    digest = hashlib.sha256(wasm_path.read_bytes()).hexdigest()[:16]
    base = Path(user_data_dir()) or Path.home() / ".local" / "share"
    cache_dir = base / "vpod"
    cache_dir.mkdir(parents=True, exist_ok=True)

    path = cache_dir / f"component-{_VERSION}-{_tier_of(wasm_path)}-{digest}.cwasm"
    _cwasm_path_cache[wasm_path] = path
    return path


def _write_cwasm_atomically(cache_path: Path, serialized: bytes) -> None:
    """Publish the .cwasm by rename, never by writing into its final name."""
    tmp_path = cache_path.with_suffix(f".{os.getpid()}.tmp")

    try:
        tmp_path.write_bytes(serialized)
        os.replace(tmp_path, cache_path)
    except Exception:
        tmp_path.unlink(missing_ok=True)
        raise


def _compile_and_cache(engine: Engine, wasm_path: Path) -> Component:
    component = Component.from_file(engine, str(wasm_path))

    try:
        cache_path = _cwasm_cache_path(wasm_path)
        _write_cwasm_atomically(cache_path, component.serialize())
        _prune_stale_cwasm(cache_path)
    except Exception:
        pass

    return component


def _load_cached(engine: Engine, wasm_path: Path) -> Component | None:
    cache_path = _cwasm_cache_path(wasm_path)
    if not cache_path.exists():
        return None

    try:
        return Component.deserialize_file(engine, str(cache_path))
    except Exception:
        cache_path.unlink(missing_ok=True)
        return None


def _precompile_in_background(wasm_path: Path, parallel: bool = False, fallback: bool = False) -> None:
    """Warm the AOT module's cache out of process."""
    key = str(wasm_path)
    if key in _precompile_started:
        return
    _precompile_started.add(key)

    cmd = [sys.executable, "-m", "vpod._precompile", str(wasm_path)]
    if parallel:
        cmd.append("--parallel")
    if fallback:
        cmd.append("--fallback")

    try:
        subprocess.Popen(
            cmd,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            stdin=subprocess.DEVNULL,
            start_new_session=True,
            cwd=str(Path(__file__).parents[1]),
        )
    except Exception:
        pass


def _promote_to_aot(engine: Engine, component: Component) -> None:
    """Swap the process onto an already-compiled AOT component."""
    global _engine, _component, _linker, _active_tier

    with _load_lock:
        if _active_tier == "aot":
            return

        linker = Linker(engine)
        linker.add_wasip2()
        wasmtime_component_linker_add_wasi_http(linker.ptr())

        _engine, _component, _linker = engine, component, linker
        _active_tier = "aot"
        _instance_cache.clear()


def _compile_aot_in_thread(wasm_path: Path) -> None:
    """Compile the AOT module in-process and hand it over without a disk trip."""
    key = str(wasm_path)
    if key in _thread_compile_started:
        return
    _thread_compile_started.add(key)

    def work() -> None:
        try:
            config = Config()
            config.parallel_compilation = os.environ.get("VPOD_AOT_EAGER") != "0"
            engine = Engine(config)
            component = Component.from_file(engine, str(wasm_path))
            _promote_to_aot(engine, component)

            cache_path = _cwasm_cache_path(wasm_path)
            if not cache_path.exists():
                _write_cwasm_atomically(cache_path, component.serialize())
                _prune_stale_cwasm(cache_path)
            retire_base_cwasm()
        except Exception:
            pass

    threading.Thread(target=work, daemon=True, name="vpod-aot-compile").start()


def prewarm() -> None:
    aot_path = _first_existing(_AOT_CANDIDATES)
    if aot_path is None or _cwasm_cache_path(aot_path).exists():
        return

    _precompile_in_background(aot_path, parallel=True)


def _prune_stale_cwasm(active_cache_path: Path) -> None:
    caches = sorted(
        active_cache_path.parent.glob("component-*.cwasm"),
        key=lambda p: p.stat().st_mtime,
        reverse=True,
    )

    for stale in caches[2:]:
        stale.unlink(missing_ok=True)


def retire_base_cwasm() -> None:
    """Drop the base tier's cache once the AOT one exists."""
    base_path = _first_existing(_BASE_CANDIDATES)
    if base_path is None or _first_existing(_AOT_CANDIDATES) is None:
        return

    try:
        _cwasm_cache_path(base_path).unlink(missing_ok=True)
    except OSError:
        pass


def _select_component(engine: Engine, wasm_path: Path) -> Component:
    """Pick a tier and load it, preferring speed-now over speed-later."""
    global _active_tier

    cached = _load_cached(engine, wasm_path)
    if cached is not None:
        _active_tier = _tier_of(wasm_path)
        return cached

    base_path = _first_existing(_BASE_CANDIDATES)
    is_aot = base_path is not None and base_path != wasm_path
    blocking = os.environ.get("VPOD_AOT_BLOCKING") == "1"

    if is_aot and not blocking:
        _precompile_in_background(wasm_path, fallback=True)

        base = _load_cached(engine, base_path)
        if base is None:
            base = _compile_and_cache(engine, base_path)
        _active_tier = "base"

        _compile_aot_in_thread(wasm_path)
        return base

    _active_tier = _tier_of(wasm_path)
    return _compile_and_cache(engine, wasm_path)


def _maybe_upgrade_tier() -> None:
    """Move new sandboxes to the AOT tier once its .cwasm lands."""
    global _engine, _component, _linker, _active_tier

    with _load_lock:
        if _component is None or _active_tier != "base":
            return

        aot_path = _first_existing(_AOT_CANDIDATES)
        if aot_path is None or not _cwasm_cache_path(aot_path).exists():
            return

        engine = Engine()
        component = _load_cached(engine, aot_path)
        if component is None:
            return

        linker = Linker(engine)
        linker.add_wasip2()
        wasmtime_component_linker_add_wasi_http(linker.ptr())

        _engine, _component, _linker = engine, component, linker
        _active_tier = "aot"
        _instance_cache.clear()


def active_tier() -> str | None:
    """Which tier this process is actually running on: 'aot', 'base', or None."""
    return _active_tier


def _get_or_load_component(wasm_path: Path):
    global _engine, _component, _linker

    with _load_lock:
        if _engine is None:
            _engine = Engine()

        if _component is None:
            _component = _select_component(_engine, wasm_path)

        if _linker is None:
            _linker = Linker(_engine)
            _linker.add_wasip2()

            wasmtime_component_linker_add_wasi_http(_linker.ptr())

        return _engine, _component, _linker


def _resolve_exports(store, instance):
    iface_index = instance.get_export_index(store, "vpod:sandbox/executor@0.1.0")
    if iface_index is None:
        raise RuntimeError("WASM component does not export 'vpod:sandbox/executor@0.1.0'")

    lock = threading.RLock()

    def get_export(name: str):
        idx = instance.get_export_index(store, name, iface_index)
        if idx is None:
            raise RuntimeError(f"WASM export '{name}' not found in executor interface")
        func = instance.get_func(store, idx)
        if func is None:
            raise RuntimeError(f"WASM export '{name}' is not a function")

        def call(*args):
            with lock:
                return func(store, *args)

        return call

    return {name: get_export(name) for name in ("session-start", "session-exec", "session-close", "session-suspend", "session-resume")}


def _instance_key(snap_dir: str, mount_dirs: list[str] | None) -> str:
    parts = [snap_dir]
    if mount_dirs:
        parts.extend(sorted(mount_dirs))
    return "|".join(parts)


def load_component(wasm_path: Path, snapshot_path: Path = None, mount_dirs: list[str] | None = None):
    from . import snapshots as _snapshots
    snap_dir = str(_snapshots.cache_dir()) if snapshot_path is None else str(snapshot_path.parent)

    key = _instance_key(snap_dir, mount_dirs)

    _maybe_upgrade_tier()

    if key in _instance_cache:
        return _instance_cache[key]

    engine, component, linker = _get_or_load_component(wasm_path)

    store = Store(engine)
    wasi = WasiConfig()
    wasi.inherit_stdout()
    wasi.inherit_stderr()
    wasi.inherit_stdin()
    wasi.preopen_dir(snap_dir, "snap")

    instances_dir = Path.home() / ".vpod" / "instances"
    instances_dir.mkdir(parents=True, exist_ok=True)
    wasi.preopen_dir(str(instances_dir), "instances")

    if mount_dirs:
        for i, dir_path in enumerate(mount_dirs):
            wasi.preopen_dir(dir_path, f"mount{i}")

    wasi_config_inherit_network(wasi.ptr())
    wasi_config_allow_ip_name_lookup(wasi.ptr(), True)
    store.set_wasi(wasi)

    wasmtime_context_set_wasi_http(store._context())

    instance = linker.instantiate(store, component)
    exports = _resolve_exports(store, instance)

    _instance_cache[key] = (store, exports)

    return store, exports
