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
| Windows 10 / 11 x64 (Server: see below) | `powershell -NoProfile -ExecutionPolicy Bypass -File scripts\build-windows-x64.ps1` | `dist\windows-x64\rocksvision.exe` |

`scripts/build-app.sh` still works and runs `build-macos-arm64.sh` (the bundle moved from
`dist/RocksVision.app` to `dist/macos-arm64/RocksVision.app`; the wrapper prints the new path).

Required inputs (Cargo.toml, Cargo.lock, rust-toolchain.toml, LICENSE, THIRD-PARTY-LICENSES.md,
src/, and for macOS src/branding.rs + the approved logo) are checked before anything is installed
or built; missing ones are listed together. Output is assembled in `dist/.staging-<platform>/`
and moved into place only at the end, so a failed run leaves the previous `dist/<platform>/`
untouched. The binary is taken from the path cargo reports, so `CARGO_TARGET_DIR` /
`build.target-dir` are honoured.

Each `dist/<platform>/` also contains `BUILD-INFO.txt` (target triple, `rustc -V`, `cargo -V`,
UTC build time, build host, source id, vcs state, whether the binary was built in this run or
verified up to date by cargo; on Windows also the DLL import list), `LICENSE` (Apache-2.0, the
project licence), `THIRD-PARTY-LICENSES.md` and `SHA256SUMS` (every file of the directory). The
macOS bundle also carries both licence files in `Contents/Resources/`. The whole directory is
packaged next to it: `dist/rocksvision-<version>-<platform>.zip` (macOS via
`ditto -c -k --keepParent`, Windows via `Compress-Archive`) or `.tar.gz` (Ubuntu), each with a
`.sha256` file.

Source id: the SHA-256 over the lines `<sha256>  <path>` of `src/**` (sorted bytewise),
`Cargo.toml` and `Cargo.lock` (`scripts/lib/common.sh` `rv_source_id`; the PowerShell script
computes the same). In a git checkout, `vcs:` also records the commit and clean/dirty state.
A CRLF checkout yields a different source id than an LF one. `.gitattributes` keeps RocksDB
data, fixtures and images byte-exact (`-text`) on Windows checkouts.

## Host and target checks

Before building, every script checks the toolchain phase separately: the pinned cargo must start
and `rustc -vV` must report the expected host (`aarch64-apple-darwin` / `x86_64-apple-darwin`,
`x86_64-unknown-linux-gnu`, `x86_64-pc-windows-msvc`). The produced binary's architecture is
then checked, so a configured foreign target (`build.target` in a cargo config,
`CARGO_BUILD_TARGET`) cannot be packaged under the wrong platform name:

- Windows and Ubuntu build with an explicit `--target <triple>` (output under
  `target/<triple>/release/`) and check the PE machine (AMD64) / ELF header (x86-64).
- macOS x64 builds with `--target x86_64-apple-darwin`; macOS arm64 builds natively, refuses a
  set `CARGO_BUILD_TARGET`, and both check the Mach-O architecture with `lipo`.

Errors name their phase: `[toolchain]` (rustup/cargo could not start; cargo did not run and
Cargo.lock is not involved), `[lockfile]` (cargo confirmed offline that Cargo.lock does not
match Cargo.toml under `--locked`) or `[cargo build]` (compilation failed).

## Toolchain

- Rust: pinned in `rust-toolchain.toml` (1.99.0). With rustup the scripts install that toolchain
  once and reuse it. Without any Rust they download the pinned rustup-init 1.28.2 from
  `static.rust-lang.org/rustup/archive/1.28.2/<triple>/`, check its SHA-256 (pinned in
  `scripts/lib/common.sh` / the .ps1) and install rustup without editing shell profiles
  (`--no-modify-path`; the scripts add `$CARGO_HOME/bin`, default `~/.cargo/bin`, to their own
  PATH). A non-rustup Rust (e.g. Homebrew) is used only if `rustc -V` equals the pin; otherwise
  the script stops and explains (`RV_ALLOW_TOOLCHAIN_MISMATCH=1` builds anyway and records it in
  BUILD-INFO.txt).
- Windows rustup: the script resolves `%CARGO_HOME%\bin\rustup.exe` (default
  `%USERPROFILE%\.cargo\bin`) and calls it by full path (`rustup.exe run 1.99.0 cargo ...`),
  prepends that directory to its own process PATH, installs the toolchain with
  `--no-self-update`, and verifies `rustup.exe run 1.99.0 cargo --version` before building. A
  fresh install, a rustup that self-updated earlier, and an existing install all work in the
  same PowerShell window without manual PATH edits or a new shell.
- C compiler (vendored zstd 1.5.7 and libbzip2 1.0.8 are compiled by `cc`; no pkg-config,
  libclang or system codec libraries are needed):
  - macOS: Xcode Command Line Tools. If missing, the script opens Apple's installer dialog and
    stops; re-run when it has finished. If the Xcode license is not accepted it stops and asks
    for `sudo xcodebuild -license`.
  - Ubuntu: `build-essential pkg-config curl ca-certificates` via `apt-get install -y`: with
    passwordless `sudo -n` when available (CI), otherwise `sudo` asks for your password on a
    terminal; without either the script prints the command and stops.
  - Windows: Visual Studio Build Tools 2022 with the C++ build tools workload. If vswhere finds
    no MSVC x64 toolset (any VS 2022 or newer edition counts), the script downloads the pinned
    fixed-version bootstrapper (17.14.42, SHA-256 checked) and runs `--add
    Microsoft.VisualStudio.Workload.VCTools --includeRecommended --passive --norestart --wait`.
    The installer shows a UAC prompt (administrator rights). If it reports that a reboot is
    needed (exit 3010) the script stops: reboot, then re-run. The script never reboots and never
    changes security settings; `-ExecutionPolicy Bypass` affects only that one PowerShell process.
- macOS x64 on an Apple Silicon Mac is a cross build: it needs rustup to add
  `x86_64-apple-darwin` (`rustup target add` runs automatically when rustup exists). With a
  non-rustup Rust the script stops without installing anything (`brew install rustup` or
  https://rustup.rs, then re-run). BUILD-INFO.txt then says "cross-built on an arm64 host, not run
  here". On an Intel Mac (or the `macos-15-intel` CI runner) it is a native build.

Re-running resumes: installed tools, the toolchain and `target/` are reused. If nothing changed,
cargo reports the binary up to date ("fresh") and the dist directory is rebuilt from it; a
binary that is neither newer than the script start nor reported fresh by cargo is an error.

## Supported systems and runtime requirements

| Platform | Minimum OS | Runtime |
|---|---|---|
| macOS arm64 / x64 | macOS 11 (Info.plist `LSMinimumSystemVersion`) | nothing extra; Metal GPU |
| Ubuntu x64 | 22.04, 24.04 (glibc of the build host) | desktop session (X11 or Wayland); `libxkbcommon0 libwayland-client0 libx11-6 libxcursor1 libxi6 libxrandr2 libdbus-1-3 xdg-desktop-portal(-gtk/-kde) libvulkan1 mesa-vulkan-drivers libegl1 libgl1` (see `dist/ubuntu-x64/README-runtime.txt`); file dialogs use the desktop portal, `zenity` as fallback |
| Windows x64 | Windows 10 / 11 | MSVC C runtime, see below; DX12 or Vulkan/GL GPU driver |

Windows C runtime (pending verification): no `+crt-static` is configured (no `.cargo/config`,
nothing in Cargo.toml), so Rust's MSVC default links the C runtime dynamically. Expected imports
are `VCRUNTIME140.dll` (Microsoft Visual C++ 2015-2022 Redistributable x64, not guaranteed on a
clean Windows install) and `api-ms-win-crt-*.dll` (Universal CRT, part of Windows 10/11 and
Server 2016+), plus system DLLs. Check with `dumpbin /dependents rocksvision.exe`; the build
script runs this from the detected MSVC toolset and records the list in BUILD-INFO.txt. A machine
that has Visual Studio / Build Tools installed already has the redistributable, so a successful
launch there does not prove a clean machine works. Static linking (`+crt-static`) is a possible
later change; it is not made in this phase.

Windows Server: the script accepts any x64 NT 10.0 system (Windows Server 2016/2019/2022/2025
included) and needs the same prerequisites (VS Build Tools with the C++ workload, rustup). The
GitHub `windows-2022` runner is Windows Server 2022 with Visual Studio 2022 preinstalled; the
script is meant to detect that toolset and skip the installer. We have not run the script or the
GUI on Windows Server (the workflow below has not been run yet either). Running the GUI on Server
additionally needs a desktop session (Desktop Experience) and a working DX12/Vulkan/GL driver;
untested.

All codecs (zstd, bzip2, zlib-rs, LZ4, Snappy) are statically linked. Licenses: `LICENSE`
(Apache-2.0) and `THIRD-PARTY-LICENSES.md`, copied into every dist directory, archive and the
macOS bundle. Settings (theme, zoom): macOS `~/Library/Application Support/RocksVision`, Linux
`$XDG_DATA_HOME/rocksvision` (default `~/.local/share/rocksvision`), Windows
`%APPDATA%\RocksVision\data`.

## GitHub Actions (`.github/workflows/release-build.yml`)

Native builds on standard GitHub-hosted runners: `macos-15` (arm64), `macos-15-intel` (x64),
`ubuntu-22.04`, `windows-2022`. Each job runs the platform script above, checks BUILD-INFO
version/target, LICENSE and checksums (`scripts/ci/check-dist.sh`), runs
`cargo test --release --locked` on all four runners (checksums verified in pure Rust), runs a
`--dump` CLI smoke check, and uploads the archive + `.sha256` as a 7-day artifact. CI success
shows compilation, packaging, tests and CLI output only, not GUI behaviour.

Triggers: only `release: published` and `workflow_dispatch` (no run on branch pushes, bare tag
pushes or draft releases). Owner steps:
1. Push the workflow to the default branch.
2. Optional: Actions > release-build > Run workflow: build/test/package + artifacts only; never
   touches a release.
3. Set `version` in Cargo.toml to X.Y.Z (>= 0.1.1), commit, push, tag that commit `vX.Y.Z`,
   then publish a GitHub release for the tag. The workflow builds the tagged commit (not the
   branch tip) and, after all four builds pass, uploads the four archives + `.sha256` files to
   that same release (`gh release upload`, no `--clobber`).

Gating: only `ta2ldv` as actor and triggering actor; release writes only in
`ta2ldv/rocksvision`; tag must be strict `vMAJOR.MINOR.PATCH`, >= 0.1.1 and equal to Cargo.toml
at the tagged commit (`scripts/ci/validate-tag.sh`, so publishing v0.1.0 again is refused); the
published release must exist and none of the eight asset names may exist yet, checked again
right before upload (`scripts/ci/check-release-absent.sh`, fail-closed); a failed platform means
no upload. The release is public without assets until the upload finishes. With GitHub's
immutable releases enabled, assets cannot be added after publication (GitHub docs, to verify), so
keep that setting OFF for this flow; the check refuses a release reported as immutable.

## What was verified where (2026-10-24)

| Platform | Static review | Compiled | Run |
|---|---|---|---|
| macOS arm64 | yes | yes, by the script on the developer Mac and by the user (v0.1.0 release) | tests, headless dumps and interactive GUI on the developer Mac |
| macOS x64 | yes, `sh -n`/`dash -n` | no (no Intel Mac/rustup cross target here; CI job not run yet) | no |
| Ubuntu x64 | yes, `bash -n` | no (no Linux host here; CI job not run yet) | no |
| Windows x64 | yes, read-through only (no PowerShell here) | yes, by the user locally for v0.1.0 after manual PATH troubleshooting; the reworked rustup resolution awaits the user's re-test | user launched the v0.1.0 GUI; no independent verification |
| GitHub Actions | YAML parsed (Ruby Psych), scripts `bash -n`, tag validation exercised locally | not run (needs the owner's push / dispatch) | no |

Version 0.1.1 (Cargo.toml; About, BUILD-INFO and archive names derive from it; v0.1.0 stays as
published). Windows 0.1.1 uses the GUI subsystem (no console window from Explorer) and attaches to
the parent console so `--dump`/`--help`/errors print in a terminal (the prompt may appear before
the output); not compiler-verified for Windows yet. Linux has no .desktop file or icon theme entry. Windows has no embedded .ico (the
window icon is set at run time). No installer, DMG, notarization or code signing beyond the
macOS ad-hoc signature.
