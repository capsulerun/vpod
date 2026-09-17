import json
import queue
import threading
from dataclasses import dataclass, field
from typing import Callable, Iterator, Optional
from urllib.parse import urlsplit

from ._result import unwrap_result

SOURCES = ("processes", "files", "network", "mounts")
DRAIN_ALL_BYTES = 0xFFFF_FFFF

NOT_ENABLED = (
    "tracing is not enabled for this sandbox. Create it with "
    "Sandbox.create(trace=True) to record what it does."
)
NOT_SUPPORTED = (
    "this sandbox runs on an engine without trace support, a snapshot's own "
    "engine from an older vpod release. Create the sandbox with engine=\"default\" "
    "to trace it."
)

EACCES = -13
EPERM = -1

NOISE_PREFIXES = ("/proc/", "/sys/", "/dev/", "/etc/ld-musl-")
NOISE_DIRECTORIES = ("/proc", "/sys", "/dev")

_WATCH_CLOSED = object()


def trace_options(trace) -> Optional[dict]:
    if trace is None or trace is False:
        return None
    if trace is True:
        return {**{source: True for source in SOURCES}, "buffer_bytes": 0}
    if isinstance(trace, dict):
        unknown = set(trace) - set(SOURCES) - {"buffer_bytes"}
        if unknown:
            raise ValueError(
                f"unknown trace options {sorted(unknown)}, expected {list(SOURCES) + ['buffer_bytes']}"
            )
        return {
            **{source: bool(trace.get(source, False)) for source in SOURCES},
            "buffer_bytes": int(trace.get("buffer_bytes", 0)),
        }
    raise TypeError(f"trace must be True or a dict of sources, got {trace!r}")


@dataclass
class FileActivity:
    path: str
    read: bool = False
    written: bool = False
    created: bool = False
    deleted: bool = False
    renamed_to: Optional[str] = None
    renamed_from: Optional[str] = None
    denied: bool = False
    processes: list[int] = field(default_factory=list)


@dataclass
class HttpRequest:
    method: str
    url: str


@dataclass
class NetworkActivity:
    host: Optional[str]
    address: str
    port: int
    protocol: Optional[str]
    requests: list[HttpRequest] = field(default_factory=list)
    bytes_out: int = 0
    bytes_in: int = 0
    failed: bool = True
    processes: list[int] = field(default_factory=list)


@dataclass
class ProcessNode:
    pid: Optional[int]
    path: Optional[str]
    argv: list[str]
    exit_code: Optional[int]
    started_at: int
    children: list["ProcessNode"] = field(default_factory=list)


class Trace:
    """What a sandbox, or one command in it, did. Plain data, safe to keep."""

    def __init__(self, events: list[dict]):
        self._events = tuple(events)

    @property
    def events(self) -> list[dict]:
        return list(self._events)

    @property
    def complete(self) -> bool:
        return not any(
            event["kind"] == "trace.dropped" or event.get("pid", 0) is None
            for event in self._events
        )

    def to_jsonl(self) -> str:
        return "".join(
            json.dumps(event, separators=(",", ":"), ensure_ascii=False) + "\n"
            for event in self._events
        )

    def files(self, internal: bool = False, noise: bool = False) -> list[FileActivity]:
        activities: dict[str, FileActivity] = {}
        touching: dict[str, set[int]] = {}
        current: dict = {}

        def activity(path) -> Optional[FileActivity]:
            if not isinstance(path, str):
                return None
            if path not in activities:
                activities[path] = FileActivity(path)
                touching[path] = set()
            if (pid := current.get("pid")) is not None:
                touching[path].add(pid)
            return activities[path]

        def mark_denied(path, result) -> None:
            if result in (EACCES, EPERM) and (entry := activity(path)) is not None:
                entry.denied = True

        for event in self._events:
            if event.get("internal") and not internal:
                continue
            kind = event["kind"]
            current["pid"] = event.get("pid")

            if kind in ("file.open", "mount.open"):
                succeeded = event["result"] >= 0 if kind == "file.open" else event["result"] == 0
                if not succeeded:
                    mark_denied(event.get("path"), event["result"])
                    continue
                if (entry := activity(event.get("path"))) is None:
                    continue

                access = event.get("access")
                entry.read |= access in ("read", "read-write")
                entry.written |= access in ("write", "read-write") or bool(event.get("truncate"))
            elif kind in ("file.rename", "mount.rename"):
                if event["result"] != 0:
                    mark_denied(event.get("from"), event["result"])
                    mark_denied(event.get("to"), event["result"])
                    continue
                source, destination = activity(event.get("from")), activity(event.get("to"))
                if source is not None:
                    source.renamed_to = event.get("to")
                if destination is not None:
                    destination.renamed_from = event.get("from")
                    destination.written = True
            elif kind in ("file.delete", "mount.delete"):
                if event["result"] != 0:
                    mark_denied(event.get("path"), event["result"])
                elif (entry := activity(event.get("path"))) is not None:
                    entry.deleted = True
            elif kind in ("dir.create", "mount.mkdir", "mount.create"):
                if event["result"] != 0:
                    mark_denied(event.get("path"), event["result"])
                elif (entry := activity(event.get("path"))) is not None:
                    entry.created = True
                    entry.written |= kind == "mount.create"
            elif kind in ("file.truncate", "mount.truncate"):
                if event["result"] != 0:
                    mark_denied(event.get("path"), event["result"])
                elif (entry := activity(event.get("path"))) is not None:
                    entry.written = True
            elif kind == "mount.close":
                bytes_read = event.get("bytes_read", 0)
                bytes_written = event.get("bytes_written", 0)
                if (bytes_read or bytes_written) and (entry := activity(event.get("path"))) is not None:
                    entry.read |= bytes_read > 0
                    entry.written |= bytes_written > 0

        for path, entry in activities.items():
            entry.processes = sorted(touching[path])

        return [entry for entry in activities.values() if noise or not _is_noise(entry)]

    def network(self, internal: bool = False) -> list[NetworkActivity]:
        activities: dict[tuple[str, int], NetworkActivity] = {}
        touching: dict[tuple[str, int], set[int]] = {}

        def activity(event) -> Optional[NetworkActivity]:
            address, port = event.get("address"), event.get("port")
            if not isinstance(address, str) or not isinstance(port, int):
                return None

            key = (address, port)
            if key not in activities:
                activities[key] = NetworkActivity(
                    host=None, address=address, port=port, protocol=None
                )
                touching[key] = set()

            entry = activities[key]
            entry.host = entry.host or event.get("host")
            if (pid := event.get("pid")) is not None:
                touching[key].add(pid)
            return entry

        for event in self._events:
            if event.get("internal") and not internal:
                continue
            kind = event["kind"]

            if kind not in ("net.connect", "net.flow", "net.udp", "net.http"):
                continue
            if (entry := activity(event)) is None:
                continue

            if kind == "net.connect":
                entry.protocol = entry.protocol or event.get("protocol")
                entry.failed &= event["result"] != 0
            elif kind == "net.flow":
                entry.protocol = entry.protocol or event.get("protocol")
                entry.bytes_out += event.get("bytes_out", 0)
                entry.bytes_in += event.get("bytes_in", 0)
                entry.failed &= bool(event.get("failed"))
            elif kind == "net.udp":
                entry.protocol = entry.protocol or "udp"
                entry.failed = False
            else:
                entry.host = entry.host or urlsplit(event["url"]).hostname
                entry.requests.append(HttpRequest(event["method"], event["url"]))

        for key, entry in activities.items():
            entry.processes = sorted(touching[key])

        return list(activities.values())

    def processes(self, internal: bool = False) -> list[ProcessNode]:
        parents = self._parents()
        roots: list[ProcessNode] = []
        latest: dict[int, ProcessNode] = {}

        for event in self._events:
            kind = event["kind"]
            pid = event.get("pid")

            if kind == "process.exec" and "result" not in event:
                if event.get("internal") and not internal:
                    continue
                node = ProcessNode(
                    pid=pid,
                    path=event.get("path"),
                    argv=list(event.get("argv", [])),
                    exit_code=None,
                    started_at=event["guest_ns"],
                )

                program = _closest_program(pid, parents, latest)
                (roots if program is None else program.children).append(node)
                if pid is not None:
                    latest[pid] = node
            elif kind == "process.exit" and pid in latest and latest[pid].exit_code is None:
                latest[pid].exit_code = event["code"]

        return roots

    def _parents(self) -> dict[int, int]:
        parents: dict[int, int] = {}
        for event in self._events:
            kind = event["kind"]
            if kind == "process.fork" and not event.get("thread"):
                if event.get("pid") is not None:
                    parents.setdefault(event["child_pid"], event["pid"])
            elif kind == "process.exec" and event.get("ppid") is not None:
                parents.setdefault(event["pid"], event["ppid"])
        return parents


def _closest_program(
    pid: Optional[int], parents: dict[int, int], latest: dict[int, ProcessNode]
) -> Optional[ProcessNode]:
    if pid is None:
        return None
    if (node := latest.get(pid)) is not None:
        return node

    seen = {pid}
    ancestor = parents.get(pid)
    while ancestor is not None and ancestor not in seen:
        if (node := latest.get(ancestor)) is not None:
            return node
        seen.add(ancestor)
        ancestor = parents.get(ancestor)
    return None


def _is_noise(entry: FileActivity) -> bool:
    path = entry.path
    if path in NOISE_DIRECTORIES or path.startswith(NOISE_PREFIXES):
        return True

    only_read = not (
        entry.written
        or entry.created
        or entry.deleted
        or entry.renamed_to is not None
        or entry.renamed_from is not None
        or entry.denied
    )
    return only_read and (path.endswith(".so") or ".so." in path)


class TraceRecorder:
    """The live record of a sandbox. `collect()` freezes what it has so far."""

    def __init__(self, options: Optional[dict], session: Callable[[], tuple]):
        self._options = options
        self._session = session
        self._events: list[dict] = []
        self._watchers: list[queue.Queue] = []
        self._lock = threading.Lock()
        self._drain_lock = threading.Lock()

    @property
    def enabled(self) -> bool:
        return self._options is not None

    def collect(self) -> Trace:
        self._require_enabled()
        self._drain()
        with self._lock:
            return Trace(self._events)

    def watch(self) -> Iterator[dict]:
        self._require_enabled()
        feed: queue.Queue = queue.Queue()
        with self._lock:
            self._watchers.append(feed)
        return self._follow(feed)

    def clear(self) -> None:
        self._require_enabled()
        self._drain()
        with self._lock:
            self._events.clear()

    def _follow(self, feed: queue.Queue) -> Iterator[dict]:
        try:
            while True:
                event = feed.get()
                if event is _WATCH_CLOSED:
                    return
                yield event
        finally:
            with self._lock:
                if feed in self._watchers:
                    self._watchers.remove(feed)

    def _require_enabled(self) -> None:
        if not self.enabled:
            raise RuntimeError(NOT_ENABLED)

    def _require_support(self, exports) -> None:
        if self.enabled and "session-trace-start" not in exports:
            raise RuntimeError(NOT_SUPPORTED)

    def _start(self, exports, session_id: int) -> None:
        if self._options is None:
            return

        record: object = object.__new__(type("TraceOptions", (), {}))
        for source in SOURCES:
            object.__setattr__(record, source, self._options[source])
        object.__setattr__(record, "buffer-bytes", self._options["buffer_bytes"])
        unwrap_result(exports["session-trace-start"](session_id, record))

    def _drain(self) -> None:
        if not self.enabled:
            return
        with self._drain_lock:
            exports, session_id = self._session()
            if session_id is None:
                return

            drained = unwrap_result(exports["session-trace-drain"](session_id, DRAIN_ALL_BYTES))
            if not drained:
                return

            events = [json.loads(line) for line in drained.splitlines() if line]
            with self._lock:
                self._events.extend(events)
                watchers = list(self._watchers)
            for feed in watchers:
                for event in events:
                    feed.put(event)

    def _mark(self) -> int:
        if not self.enabled:
            return 0
        self._drain()
        with self._lock:
            return len(self._events)

    def _since(self, mark: int) -> Optional[Trace]:
        if not self.enabled:
            return None
        self._drain()
        with self._lock:
            return Trace(self._events[mark:])

    def _close(self) -> None:
        with self._lock:
            watchers, self._watchers = self._watchers, []
        for feed in watchers:
            feed.put(_WATCH_CLOSED)
