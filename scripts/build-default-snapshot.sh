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

ALPINE_MINOR="${ALPINE_VERSION%.*}"   # e.g. 3.23
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
for cmd in curl bsdtar cpio gzip cargo; do
    command -v "$cmd" >/dev/null || MISSING="$MISSING $cmd"
done
if [ -n "$MISSING" ]; then
    echo "ERROR: missing tools:$MISSING"
    echo "  macOS  : brew install libarchive"
    echo "  Debian : apt install libarchive-tools bsdtar cpio"
    echo "  Fedora : dnf install bsdtar libarchive"
    echo "  Windows: use WSL2 and follow the Linux instructions"
    exit 1
fi
echo "   OK"

mkdir -p "$ROOT/dist" "$ALPINE_DIR"

if [ ! -f "$VPOD" ]; then
    echo "── Building vpod..."
    (cd "$ROOT" && cargo build --release --bin vpod-native)
else
    echo "── vpod already built, skipping."
fi

# Download pre-built OpenSBI fw_jump.bin
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

# Download Alpine ISO + extract kernel
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


echo "── Building agent overlay..."
rm -rf "$OVERLAY"
mkdir -p "$OVERLAY/sbin" "$OVERLAY/etc/apk"

printf 'https://dl-cdn.alpinelinux.org/alpine/v%s/main\nhttps://dl-cdn.alpinelinux.org/alpine/v%s/community\n' \
    "$ALPINE_MINOR" "$ALPINE_MINOR" > "$OVERLAY/etc/apk/repositories"
echo 'nameserver 8.8.8.8' > "$OVERLAY/etc/resolv.conf"

cat > "$OVERLAY/sbin/init" << 'INIT_EOF'
#!/bin/sh

export PATH=/usr/bin:/usr/sbin:/bin:/sbin

mount -t proc     proc     /proc
mount -t sysfs    sysfs    /sys
mount -t devtmpfs devtmpfs /dev
mount -t tmpfs    tmpfs    /tmp

ip link set lo up 2>/dev/null || true

modprobe virtio_mmio 2>/dev/null || true
modprobe virtio_net  2>/dev/null || true
modprobe virtio_blk  2>/dev/null || true

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
export SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
export ENV=''
unset HISTFILE
set +o history 2>/dev/null || true
exec setsid sh -c 'HISTFILE=/dev/null HISTSIZE=0 SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt exec sh </dev/ttyS0 >/dev/ttyS0 2>&1'
INIT_EOF
chmod +x "$OVERLAY/sbin/init"
ln -sf /sbin/init "$OVERLAY/init"

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

SNAP="$ROOT/dist/alpine-3.23.0-256mb.snap"
BOOTARGS="root=/dev/ram0 rw console=ttyS0 earlycon init=/sbin/init"

echo "── Booting guest to pre-install ca-certificates + python3..."
"$VPOD" \
    "$KERNEL" \
    --bios "$OPENSBI_FW" \
    --initrd "$OUT" \
    --ram "$RAM_MB" \
    --bootargs "$BOOTARGS" \
    --net \
    --setup "date -s '$(date -u '+%Y-%m-%d %H:%M:%S')'; sed -i 's|https://|http://|g' /etc/apk/repositories; apk update --allow-untrusted; apk add --allow-untrusted ca-certificates python3 py3-pip; sed -i 's|http://|https://|g' /etc/apk/repositories; sync" \
    --setup "HISTFILE=/dev/null HISTSIZE=0 exec sh" \
    --snapshot-save "$SNAP" \
    --snapshot-warm

echo ""
echo "=== Done ==="
echo ""
echo "Snapshot: $SNAP"
