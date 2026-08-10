#!/bin/sh

# Build a vpod snapshot from a Dockerfile.
#
# The Dockerfile is built for linux/riscv64 with Apple's `container` CLI
# (https://github.com/apple/container), whose BuildKit builder executes
# RUN steps under riscv64 emulation and can export the flattened root
# filesystem as a tarball. That rootfs replaces the Alpine minirootfs of
# build-default-snapshot.sh; everything after that follows the same
# pipeline — vpod overlay on top, boot in vpod-native, capture machine
# state with --snapshot-save.
#
#   ./scripts/build-snapshot-from-dockerfile.sh -f Dockerfile -n my-image
#   ./scripts/build-snapshot-from-dockerfile.sh -f tools/Dockerfile -C tools \
#       -n my-tools --ram 512
#
# Notes:
#   - FROM should resolve to a riscv64-capable image (e.g. alpine:3.23,
#     riscv64/*). The guest kernel always comes from the Alpine ISO, so
#     the image only provides userspace.
#   - Only the filesystem survives the export: ENV/CMD/ENTRYPOINT image
#     config does not. Persist environment via /etc/profile.d/ in a RUN.
#   - Python warm-start (shim + daemon) is applied only when the built
#     image contains python3; other images produce shell-only snapshots.

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

ROOTFS_TAR="$ROOT/dist/df-rootfs.tar"
ROOTFS="$ROOT/dist/df-rootfs"
OVERLAY="$ROOT/dist/df-overlay"
PART_MINI="$ROOT/dist/df-mini.cpio.gz"
PART_OVL="$ROOT/dist/df-overlay.cpio.gz"

cleanup() {
    rm -rf "$ROOTFS" "$ROOTFS_TAR" "$OVERLAY" "$PART_MINI" "$PART_OVL"
}
trap cleanup EXIT

ALPINE_VERSION="3.23.0"
DOCKERFILE=""
CONTEXT=""
NAME=""
OUT=""
RAM_MB=256
PLATFORM="linux/riscv64"
AOT=0
TRACE_CMDS=""

add_trace_cmd() {
    if [ -z "$TRACE_CMDS" ]; then
        TRACE_CMDS="$1"
    else
        TRACE_CMDS="$TRACE_CMDS
$1"
    fi
}

usage() {
    echo "usage: $0 -f <Dockerfile> -n <name> [-C <context-dir>] [--ram <MB>]"
    echo "          [--out <file>] [--platform <os/arch>] [--alpine-version <v>]"
    echo "          [--aot --trace-cmd <sh> [--trace-cmd <sh>]...]"
    echo ""
    echo "  --aot runs scripts/aot-snapshot.sh --workload custom afterwards,"
    echo "  tracing the given --trace-cmd steps (exercise the image's hot"
    echo "  code: the interpreter/tool your Dockerfile installed)."
    exit "${1:-1}"
}

need() {
    [ -n "$2" ] || { echo "ERROR: $1 requires a value"; usage; }
}

while [ $# -gt 0 ]; do
    case "$1" in
        -f|--file)        need "$@"; DOCKERFILE="$2";     shift 2 ;;
        -C|--context)     need "$@"; CONTEXT="$2";        shift 2 ;;
        -n|--name)        need "$@"; NAME="$2";           shift 2 ;;
        --ram)            need "$@"; RAM_MB="$2";         shift 2 ;;
        --out)            need "$@"; OUT="$2";            shift 2 ;;
        --platform)       need "$@"; PLATFORM="$2";       shift 2 ;;
        --alpine-version) need "$@"; ALPINE_VERSION="$2"; shift 2 ;;
        --aot)            AOT=1;                          shift ;;
        --trace-cmd)      need "$@"; add_trace_cmd "$2";  shift 2 ;;
        -h|--help)        usage 0 ;;
        *) echo "unknown arg: $1"; usage ;;
    esac
done

if [ "$AOT" = "1" ] && [ -z "$TRACE_CMDS" ]; then
    echo "ERROR: --aot needs at least one --trace-cmd (the workload to trace)"
    usage
fi

[ -n "$DOCKERFILE" ] || usage
[ -n "$NAME" ] || usage
[ -f "$DOCKERFILE" ] || { echo "ERROR: Dockerfile not found: $DOCKERFILE"; exit 1; }
[ -n "$CONTEXT" ] || CONTEXT="$(dirname "$DOCKERFILE")"
[ -n "$OUT" ] || OUT="$ROOT/dist/${NAME}-${RAM_MB}mb.snap"

ALPINE_MINOR="${ALPINE_VERSION%.*}"
ALPINE_DIR="$ROOT/dist/alpine-standard-${ALPINE_VERSION}-riscv64"
ISO_URL="https://dl-cdn.alpinelinux.org/alpine/v${ALPINE_MINOR}/releases/riscv64/alpine-standard-${ALPINE_VERSION}-riscv64.iso"
ISO="$ROOT/dist/alpine-standard-${ALPINE_VERSION}-riscv64.iso"
KERNEL="$ALPINE_DIR/kernel"
INITRAMFS_LTS="$ALPINE_DIR/initramfs-lts"
OPENSBI_VERSION="1.6"
OPENSBI_FW="$ROOT/dist/fw_jump.bin"
OPENSBI_URL="https://github.com/riscv-software-src/opensbi/releases/download/v${OPENSBI_VERSION}/opensbi-${OPENSBI_VERSION}-rv-bin.tar.xz"
OPENSBI_TAR="$ROOT/dist/opensbi-${OPENSBI_VERSION}-rv-bin.tar.xz"
INITRD="$ROOT/dist/${NAME}-rootfs.cpio.gz"
VPOD="$ROOT/target/release/vpod-native"

echo "=== Capsulev snapshot builder (Dockerfile) ==="
echo "Dockerfile : ${DOCKERFILE}"
echo "Context    : ${CONTEXT}"
echo "Platform   : ${PLATFORM}"
echo "Name       : ${NAME}"
echo "RAM        : ${RAM_MB} MB"
echo "Out        : ${OUT}"
echo ""

echo "── Checking host tools..."
MISSING=""
for cmd in curl bsdtar cpio gzip cargo zig container; do
    command -v "$cmd" >/dev/null || MISSING="$MISSING $cmd"
done
if [ -n "$MISSING" ]; then
    echo "ERROR: missing tools:$MISSING"
    echo "  macOS  : brew install libarchive zig; container from https://github.com/apple/container/releases"
    echo "  (this script drives Apple's container CLI and is macOS-only for now)"
    exit 1
fi
echo "   OK"

mkdir -p "$ROOT/dist" "$ALPINE_DIR"


echo "── Building vpod..."
(cd "$ROOT" && cargo build --release -p native-cli --bin vpod-native)


if [ ! -f "$OPENSBI_FW" ]; then
    echo "── Downloading OpenSBI ${OPENSBI_VERSION} pre-built firmware..."
    curl -L --progress-bar -o "$OPENSBI_TAR" "$OPENSBI_URL"
    bsdtar -xf "$OPENSBI_TAR" -C "$ROOT/dist" \
        "opensbi-${OPENSBI_VERSION}-rv-bin/share/opensbi/lp64/generic/firmware/fw_jump.bin"
    mv "$ROOT/dist/opensbi-${OPENSBI_VERSION}-rv-bin/share/opensbi/lp64/generic/firmware/fw_jump.bin" \
       "$OPENSBI_FW"
    rm -rf "$OPENSBI_TAR" "$ROOT/dist/opensbi-${OPENSBI_VERSION}-rv-bin"
    echo "   OpenSBI firmware: $(du -sh "$OPENSBI_FW" | cut -f1)"
else
    echo "── OpenSBI firmware already present, skipping."
fi

if [ ! -f "$ISO" ]; then
    echo "── Downloading Alpine standard ISO ${ALPINE_VERSION} (kernel + modules)..."
    curl -L --progress-bar -o "$ISO" "$ISO_URL"
else
    echo "── Alpine ISO already present, skipping download."
fi

if [ ! -f "$KERNEL" ] || [ ! -f "$INITRAMFS_LTS" ]; then
    echo "── Extracting kernel and initramfs-lts from ISO..."
    bsdtar -xf "$ISO" -C "$ALPINE_DIR" \
        --include "boot/vmlinuz-lts" \
        --include "boot/initramfs-lts" \
        --strip-components=1

    MAGIC=$(dd if="$ALPINE_DIR/vmlinuz-lts" bs=2 count=1 2>/dev/null | od -A n -t x1 | tr -d ' \n')
    if [ "$MAGIC" = "1f8b" ]; then
        gzip -dc "$ALPINE_DIR/vmlinuz-lts" > "$KERNEL"
    else
        cp "$ALPINE_DIR/vmlinuz-lts" "$KERNEL"
    fi
    echo "   kernel     : $(du -sh "$KERNEL" | cut -f1)"
    echo "   initramfs  : $(du -sh "$INITRAMFS_LTS" | cut -f1)"
else
    echo "── Kernel and initramfs already extracted, skipping."
fi


echo "── Building ${PLATFORM} image with Apple container..."
container system start >/dev/null 2>&1 || true
container build \
    --platform "$PLATFORM" \
    --output "type=tar,dest=$ROOTFS_TAR" \
    -t "vpod-snapshot-build-$NAME" \
    -f "$DOCKERFILE" \
    "$CONTEXT"
echo "   rootfs tar : $(du -sh "$ROOTFS_TAR" | cut -f1)"

echo "── Extracting rootfs..."
rm -rf "$ROOTFS"
mkdir -p "$ROOTFS"
# BuildKit's tar exporter nests the filesystem under a per-platform
# directory (e.g. linux_riscv64/) when --platform is given; plain builds
# export at the archive root. Handle both.
PLATFORM_PREFIX="$(printf '%s' "$PLATFORM" | tr '/' '_')"
FIRST_ENTRY="$(bsdtar -tf "$ROOTFS_TAR" | head -1)"
DOT_COMPONENT=0
case "$FIRST_ENTRY" in
    ./*) DOT_COMPONENT=1 ;;
esac
NORMALIZED="${FIRST_ENTRY#./}"
if [ "${NORMALIZED%%/*}" = "$PLATFORM_PREFIX" ]; then
    bsdtar -xf "$ROOTFS_TAR" -C "$ROOTFS" \
        --strip-components=$((DOT_COMPONENT + 1)) --no-same-owner
    # BuildKit's tar exporter rewrites ABSOLUTE symlink targets to include
    # the platform directory (/bin/sh -> /linux_riscv64/bin/busybox). With
    # the prefix stripped those all dangle — including every busybox
    # applet — and the guest panics on a broken /bin/sh. Point them back
    # at the real root.
    find "$ROOTFS" -type l | while IFS= read -r LINK; do
        TGT="$(readlink "$LINK")"
        case "$TGT" in
            "/${PLATFORM_PREFIX}/"*)
                ln -sf "${TGT#"/${PLATFORM_PREFIX}"}" "$LINK" ;;
        esac
    done
else
    bsdtar -xf "$ROOTFS_TAR" -C "$ROOTFS" --no-same-owner
fi

if [ ! -e "$ROOTFS/bin/sh" ] && [ ! -L "$ROOTFS/bin/sh" ]; then
    echo "ERROR: the built image has no /bin/sh — the vpod init and setup"
    echo "       console are shell scripts, so a shell-less image cannot"
    echo "       become a snapshot. Add a shell to the Dockerfile."
    exit 1
fi

PY_PRESENT=0
if [ -e "$ROOTFS/usr/bin/python3" ] || [ -e "$ROOTFS/bin/python3" ]; then
    PY_PRESENT=1
    echo "   python3 detected — the snapshot will include the warm-python daemon."
else
    echo "   no python3 in the image — building a shell-only snapshot."
fi

echo "── Extracting kernel modules into rootfs..."
mkdir -p "$ROOTFS/lib"
gunzip -c "$INITRAMFS_LTS" | (cd "$ROOTFS" && cpio -idmu --quiet 'usr/lib/modules/*') 2>/dev/null || true
if [ -d "$ROOTFS/usr/lib/modules" ] && [ ! -e "$ROOTFS/lib/modules" ]; then
    ln -sf /usr/lib/modules "$ROOTFS/lib/modules"
fi

if [ ! -d "$ROOTFS/usr/lib/modules" ]; then
    echo "   WARNING: no kernel modules extracted from initramfs-lts."
    echo "            If the guest has no network or cannot mount virtiofs,"
    echo "            this is why (is the initramfs still gzip-compressed?)."
fi


echo "── Building overlay..."
rm -rf "$OVERLAY"
mkdir -p "$OVERLAY/sbin" "$OVERLAY/usr/lib/vpod"

if [ -d "$ROOTFS/etc/apk" ]; then
    mkdir -p "$OVERLAY/etc/apk"
    printf 'https://dl-cdn.alpinelinux.org/alpine/v%s/main\nhttps://dl-cdn.alpinelinux.org/alpine/v%s/community\n' \
        "$ALPINE_MINOR" "$ALPINE_MINOR" > "$OVERLAY/etc/apk/repositories"
fi
mkdir -p "$OVERLAY/etc"
echo 'nameserver 8.8.8.8' > "$OVERLAY/etc/resolv.conf"

mkdir -p "$OVERLAY/usr/local/share/ca-certificates"
cp "$ROOT/crates/machine/assets/tls/vpod-ca-cert.pem" \
    "$OVERLAY/usr/local/share/ca-certificates/vpod-ca.crt"

mkdir -p "$OVERLAY/etc/ssl/vpod"
cp "$ROOT/crates/machine/assets/tls/vpod-ca-cert.pem" \
    "$OVERLAY/etc/ssl/vpod/ca-only.pem"


echo "── Cross-compiling vpod ssl_client (riscv64-musl, static)..."
zig cc -target riscv64-linux-musl -Os -static -s \
    -o "$OVERLAY/usr/lib/vpod/vpod-ssl-client" \
    "$ROOT/guest/tls/vpod_ssl_client.c"
chmod +x "$OVERLAY/usr/lib/vpod/vpod-ssl-client"

echo "── Cross-compiling vpod entropy seeder (riscv64-musl, static)..."
# Deterministic emulation produces no jitter entropy, so the guest crng
# never initializes and blocking getrandom() — node/V8 startup — hangs
# forever. Seed + CREDIT the crng during finalize from host randomness;
# the seed file is deleted before the snapshot is captured.
zig cc -target riscv64-linux-musl -Os -static -s \
    -o "$OVERLAY/usr/lib/vpod/vpod-seed-entropy" \
    "$ROOT/guest/entropy/vpod_seed_entropy.c"
chmod +x "$OVERLAY/usr/lib/vpod/vpod-seed-entropy"
mkdir -p "$OVERLAY/etc/vpod"
head -c 512 /dev/urandom > "$OVERLAY/etc/vpod/entropy-seed"
chmod 600 "$OVERLAY/etc/vpod/entropy-seed"

mkdir -p "$OVERLAY/etc/vpod"
cat > "$OVERLAY/etc/vpod/pydaemon-warm-imports" << 'WARM_EOF'
# Warm set for the python3 shim/daemon path (commands.run("python3 ...")
WARM_EOF
cat > "$OVERLAY/etc/vpod/pyrunner-warm-imports" << 'WARM_EOF'
# Warm set for pyrunner, the persistent code.run() interpreter.
WARM_EOF

mkdir -p "$OVERLAY/etc/uv"
cat > "$OVERLAY/etc/uv/uv.toml" << 'UV_EOF'
python-preference = "only-system"
UV_EOF

# Enable docker usage
mkdir -p "$OVERLAY/etc/docker"
cat > "$OVERLAY/etc/docker/daemon.json" << 'DOCKER_EOF'
{
  "iptables": false,
  "bridge": "none",
  "storage-driver": "overlay2"
}
DOCKER_EOF

if [ "$PY_PRESENT" = "1" ]; then
    echo "── Cross-compiling vpod python shim (riscv64-musl, dynamic)..."
    zig cc -target riscv64-linux-musl -Os -dynamic -s \
        -o "$OVERLAY/usr/lib/vpod/vpod-python-shim" \
        "$ROOT/guest/warmpy/vpod_python_shim.c"
    chmod +x "$OVERLAY/usr/lib/vpod/vpod-python-shim"
    cp "$ROOT/guest/warmpy/pydaemon.py" "$OVERLAY/usr/lib/vpod/pydaemon.py"

    cat > "$OVERLAY/usr/lib/vpod/pyrunner.py" << 'PYRUNNER_EOF'
import sys, io, traceback, base64

try:
    import ssl, urllib.request
except ImportError:
    pass

try:
    with open("/etc/vpod/pyrunner-warm-imports") as _f:
        for _line in _f:
            _name = _line.split("#", 1)[0].strip()
            if _name:
                try:
                    __import__(_name)
                except Exception:
                    pass
except OSError:
    pass

_globals = {}
_sentinel = "---VPOD_DONE---"
_real_stdout = sys.stdout
_real_stderr = sys.stderr
_data_out = open("/dev/ttyS3", "w")
_data_in = open("/dev/ttyS3", "r", buffering=1)
_exit_code_out = open("/dev/ttyS2", "wb", buffering=0)
# The host drains this separately and hands it back as `stderr`. Keeping the two
# streams apart is what lets a caller tell a diagnostic from ordinary output.
_err_out = open("/dev/ttyS1", "w")

while True:
    _line = _data_in.readline()
    if not _line:
        break
    _line = _line.rstrip("\n")
    if not _line:
        continue
    try:
        _code = base64.b64decode(_line).decode()
    except Exception:
        _code = _line

    _out_buf = io.StringIO()
    _err_buf = io.StringIO()
    sys.stdout = _out_buf
    sys.stderr = _err_buf
    _exit_code = 0
    try:
        exec(compile(_code, "<vpod>", "exec"), _globals)
    except SystemExit as _e:
        if isinstance(_e.code, int):
            _exit_code = _e.code & 0xFF
        elif _e.code is not None:
            # CPython prints a non-integer exit argument to stderr, so do that.
            _err_buf.write(str(_e.code) + "\n")
            _exit_code = 1
    except Exception:
        _err_buf.write(traceback.format_exc())
        _exit_code = 1
    finally:
        sys.stdout = _real_stdout
        sys.stderr = _real_stderr

    # Both before the sentinel: the host waits on that, then drains stderr.
    _err = _err_buf.getvalue()
    if _err:
        _err_out.write(_err)
        _err_out.flush()

    _val = _out_buf.getvalue()
    if _val:
        _data_out.write(_val)
    _exit_code_out.write(bytes([_exit_code]))
    _data_out.write(_sentinel + "\n")
    _data_out.flush()
PYRUNNER_EOF
fi

cat > "$OVERLAY/sbin/init" << 'INIT_EOF'
#!/bin/sh

export PATH=/usr/bin:/usr/sbin:/bin:/sbin

mount -t proc     proc     /proc
mount -t sysfs    sysfs    /sys
mount -t devtmpfs devtmpfs /dev
mount -t tmpfs    tmpfs    /tmp

# Container runtime primitives (docker/podman/buildah)
mount -t cgroup2 none /sys/fs/cgroup 2>/dev/null || true
echo '+cpu +memory +pids +io' > /sys/fs/cgroup/cgroup.subtree_control 2>/dev/null || true
modprobe overlay 2>/dev/null || true

hostname vpod
ip link set lo up 2>/dev/null || true

modprobe virtio_mmio 2>/dev/null || true
modprobe virtio_net  2>/dev/null || true
modprobe virtio_blk  2>/dev/null || true
modprobe virtiofs    2>/dev/null || true

ip link set eth0 up                       2>/dev/null || true
ip addr add 10.0.2.15/24 dev eth0         2>/dev/null || true
ip route add default via 10.0.2.2         2>/dev/null || true
echo "nameserver 10.0.2.2" > /etc/resolv.conf

if [ -c /dev/hvc0 ]; then
    (
        echo "VPOD_READY" >/dev/hvc0
        while IFS= read -r cmd </dev/hvc0; do
            [ -z "$cmd" ] && continue
            sh -c "$cmd" >/dev/hvc0 2>&1
            printf 'VPOD_EXIT:%d\n' "$?" >/dev/hvc0
        done
    ) &
fi

export TERM=dumb
export HOME=/root

export SSL_CERT_FILE=/etc/ssl/vpod/ca-only.pem
export REQUESTS_CA_BUNDLE=/etc/ssl/vpod/ca-only.pem
export PIP_CERT=/etc/ssl/vpod/ca-only.pem
export NODE_EXTRA_CA_CERTS=/etc/ssl/vpod/ca-only.pem

# Enables container/docker runtimes
export DOCKER_RAMDISK=1

export ENV=''
unset HISTFILE
set +o history 2>/dev/null || true
exec setsid sh -c 'HISTFILE=/dev/null HISTSIZE=0 HOME=/root SSL_CERT_FILE=/etc/ssl/vpod/ca-only.pem exec sh </dev/ttyS0 >/dev/ttyS0 2>&1'
INIT_EOF
chmod +x "$OVERLAY/sbin/init"
ln -sf /sbin/init "$OVERLAY/init"


echo "── Packing ${NAME} initrd (image rootfs + modules + overlay)..."
(cd "$ROOTFS"  && find . | sort | cpio -H newc -o --quiet) | gzip -9 > "$PART_MINI"
(cd "$OVERLAY" && find . | sort | cpio -H newc -o --quiet) | gzip -9 > "$PART_OVL"
cat "$PART_MINI" "$PART_OVL" > "$INITRD"
rm -f "$PART_MINI" "$PART_OVL"
echo "   Done: $INITRD ($(du -sh "$INITRD" | cut -f1))"

BOOTARGS="root=/dev/ram0 rw console=ttyS0 earlycon init=/sbin/init"

echo "── Booting guest to finalize the snapshot..."

CA_MARKER="$(sed -n '2p' "$ROOT/crates/machine/assets/tls/vpod-ca-cert.pem")"
BUILD_LOG="$ROOT/dist/.snapshot-build.log"
NOW="$(date -u '+%Y-%m-%d %H:%M:%S')"

SETUP_CMD=""
SETUP_CMD="${SETUP_CMD}date -s '$NOW'; "

# Initialize the crng first — anything started later in this boot (the
# warm-python daemon included) may issue a blocking getrandom().
SETUP_CMD="${SETUP_CMD}/usr/lib/vpod/vpod-seed-entropy && rm -f /etc/vpod/entropy-seed && echo VPOD_ENTROPY_SEEDED; "

# Trust the vpod proxy CA. `update-ca-certificates` when the image ships
# it (Alpine ca-certificates package); plain bundle append otherwise.
SETUP_CMD="${SETUP_CMD}command -v update-ca-certificates >/dev/null && update-ca-certificates; "
SETUP_CMD="${SETUP_CMD}mkdir -p /etc/ssl/certs; "
SETUP_CMD="${SETUP_CMD}grep -qF '$CA_MARKER' /etc/ssl/certs/ca-certificates.crt 2>/dev/null || cat /usr/local/share/ca-certificates/vpod-ca.crt >> /etc/ssl/certs/ca-certificates.crt; "
SETUP_CMD="${SETUP_CMD}if grep -qF '$CA_MARKER' /etc/ssl/certs/ca-certificates.crt; then echo VPOD_CA_INSTALLED; else echo VPOD_CA_FAILED; fi; "

# Swap busybox's ssl_client for the vpod one so wget https terminates TLS
# host-side. Install ours even when the image has no busybox ssl_client.
SETUP_CMD="${SETUP_CMD}[ -f /usr/bin/ssl_client ] && cp /usr/bin/ssl_client /usr/bin/ssl_client.real; "
SETUP_CMD="${SETUP_CMD}cp /usr/lib/vpod/vpod-ssl-client /usr/bin/ssl_client && chmod +x /usr/bin/ssl_client && echo VPOD_SSL_CLIENT_SWAPPED; "

if [ "$PY_PRESENT" = "1" ]; then
    SETUP_CMD="${SETUP_CMD}rm -f /usr/lib/python3.*/EXTERNALLY-MANAGED; mkdir -p /root/.cache; "
    SETUP_CMD="${SETUP_CMD}PYBIN=\$(readlink -f /usr/bin/python3) && cp \$PYBIN /usr/bin/python3.real && cp /usr/lib/vpod/vpod-python-shim \$PYBIN && chmod +x \$PYBIN /usr/bin/python3.real && echo VPOD_PY_SHIM_INSTALLED; "
    SETUP_CMD="${SETUP_CMD}/usr/bin/python3.real /usr/lib/vpod/pydaemon.py </dev/null >/dev/null 2>&1 & "
    SETUP_CMD="${SETUP_CMD}n=0; while [ ! -S /run/vpod-pyd.sock ] && [ \$n -lt 300 ]; do sleep 0.1; n=\$((n+1)); done; "
    SETUP_CMD="${SETUP_CMD}[ -S /run/vpod-pyd.sock ] && /usr/bin/python3 -c 'print(\"VPOD_PYD_READY\")'; "
fi

SETUP_CMD="${SETUP_CMD}sync"

SNAP_PY_FLAG=""
[ "$PY_PRESENT" = "1" ] && SNAP_PY_FLAG="--snapshot-python"

"$VPOD" \
    "$KERNEL" \
    --bios "$OPENSBI_FW" \
    --initrd "$INITRD" \
    --ram "$RAM_MB" \
    --bootargs "$BOOTARGS" \
    --setup "$SETUP_CMD" \
    --snapshot-save "$OUT" \
    $SNAP_PY_FLAG 2>&1 | tee "$BUILD_LOG"

# Anchored: the setup harness echoes the command line (which embeds the
# marker names) into the log, so an unanchored grep would pass without
# the guest ever running anything. Real marker output starts the line.
if grep -q "Kernel panic" "$BUILD_LOG"; then
    echo "" >&2
    echo "error: the guest kernel panicked during boot — the saved snapshot is unusable." >&2
    echo "       (see $BUILD_LOG for the boot output)" >&2
    exit 1
fi
if ! grep -q "^VPOD_ENTROPY_SEEDED" "$BUILD_LOG"; then
    echo "" >&2
    echo "error: the guest crng was not seeded — blocking getrandom() users" >&2
    echo "       (node, jvm) would hang forever in resumed guests. Aborting." >&2
    echo "       (see $BUILD_LOG for the guest setup output)" >&2
    exit 1
fi
if ! grep -q "^VPOD_CA_INSTALLED" "$BUILD_LOG"; then
    echo "" >&2
    echo "error: the vpod proxy CA is not in the guest trust store." >&2
    echo "       HTTPS interception (:443) would fail at runtime. Aborting the build." >&2
    echo "       (see $BUILD_LOG for the guest setup output)" >&2
    exit 1
fi
if ! grep -q "^VPOD_SSL_CLIENT_SWAPPED" "$BUILD_LOG"; then
    echo "" >&2
    echo "error: the vpod ssl_client was not installed." >&2
    echo "       wget https would pay the full guest-TLS cost. Aborting the build." >&2
    echo "       (see $BUILD_LOG for the guest setup output)" >&2
    exit 1
fi
if [ "$PY_PRESENT" = "1" ]; then
    if ! grep -q "^VPOD_PY_SHIM_INSTALLED" "$BUILD_LOG"; then
        echo "" >&2
        echo "error: the vpod python shim was not installed over the real python3." >&2
        echo "       (see $BUILD_LOG for the guest setup output)" >&2
        exit 1
    fi
    if ! grep -q "^VPOD_PYD_READY" "$BUILD_LOG"; then
        echo "" >&2
        echo "error: the warm-python daemon did not come up." >&2
        echo "       (see $BUILD_LOG for the guest setup output)" >&2
        exit 1
    fi
fi
rm -f "$BUILD_LOG"

SNAP_SHA256="$(shasum -a 256 "$OUT" 2>/dev/null | cut -d' ' -f1 || sha256sum "$OUT" | cut -d' ' -f1)"
SNAP_SIZE="$(wc -c < "$OUT" | tr -d ' ')"

echo ""
echo "=== Done ==="
echo ""
echo "Snapshot: $OUT"
echo ""
echo "Catalogue entry (registry/catalog.json):"
cat <<ENTRY_EOF
    {
      "id": "${NAME}-${RAM_MB}mb",
      "name": "${NAME}",
      "tag": "latest",
      "memory_label": "${RAM_MB}mb",
      "description": "Built from ${DOCKERFILE}",
      "url": "<host this file and put its URL here>",
      "sha256": "${SNAP_SHA256}",
      "size": ${SNAP_SIZE}
    }
ENTRY_EOF
if [ "$AOT" = "1" ]; then
    echo ""
    echo "── Running AOT translation pass (custom workload)..."
    set --
    OLD_IFS="$IFS"; IFS='
'
    for CMD in $TRACE_CMDS; do
        set -- "$@" --trace-cmd "$CMD"
    done
    IFS="$OLD_IFS"
    "$SCRIPT_DIR/aot-snapshot.sh" "$OUT" --workload custom "$@"
else
    echo ""
    echo "Next (optional), to build and bake AOT-translated blocks for this image:"
    echo "  ./scripts/aot-snapshot.sh \"$OUT\" --workload custom --trace-cmd '<hot command>'"
fi
echo ""
echo "Try it:"
echo "  ./target/release/vpod-native --snapshot-load \"$OUT\""
