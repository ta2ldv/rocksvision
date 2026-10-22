# Building RocksVision

One command per platform. Every script resolves paths from its own location (any working
directory, checkout paths with spaces), checks the host, installs missing build prerequisites
where it can, builds with `cargo build --release --locked` (Cargo.lock is never rewritten) and
writes only its own `dist/<platform>/` directory plus its own archive.

| Platform | Command (from the checkout root) | Output |
|---|---|---|
| macOS Apple Silicon (arm64) | `scripts/build-macos-arm64.sh` | `dist/macos-arm64/RocksVision.app` |
| macOS Intel (x64) | `scripts/build-macos-x64.sh` | `dist/macos-x64/RocksVision.app` |
| Ubuntu 22.04 / 24.04 x64 | `scripts/build-ubuntu-x64.sh` | `dist/ubuntu-x64/rocksvision` |
| Windows 10 / 11 x64 | `powershell -NoProfile -ExecutionPolicy Bypass -File scripts\build-windows-x64.ps1` | `dist\windows-x64\rocksvision.exe` |

`scripts/build-app.sh` still works and runs `build-macos-arm64.sh` (the bundle moved from
`dist/RocksVision.app` to `dist/macos-arm64/RocksVision.app`; the wrapper prints the new path).

Required inputs (Cargo.toml, Cargo.lock, rust-toolchain.toml, THIRD-PARTY-LICENSES.md, src/, and
for macOS src/branding.rs + the approved logo) are checked before anything is installed or built;
missing ones are listed together. Output is assembled in `dist/.staging-<platform>/` and moved
into place only at the end, so a failed run leaves the previous `dist/<platform>/` untouched.
The binary is taken from the path cargo reports, so `CARGO_TARGET_DIR` / `build.target-dir`
are honoured.

Each `dist/<platform>/` also contains `BUILD-INFO.txt` (target triple, `rustc -V`, `cargo -V`,
UTC build time, build host, source id, vcs state, whether the binary was built in this run or
verified up to date by cargo), `THIRD-PARTY-LICENSES.md` and `SHA256SUMS` (every file of the
directory). The whole directory is packaged next to it:
`dist/rocksvision-<version>-<platform>.zip` (macOS via `ditto -c -k --keepParent`, Windows via
`Compress-Archive`) or `.tar.gz` (Ubuntu), each with a `.sha256` file.

Source id: there is no git repository, so the identity is the SHA-256 over the lines
`<sha256>  <path>` of `src/**` (sorted bytewise), `Cargo.toml` and `Cargo.lock`
(`scripts/lib/common.sh` `rv_source_id`; the PowerShell script computes the same). If the
checkout is a git repository, `vcs:` also records the commit and clean/dirty state.

## Toolchain

- Rust: pinned in `rust-toolchain.toml` (1.99.0). With rustup the scripts install that toolchain
  once and reuse it. Without any Rust they download the pinned rustup-init 1.28.2 from
  `static.rust-lang.org/rustup/archive/1.28.2/<triple>/`, check its SHA-256 (pinned in
  `scripts/lib/common.sh` / the .ps1) and install rustup without editing shell profiles
  (`--no-modify-path`; the scripts add `~/.cargo/bin` to their own PATH). A non-rustup Rust
  (e.g. Homebrew) is used only if `rustc -V` equals the pin; otherwise the script stops and
  explains (`RV_ALLOW_TOOLCHAIN_MISMATCH=1` builds anyway and records it in BUILD-INFO.txt).
- C compiler (vendored zstd 1.5.7 and libbzip2 1.0.8 are compiled by `cc`; no pkg-config,
  libclang or system codec libraries are needed):
  - macOS: Xcode Command Line Tools. If missing, the script opens Apple's installer dialog and
    stops; re-run when it has finished. If the Xcode license is not accepted it stops and asks
    for `sudo xcodebuild -license`.
  - Ubuntu: `build-essential pkg-config curl ca-certificates` via `sudo apt-get install -y`
    (sudo asks for your password; without sudo the script prints the command and stops).
  - Windows: Visual Studio Build Tools 2022 with the C++ build tools workload. If vswhere finds
    no MSVC x64 toolset, the script downloads the pinned fixed-version bootstrapper (17.14.42,
    SHA-256 checked) and runs `--add Microsoft.VisualStudio.Workload.VCTools
    --includeRecommended --passive --norestart --wait`. The installer shows a UAC prompt. If it
    reports that a reboot is needed (exit 3010) the script stops: reboot, then re-run. The script
    never reboots and never changes security settings; `-ExecutionPolicy Bypass` affects only
    that one PowerShell process.
- macOS x64 on an Apple Silicon Mac is a cross build: it needs rustup to add
  `x86_64-apple-darwin` (`rustup target add` runs automatically when rustup exists). With a
  non-rustup Rust the script stops without installing anything (`brew install rustup` or
  https://rustup.rs, then re-run). BUILD-INFO.txt then says "cross-built on an arm64 host, not run
  here".

Re-running resumes: installed tools, the toolchain and `target/` are reused. If nothing changed,
cargo reports the binary up to date ("fresh") and the dist directory is rebuilt from it; a
binary that is neither newer than the script start nor reported fresh by cargo is an error.

## Supported systems and runtime requirements

| Platform | Minimum OS | Runtime |
|---|---|---|
| macOS arm64 / x64 | macOS 11 (Info.plist `LSMinimumSystemVersion`) | nothing extra; Metal GPU |
| Ubuntu x64 | 22.04, 24.04 (glibc of the build host) | desktop session (X11 or Wayland); `libxkbcommon0 libwayland-client0 libx11-6 libxcursor1 libxi6 libxrandr2 libdbus-1-3 xdg-desktop-portal(-gtk/-kde) libvulkan1 mesa-vulkan-drivers libegl1 libgl1` (see `dist/ubuntu-x64/README-runtime.txt`); file dialogs use the desktop portal, `zenity` as fallback |
| Windows x64 | Windows 10 / 11 | no DLLs to ship (static MSVC runtime, UCRT is part of Windows); DX12 or Vulkan/GL GPU driver |

All codecs (zstd, bzip2, zlib-rs, LZ4, Snappy) are statically linked. Licenses:
`THIRD-PARTY-LICENSES.md` (copied into every dist directory and the macOS bundle).
Settings (theme, zoom): macOS `~/Library/Application Support/RocksVision`, Linux
`$XDG_DATA_HOME/rocksvision` (default `~/.local/share/rocksvision`), Windows
`%APPDATA%\RocksVision\data`.

## What was verified where (2026-10-22)

| Platform | Static review | Compiled | Run |
|---|---|---|---|
| macOS arm64 | yes | yes, by the script on this Mac (also from a checkout path with spaces) | GUI not launched in this phase; tests + headless dumps only |
| macOS x64 | yes, `sh -n`/`dash -n` | no: this Mac has Homebrew Rust without rustup, so the script stopped as designed | no |
| Ubuntu x64 | yes, `bash -n` | no (no Linux std/host here) | no |
| Windows x64 | yes, read-through only (no PowerShell here) | no | no |

Known gaps: Windows `rocksvision.exe` opens a console window next to the GUI (the binary also
serves `--dump`; no `windows_subsystem` attribute) and has no embedded .ico (the window icon is
set at run time). Linux has no .desktop file or icon theme entry. `cargo test` on Windows still
needs `shasum` (tests/zz_readonly.rs), which Windows lacks. No installer, DMG, notarization or
code signing beyond the macOS ad-hoc signature.
