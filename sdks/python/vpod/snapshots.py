import hashlib
import json
import shutil
import ssl
import os
import time
import urllib.request
from pathlib import Path

import certifi
import platformdirs

REGISTRY_URL = os.environ.get("VPOD_REGISTRY", "https://registry.vpod.sh/v1/snapshots.json")


def _create_ssl_context():
    """Create SSL context with certifi certificates."""
    return ssl.create_default_context(cafile=certifi.where())


def cache_dir() -> Path:
    base = Path(platformdirs.user_data_dir()) or Path.home() / ".local" / "share"
    return base / "vpod" / "snapshots"


def pull(name: str = "vsnap-base:latest") -> Path:
    """
    Downloads from the registry if not already cached.
    If the cached snapshot is corrupt, force-refreshes the registry and re-downloads.
    """
    override_path = os.environ.get("VPOD_SNAPSHOT")
    if override_path:
        custom_path = Path(override_path)
        if custom_path.exists():
            return custom_path

    registry = fetch_registry()
    snapshot = resolve_snapshot(registry, name)

    dest = cache_dir() / f"{snapshot['id']}.snap"
    meta = dest.with_suffix(".meta")

    if dest.exists() and meta.exists() and meta.read_text().strip() == snapshot["sha256"]:
        if _validate_snapshot_magic(dest):
            return dest

        _REGISTRY_CACHE.unlink(missing_ok=True)
        registry = fetch_registry()
        snapshot = resolve_snapshot(registry, name)
        dest = cache_dir() / f"{snapshot['id']}.snap"
        meta = dest.with_suffix(".meta")
        dest.unlink(missing_ok=True)
        meta.unlink(missing_ok=True)

    dest.parent.mkdir(parents=True, exist_ok=True)

    from ._component import prewarm
    prewarm()

    _download_and_decompress(snapshot["url"], dest, snapshot["sha256"])
    meta.write_text(snapshot["sha256"])
    _prune_stale_snapshots(registry)

    return dest


def _prune_stale_snapshots(registry: list[dict]) -> None:
    known_ids = {snapshot["id"] for snapshot in registry}
    referenced_by_instances = _snapshots_referenced_by_instances()

    for snap_file in list(cache_dir().glob("*.snap")) + list(cache_dir().glob("*.raw")):
        if snap_file.stem in referenced_by_instances:
            continue
        if snap_file.stem not in known_ids:
            snap_file.unlink(missing_ok=True)
            snap_file.with_suffix(".meta").unlink(missing_ok=True)

    for leftover in list(cache_dir().glob("*.tmp")) + list(cache_dir().glob("*.tmp.dl")):
        leftover.unlink(missing_ok=True)


def _snapshots_referenced_by_instances() -> set[str]:
    referenced = set()
    instances_dir = Path.home() / ".vpod" / "instances"
    if not instances_dir.exists():
        return referenced

    for meta_file in instances_dir.glob("*/meta.json"):
        try:
            meta = json.loads(meta_file.read_text())
        except (OSError, json.JSONDecodeError):
            continue
        snapshot_name = meta.get("snapshot", "").removeprefix("snap/")
        if snapshot_name.endswith(".snap"):
            referenced.add(snapshot_name.removesuffix(".snap"))

    return referenced


_REGISTRY_TTL = 86400
_REGISTRY_CACHE = cache_dir() / "snapshots.json"


def catalog() -> list[dict]:
    """Return the list of available snapshots, fetching from the registry if needed."""
    return fetch_registry()


def fetch_registry() -> list[dict]:
    if _REGISTRY_CACHE.exists():
        age = time.time() - _REGISTRY_CACHE.stat().st_mtime
        if age < _REGISTRY_TTL:
            return json.loads(_REGISTRY_CACHE.read_text())["snapshots"]

    try:
        request = urllib.request.Request(
            REGISTRY_URL,
            headers={"User-Agent": f"vpod-py/{_version()}"},
        )
        context = _create_ssl_context()

        with urllib.request.urlopen(request, timeout=10, context=context) as response:
            data = response.read()

        _REGISTRY_CACHE.parent.mkdir(parents=True, exist_ok=True)
        _REGISTRY_CACHE.write_bytes(data)
        return json.loads(data)["snapshots"]
    except Exception as e:
        if _REGISTRY_CACHE.exists():
            return json.loads(_REGISTRY_CACHE.read_text())["snapshots"]
        raise ConnectionError(
            f"Failed to fetch snapshot registry from {REGISTRY_URL}: {e}"
        ) from e


def _version() -> str:
    try:
        from importlib.metadata import version
        return version("vpod")
    except Exception:
        return "0.0.0"


def resolve_snapshot(registry: list[dict], name: str) -> dict:
    want_name, _, want_tag = name.partition(":")
    want_tag = want_tag or "latest"

    for snapshot in registry:
        name_matches = snapshot["name"] == want_name
        tag_matches = want_tag in ("latest", snapshot["tag"])

        if snapshot["id"] == name or (name_matches and tag_matches):
            return snapshot

    available = ", ".join(f"{s['name']}:{s['tag']}" for s in registry)
    raise ValueError(f"Snapshot '{name}' not found. Available: {available}")


def _download_and_decompress(url: str, dest: Path, expected_sha256: str) -> None:
    tmp_compressed = dest.with_suffix(".tmp.dl")
    tmp_raw = dest.with_suffix(".tmp")
    try:
        request = urllib.request.Request(
            url,
            headers={"User-Agent": f"vpod-py/{_version()}"},
        )
        context = _create_ssl_context()
        with urllib.request.urlopen(request, timeout=60, context=context) as response:
            with open(tmp_compressed, "wb") as f:
                shutil.copyfileobj(response, f)

        actual_sha256 = _file_sha256(tmp_compressed)
        if actual_sha256 != expected_sha256:
            raise ValueError(
                f"Checksum mismatch: expected {expected_sha256}, got {actual_sha256}"
            )

        _decompress_file(tmp_compressed, tmp_raw)
        tmp_compressed.unlink()
        shutil.move(tmp_raw, dest)
    except Exception:
        tmp_compressed.unlink(missing_ok=True)
        tmp_raw.unlink(missing_ok=True)
        raise


def _decompress_file(src: Path, dst: Path) -> None:
    with open(src, "rb") as f:
        magic = f.read(4)

    if magic == b"\x04\x22\x4d\x18":
        import lz4.frame
        with lz4.frame.open(str(src), "rb") as f_in, open(dst, "wb") as f_out:
            shutil.copyfileobj(f_in, f_out)
    else:
        shutil.copy2(src, dst)


def _validate_snapshot_magic(path: Path) -> bool:
    try:
        with open(path, "rb") as f:
            return f.read(4) == b"VPOD"
    except OSError:
        return False


def _file_sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()
