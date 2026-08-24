"""Auth for private snapshots.

Every test here corresponds to a line in
docs/design/console/sdk-private-snapshots.md#testing. They are unit tests
against a local HTTP server rather than integration tests, because the failures
they guard are silent ones: a key that goes to the wrong origin, a retry loop
that never terminates, a cache that one org reads out of another's.
"""

import http.server
import json
import threading
from pathlib import Path

import pytest

from vpod import snapshots

_REAL_PULL = snapshots.pull


# --- the rules, with no network at all --------------------------------------

def test_registry_precedence_is_one_chain():
    assert snapshots._resolve_registry_url(None, None) == snapshots.PUBLIC_REGISTRY_URL
    assert snapshots._resolve_registry_url(None, "vpod_sk_k") == snapshots.PRIVATE_REGISTRY_URL
    assert snapshots._resolve_registry_url("https://example.com/c.json", "vpod_sk_k") == "https://example.com/c.json"


def test_explicit_key_beats_the_environment(monkeypatch):
    monkeypatch.setenv("VPOD_API_KEY", "vpod_sk_from_env")
    assert snapshots._resolve_api_key("vpod_sk_explicit") == "vpod_sk_explicit"
    assert snapshots._resolve_api_key(None) == "vpod_sk_from_env"


def test_environment_registry_beats_the_key_default(monkeypatch):
    monkeypatch.setenv("VPOD_REGISTRY", "https://self.hosted/c.json")
    assert snapshots._resolve_registry_url(None, "vpod_sk_k") == "https://self.hosted/c.json"


def test_publishable_key_is_refused_outside_a_browser():
    with pytest.raises(ValueError, match="not a browser"):
        snapshots._check_api_key_kind("vpod_pk_abc")


def test_unrecognised_key_prefix_is_refused():
    with pytest.raises(ValueError, match="vpod_sk_"):
        snapshots._check_api_key_kind("sk-openai-style")


def test_secret_key_is_accepted():
    snapshots._check_api_key_kind("vpod_sk_abc")


def test_key_never_leaves_the_registry_origin():
    registry = "https://api.vpod.sh/v1/snapshots.json"
    key = "vpod_sk_secret"

    same = snapshots._request_headers("https://api.vpod.sh/v1/blob/x", registry, key)
    assert same["Authorization"] == f"Bearer {key}"

    for hostile in (
        "https://attacker.com/blob",
        "http://api.vpod.sh/v1/blob/x",
        "https://api.vpod.sh.attacker.com/x",
        "https://api.vpod.sh:8443/v1/blob/x",
    ):
        headers = snapshots._request_headers(hostile, registry, key)
        assert "Authorization" not in headers, hostile


def test_public_entries_carry_no_header():
    headers = snapshots._request_headers(
        "https://registry.vpod.sh/v1/x.snap", snapshots.PUBLIC_REGISTRY_URL, None
    )
    assert "Authorization" not in headers


def test_two_keys_do_not_share_a_catalogue_cache():
    url = snapshots.PRIVATE_REGISTRY_URL
    a = snapshots._registry_cache_path(url, "vpod_sk_orgA")
    b = snapshots._registry_cache_path(url, "vpod_sk_orgB")
    anonymous = snapshots._registry_cache_path(url, None)
    assert a != b != anonymous and a != anonymous


def test_public_cache_path_is_unchanged_without_a_key():
    """Existing installs must not re-download on upgrade."""
    assert (
        snapshots._registry_cache_path(snapshots.PUBLIC_REGISTRY_URL, None)
        == snapshots._REGISTRY_CACHE
    )


def test_key_fingerprint_matches_the_typescript_sdk():
    assert snapshots._key_fingerprint("vpod_sk_example") == "b5e68514b5f1"


def test_origin_tag_is_scoped_by_key_but_bare_without_one():
    url = snapshots.PRIVATE_REGISTRY_URL
    assert snapshots._origin_tag(url, None) == url
    assert snapshots._origin_tag(url, "vpod_sk_a") != snapshots._origin_tag(url, "vpod_sk_b")
    assert "vpod_sk_a" not in snapshots._origin_tag(url, "vpod_sk_a")


def test_not_found_says_whether_a_key_was_sent():
    catalogue = [{"id": "vsnap-1", "name": "other", "tag": "1.0"}]

    with pytest.raises(ValueError, match="No API key was sent"):
        snapshots.resolve_snapshot(catalogue, "missing", "https://r/c.json", False)

    with pytest.raises(ValueError, match="An API key WAS sent"):
        snapshots.resolve_snapshot(catalogue, "missing", "https://r/c.json", True)


def test_default_snapshot_still_resolves_with_a_key_present():
    org_catalogue = [
        {"id": "vsnap-base-256mb", "name": "vsnap-base", "tag": "1.0.0"},
        {"id": "vsnap-private", "name": "mine", "tag": "1.0"},
    ]
    entry = snapshots.resolve_snapshot(org_catalogue, "vsnap-base:latest", None, True)
    assert entry["id"] == "vsnap-base-256mb"


# --- the expired signed URL, and the exactly-one retry ----------------------

class _Registry(http.server.BaseHTTPRequestHandler):
    catalogue_hits = 0
    blob_hits = 0
    blob_403_until = 0
    payload = b"VPODtest-snapshot-bytes"
    seen_blob_auth: list[str | None] = []

    def do_GET(self):
        if self.path.startswith("/catalogue"):
            type(self).catalogue_hits += 1
            import hashlib as _h
            body = json.dumps({
                "version": "1",
                "snapshots": [{
                    "id": "vsnap-priv", "name": "mine", "tag": "1.0",
                    "memory_label": "256MB", "description": "",
                    "url": f"http://{self.headers['Host']}/blob/vsnap-priv",
                    "sha256": _h.sha256(type(self).payload).hexdigest(),
                    "size": len(type(self).payload),
                }],
            }).encode()
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return

        type(self).blob_hits += 1
        type(self).seen_blob_auth.append(self.headers.get("Authorization"))
        if type(self).blob_hits <= type(self).blob_403_until:
            self.send_response(403)
            self.end_headers()
            return

        self.send_response(200)
        self.send_header("content-length", str(len(type(self).payload)))
        self.end_headers()
        self.wfile.write(type(self).payload)

    def log_message(self, *args):
        pass


@pytest.fixture
def registry_server(tmp_path, monkeypatch):
    _Registry.catalogue_hits = 0
    _Registry.blob_hits = 0
    _Registry.blob_403_until = 0
    _Registry.seen_blob_auth = []

    server = http.server.HTTPServer(("127.0.0.1", 0), _Registry)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()

    monkeypatch.setattr(snapshots, "cache_dir", lambda: tmp_path)
    monkeypatch.setattr(snapshots, "_REGISTRY_VERSION_MARKER", tmp_path / "sdkver")
    monkeypatch.setattr(snapshots, "_REGISTRY_CACHE", tmp_path / "snapshots.json")
    import vpod._component
    monkeypatch.setattr(vpod._component, "prewarm", lambda *a, **k: None)
    monkeypatch.setattr(snapshots, "pull", _REAL_PULL)

    yield f"http://127.0.0.1:{server.server_port}/catalogue"
    server.shutdown()


def test_expired_signed_url_refreshes_once_and_retries_once(registry_server):
    _Registry.blob_403_until = 1  # first blob 403s, second succeeds

    path = snapshots.pull("mine:1.0", registry_url=registry_server, api_key="vpod_sk_k")

    assert path.read_bytes() == _Registry.payload
    assert _Registry.blob_hits == 2, "expected exactly one retry"
    assert _Registry.catalogue_hits == 2, "expected exactly one forced refresh"


def test_second_refusal_fails_readably_instead_of_looping(registry_server):
    _Registry.blob_403_until = 99  # always refuses

    with pytest.raises(snapshots.SnapshotAuthError, match="refused again after refreshing"):
        snapshots.pull("mine:1.0", registry_url=registry_server, api_key="vpod_sk_k")

    assert _Registry.blob_hits == 2, "must stop after one retry, not loop"


def test_the_key_is_actually_sent_to_a_same_origin_blob(registry_server):
    snapshots.pull("mine:1.0", registry_url=registry_server, api_key="vpod_sk_k")
    assert _Registry.seen_blob_auth == ["Bearer vpod_sk_k"]


def test_no_key_means_no_header_anywhere(registry_server):
    snapshots.pull("mine:1.0", registry_url=registry_server)
    assert _Registry.seen_blob_auth == [None]
