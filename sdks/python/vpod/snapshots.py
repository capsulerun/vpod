import hashlib
import json
import shutil
import ssl
import urllib.request
from pathlib import Path

import certifi
import platformdirs

REGISTRY_URL = "https://registry.vpod.sh/v1/snapshots.json"


def _create_ssl_context():
    """Create SSL context with certifi certificates."""
    return ssl.create_default_context(cafile=certifi.where())


def cache_dir() -> Path:
    base = Path(platformdirs.user_data_dir()) or Path.home() / ".local" / "share"
    return base / "vpod" / "snapshots"


def pull(name: str = "alpine:latest") -> Path:
    """
    Resolve and return the local path of a snapshot.
    Downloads from the registry if not already cached.
    """
    registry = fetch_registry()
    snapshot = resolve_snapshot(registry, name)

    dest = cache_dir() / f"{snapshot['id']}.snap"

    if dest.exists() and _file_sha256(dest) == snapshot["sha256"]:
        return dest

    dest.parent.mkdir(parents=True, exist_ok=True)
    _download_to(snapshot["url"], dest, snapshot["sha256"])

    return dest


def fetch_registry() -> list[dict]:
    try:
        context = _create_ssl_context()
        with urllib.request.urlopen(REGISTRY_URL, timeout=10, context=context) as response:
            return json.loads(response.read())["snapshots"]
    except Exception as e:
        raise ConnectionError(
            f"Failed to fetch snapshot registry from {REGISTRY_URL}: {e}"
        ) from e


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


def _download_to(url: str, dest: Path, expected_sha256: str) -> None:
    tmp = dest.with_suffix(".tmp")
    try:
        context = _create_ssl_context()
        opener = urllib.request.build_opener(urllib.request.HTTPSHandler(context=context))
        urllib.request.install_opener(opener)
        urllib.request.urlretrieve(url, tmp)

        actual_sha256 = _file_sha256(tmp)
        if actual_sha256 != expected_sha256:
            tmp.unlink(missing_ok=True)
            raise ValueError(
                f"Checksum mismatch: expected {expected_sha256}, got {actual_sha256}"
            )

        shutil.move(tmp, dest)
    except Exception:
        tmp.unlink(missing_ok=True)
        raise


def _file_sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()
