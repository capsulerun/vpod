"""Per-image engines: a snapshot's own translation, compiled into its own engine.

A catalogue entry may list `engines`. Each is a whole engine component built
from a trace of that snapshot, so the tools installed in it run translated
instead of interpreted. An SDK can run any engine whose interface fingerprint
matches its bundled engine's, whichever release built it.

A sandbox never changes engine after it starts. When a usable engine is not
compiled yet, the sandbox starts on the bundled engine and a detached process
downloads and compiles the image engine, so the next sandbox starts on it.

Files, per engine, named by the sha256 of the download:

    {engines_dir}/{sha256}.download          the bytes as served
    {engines_dir}/{sha256}.{wasmtime}.cwasm  compiled for this wasmtime
    {engines_dir}/{sha256}.meta              JSON; marks the files as ours
    {engines_dir}/{sha256}.origin            the registry that listed it
    {engines_dir}/{sha256}.lock              held while preparing
"""

import gzip
import json
import os
import re
import subprocess
import sys
import time
from importlib.metadata import version as _package_version
from pathlib import Path

from . import snapshots

ENGINE_MODES = ("auto", "default")

_GZIP_MAGIC = b"\x1f\x8b"
_LZ4_FRAME_MAGIC = b"\x04\x22\x4d\x18"
_WASM_MAGIC = b"\0asm"
_STALE_LOCK_SECONDS = 15 * 60

_bundled_interface: str | None = None
_announced: set[str] = set()


def engines_dir() -> Path:
    return snapshots.cache_dir().parent / "engines"


def _wasmtime_version() -> str:
    try:
        return _package_version("wasmtime")
    except Exception:
        return "unknown"


def _sdk_version() -> str:
    return snapshots._version()


def bundled_interface() -> str | None:
    """The interface fingerprint of the engine this SDK ships, or None if unreadable."""
    global _bundled_interface

    override = os.environ.get("VPOD_ENGINE_INTERFACE")
    if override:
        return override

    if _bundled_interface is None:
        from ._component import locate_wasm
        from ._engine_interface import NotAnEngineComponent, fingerprint

        try:
            _bundled_interface = fingerprint(locate_wasm().read_bytes())
        except (OSError, FileNotFoundError, NotAnEngineComponent) as unreadable:
            _note_degraded("could not fingerprint the bundled engine, so image engines are off", unreadable)
            return None

    return _bundled_interface


def _version_key(version: str) -> tuple:
    """Order release versions, with a release above its own pre-releases."""
    match = re.match(r"^(\d+)\.(\d+)\.(\d+)(.*)$", version)
    if match is None:
        return (-1,)
    major, minor, patch, suffix = match.groups()
    suffix_numbers = tuple(int(n) for n in re.findall(r"\d+", suffix))
    return (int(major), int(minor), int(patch), 0 if suffix else 1, suffix_numbers)


def select(snapshot_entry: dict, interface: str | None) -> dict | None:
    """The newest engine listed for this snapshot that this SDK can run."""
    if interface is None:
        return None
    usable = [
        engine
        for engine in snapshot_entry.get("engines") or []
        if engine.get("interface") == interface and engine.get("sha256") and engine.get("url")
    ]
    if not usable:
        return None
    return max(usable, key=lambda engine: _version_key(engine.get("vpod_version", "")))


def _paths(sha256: str) -> dict[str, Path]:
    directory = engines_dir()
    return {
        "download": directory / f"{sha256}.download",
        "cwasm": directory / f"{sha256}.{_wasmtime_version()}.cwasm",
        "meta": directory / f"{sha256}.meta",
        "origin": directory / f"{sha256}.origin",
        "lock": directory / f"{sha256}.lock",
    }


def compiled_path(engine: dict) -> Path | None:
    """The compiled engine, if it is ready to load now."""
    cwasm = _paths(engine["sha256"])["cwasm"]
    return cwasm if cwasm.exists() else None


def announce(snapshot_entry: dict, chosen: dict | None) -> None:
    """Say once per process when a snapshot's engine is older or unusable."""
    listed = snapshot_entry.get("engines") or []
    if not listed:
        return

    sdk_version = _sdk_version()
    snapshot_id = snapshot_entry.get("id", "this snapshot")

    if chosen is None:
        key = f"unusable:{snapshot_id}"
        message = (
            f"vpod: {snapshot_id} has an engine, but none of its builds fits vpod "
            f"{sdk_version}, so it runs on the bundled engine. Rebuild the "
            f"snapshot's engine to get its speed back."
        )
    elif sdk_version != "0.0.0" and _version_key(chosen.get("vpod_version", "")) < _version_key(sdk_version):
        key = f"older:{chosen['sha256']}"
        message = (
            f"vpod: {snapshot_id}'s engine was built with vpod {chosen.get('vpod_version')}. "
            f"It still works; rebuild it to get the fixes in {sdk_version}."
        )
    else:
        return

    if key not in _announced:
        _announced.add(key)
        print(message, file=sys.stderr)


def _note_degraded(what: str, failure: BaseException) -> None:
    from ._component import _note_degraded as note

    note(what, failure)


def _decompressed(download: bytes) -> bytes:
    """Engines are served gzip, which every SDK can decode without a dependency."""
    if download[:2] == _GZIP_MAGIC:
        return gzip.decompress(download)
    if download[:4] == _LZ4_FRAME_MAGIC:
        import lz4.frame

        return lz4.frame.decompress(download)
    return download


def _acquire_lock(lock: Path) -> bool:
    lock.parent.mkdir(parents=True, exist_ok=True)
    try:
        if time.time() - lock.stat().st_mtime > _STALE_LOCK_SECONDS:
            lock.unlink(missing_ok=True)
    except OSError:
        pass
    try:
        descriptor = os.open(lock, os.O_CREAT | os.O_EXCL | os.O_WRONLY)
    except FileExistsError:
        return False
    os.write(descriptor, str(os.getpid()).encode())
    os.close(descriptor)
    return True


def _download(snapshot_id: str, engine: dict, registry_url: str, api_key: str | None) -> None:
    paths = _paths(engine["sha256"])
    try:
        snapshots._download_and_decompress(
            engine["url"], paths["download"], engine["sha256"], registry_url, api_key, decompress=False
        )
    except snapshots.SnapshotAuthError:
        registry = snapshots.fetch_registry(registry_url, api_key, force=True)
        refreshed = next(
            (
                listed
                for snapshot in registry
                if snapshot.get("id") == snapshot_id
                for listed in snapshot.get("engines") or []
                if listed.get("sha256") == engine["sha256"]
            ),
            None,
        )
        if refreshed is None:
            raise
        snapshots._download_and_decompress(
            refreshed["url"], paths["download"], engine["sha256"], registry_url, api_key, decompress=False
        )


def prepare(snapshot_id: str, engine: dict, registry_url: str, api_key: str | None) -> Path | None:
    """Download, verify and compile an engine. Returns the compiled path, or None if another process holds it."""
    from wasmtime import Config, Engine
    from wasmtime.component import Component

    from ._component import _write_cwasm_atomically

    paths = _paths(engine["sha256"])
    if paths["cwasm"].exists():
        return paths["cwasm"]
    if not _acquire_lock(paths["lock"]):
        return None

    try:
        if not paths["download"].exists():
            # Ownership first: a download that dies halfway must still be ours to clean up.
            paths["meta"].write_text(json.dumps({
                "snapshot": snapshot_id,
                "vpod_version": engine.get("vpod_version"),
                "interface": engine.get("interface"),
            }))
            paths["origin"].write_text(snapshots._origin_tag(registry_url, api_key))
            _download(snapshot_id, engine, registry_url, api_key)

        component_bytes = _decompressed(paths["download"].read_bytes())
        if component_bytes[:4] != _WASM_MAGIC:
            paths["download"].unlink(missing_ok=True)
            raise ValueError(f"engine {engine['sha256'][:12]} is not a wasm component")

        config = Config()
        config.parallel_compilation = True
        component = Component(Engine(config), component_bytes)
        _write_cwasm_atomically(paths["cwasm"], component.serialize())

        for other_build in engines_dir().glob(f"{engine['sha256']}.*.cwasm"):
            if other_build != paths["cwasm"]:
                other_build.unlink(missing_ok=True)

        return paths["cwasm"]
    finally:
        paths["lock"].unlink(missing_ok=True)


def prepare_in_background(snapshot_id: str, engine: dict, registry_url: str, api_key: str | None) -> None:
    """Prepare an engine in a detached process, so it survives a short script exiting."""
    paths = _paths(engine["sha256"])
    if paths["cwasm"].exists() or paths["lock"].exists():
        return

    environment = dict(os.environ)
    if api_key is not None:
        environment["VPOD_API_KEY"] = api_key

    try:
        subprocess.Popen(
            [
                sys.executable, "-m", "vpod.engines",
                "--snapshot", snapshot_id,
                "--sha256", engine["sha256"],
                "--registry", registry_url,
            ],
            env=environment,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            stdin=subprocess.DEVNULL,
            start_new_session=True,
            cwd=str(Path(__file__).parents[1]),
        )
    except Exception as failure:
        _note_degraded("could not start preparing the snapshot's engine", failure)


def referenced_by_instances() -> tuple[set[str], bool]:
    """Engines a suspended instance was running on, and whether every instance was read."""
    referenced: set[str] = set()
    instances_dir = Path.home() / ".vpod" / "instances"
    if not instances_dir.exists():
        return referenced, True

    complete = True
    for meta_file in instances_dir.glob("*/meta.json"):
        try:
            meta = json.loads(meta_file.read_text())
        except (OSError, json.JSONDecodeError):
            complete = False
            continue
        engine = meta.get("engine") or {}
        if engine.get("sha256"):
            referenced.add(engine["sha256"])
    return referenced, complete


def prune(registry: list[dict], current_origin: str) -> None:
    """Delete engines the catalogue no longer lists, by the same rules as snapshots."""
    directory = engines_dir()
    if not directory.exists():
        return

    listed_by_snapshot: dict[str, set[str]] = {}
    for snapshot in registry:
        listed_by_snapshot[snapshot.get("id", "")] = {
            engine["sha256"] for engine in snapshot.get("engines") or [] if engine.get("sha256")
        }
    listed = set().union(*listed_by_snapshot.values()) if listed_by_snapshot else set()

    referenced, complete = referenced_by_instances()
    if not complete:
        return

    for meta_file in directory.glob("*.meta"):
        sha256 = meta_file.stem
        if sha256 in listed or sha256 in referenced:
            continue

        paths = _paths(sha256)
        try:
            if paths["origin"].read_text().strip() != current_origin:
                continue
            snapshot_id = json.loads(meta_file.read_text()).get("snapshot", "")
        except (OSError, json.JSONDecodeError):
            continue

        replacements = listed_by_snapshot.get(snapshot_id, set())
        if replacements and not any(_paths(sha)["download"].exists() for sha in replacements):
            continue

        for leftover in directory.glob(f"{sha256}.*"):
            leftover.unlink(missing_ok=True)

    # Only abandoned ones: a younger temporary file may be a compile in progress.
    for leftover in list(directory.glob("*.tmp")) + list(directory.glob("*.tmp.dl")):
        try:
            if time.time() - leftover.stat().st_mtime > _STALE_LOCK_SECONDS:
                leftover.unlink(missing_ok=True)
        except OSError:
            pass


def _main(arguments: list[str]) -> int:
    options = dict(zip(arguments[::2], arguments[1::2]))
    snapshot_id = options.get("--snapshot")
    sha256 = options.get("--sha256")
    registry_url = options.get("--registry")
    if not (snapshot_id and sha256 and registry_url):
        print("usage: python -m vpod.engines --snapshot ID --sha256 HEX --registry URL", file=sys.stderr)
        return 2

    api_key = snapshots._resolve_api_key(None)
    registry = snapshots.fetch_registry(registry_url, api_key)
    engine = next(
        (
            listed
            for snapshot in registry
            if snapshot.get("id") == snapshot_id
            for listed in snapshot.get("engines") or []
            if listed.get("sha256") == sha256
        ),
        None,
    )
    if engine is None:
        return 1

    prepare(snapshot_id, engine, registry_url, api_key)
    return 0


if __name__ == "__main__":
    sys.exit(_main(sys.argv[1:]))
