# Shared POSIX sh helpers for scripts/build-*.sh (sourced, not executed).
#
# The caller sets RV_SCRIPT_DIR to its own directory before sourcing:
#   RV_SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
#   . "$RV_SCRIPT_DIR/lib/common.sh"
# Every path below is quoted, so a checkout path with spaces works.
#
# Toolchain policy: the compiler version comes from rust-toolchain.toml (single source).
# - rustup present: `rustup toolchain install <pin>` (reuses an installed one) and all cargo
#   calls go through `rustup run <pin>`.
# - only a non-rustup cargo (e.g. Homebrew): used when `rustc -V` equals the pin; otherwise the
#   script stops and explains (RV_ALLOW_TOOLCHAIN_MISMATCH=1 builds anyway and records it).
# - no cargo at all: the pinned rustup-init below is downloaded, its SHA-256 checked, and run
#   with --no-modify-path --default-toolchain none (installs into ~/.rustup and ~/.cargo).

set -eu

# rustup-init 1.28.2, hashes from
# https://static.rust-lang.org/rustup/archive/1.28.2/<triple>/rustup-init.sha256 (fetched
# 2026-10-22; the aarch64-apple-darwin binary was downloaded and re-hashed to confirm).
RV_RUSTUP_VERSION="1.28.2"
rv_rustup_sha256() {
  case "$1" in
    aarch64-apple-darwin) echo "20ef5516c31b1ac2290084199ba77dbbcaa1406c45c1d978ca68558ef5964ef5" ;;
    x86_64-apple-darwin) echo "9c331076f62b4d0edeae63d9d1c9442d5fe39b37b05025ec8d41c5ed35486496" ;;
    x86_64-unknown-linux-gnu) echo "20a06e644b0d9bd2fbdbfd52d42540bdde820ea7df86e92e533c073da0cdd43c" ;;
    *) return 1 ;;
  esac
}

rv_log() { printf '==> %s\n' "$*" >&2; }
rv_warn() { printf 'warning: %s\n' "$*" >&2; }
rv_die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

# Root of the checkout (parent of scripts/), temp dir, start marker for the freshness check.
rv_init() {
  [ -n "${RV_SCRIPT_DIR:-}" ] || rv_die "RV_SCRIPT_DIR is not set by the calling script"
  ROOT=$(CDPATH= cd -- "$RV_SCRIPT_DIR/.." && pwd -P)
  [ -f "$ROOT/Cargo.toml" ] && [ -f "$ROOT/Cargo.lock" ] || rv_die "no Cargo.toml/Cargo.lock in $ROOT"
  RV_TMP=$(mktemp -d "${TMPDIR:-/tmp}/rocksvision-build.XXXXXX")
  RV_STAGE=""
  trap 'rv_cleanup' EXIT
  trap 'exit 130' INT TERM
  RV_MARK="$RV_TMP/start-marker"
  : >"$RV_MARK"
  # Make sure the marker's mtime is strictly older than anything written from now on.
  sleep 1
  RV_EXE=$(sed -n 's/^name *= *"\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -n1)
  RV_VERSION=$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -n1)
  RV_RUST_PIN=$(sed -n 's/^channel *= *"\(.*\)"/\1/p' "$ROOT/rust-toolchain.toml" | head -n1)
  [ -n "$RV_EXE" ] && [ -n "$RV_VERSION" ] || rv_die "package name/version not found in Cargo.toml"
  [ -n "$RV_RUST_PIN" ] || rv_die "channel not found in rust-toolchain.toml"
  RV_FRESH=""
  RV_NOTE=""
}

# Removes the temp dir and an unfinished staging dir (a failed run never leaves a partial
# dist/<platform>/: output is staged and moved into place only at the end).
rv_cleanup() {
  rm -rf "$RV_TMP"
  if [ -n "${RV_STAGE:-}" ]; then rm -rf "$RV_STAGE"; fi
}

# rv_require_inputs <relative path>...: all must exist below ROOT; checked BEFORE any build,
# missing ones are listed together.
rv_require_inputs() {
  miss=""
  for f in Cargo.toml Cargo.lock rust-toolchain.toml THIRD-PARTY-LICENSES.md src/main.rs "$@"; do
    [ -f "$ROOT/$f" ] || miss="$miss
  $f"
  done
  [ -z "$miss" ] || rv_die "required input files missing in $ROOT (nothing was built):$miss"
}

# rv_require_host <uname -s> <uname -m>...  (any of the listed machine names)
rv_require_host() {
  want_os=$1
  shift
  os=$(uname -s)
  m=$(uname -m)
  [ "$os" = "$want_os" ] || rv_die "this script builds on $want_os; this host is $os"
  for a in "$@"; do
    [ "$m" = "$a" ] && return 0
  done
  rv_die "host architecture $m is not supported by this script (expected: $*)"
}

rv_sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

rv_have_rustup() {
  command -v rustup >/dev/null 2>&1
}

# cargo / rustc of the pinned toolchain.
rv_cargo() {
  if rv_have_rustup; then rustup run "$RV_RUST_PIN" cargo "$@"; else cargo "$@"; fi
}
rv_rustc() {
  if rv_have_rustup; then rustup run "$RV_RUST_PIN" rustc "$@"; else rustc "$@"; fi
}

# Downloads the pinned rustup-init for <host triple>, checks its SHA-256 and installs rustup
# without a default toolchain and without editing shell profiles.
rv_bootstrap_rustup() {
  triple=$1
  want=$(rv_rustup_sha256 "$triple") || rv_die "no pinned rustup-init hash for $triple"
  command -v curl >/dev/null 2>&1 || rv_die "curl is required to download rustup-init"
  url="https://static.rust-lang.org/rustup/archive/$RV_RUSTUP_VERSION/$triple/rustup-init"
  rv_log "downloading rustup-init $RV_RUSTUP_VERSION ($triple)"
  curl --proto '=https' --tlsv1.2 -fsSL -o "$RV_TMP/rustup-init" "$url" || rv_die "download failed: $url"
  got=$(rv_sha256 "$RV_TMP/rustup-init")
  [ "$got" = "$want" ] || rv_die "rustup-init SHA-256 mismatch (got $got, pinned $want); not running it"
  chmod +x "$RV_TMP/rustup-init"
  "$RV_TMP/rustup-init" -y --no-modify-path --default-toolchain none --profile minimal || rv_die "rustup-init failed"
  PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
  export PATH
  rv_have_rustup || rv_die "rustup was installed but is not on PATH (${CARGO_HOME:-$HOME/.cargo}/bin)"
  rv_log "rustup installed in ${CARGO_HOME:-$HOME/.cargo} (PATH not modified; add ${CARGO_HOME:-$HOME/.cargo}/bin yourself if you want it permanently)"
}

# Ensures the pinned toolchain (and, if given, the std of <target triple>) is usable.
# rv_ensure_rust <host triple> [<target triple>]
rv_ensure_rust() {
  host=$1
  target=${2:-}
  # A rustup installed by an earlier run of these scripts (PATH not modified).
  if ! rv_have_rustup && [ -x "${CARGO_HOME:-$HOME/.cargo}/bin/rustup" ]; then
    PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
    export PATH
  fi
  if ! rv_have_rustup && ! command -v cargo >/dev/null 2>&1; then
    rv_bootstrap_rustup "$host"
  fi
  if rv_have_rustup; then
    rv_log "toolchain $RV_RUST_PIN via rustup (installed once, reused afterwards)"
    rustup toolchain install "$RV_RUST_PIN" --profile minimal >&2 || rv_die "rustup could not install toolchain $RV_RUST_PIN"
    if [ -n "$target" ]; then
      rustup target add --toolchain "$RV_RUST_PIN" "$target" >&2 || rv_die "rustup could not add target $target"
    fi
  else
    have=$(rustc -V | cut -d' ' -f2)
    if [ "$have" != "$RV_RUST_PIN" ]; then
      if [ "${RV_ALLOW_TOOLCHAIN_MISMATCH:-0}" = 1 ]; then
        rv_warn "rustc $have differs from the pinned $RV_RUST_PIN (RV_ALLOW_TOOLCHAIN_MISMATCH=1)"
        RV_NOTE="${RV_NOTE}toolchain: rustc $have used instead of the pinned $RV_RUST_PIN
"
      else
        rv_die "rustc $have (no rustup) differs from the pinned $RV_RUST_PIN in rust-toolchain.toml. Install rustup (https://rustup.rs, or re-run without cargo on PATH to let this script bootstrap the pinned rustup-init), or set RV_ALLOW_TOOLCHAIN_MISMATCH=1 to build with $have."
      fi
    fi
    if [ -n "$target" ] && [ ! -d "$(rustc --print sysroot)/lib/rustlib/$target" ]; then
      rv_die "the standard library for $target is not installed and this cargo/rustc ($(command -v rustc)) is not managed by rustup, so it cannot be added. Nothing was installed. Install rustup (e.g. 'brew install rustup' or https://rustup.rs) and re-run."
    fi
  fi
}

# rv_build [<target triple>] — cargo build --release --locked; sets RV_BIN and RV_FRESH.
# The binary path is the "executable" cargo reports for the bin artifact, so CARGO_TARGET_DIR,
# build.target-dir in .cargo/config.toml and the --target subdirectory are all honoured.
# Freshness: the binary must be newer than the script start, or cargo must report the artifact
# as up to date with the current sources/lockfile ("fresh": nothing changed since the last build,
# the cache is reused). Anything else is an error, never a stale success.
rv_build() {
  target=${1:-}
  if [ -n "$target" ]; then set -- --target "$target"; else set --; fi
  rv_log "cargo build --release --locked $*"
  # rustup selects the toolchain from the working directory; run in a subshell inside ROOT.
  if ! (cd "$ROOT" && rv_cargo build --release --locked --message-format=json-render-diagnostics "$@") >"$RV_TMP/cargo.json"; then
    rv_die "cargo build failed (see above). If cargo says the lock file needs to be updated, Cargo.lock is stale for Cargo.toml: this build uses --locked and never rewrites it."
  fi
  art=$(grep '"reason":"compiler-artifact"' "$RV_TMP/cargo.json" | grep '"kind":\["bin"\]' | grep -F "\"name\":\"$RV_EXE\"" | grep '"executable":"' | tail -n1 || true)
  [ -n "$art" ] || rv_die "cargo did not report the $RV_EXE executable (no bin compiler-artifact message)"
  RV_BIN=$(printf '%s\n' "$art" | sed 's/.*"executable":"\([^"]*\)".*/\1/')
  case "$RV_BIN" in
    /*) ;;
    *) rv_die "unexpected executable path from cargo: $RV_BIN" ;;
  esac
  if [ -n "$target" ]; then
    case "$RV_BIN" in
      */"$target"/release/*) ;;
      *) rv_die "cargo reported $RV_BIN, which is not a $target build" ;;
    esac
  fi
  [ -f "$RV_BIN" ] || rv_die "cargo succeeded but $RV_BIN does not exist"
  if [ -n "$(find "$RV_BIN" -newer "$RV_MARK" 2>/dev/null)" ]; then
    RV_FRESH="built in this run"
  elif printf '%s' "$art" | grep -q '"fresh":true'; then
    RV_FRESH="unchanged since the previous build (cargo verified it is up to date with the sources and Cargo.lock; cache reused)"
  else
    rv_die "$RV_BIN is older than this run and cargo did not report it up to date: refusing to package stale output"
  fi
  rv_log "binary: $RV_BIN ($RV_FRESH)"
}

# SHA-256 over sorted src/** + Cargo.toml + Cargo.lock: each line "<sha256>  <relative path>",
# '/' separators, paths sorted bytewise (LC_ALL=C); the id is the SHA-256 of those lines.
# build-windows-x64.ps1 computes the same id.
rv_source_id() {
  (
    cd "$ROOT"
    { find src -type f | LC_ALL=C sort; printf '%s\n' Cargo.toml Cargo.lock; } | while IFS= read -r f; do
      printf '%s  %s\n' "$(rv_sha256 "$f")" "$f"
    done
  ) >"$RV_TMP/source-manifest"
  rv_sha256 "$RV_TMP/source-manifest"
}

rv_vcs() {
  if command -v git >/dev/null 2>&1 && git -C "$ROOT" rev-parse --git-dir >/dev/null 2>&1; then
    rev=$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || echo "no commit")
    if [ -n "$(git -C "$ROOT" status --porcelain 2>/dev/null)" ]; then state=dirty; else state=clean; fi
    echo "git $rev ($state)"
  else
    echo "none (no git repository; identity = source id)"
  fi
}

# rv_build_info <out file> <platform> <target triple>
rv_build_info() {
  {
    echo "name: $RV_EXE"
    echo "version: $RV_VERSION"
    echo "platform: $2"
    echo "target: $3"
    echo "rustc: $(cd "$ROOT" && rv_rustc -V)"
    echo "cargo: $(cd "$ROOT" && rv_cargo -V)"
    echo "built (UTC): $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
    echo "build host: $(uname -srm)"
    echo "source id (sha256 of src/**, Cargo.toml, Cargo.lock): $(rv_source_id)"
    echo "vcs: $(rv_vcs)"
    echo "build: cargo build --release --locked; $RV_FRESH"
    printf '%s' "$RV_NOTE"
  } >"$1"
}

# Staging: output is assembled in dist/.staging-<platform>/<platform>/ (archive next to it) and
# moved into place by rv_publish_dist only after every step succeeded; on failure the EXIT trap
# removes the staging dir and the previous dist/<platform>/ stays untouched.
# rv_stage_dist <platform>  -> sets RV_STAGE (staging root) and RV_DIST (staged platform dir)
rv_stage_dist() {
  case "$1" in
    macos-arm64 | macos-x64 | ubuntu-x64) ;;
    *) rv_die "unknown platform dir '$1'" ;;
  esac
  RV_STAGE="$ROOT/dist/.staging-$1"
  rm -rf "$RV_STAGE"
  RV_DIST="$RV_STAGE/$1"
  mkdir -p "$RV_DIST"
}

# Replaces only dist/<platform> and this platform's archive files with the staged ones.
# rv_publish_dist <platform>  -> RV_DIST = dist/<platform>
rv_publish_dist() {
  rm -rf "$ROOT/dist/$1" "$ROOT/dist/$RV_EXE-$RV_VERSION-$1".*
  mv "$RV_STAGE/$1" "$ROOT/dist/$1"
  for a in "$RV_STAGE/$RV_EXE-$RV_VERSION-$1".*; do
    [ -e "$a" ] && mv "$a" "$ROOT/dist/"
  done
  rm -rf "$RV_STAGE"
  RV_STAGE=""
  RV_DIST="$ROOT/dist/$1"
}

# SHA256SUMS of every file below <dir> (relative paths, sorted), written into <dir>.
rv_sha256sums() {
  (
    cd "$1"
    find . -type f ! -name SHA256SUMS | sed 's|^\./||' | LC_ALL=C sort | while IFS= read -r f; do
      printf '%s  %s\n' "$(rv_sha256 "$f")" "$f"
    done
  ) >"$RV_TMP/SHA256SUMS"
  mv "$RV_TMP/SHA256SUMS" "$1/SHA256SUMS"
}

# Writes "<sha256>  <file name>" next to an archive.
rv_archive_sum() {
  (cd "$(dirname "$1")" && printf '%s  %s\n' "$(rv_sha256 "$(basename "$1")")" "$(basename "$1")") >"$1.sha256"
}
