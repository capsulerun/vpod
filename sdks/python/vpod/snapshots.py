import hashlib
import json
import shutil
import ssl
import os
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

import certifi
import platformdirs

PUBLIC_REGISTRY_URL = "https://registry.vpod.sh/v1/snapshots.json"
PRIVATE_REGISTRY_URL = "https://api.vpod.sh/v1/snapshots.json"


REGISTRY_URL = os.environ.get("VPOD_REGISTRY", PUBLIC_REGISTRY_URL)


def _resolve_api_key(api_key: str | None) -> str | None:
    if api_key:
        return api_key
    return os.environ.get("VPOD_API_KEY") or None


def _check_api_key_kind(api_key: str) -> None:
    if api_key.startswith("vpod_pk_"):
        raise ValueError(
            "vpod: this is a publishable key (vpod_pk_), and Python is not a "
            "browser. Publishable keys are protected by an allowlist of "
            "Origins, and nothing outside a browser sends an Origin the server "
            "can trust, so the key buys you nothing here. Use a secret key "
            "(vpod_sk_) instead."
        )
    if not api_key.startswith("vpod_sk_"):
        raise ValueError(
            "vpod: an API key must start with vpod_sk_ (server side) or "
            "vpod_pk_ (browser). Got a key starting with "
            f"{api_key[:8]!r}."
        )


def _resolve_registry_url(registry_url: str | None, api_key: str | None = None) -> str:
    if registry_url:
        return registry_url
    from_environment = os.environ.get("VPOD_REGISTRY")
    if from_environment:
        return from_environment
    return PRIVATE_REGISTRY_URL if api_key else PUBLIC_REGISTRY_URL


def _key_fingerprint(api_key: str) -> str:
    """Identifies a key on disk without ever writing the key to disk."""
    return hashlib.sha256(api_key.encode()).hexdigest()[:12]


def _origin_tag(registry_url: str, api_key: str | None) -> str:
    return registry_url if api_key is None else f"{registry_url}#{_key_fingerprint(api_key)}"


def _same_origin(url: str, other: str) -> bool:
    from urllib.parse import urlsplit

    left, right = urlsplit(url), urlsplit(other)
    return (left.scheme, left.hostname, left.port) == (
        right.scheme,
        right.hostname,
        right.port,
    )


def _request_headers(url: str, registry_url: str, api_key: str | None) -> dict[str, str]:
    headers = {"User-Agent": f"vpod-py/{_version()}"}
    if api_key is not None and _same_origin(url, registry_url):
        headers["Authorization"] = f"Bearer {api_key}"
    return headers


def _create_ssl_context():
    """Create SSL context with certifi certificates."""
    return ssl.create_default_context(cafile=certifi.where())


def cache_dir() -> Path:
    base = Path(platformdirs.user_data_dir()) or Path.home() / ".local" / "share"
    return base / "vpod" / "snapshots"


class SnapshotAuthError(RuntimeError):
    """The registry or the blob store refused the key, not the network."""


def pull(
    name: str = "vsnap-base:latest",
    registry_url: str | None = None,
    api_key: str | None = None,
) -> Path:
    """
    Downloads from the registry if not already cached.
    If the cached snapshot is corrupt, force-refreshes the registry and re-downloads.
    """
    override_path = os.environ.get("VPOD_SNAPSHOT")
    if override_path:
        custom_path = Path(override_path)
        if custom_path.exists():
            return custom_path

    api_key = _resolve_api_key(api_key)
    if api_key is not None:
        _check_api_key_kind(api_key)

    resolved_registry = _resolve_registry_url(registry_url, api_key)
    origin = _origin_tag(resolved_registry, api_key)
    registry = fetch_registry(resolved_registry, api_key)
    snapshot = resolve_snapshot(registry, name, resolved_registry, api_key is not None)

    dest = cache_dir() / f"{snapshot['id']}.snap"
    meta = dest.with_suffix(".meta")

    if dest.exists() and meta.exists() and meta.read_text().strip() == snapshot["sha256"]:
        if _validate_snapshot_magic(dest):
            _record_origin(dest, origin)
            return dest

        _registry_cache_path(resolved_registry, api_key).unlink(missing_ok=True)
        registry = fetch_registry(resolved_registry, api_key)
        snapshot = resolve_snapshot(registry, name, resolved_registry, api_key is not None)
        dest = cache_dir() / f"{snapshot['id']}.snap"
        meta = dest.with_suffix(".meta")
        dest.unlink(missing_ok=True)
        meta.unlink(missing_ok=True)

    dest.parent.mkdir(parents=True, exist_ok=True)

    from ._component import prewarm
    prewarm()

    try:
        _download_and_decompress(
            snapshot["url"], dest, snapshot["sha256"], resolved_registry, api_key
        )
    except SnapshotAuthError:
        _registry_cache_path(resolved_registry, api_key).unlink(missing_ok=True)
        registry = fetch_registry(resolved_registry, api_key, force=True)
        snapshot = resolve_snapshot(registry, name, resolved_registry, api_key is not None)
        dest = cache_dir() / f"{snapshot['id']}.snap"
        meta = dest.with_suffix(".meta")
        try:
            _download_and_decompress(
                snapshot["url"], dest, snapshot["sha256"], resolved_registry, api_key
            )
        except SnapshotAuthError as refused:
            raise SnapshotAuthError(
                f"vpod: {snapshot['id']} was refused again after refreshing the "
                f"catalogue from {resolved_registry}. A stale signed URL would "
                f"have been fixed by that refresh, so this is the key or the "
                f"org, not the cache. ({refused})"
            ) from refused

    meta.write_text(snapshot["sha256"])
    _record_origin(dest, origin)
    _prune_stale_snapshots(registry, origin)

    return dest


def _record_origin(dest: Path, origin: str) -> None:
    origin_file = dest.with_suffix(".origin")
    if origin_file.exists():
        return
    try:
        origin_file.write_text(origin)
    except OSError as unwritable:
        print(f"vpod: cannot record the origin of {dest.name}: {unwritable}", file=sys.stderr)


def _prune_stale_snapshots(registry: list[dict], current_origin: str) -> None:
    known_ids = {snapshot["id"] for snapshot in registry}
    referenced_by_instances, references_are_complete = _snapshots_referenced_by_instances()

    if references_are_complete:
        for snap_file in list(cache_dir().glob("*.snap")) + list(cache_dir().glob("*.raw")):
            if snap_file.stem in referenced_by_instances or snap_file.stem in known_ids:
                continue

            meta_file = snap_file.with_suffix(".meta")
            if not meta_file.exists():
                continue

            origin_file = snap_file.with_suffix(".origin")
            try:
                recorded_origin = origin_file.read_text().strip()
            except OSError:
                continue
            if recorded_origin != current_origin:
                continue

            print(
                f"vpod: removing {snap_file.name}, which we downloaded and the "
                f"registry no longer lists",
                file=sys.stderr,
            )
            snap_file.unlink(missing_ok=True)
            meta_file.unlink(missing_ok=True)
            origin_file.unlink(missing_ok=True)

    for leftover in list(cache_dir().glob("*.tmp")) + list(cache_dir().glob("*.tmp.dl")):
        leftover.unlink(missing_ok=True)


def _snapshots_referenced_by_instances() -> tuple[set[str], bool]:
    """Snapshots a suspended instance still needs, and whether we read them all."""
    referenced: set[str] = set()
    instances_dir = Path.home() / ".vpod" / "instances"
    if not instances_dir.exists():
        return referenced, True

    complete = True
    for meta_file in instances_dir.glob("*/meta.json"):
        try:
            meta = json.loads(meta_file.read_text())
        except (OSError, json.JSONDecodeError) as unreadable:
            print(
                f"vpod: cannot read {meta_file}, skipping snapshot cleanup: {unreadable}",
                file=sys.stderr,
            )
            complete = False
            continue
        snapshot_name = meta.get("snapshot", "").removeprefix("snap/")
        if snapshot_name.endswith(".snap"):
            referenced.add(snapshot_name.removesuffix(".snap"))

    return referenced, complete


_REGISTRY_TTL = 86400
_REGISTRY_CACHE = cache_dir() / "snapshots.json"
_REGISTRY_VERSION_MARKER = cache_dir() / "snapshots.json.sdkver"


def _registry_cache_path(registry_url: str, api_key: str | None = None) -> Path:
    if registry_url == PUBLIC_REGISTRY_URL and api_key is None:
        return _REGISTRY_CACHE
    material = registry_url if api_key is None else f"{registry_url}#{_key_fingerprint(api_key)}"
    digest = hashlib.sha256(material.encode()).hexdigest()[:8]

    return cache_dir() / f"snapshots-{digest}.json"


def catalog(registry_url: str | None = None, api_key: str | None = None) -> list[dict]:
    """Return the list of available snapshots, fetching from the registry if needed."""
    api_key = _resolve_api_key(api_key)
    if api_key is not None:
        _check_api_key_kind(api_key)
    return fetch_registry(_resolve_registry_url(registry_url, api_key), api_key)


def _registry_cache_version_matches() -> bool:
    try:
        return _REGISTRY_VERSION_MARKER.read_text().strip() == _version()
    except OSError:
        return False


def fetch_registry(
    registry_url: str | None = None,
    api_key: str | None = None,
    force: bool = False,
) -> list[dict]:
    api_key = _resolve_api_key(api_key)
    if api_key is not None:
        _check_api_key_kind(api_key)
    registry_url = _resolve_registry_url(registry_url, api_key)
    cache_path = _registry_cache_path(registry_url, api_key)

    if not force and cache_path.exists() and _registry_cache_version_matches():
        age = time.time() - cache_path.stat().st_mtime
        if age < _REGISTRY_TTL:
            return json.loads(cache_path.read_text())["snapshots"]

    try:
        request = urllib.request.Request(
            registry_url,
            headers=_request_headers(registry_url, registry_url, api_key),
        )
        context = _create_ssl_context()

        with urllib.request.urlopen(request, timeout=10, context=context) as response:
            data = response.read()

        cache_path.parent.mkdir(parents=True, exist_ok=True)
        cache_path.write_bytes(data)
        _REGISTRY_VERSION_MARKER.write_text(_version())
        return json.loads(data)["snapshots"]
    except urllib.error.HTTPError as http_error:
        if http_error.code in (401, 403):
            reason = (
                "The key may be revoked, or it may belong to a different "
                "organisation than the snapshot you asked for."
                if api_key
                else "No API key was sent. Set VPOD_API_KEY or pass api_key=."
            )
            raise SnapshotAuthError(
                f"vpod: {registry_url} refused the request "
                f"({http_error.code}). {reason}"
            ) from http_error
        if cache_path.exists():
            return json.loads(cache_path.read_text())["snapshots"]
        raise ConnectionError(
            f"Failed to fetch snapshot registry from {registry_url}: {http_error}"
        ) from http_error
    except Exception as e:
        if cache_path.exists():
            return json.loads(cache_path.read_text())["snapshots"]
        raise ConnectionError(
            f"Failed to fetch snapshot registry from {registry_url}: {e}"
        ) from e


def _version() -> str:
    try:
        from importlib.metadata import version
        return version("vpod")
    except Exception:
        return "0.0.0"


def resolve_snapshot(
    registry: list[dict],
    name: str,
    registry_url: str | None = None,
    authenticated: bool = False,
) -> dict:
    want_name, _, want_tag = name.partition(":")
    want_tag = want_tag or "latest"

    for snapshot in registry:
        name_matches = snapshot["name"] == want_name
        tag_matches = want_tag in ("latest", snapshot["tag"])

        if snapshot["id"] == name or (name_matches and tag_matches):
            return snapshot

    available = ", ".join(f"{s['name']}:{s['tag']}" for s in registry) or "nothing"
    searched = f" in {registry_url}" if registry_url else ""
    credentials = (
        " An API key WAS sent, so this catalogue is what that key can reach."
        if authenticated
        else " No API key was sent, so only public snapshots were searched."
    )
    raise ValueError(
        f"Snapshot '{name}' not found{searched}. Available: {available}.{credentials}"
    )


def _download_and_decompress(
    url: str,
    dest: Path,
    expected_sha256: str,
    registry_url: str | None = None,
    api_key: str | None = None,
) -> None:
    tmp_compressed = dest.with_suffix(".tmp.dl")
    tmp_raw = dest.with_suffix(".tmp")
    try:
        request = urllib.request.Request(
            url,
            headers=_request_headers(url, registry_url or url, api_key),
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
    except urllib.error.HTTPError as http_error:
        tmp_compressed.unlink(missing_ok=True)
        tmp_raw.unlink(missing_ok=True)
        if http_error.code in (401, 403):
            raise SnapshotAuthError(
                f"vpod: {url} returned {http_error.code}"
            ) from http_error
        raise
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
