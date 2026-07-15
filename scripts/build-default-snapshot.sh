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
RAM_MB=256

while [ $# -gt 0 ]; do
    case "$1" in
        --version) ALPINE_VERSION="$2"; shift 2 ;;
        --out)     OUT="$2";            shift 2 ;;
        --ram)     RAM_MB="$2";         shift 2 ;;
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

# vpod TLS-terminating proxy (phase 1): ship the proxy's CA so the guest trusts
# the leaf certs it mints for :443. update-ca-certificates (run below, after the
# ca-certificates package is installed) merges this into the system bundle.
mkdir -p "$OVERLAY/usr/local/share/ca-certificates"
cp "$ROOT/crates/machine/assets/tls/vpod-ca-cert.pem" \
    "$OVERLAY/usr/local/share/ca-certificates/vpod-ca.crt"

# Minimal trust bundle: just the vpod CA. The env vars in /sbin/init point
# tools here so they skip parsing the full Mozilla bundle (see comment there).
mkdir -p "$OVERLAY/etc/ssl/vpod"
cp "$ROOT/crates/machine/assets/tls/vpod-ca-cert.pem" \
    "$OVERLAY/etc/ssl/vpod/ca-only.pem"

# vpod ssl_client: replaces busybox's for wget https. Speaks the plaintext
# preamble to the vpod proxy instead of doing ~1s of emulated TLS per
# connection. The original is preserved as ssl_client.real for non-443
# destinations (see guest/vpod-ssl-client/vpod_ssl_client.c). The swap itself
# happens in the guest setup step below, after the base rootfs is unpacked.
echo "── Cross-compiling vpod ssl_client (riscv64-musl, static)..."
zig cc -target riscv64-linux-musl -Os -static -s \
    -o "$OVERLAY/usr/lib/vpod/vpod-ssl-client" \
    "$ROOT/guest/vpod-ssl-client/vpod_ssl_client.c"
chmod +x "$OVERLAY/usr/lib/vpod/vpod-ssl-client"

# vpod libssl shim: the ssl_client equivalent for OpenSSL clients (python,
# pip, apk, node). LD_PRELOAD'd (see /sbin/init), it intercepts SSL_* on :443
# and speaks the plaintext preamble to the proxy instead of doing guest TLS —
# real upstream TLS happens host-side. Delegates to real libssl for non-443,
# memory-BIO clients, and when VPOD_REAL_TLS=1
# (see guest/vpod-libssl-shim/vpod_libssl_shim.c).
echo "── Cross-compiling vpod libssl shim (riscv64-musl, shared)..."
zig cc -target riscv64-linux-musl -Os -shared -fPIC -s \
    -o "$OVERLAY/usr/lib/vpod/vpod-libssl-shim.so" \
    "$ROOT/guest/vpod-libssl-shim/vpod_libssl_shim.c"
chmod +x "$OVERLAY/usr/lib/vpod/vpod-libssl-shim.so"

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
# All :443 traffic terminates at the vpod proxy, so the only certificate the
# guest ever needs to verify is one minted by the vpod CA. Pointing every tool
# at a bundle containing *only* that CA makes trust-store loading ~free:
# parsing the full ~150-cert Mozilla bundle cost a measured ~0.76s per
# `ssl.create_default_context()` under emulation, paid on every urlopen().
# The full system bundle stays untouched at /etc/ssl/certs/ca-certificates.crt
# for the passthrough fallback (tools doing real TLS to real origins).
export SSL_CERT_FILE=/etc/ssl/vpod/ca-only.pem
export REQUESTS_CA_BUNDLE=/etc/ssl/vpod/ca-only.pem
export PIP_CERT=/etc/ssl/vpod/ca-only.pem
export NODE_EXTRA_CA_CERTS=/etc/ssl/vpod/ca-only.pem
# LD_PRELOAD the libssl shim for every process so OpenSSL clients (python,
# pip, apk, node) skip guest-side TLS on :443. Gated on the file existing so a
# missing/failed shim can never make exec unusable; VPOD_REAL_TLS=1 disables
# it per-process. Non-OpenSSL tools load it as a no-op.
if [ -f /usr/lib/vpod/vpod-libssl-shim.so ]; then
    export LD_PRELOAD=/usr/lib/vpod/vpod-libssl-shim.so
fi
export ENV=''
unset HISTFILE
set +o history 2>/dev/null || true
exec setsid sh -c 'HISTFILE=/dev/null HISTSIZE=0 SSL_CERT_FILE=/etc/ssl/vpod/ca-only.pem exec sh </dev/ttyS0 >/dev/ttyS0 2>&1'
INIT_EOF
chmod +x "$OVERLAY/sbin/init"
ln -sf /sbin/init "$OVERLAY/init"

cat > "$OVERLAY/usr/lib/vpod/pyrunner.py" << 'PYRUNNER_EOF'
import sys, io, traceback, base64

# Pre-import the https stack while the snapshot is being built: these imports
# cost a measured ~2.5s under emulation, and pyrunner is live when the snapshot
# is taken, so every session inherits them already warm.
try:
    import ssl, urllib.request
except ImportError:
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

# SNAP="$ROOT/dist/vsnap-base-${RAM_MB}mb.snap"
SNAP="$ROOT/dist/alpine-3.23.0-256mb.snap"
BOOTARGS="root=/dev/ram0 rw console=ttyS0 earlycon init=/sbin/init"

echo "── Booting guest to pre-install ca-certificates + python3..."

# A stable, cert-specific line from the vpod CA PEM. The guest greps its trust
# bundle for this after installing, so we can prove the CA actually landed
# (a silently-missing CA = confusing HTTPS breakage at runtime, not build time).
CA_MARKER="$(sed -n '2p' "$ROOT/crates/machine/assets/tls/vpod-ca-cert.pem")"
BUILD_LOG="$ROOT/dist/.snapshot-build.log"

"$VPOD" \
    "$KERNEL" \
    --bios "$OPENSBI_FW" \
    --initrd "$OUT" \
    --ram "$RAM_MB" \
    --bootargs "$BOOTARGS" \
    --net \
    --setup "date -s '$(date -u '+%Y-%m-%d %H:%M:%S')'; sed -i 's|https://|http://|g' /etc/apk/repositories; apk update --allow-untrusted; apk add --allow-untrusted ca-certificates python3; update-ca-certificates; grep -qF '$CA_MARKER' /etc/ssl/certs/ca-certificates.crt || cat /usr/local/share/ca-certificates/vpod-ca.crt >> /etc/ssl/certs/ca-certificates.crt; if grep -qF '$CA_MARKER' /etc/ssl/certs/ca-certificates.crt; then echo VPOD_CA_INSTALLED; else echo VPOD_CA_FAILED; fi; sed -i 's|http://|https://|g' /etc/apk/repositories; cp /usr/bin/ssl_client /usr/bin/ssl_client.real && cp /usr/lib/vpod/vpod-ssl-client /usr/bin/ssl_client && chmod +x /usr/bin/ssl_client && echo VPOD_SSL_CLIENT_SWAPPED; sync" \
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
rm -f "$BUILD_LOG"

echo ""
echo "=== Done ==="
echo ""
echo "Snapshot: $SNAP"
