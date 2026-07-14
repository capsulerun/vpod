import hashlib
from pathlib import Path

from platformdirs import user_data_dir
from wasmtime import Engine, Store, WasiConfig
from wasmtime._bindings import wasi_config_allow_ip_name_lookup, wasi_config_inherit_network
from wasmtime.component import Component, Linker

_BUNDLED_WASM = Path(__file__).parent / "vpod_wasi_lib.wasm"
_REPO_WASM = Path(__file__).parents[4] / "target" / "wasm32-wasip2" / "release" / "vpod_wasi_lib.wasm"

try:
    from importlib.metadata import version
    _VERSION = version("vpod")
except Exception:
    _VERSION = "0.0.0"

_engine = None
_component = None
_linker = None
_instance_cache = {}


def locate_wasm() -> Path:
    for candidate in (_BUNDLED_WASM, _REPO_WASM):
        if candidate.exists():
            return candidate

    raise FileNotFoundError(
        f"WASM library not found. Either:\n"
        f"  - Run: cargo build -p wasi-component --lib --release --target wasm32-wasip2\n"
        f"  - Or copy vpod_wasi_lib.wasm to {_BUNDLED_WASM}"
    )


def _cwasm_cache_path(wasm_path: Path) -> Path:
    wasm_bytes = wasm_path.read_bytes()
    digest = hashlib.sha256(wasm_bytes).hexdigest()[:16]
    base = Path(user_data_dir()) or Path.home() / ".local" / "share"
    cache_dir = base / "vpod"
    cache_dir.mkdir(parents=True, exist_ok=True)

    return cache_dir / f"component-{_VERSION}-{digest}.cwasm"


def _load_or_compile_component(engine: Engine, wasm_path: Path) -> Component:
    cache_path = _cwasm_cache_path(wasm_path)

    if cache_path.exists():
        try:
            return Component.deserialize_file(engine, str(cache_path))
        except Exception:
            cache_path.unlink(missing_ok=True)

    component = Component.from_file(engine, str(wasm_path))

    try:
        serialized = component.serialize()
        cache_path.write_bytes(serialized)
        _prune_stale_cwasm(cache_path)
    except Exception:
        pass

    return component


def _prune_stale_cwasm(active_cache_path: Path) -> None:
    caches = sorted(
        active_cache_path.parent.glob("component-*.cwasm"),
        key=lambda p: p.stat().st_mtime,
        reverse=True,
    )

    for stale in caches[2:]:
        stale.unlink(missing_ok=True)


def _get_or_load_component(wasm_path: Path):
    global _engine, _component, _linker

    if _engine is None:
        _engine = Engine()

    if _component is None:
        _component = _load_or_compile_component(_engine, wasm_path)

    if _linker is None:
        _linker = Linker(_engine)
        _linker.add_wasip2()

    return _engine, _component, _linker


def _resolve_exports(store, instance):
    iface_index = instance.get_export_index(store, "vpod:sandbox/executor@0.2.0")
    if iface_index is None:
        raise RuntimeError("WASM component does not export 'vpod:sandbox/executor@0.2.0'")

    def get_export(name: str):
        idx = instance.get_export_index(store, name, iface_index)
        if idx is None:
            raise RuntimeError(f"WASM export '{name}' not found in executor interface")
        func = instance.get_func(store, idx)
        if func is None:
            raise RuntimeError(f"WASM export '{name}' is not a function")
        return lambda *args: func(store, *args)

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

    instance = linker.instantiate(store, component)
    exports = _resolve_exports(store, instance)

    _instance_cache[key] = (store, exports)

    return store, exports
