"""vpod warm-python prefork server. (for python3 shell cmd)

Protocol: see guest/warmpy/vpod_python_shim.c.
"""

import array
import importlib
import io
import os
import runpy
import signal
import socket
import struct
import sys
import traceback
import warnings

SOCK_PATH = os.environ.get("VPOD_PYD_SOCK", "/run/vpod-pyd.sock")

CHILD_EXECUTABLE = os.environ.get("VPOD_PYD_EXECUTABLE", "/usr/bin/python3.real")
MAGIC = b"VPY1"

for _mod in (
    "abc", "base64", "codecs", "collections", "encodings.utf_8",
    "functools", "json", "re", "shutil", "subprocess", "types",
):
    try:
        __import__(_mod)
    except ImportError:
        pass


WARM_IMPORTS_FILE = os.environ.get(
    "VPOD_PYD_WARM_IMPORTS", "/etc/vpod/pydaemon-warm-imports"
)


def warm_heavy_tools():
    """Pre-import expensive module trees so forked children inherit them via
    copy-on-write"""
    modules = ["pip._internal.cli.main"]
    try:
        with open(WARM_IMPORTS_FILE) as f:
            for line in f:
                name = line.split("#", 1)[0].strip()
                if name:
                    modules.append(name)
    except OSError:
        pass
    for name in modules:
        try:
            __import__(name)
        except Exception:
            pass


warm_heavy_tools()


def _watch_dirs():
    """The directories a runtime `pip install` writes into, plus the warm-list
    file itself (so appending a module to it takes effect live)."""
    dirs = {WARM_IMPORTS_FILE}
    try:
        import site

        for getter in ("getsitepackages", "getusersitepackages"):
            fn = getattr(site, getter, None)
            if not fn:
                continue
            try:
                result = fn()
            except Exception:
                continue
            dirs.update([result] if isinstance(result, str) else result)

    except Exception:
        pass
    for entry in sys.path:
        if entry and "site-packages" in entry:
            dirs.add(entry)
    return sorted(dirs)


def _dir_signature(dirs):
    signature = []
    for path in dirs:
        try:
            signature.append((path, os.stat(path).st_mtime_ns))
        except OSError:
            signature.append((path, -1))
    return tuple(signature)


_WATCH_DIRS = _watch_dirs()
_last_signature = _dir_signature(_WATCH_DIRS)


def refresh_import_caches():
    """Called in the daemon (once per accept)."""
    global _last_signature
    signature = _dir_signature(_WATCH_DIRS)
    if signature != _last_signature:
        importlib.invalidate_caches()
        _last_signature = signature
        warm_heavy_tools()


def recv_request(conn):
    """returns ([stdin_fd, stdout_fd, stderr_fd], payload)."""
    fds = array.array("i")
    header = b""

    while len(header) < 8:
        data, ancdata, _flags, _addr = conn.recvmsg(
            8 - len(header), socket.CMSG_SPACE(3 * 4)
        )
        if not data:
            raise ConnectionError("client closed during header")
        header += data
        for level, ctype, cdata in ancdata:
            if level == socket.SOL_SOCKET and ctype == socket.SCM_RIGHTS:
                fds.frombytes(cdata[: len(cdata) - len(cdata) % 4])

    if header[:4] != MAGIC:
        raise ValueError(f"bad magic: {header[:4]!r}")

    if len(fds) != 3:
        raise ValueError(f"expected 3 fds, got {len(fds)}")

    (payload_len,) = struct.unpack("<I", header[4:8])
    payload = b""
    while len(payload) < payload_len:
        chunk = conn.recv(min(65536, payload_len - len(payload)))
        if not chunk:
            raise ConnectionError("client closed during payload")
        payload += chunk
    return list(fds), payload


def parse_payload(payload):
    fields = payload.decode("utf-8", "surrogateescape").split("\0")
    argc = int(fields[0])
    argv = fields[1 : 1 + argc]
    cwd = fields[1 + argc]
    env = {}

    for entry in fields[2 + argc :]:
        if entry and "=" in entry:
            key, _, value = entry.partition("=")
            env[key] = value

    return argv, cwd, env


class Request:
    """A parsed python command line and fall back to python3.real."""

    def __init__(self):
        self.mode = None
        self.target = None
        self.args = []
        self.unbuffered = False
        self.ignore_env = False
        self.warn_options = []


def parse_argv(argv):
    req = Request()
    args = argv[1:]
    i = 0
    while i < len(args):
        arg = args[i]
        if not arg.startswith("-") or arg == "-":
            if arg == "-":
                req.mode = "stdin"
                req.args = ["-"] + args[i + 1 :]
            else:
                req.mode = "script"
                req.target = arg
                req.args = args[i:]
            return req
        if arg == "--":
            i += 1
            continue

        j = 1
        while j < len(arg):
            flag = arg[j]
            if flag in ("c", "m", "W", "X"):
                inline = arg[j + 1 :]
                if inline:
                    value = inline
                else:
                    i += 1
                    if i >= len(args):
                        return None
                    value = args[i]
                if flag == "c":
                    req.mode = "c"
                    req.target = value
                    req.args = ["-c"] + args[i + 1 :]
                    return req
                if flag == "m":
                    req.mode = "m"
                    req.target = value
                    req.args = [value] + args[i + 1 :]
                    return req
                if flag == "W":
                    req.warn_options.append(value)
                else:
                    pass
                break
            elif flag == "u":
                req.unbuffered = True
            elif flag == "E":
                req.ignore_env = True
            elif flag in ("b", "B", "q", "s"):
                pass
            else:
                return None

            j += 1
        i += 1
    req.mode = "stdin"
    return req


def wire_stdio(fds, unbuffered):
    for i, fd in enumerate(fds):
        if fd != i:
            os.dup2(fd, i)
    for fd in set(fds):
        if fd > 2:
            os.close(fd)

    stdin_raw = io.FileIO(0, "rb", closefd=False)
    sys.stdin = sys.__stdin__ = io.TextIOWrapper(
        io.BufferedReader(stdin_raw), encoding="utf-8", errors="strict"
    )

    def make_writer(fd):
        raw = io.FileIO(fd, "wb", closefd=False)
        if unbuffered:
            return io.TextIOWrapper(raw, encoding="utf-8", write_through=True)
        return io.TextIOWrapper(
            io.BufferedWriter(raw), encoding="utf-8",
            line_buffering=os.isatty(fd),
        )

    sys.stdout = sys.__stdout__ = make_writer(1)
    sys.stderr = sys.__stderr__ = make_writer(2)


def run_child(fds, req, argv0, cwd, env):
    exit_code = 0
    try:
        signal.signal(signal.SIGINT, signal.default_int_handler)
        for sig in (signal.SIGTERM, signal.SIGHUP, signal.SIGQUIT,
                    signal.SIGPIPE, signal.SIGUSR1, signal.SIGUSR2):
            signal.signal(sig, signal.SIG_DFL)

        os.chdir(cwd)
        os.environ.clear()
        os.environ.update(env)

        unbuffered = req.unbuffered or (
            not req.ignore_env and bool(env.get("PYTHONUNBUFFERED"))
        )
        wire_stdio(fds, unbuffered)

        sys.executable = CHILD_EXECUTABLE
        sys.argv = req.args or [argv0]

        if not req.ignore_env:
            for path in reversed(env.get("PYTHONPATH", "").split(":")):
                if path and path not in sys.path:
                    sys.path.insert(0, path)
        for warn_option in req.warn_options:
            warnings._processoptions([warn_option])

        if req.mode == "c":
            sys.path.insert(0, "")
            exec(compile(req.target, "<string>", "exec"),
                 {"__name__": "__main__", "__doc__": None, "__package__": None,
                  "__spec__": None, "__builtins__": __builtins__})
        elif req.mode == "m":
            sys.path.insert(0, os.getcwd())
            runpy.run_module(req.target, run_name="__main__", alter_sys=True)
        elif req.mode == "script":
            runpy.run_path(req.target, run_name="__main__")
        else:
            source = sys.stdin.read()
            sys.path.insert(0, "")
            exec(compile(source, "<stdin>", "exec"),
                 {"__name__": "__main__", "__doc__": None, "__package__": None,
                  "__spec__": None, "__builtins__": __builtins__})

    except SystemExit as exc:
        if exc.code is None:
            exit_code = 0
        elif isinstance(exc.code, int):
            exit_code = exc.code
        else:
            print(exc.code, file=sys.stderr)
            exit_code = 1
    except BaseException:
        traceback.print_exc()
        exit_code = 1
    finally:
        try:
            sys.stdout.flush()
        except Exception:
            pass
        try:
            sys.stderr.flush()
        except Exception:
            pass
        os._exit(exit_code & 0xFF)


def handle_connection(conn):
    fds, payload = recv_request(conn)
    argv, cwd, env = parse_payload(payload)
    req = parse_argv(argv)

    if req is None or (req.mode == "stdin" and os.isatty(fds[0])):
        conn.sendall(b"F")
        return

    child = os.fork()
    if child == 0:
        conn.close()
        run_child(fds, req, argv[0] if argv else "python3", cwd, env)

    for fd in set(fds):
        os.close(fd)
    conn.sendall(b"P" + struct.pack("<I", child))
    _, status = os.waitpid(child, 0)
    conn.sendall(b"X" + struct.pack("<I", status))


def main():
    sock_dir = os.path.dirname(SOCK_PATH)
    if sock_dir:
        os.makedirs(sock_dir, exist_ok=True)
    try:
        os.unlink(SOCK_PATH)
    except FileNotFoundError:
        pass

    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(SOCK_PATH)
    os.chmod(SOCK_PATH, 0o777)
    server.listen(64)

    signal.signal(signal.SIGCHLD, signal.SIG_IGN)
    signal.signal(signal.SIGPIPE, signal.SIG_IGN)

    while True:
        try:
            conn, _ = server.accept()
        except InterruptedError:
            continue
        refresh_import_caches()
        pid = os.fork()
        if pid == 0:
            server.close()
            signal.signal(signal.SIGCHLD, signal.SIG_DFL)
            code = 0
            try:
                handle_connection(conn)
            except (ConnectionError, BrokenPipeError, ValueError, OSError):
                code = 1
            finally:
                try:
                    conn.close()
                except OSError:
                    pass
                os._exit(code)
        conn.close()


if __name__ == "__main__":
    main()
