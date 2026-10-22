#!/usr/bin/env bash
# One command: builds dist/ubuntu-x64/rocksvision on Ubuntu 22.04 or 24.04 (x86_64), plus
# BUILD-INFO.txt, README-runtime.txt, THIRD-PARTY-LICENSES.md, SHA256SUMS and
# dist/rocksvision-<version>-ubuntu-x64.tar.gz (+ .sha256). See docs/BUILD.md.
# Usage: scripts/build-ubuntu-x64.sh   (from any directory; paths with spaces are fine)
#
# Build prerequisites are installed with apt (sudo asks for your password; without sudo the
# script stops and lists the command). Rust comes from the pinned rustup-init (lib/common.sh).
# Runtime libraries are NOT installed; missing ones are reported (README-runtime.txt).
set -euo pipefail
RV_SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
# shellcheck source=lib/common.sh
. "$RV_SCRIPT_DIR/lib/common.sh"
rv_init
rv_require_host Linux x86_64
rv_require_inputs

[ -r /etc/os-release ] || rv_die "/etc/os-release not found; supported: Ubuntu 22.04 / 24.04"
# shellcheck disable=SC1091
. /etc/os-release
case "${ID:-}:${VERSION_ID:-}" in
  ubuntu:22.04 | ubuntu:24.04) ;;
  *) rv_die "unsupported OS '${PRETTY_NAME:-unknown}'; supported: Ubuntu 22.04 / 24.04 x86_64" ;;
esac

BUILD_PKGS=(build-essential pkg-config curl ca-certificates)
# Runtime (dlopen'ed or linked by winit/wgpu/rfd/arboard): see README-runtime.txt.
RUNTIME_PKGS=(libxkbcommon0 libwayland-client0 libx11-6 libxcursor1 libxi6 libxrandr2 libdbus-1-3 xdg-desktop-portal libvulkan1 mesa-vulkan-drivers libegl1 libgl1)

missing() {
  local p
  for p in "$@"; do
    dpkg-query -W -f='${Status}' "$p" 2>/dev/null | grep -q "install ok installed" || printf '%s ' "$p"
  done
}

need=$(missing "${BUILD_PKGS[@]}")
if [ -n "$need" ]; then
  # shellcheck disable=SC2086
  if [ "$(id -u)" = 0 ]; then
    rv_log "installing build prerequisites: $need"
    apt-get update && apt-get install -y $need || rv_die "apt-get install failed"
  elif command -v sudo >/dev/null 2>&1; then
    rv_log "installing build prerequisites with sudo (may ask for your password): $need"
    sudo apt-get update && sudo apt-get install -y $need || rv_die "sudo apt-get install failed"
  else
    rv_die "missing build packages: $need. Run as root: apt-get update && apt-get install -y $need"
  fi
fi

rv_ensure_rust x86_64-unknown-linux-gnu
rv_build ""
rv_stage_dist ubuntu-x64
cp "$RV_BIN" "$RV_DIST/$RV_EXE"
cp "$ROOT/THIRD-PARTY-LICENSES.md" "$RV_DIST/THIRD-PARTY-LICENSES.md"
cat >"$RV_DIST/README-runtime.txt" <<EOF
RocksVision $RV_VERSION for Ubuntu 22.04 / 24.04 x86_64 (GNU libc)

Run: ./rocksvision [PATH]        Headless: ./rocksvision --dump PATH

Single executable; compression libraries (zstd, bzip2, zlib-rs, lz4, snappy) are statically
linked. A desktop session (X11 or Wayland) needs these Ubuntu packages, normally present on a
desktop install:
  sudo apt-get install ${RUNTIME_PKGS[*]}
- libxkbcommon0, libwayland-client0, libx11-6, libxcursor1, libxi6, libxrandr2: windowing
  and input (winit loads them at run time).
- libvulkan1 + mesa-vulkan-drivers: GPU rendering (wgpu); libegl1/libgl1: OpenGL fallback
  (VMs without Vulkan).
- libdbus-1-3 + xdg-desktop-portal (with -gtk or -kde backend): Open File / Open Folder
  dialogs; zenity is used as a fallback when no portal is available.
Settings (theme, zoom) are stored in \$XDG_DATA_HOME/rocksvision (default ~/.local/share/rocksvision).
EOF
rv_build_info "$RV_DIST/BUILD-INFO.txt" ubuntu-x64 x86_64-unknown-linux-gnu
rv_sha256sums "$RV_DIST"
tgz_name="$RV_EXE-$RV_VERSION-ubuntu-x64.tar.gz"
tar -C "$RV_STAGE" -czf "$RV_STAGE/$tgz_name" ubuntu-x64
rv_archive_sum "$RV_STAGE/$tgz_name"
rv_publish_dist ubuntu-x64
tgz="$ROOT/dist/$tgz_name"

rt=$(missing "${RUNTIME_PKGS[@]}")
[ -z "$rt" ] || rv_warn "runtime packages not installed on this host (needed to run the GUI here): $rt"
printf 'Built: %s\nArchive: %s\nSHA-256: %s\n' "$RV_DIST/$RV_EXE" "$tgz" "$(cut -d' ' -f1 "$tgz.sha256")"
