#!/bin/sh

set -e

cleanup() {
    rm -rf "$ROOT/dist/agent-minirootfs" "$ROOT/dist/agent-mini.cpio.gz" "$ROOT/dist/agent-overlay.cpio.gz"
}
trap cleanup EXIT

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

ALPINE_VERSION="3.23.0"
OUT="$ROOT/dist/rootfs.cpio.gz"
RAM_MB=512
NO_AOT=0

while [ $# -gt 0 ]; do
    case "$1" in
        --version) ALPINE_VERSION="$2"; shift 2 ;;
        --out)     OUT="$2";            shift 2 ;;
        --ram)     RAM_MB="$2";         shift 2 ;;
        --no-aot)  NO_AOT=1;            shift ;;
        *) echo "unknown arg: $1"; exit 1 ;;
    esac
done

ALPINE_MINOR="${ALPINE_VERSION%.*}"
ALPINE_DIR="$ROOT/dist/alpine-standard-${ALPINE_VERSION}-riscv64"
ISO_URL="https://dl-cdn.alpinelinux.org/alpine/v${ALPINE_MINOR}/releases/riscv64/alpine-standard-${ALPINE_VERSION}-riscv64.iso"
MINIROOTFS_URL="https://dl-cdn.alpinelinux.org/alpine/v${ALPINE_MINOR}/releases/riscv64/alpine-minirootfs-${ALPINE_VERSION}-riscv64.tar.gz"
ISO="$ROOT/dist/alpine-standard-${ALPINE_VERSION}-riscv64.iso"
MINIROOTFS="$ROOT/dist/alpine-minirootfs-${ALPINE_VERSION}-riscv64.tar.gz"
KERNEL="$ALPINE_DIR/kernel"
INITRAMFS_LTS="$ALPINE_DIR/initramfs-lts"
OPENSBI_VERSION="1.6"
OPENSBI_FW="$ROOT/dist/fw_jump.bin"
OPENSBI_URL="https://github.com/riscv-software-src/opensbi/releases/download/v${OPENSBI_VERSION}/opensbi-${OPENSBI_VERSION}-rv-bin.tar.xz"
OPENSBI_TAR="$ROOT/dist/opensbi-${OPENSBI_VERSION}-rv-bin.tar.xz"
OVERLAY="$ROOT/dist/agent-overlay"
VPOD="$ROOT/target/release/vpod-native"

echo "=== Capsulev snapshot builder ==="
echo "Alpine : ${ALPINE_VERSION}"
echo "RAM    : ${RAM_MB} MB"
echo "Out    : ${OUT}"
echo ""

echo "── Checking host tools..."
MISSING=""
for cmd in curl bsdtar cpio gzip cargo zig; do
    command -v "$cmd" >/dev/null || MISSING="$MISSING $cmd"
done
if [ -n "$MISSING" ]; then
    echo "ERROR: missing tools:$MISSING"
    echo "  macOS  : brew install libarchive zig"
    echo "  Debian : apt install libarchive-tools bsdtar cpio zig"
    echo "  Fedora : dnf install bsdtar libarchive zig"
    echo "  Windows: use WSL2 and follow the Linux instructions"
    exit 1
fi
echo "   OK"

mkdir -p "$ROOT/dist" "$ALPINE_DIR"


echo "── Building vpod..."
(cd "$ROOT" && cargo build --release --bin vpod-native)


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
    echo "── Downloading Alpine standard ISO ${ALPINE_VERSION}..."
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

if [ ! -f "$MINIROOTFS" ]; then
    echo "── Downloading Alpine minirootfs ${ALPINE_VERSION}..."
    curl -L --progress-bar -o "$MINIROOTFS" "$MINIROOTFS_URL"
else
    echo "── Minirootfs already present, skipping download."
fi


echo "── Building overlay..."
rm -rf "$OVERLAY"
mkdir -p "$OVERLAY/sbin" "$OVERLAY/etc/apk" "$OVERLAY/usr/lib/vpod"

printf 'https://dl-cdn.alpinelinux.org/alpine/v%s/main\nhttps://dl-cdn.alpinelinux.org/alpine/v%s/community\n' \
    "$ALPINE_MINOR" "$ALPINE_MINOR" > "$OVERLAY/etc/apk/repositories"
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

mkdir -p "$OVERLAY/etc/vpod"
cat > "$OVERLAY/etc/vpod/pydaemon-warm-imports" << 'WARM_EOF'
#Warm set for the python3 shim/daemon path (commands.run("python3 ...")
#numpy
#pandas
WARM_EOF
cat > "$OVERLAY/etc/vpod/pyrunner-warm-imports" << 'WARM_EOF'
# Warm set for pyrunner, the persistent code.run() interpreter — the primary
numpy
pandas
scipy
scipy.stats
scipy.optimize
scipy.interpolate
scipy.fft
WARM_EOF

mkdir -p "$OVERLAY/etc/uv"
cat > "$OVERLAY/etc/uv/uv.toml" << 'UV_EOF'
python-preference = "only-system"
UV_EOF

echo "── Cross-compiling vpod python shim (riscv64-musl, dynamic)..."
zig cc -target riscv64-linux-musl -Os -dynamic -s \
    -o "$OVERLAY/usr/lib/vpod/vpod-python-shim" \
    "$ROOT/guest/warmpy/vpod_python_shim.c"
chmod +x "$OVERLAY/usr/lib/vpod/vpod-python-shim"
cp "$ROOT/guest/warmpy/pydaemon.py" "$OVERLAY/usr/lib/vpod/pydaemon.py"

cat > "$OVERLAY/sbin/init" << 'INIT_EOF'
#!/bin/sh

export PATH=/usr/bin:/usr/sbin:/bin:/sbin

mount -t proc     proc     /proc
mount -t sysfs    sysfs    /sys
mount -t devtmpfs devtmpfs /dev
mount -t tmpfs    tmpfs    /tmp

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

export ENV=''
unset HISTFILE
set +o history 2>/dev/null || true
exec setsid sh -c 'HISTFILE=/dev/null HISTSIZE=0 HOME=/root SSL_CERT_FILE=/etc/ssl/vpod/ca-only.pem exec sh </dev/ttyS0 >/dev/ttyS0 2>&1'
INIT_EOF
chmod +x "$OVERLAY/sbin/init"
ln -sf /sbin/init "$OVERLAY/init"

cat > "$OVERLAY/usr/lib/vpod/pyrunner.py" << 'PYRUNNER_EOF'
import sys, io, traceback, base64

try:
    import ssl, urllib.request
except ImportError:
    pass

# pyrunner's own warm list (pydaemon has a separate one): pyrunner is a
# persistent process, so one import at startup (snapshot build time) makes
# it warm for every code.run().
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

    _buf = io.StringIO()
    sys.stdout = _buf
    sys.stderr = _buf
    _exit_code = 0
    try:
        exec(compile(_code, "<vpod>", "exec"), _globals)
    except SystemExit as _e:
        if isinstance(_e.code, int):
            _exit_code = _e.code & 0xFF
        elif _e.code is not None:
            _buf.write(str(_e.code) + "\n")
            _exit_code = 1
    except Exception:
        _buf.write(traceback.format_exc())
        _exit_code = 1
    finally:
        sys.stdout = _real_stdout
        sys.stderr = _real_stderr

    _val = _buf.getvalue()
    if _val:
        _data_out.write(_val)
    _exit_code_out.write(bytes([_exit_code]))
    _data_out.write(_sentinel + "\n")
    _data_out.flush()
PYRUNNER_EOF

echo "── Repacking minirootfs as cpio..."
MINI_WORK="$ROOT/dist/agent-minirootfs"
rm -rf "$MINI_WORK"
mkdir -p "$MINI_WORK"
bsdtar -xf "$MINIROOTFS" -C "$MINI_WORK" --no-same-owner

echo "── Extracting kernel modules into minirootfs..."
mkdir -p "$MINI_WORK/lib"
gunzip -c "$INITRAMFS_LTS" | (cd "$MINI_WORK" && cpio -idmu --quiet 'usr/lib/modules/*') 2>/dev/null || true
if [ -d "$MINI_WORK/usr/lib/modules" ] && [ ! -e "$MINI_WORK/lib/modules" ]; then
    ln -sf /usr/lib/modules "$MINI_WORK/lib/modules"
fi

echo "── Packing rootfs.cpio.gz (minirootfs + modules + overlay)..."
PART_MINI="$ROOT/dist/agent-mini.cpio.gz"
PART_OVL="$ROOT/dist/agent-overlay.cpio.gz"
(cd "$MINI_WORK" && find . | sort | cpio -H newc -o --quiet) | gzip -9 > "$PART_MINI"
(cd "$OVERLAY"   && find . | sort | cpio -H newc -o --quiet) | gzip -9 > "$PART_OVL"
cat "$PART_MINI" "$PART_OVL" > "$OUT"
rm -f "$PART_MINI" "$PART_OVL"
echo "   Done: $OUT ($(du -sh "$OUT" | cut -f1))"

SNAP="$ROOT/dist/vsnap-data-${RAM_MB}mb.snap"
BOOTARGS="root=/dev/ram0 rw console=ttyS0 earlycon init=/sbin/init"

echo "── Booting guest to pre-install ca-certificates + python3 + data stack..."

CA_MARKER="$(sed -n '2p' "$ROOT/crates/machine/assets/tls/vpod-ca-cert.pem")"
BUILD_LOG="$ROOT/dist/.snapshot-build.log"
NOW="$(date -u '+%Y-%m-%d %H:%M:%S')"


SETUP_CMD=""
SETUP_CMD="${SETUP_CMD}date -s '$NOW'; "
SETUP_CMD="${SETUP_CMD}sed -i 's|https://|http://|g' /etc/apk/repositories; "
SETUP_CMD="${SETUP_CMD}apk update --allow-untrusted; "

SETUP_CMD="${SETUP_CMD}apk add --allow-untrusted ca-certificates python3 py3-pip uv py3-numpy py3-pandas py3-scipy; "
SETUP_CMD="${SETUP_CMD}rm -f /usr/lib/python3.*/EXTERNALLY-MANAGED; mkdir -p /root/.cache; "

SETUP_CMD="${SETUP_CMD}update-ca-certificates; "
SETUP_CMD="${SETUP_CMD}grep -qF '$CA_MARKER' /etc/ssl/certs/ca-certificates.crt || cat /usr/local/share/ca-certificates/vpod-ca.crt >> /etc/ssl/certs/ca-certificates.crt; "
SETUP_CMD="${SETUP_CMD}if grep -qF '$CA_MARKER' /etc/ssl/certs/ca-certificates.crt; then echo VPOD_CA_INSTALLED; else echo VPOD_CA_FAILED; fi; "
SETUP_CMD="${SETUP_CMD}sed -i 's|http://|https://|g' /etc/apk/repositories; "
SETUP_CMD="${SETUP_CMD}cp /usr/bin/ssl_client /usr/bin/ssl_client.real && cp /usr/lib/vpod/vpod-ssl-client /usr/bin/ssl_client && chmod +x /usr/bin/ssl_client && echo VPOD_SSL_CLIENT_SWAPPED; "
SETUP_CMD="${SETUP_CMD}PYBIN=\$(readlink -f /usr/bin/python3) && cp \$PYBIN /usr/bin/python3.real && cp /usr/lib/vpod/vpod-python-shim \$PYBIN && chmod +x \$PYBIN /usr/bin/python3.real && echo VPOD_PY_SHIM_INSTALLED; "
SETUP_CMD="${SETUP_CMD}/usr/bin/python3.real /usr/lib/vpod/pydaemon.py </dev/null >/dev/null 2>&1 & "
SETUP_CMD="${SETUP_CMD}n=0; while [ ! -S /run/vpod-pyd.sock ] && [ \$n -lt 300 ]; do sleep 0.1; n=\$((n+1)); done; "
SETUP_CMD="${SETUP_CMD}[ -S /run/vpod-pyd.sock ] && /usr/bin/python3 -c 'print(\"VPOD_PYD_READY\")'; "

SETUP_CMD="${SETUP_CMD}sync"

"$VPOD" \
    "$KERNEL" \
    --bios "$OPENSBI_FW" \
    --initrd "$OUT" \
    --ram "$RAM_MB" \
    --bootargs "$BOOTARGS" \
    --net \
    --setup "$SETUP_CMD" \
    --snapshot-save "$SNAP" \
    --snapshot-python 2>&1 | tee "$BUILD_LOG"

if ! grep -q VPOD_CA_INSTALLED "$BUILD_LOG"; then
    echo "" >&2
    echo "error: the vpod proxy CA is not in the guest trust store." >&2
    echo "       HTTPS interception (:443) would fail at runtime. Aborting the build." >&2
    echo "       (see $BUILD_LOG for the guest setup output)" >&2
    exit 1
fi
if ! grep -q VPOD_SSL_CLIENT_SWAPPED "$BUILD_LOG"; then
    echo "" >&2
    echo "error: the vpod ssl_client was not installed over busybox's." >&2
    echo "       wget https would pay the full guest-TLS cost. Aborting the build." >&2
    echo "       (see $BUILD_LOG for the guest setup output)" >&2
    exit 1
fi
if ! grep -q VPOD_PY_SHIM_INSTALLED "$BUILD_LOG"; then
    echo "" >&2
    echo "error: the vpod python shim was not installed over the real python3." >&2
    echo "       every python3 invocation would pay the ~0.65s cold start. Aborting the build." >&2
    echo "       (see $BUILD_LOG for the guest setup output)" >&2
    exit 1
fi
if ! grep -q VPOD_PYD_READY "$BUILD_LOG"; then
    echo "" >&2
    echo "error: the warm-python daemon did not come up (no socket, or the" >&2
    echo "       shim→daemon round-trip failed). Aborting the build." >&2
    echo "       (see $BUILD_LOG for the guest setup output)" >&2
    exit 1
fi
rm -f "$BUILD_LOG"

if [ "$NO_AOT" = "1" ]; then
    echo "── Skipping AOT translation pass (--no-aot)."
else
    echo "── AOT: tracing representative workload on the snapshot..."
    (cd "$ROOT" && cargo build --release -p native-cli --features aot-trace)
    AOT_TRACE="$ROOT/dist/.aot-trace.txt"
    VPOD_AOT_TRACE="$AOT_TRACE" "$VPOD" --snapshot-load "$SNAP" \
        --setup "python3 -c 'print(sum(i*i for i in range(200000)))'" \
        --setup "python3 -c 'exec(\"s=0\nfor i in range(200000): s=(s+i*i)^(i&0xff)\nprint(s)\")'" \
        --setup "python3 -c 'import numpy as np; a = np.arange(100000); print(int((a * a).sum()))'" \
        --setup "python3 -c 'import pandas as pd; df = pd.DataFrame({\"x\": range(20000)}); print(int(df.x.sum()))'" \
        --setup "i=0; while [ \$i -lt 100 ]; do echo x > /tmp/aot-\$i; i=\$((\$i+1)); done; cat /tmp/aot-* | wc -l; rm -f /tmp/aot-*" \
        --setup "uv venv /tmp/aot-venv && rm -rf /tmp/aot-venv"
    if [ ! -s "$AOT_TRACE" ]; then
        echo "error: aot trace is empty — the workload did not run" >&2
        exit 1
    fi

    echo "── AOT: translating hot blocks..."
    (cd "$ROOT" && cargo build --release -p vpod-translate)
    "$ROOT/target/release/vpod-translate" "$SNAP" "$AOT_TRACE" \
        "$ROOT/crates/riscv-core/src/aot/generated.rs"

    echo "── AOT: rebuilding vpod with translated blocks..."
    (cd "$ROOT" && cargo build --release -p native-cli --features aot)
    rm -f "$AOT_TRACE"
fi

echo ""
echo "=== Done ==="
echo ""
echo "Snapshot: $SNAP"
