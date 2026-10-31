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

The crate uses Rust edition 2024 (`Cargo.toml`: `edition = "2024"`, `rust-version = "1.95"`).
1.95 is the highest `rust-version` any locked dependency declares (eframe/egui/epaint 0.36.2;
edition 2024 itself needs 1.85), so it is the minimum that can build `Cargo.lock`, and the
MSRV-aware resolver keeps future `cargo update` runs within it. Releases and CI are built and
tested only with the pinned toolchain, 1.99.0 (`rust-toolchain.toml`); no older compiler is
verified.

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
GUI on Windows Server (the first workflow run on `windows-2022` built and packaged with this
script; its tests stopped at one arch test, fixed since, so the CLI smoke there is unverified). Running the GUI on Server
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

One workflow ("Build and release packages") with one shared build job; the ref type picks the
mode, which the run name, every job name and the run summary state:

| Trigger | Mode | Release action (only after all four builds/tests pass) |
|---|---|---|
| Push of tag `vX.Y.Z` | Release packages (build, then create or upload release vX.Y.Z) | no release for the tag: create it with the eight assets; published release exists: upload the eight assets only |
| Run workflow (`workflow_dispatch`) on a tag | same as a tag push | same as a tag push |
| Run workflow on a branch | Build test packages (artifacts only, no release) | none; 7-day artifacts only |

Nothing else triggers it: no branch pushes, pull requests or release events. `release: published`
was removed (task 0008): creating a release in the UI with a new tag also pushes that tag, so both
triggers would have started two runs. Tags must match `v[0-9]+.[0-9]+.[0-9]+` to start a run at
all. The workflow file of the TAGGED commit is the one that runs, so the tagged commit must already
contain this version of the workflow.

Owner steps:
1. Release: set `version` in Cargo.toml to X.Y.Z (> the last release, >= 0.1.1), commit, then
   `git push`, then `git tag vX.Y.Z` and `git push origin vX.Y.Z`. `git push` alone does not send
   local tags; the tag push is what starts the release run.
2. Or create the release in the GitHub UI with a NEW tag `vX.Y.Z` targeting the bumped, pushed
   commit (your own title and notes). Publishing creates the tag, the tag push runs the workflow,
   and the workflow only uploads the eight assets to your release (immutable releases must be
   OFF for this variant, see below). A draft does not create the tag; publishing it does.
3. Test build: Actions > "Build and release packages" > Run workflow on a branch: artifacts only.
   Run workflow on a tag takes the release path (e.g. to retry after fixing a cause outside the
   commit). Inspect all four jobs, artifacts and checksums; do the native Windows GUI smoke
   separately.

Release notes and existing releases: a release created by the workflow gets the title
`RocksVision vX.Y.Z` and a minimal body (version, commit, run link; no `--generate-notes`, so the
text does not depend on earlier tags or PRs), edit it on GitHub afterwards if you like. An existing
release (created by you or by an earlier run) is never edited: title, notes, draft and latest
flags stay as they are; only missing assets are added, and an existing asset name stops the run.
A release created by the workflow becomes "Latest" (GitHub default).

Gating (every job: actor and triggering actor `ta2ldv`; tag deletions are skipped):

| Check (script) | Outcome |
|---|---|
| tag strict `vMAJOR.MINOR.PATCH`, equal to Cargo.toml at the tagged commit (`validate-tag.sh`) | exit 0 continue; 1 (not strict, e.g. `v0.1.02`, or != Cargo.toml) validate FAILS; 10 (older than 0.1.1, e.g. `v0.1.0`) NEUTRAL SKIP: validate passes with a notice, nothing else runs |
| tag on GitHub still points to the built commit (`check-tag-commit.sh`; lightweight or annotated: `git/ref/tags/<tag>`, an annotated tag object is peeled once via `git/tags/<sha>`) | 0 continue; 1 moved, missing (404) or not pointing to a commit; 3 API/network error (fail-closed) |
| release state (`check-release-ready.sh`) | 0 published release, `"immutable": false`, no same-name asset: upload-only; 10 no release: create; 1 draft, immutable true/unverifiable, or an asset name exists; 3 API/network error (fail-closed) |

`contents: write` exists only in the release job, which runs only in `ta2ldv/rocksvision` and only
when validate and all four platform jobs succeeded. The checks run in validate (before building)
and again in the release job (`publish-release.sh`, write token, right before the write); the
path is decided again there because a release may have been created meanwhile. The create path
uses `gh release create --verify-tag` (never creates a tag); afterwards all eight assets must be
attached (`check-release-ready.sh --verify-uploaded`). Runs for the same tag (tag push plus
manual run, or a re-run) share the concurrency group `release-refs/tags/vX.Y.Z` and run one after
another (`cancel-in-progress: false`; GitHub keeps one pending run per group); the later one stops
at "asset already exists". A UI-created release is public without assets until the run finishes.
Immutable releases: a published immutable release cannot get assets later, so the upload-only
path refuses it (REST `immutable` field confirmed on v0.1.1, 2026-10-27). The create path passes
the assets to `gh release create`, which uploads them before publishing (gh behaviour, to verify
on the first real run); the post-write check catches a release left without assets.

Local check without GitHub access: `bash scripts/ci/selftest-release-ready.sh` runs all gate
scripts and `publish-release.sh` against a fake `curl`/`gh` in `/private/tmp/rocksvision-ci-fake/`
(the fake gh records every call).

### Recovery after a partial upload

If the release step fails part way (network, API error), some of the eight assets may already be
attached, or a created release may lack assets. Re-running the workflow does not overwrite them:
the readiness check finds the existing names (or the draft) and stops before building or
uploading anything. The owner decides what to do:
1. Compare each attached asset with the failed run's artifacts (download the
   `rocksvision-<platform>` artifacts; the SHA-256 of each attached archive must equal its
   `.sha256` file and the artifact's checksum).
2. Delete (or rename) the attached assets by hand on GitHub, or keep them if they match and
   attach the missing ones by hand from the same run's artifacts.
3. Re-run the failed run (or Run workflow on the tag) only once none of the eight names is
   attached, so all eight come from one run. A leftover draft for the tag must be published or
   deleted first (the workflow neither publishes nor edits drafts).

The workflow never deletes or replaces assets and `--clobber` is never used; do not add it to
make retries convenient.

## What was verified where (2026-10-27)

| Platform | CI (owner run 40328142624, commit 81a6d1c, v0.1.1 source) | Run by people |
|---|---|---|
| macOS arm64 | build/package, dist checks, tests, CLI smoke, artifact upload: passed | tests, headless dumps and the packaged GUI on the developer Mac; user ran v0.1.1 |
| macOS x64 | same steps: passed | user ran v0.1.1 |
| Ubuntu x64 | same steps: passed | GUI launch not tested |
| Windows x64 | same steps: passed | user ran v0.1.1 (console/panic-hook behaviour not individually tested) |
| GitHub Actions | tag-push release flow (2026-10-27): gates and `publish-release.sh` exercised locally with fake curl/gh (`selftest-release-ready.sh`, 32 cases), not yet run on GitHub | release upload path not yet run by the new workflow |

Version 0.1.2 (Cargo.toml; About, BUILD-INFO and archive names derive from it; v0.1.0 and v0.1.1
stay as published). Its changes (edition 2024, release automation, status/navigation fixes) are
verified locally on macOS arm64 and await the owner's four-platform CI run. Windows uses the GUI
subsystem (no console window from Explorer) and attaches to the parent console so
`--dump`/`--help`/errors print in a terminal (the prompt may appear before the output). Linux has no .desktop file or icon theme entry. Windows has no embedded .ico (the
window icon is set at run time). No installer, DMG, notarization or code signing beyond the
macOS ad-hoc signature.
