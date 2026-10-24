# One command: builds dist\windows-x64\rocksvision.exe on x64 Windows (MSVC), plus
# BUILD-INFO.txt, LICENSE, THIRD-PARTY-LICENSES.md, SHA256SUMS and
# dist\rocksvision-<version>-windows-x64.zip (+ .sha256). See docs/BUILD.md.
#
# Usage (Windows PowerShell 5.1 or PowerShell 7, from any directory; paths with spaces are fine):
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\build-windows-x64.ps1
# (-ExecutionPolicy Bypass applies to this one process only; no system setting is changed.)
# Tested by the user on Windows 10/11 desktop (v0.1.0). Windows Server (NT 10.0, e.g. the GitHub
# windows-2022 runner) needs the same prerequisites; see docs/BUILD.md for what was verified.
#
# Prerequisites it installs when missing:
# - Visual Studio Build Tools 2022, workload "C++ build tools" (MSVC + Windows SDK) from the pinned
#   fixed-version bootstrapper below. The installer asks for administrator rights (UAC prompt).
#   If it reports that a reboot is required, the script stops: reboot yourself, then re-run.
#   The script never reboots and never disables security settings. An existing MSVC x64 toolset
#   (any Visual Studio 2022+ edition, e.g. on CI runners) is detected and the installer skipped.
# - rustup from the pinned rustup-init.exe (SHA-256 checked), toolchain from rust-toolchain.toml.
#   rustup is always invoked by its resolved full path (CARGO_HOME\bin\rustup.exe), and the
#   script prepends CARGO_HOME\bin to its own process PATH only: no new shell, no manual PATH
#   edit and no reinstall is needed after a fresh install or a rustup self-update.
# Re-running reuses installed tools and the cargo cache.

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 3.0

# --- pins ---------------------------------------------------------------------------------
$RustupVersion = '1.28.2'
# https://static.rust-lang.org/rustup/archive/1.28.2/x86_64-pc-windows-msvc/rustup-init.exe.sha256 (fetched 2026-10-22)
$RustupSha256 = '88d8258dcf6ae4f7a80c7d1088e1f36fa7025a1cfd1343731b4ee6f385121fc0'
# Visual Studio 2022 Build Tools 17.14.42 (Current channel, 2026-10-13) fixed-version bootstrapper
# from https://learn.microsoft.com/en-us/visualstudio/releases/2022/release-history ; the file was
# downloaded and hashed 2026-10-22 (Microsoft also encodes the SHA-256 in the URL).
$VsBuildToolsUrl = 'https://download.visualstudio.microsoft.com/download/pr/c95450be-0a45-4779-b142-33f567ccfd6e/68978bb28916156802e60e04c5ee50b94348d802fe151a3ca24433f27984187b/vs_BuildTools.exe'
$VsBuildToolsSha256 = '68978bb28916156802e60e04c5ee50b94348d802fe151a3ca24433f27984187b'
$Triple = 'x86_64-pc-windows-msvc'
$Platform = 'windows-x64'

function Fail([string]$msg) {
    Write-Host "error: $msg" -ForegroundColor Red
    exit 1
}
function Log([string]$msg) { Write-Host "==> $msg" }
# Runs a native command whose stderr may legitimately be non-empty (Windows PowerShell 5.1 turns
# redirected native stderr into errors, which 'Stop' would make fatal). Returns its stdout.
function Invoke-Native([scriptblock]$cmd) {
    $old = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try { & $cmd 2>$null } finally { $ErrorActionPreference = $old }
}
# Runs <exe> <args> and returns exit code plus combined stdout/stderr text (for checks whose
# output is shown only on failure). A missing/unstartable exe yields Code = -1.
function Invoke-Captured([string]$exe, [string[]]$argList) {
    $old = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    $global:LASTEXITCODE = 0
    try {
        $out = & $exe @argList 2>&1 | ForEach-Object { "$_" }
        $code = $LASTEXITCODE
    } catch {
        $out = @("$_")
        $code = -1
    } finally {
        $ErrorActionPreference = $old
    }
    return [pscustomobject]@{ Code = $code; Text = (@($out) -join "`n").Trim() }
}

$Start = (Get-Date).ToUniversalTime()
Start-Sleep -Seconds 1
$Root = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
# Every required input is checked BEFORE any install or build; missing ones are listed together.
$missing = @('Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', 'LICENSE', 'THIRD-PARTY-LICENSES.md', 'src\main.rs') | Where-Object { -not (Test-Path -LiteralPath (Join-Path $Root $_) -PathType Leaf) }
if ($missing) { Fail "required input files missing in $Root (nothing was built): $($missing -join ', ')" }
$cargoToml = Get-Content -LiteralPath (Join-Path $Root 'Cargo.toml') -Raw
$Exe = ([regex]::Match($cargoToml, '(?m)^name\s*=\s*"([^"]+)"')).Groups[1].Value
$Version = ([regex]::Match($cargoToml, '(?m)^version\s*=\s*"([^"]+)"')).Groups[1].Value
$RustPin = ([regex]::Match((Get-Content -LiteralPath (Join-Path $Root 'rust-toolchain.toml') -Raw), '(?m)^channel\s*=\s*"([^"]+)"')).Groups[1].Value
if (-not $Exe -or -not $Version -or -not $RustPin) { Fail 'name/version (Cargo.toml) or channel (rust-toolchain.toml) not found' }

# --- host check ---------------------------------------------------------------------------
$arch = $env:PROCESSOR_ARCHITECTURE
if ($env:PROCESSOR_ARCHITEW6432) { $arch = $env:PROCESSOR_ARCHITEW6432 }
if ($arch -ne 'AMD64') { Fail "this script builds on x64 (AMD64) Windows; this host is $arch" }
# Windows 10/11 and Windows Server 2016/2019/2022/2025 all report NT 10.0.
if ([Environment]::OSVersion.Version.Major -lt 10) { Fail 'Windows 10/11 or Windows Server 2016 or newer (NT 10.0) is required' }

$Tmp = Join-Path ([IO.Path]::GetTempPath()) 'rocksvision-bootstrap'
New-Item -ItemType Directory -Force -Path $Tmp | Out-Null

function Get-PinnedFile([string]$url, [string]$sha, [string]$name) {
    $dest = Join-Path $Tmp $name
    if ((Test-Path -LiteralPath $dest) -and ((Get-FileHash -LiteralPath $dest -Algorithm SHA256).Hash.ToLower() -eq $sha)) { return $dest }
    Log "downloading $name"
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    $ProgressPreference = 'SilentlyContinue'
    Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $dest
    $got = (Get-FileHash -LiteralPath $dest -Algorithm SHA256).Hash.ToLower()
    if ($got -ne $sha) {
        Remove-Item -LiteralPath $dest -Force
        Fail "$name SHA-256 mismatch (got $got, pinned $sha); not running it"
    }
    return $dest
}

# --- MSVC (C compiler for the vendored zstd/bzip2 sources, linker) ------------------------
function Find-Msvc {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path -LiteralPath $vswhere)) { return $null }
    $path = & $vswhere -latest -products * -version '[17.0,)' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if ($LASTEXITCODE -ne 0 -or -not $path) { return $null }
    return $path
}

$vs = Find-Msvc
if (-not $vs) {
    Log 'Visual Studio Build Tools 2022 (C++ build tools) not found; installing (a UAC prompt asks for administrator rights)'
    $installer = Get-PinnedFile $VsBuildToolsUrl $VsBuildToolsSha256 'vs_BuildTools.exe'
    $p = Start-Process -FilePath $installer -ArgumentList @('--add', 'Microsoft.VisualStudio.Workload.VCTools', '--includeRecommended', '--passive', '--norestart', '--wait') -Wait -PassThru
    switch ($p.ExitCode) {
        0 { }
        3010 { Fail 'Build Tools installed, but Windows requires a reboot. Reboot (the script never does), then re-run this script.' }
        1641 { Fail 'Build Tools installed and a reboot was started. Re-run this script after the reboot.' }
        1602 { Fail 'Build Tools installation was cancelled (UAC prompt or installer window). Re-run and accept the prompt, or install "Desktop development with C++" / "C++ build tools" yourself.' }
        5007 { Fail 'Build Tools installation needs administrator rights (UAC was declined or unavailable). Re-run and accept the prompt, or run from an elevated PowerShell.' }
        default { Fail "Build Tools installer exited with code $($p.ExitCode). See %TEMP%\dd_*.log." }
    }
    $vs = Find-Msvc
    if (-not $vs) { Fail 'Build Tools installer finished but no MSVC x64 toolset was found (vswhere). A reboot may be pending.' }
}
Log "MSVC: $vs"

# --- Rust: resolve rustup by full path ----------------------------------------------------
# rustup-init installs into CARGO_HOME (default %USERPROFILE%\.cargo). Every rustup call below
# uses the resolved $RustupExe; nothing depends on a bare 'rustup' being found on PATH.
$CargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
$CargoBin = Join-Path $CargoHome 'bin'
$RustupExe = Join-Path $CargoBin 'rustup.exe'
if (-not (Test-Path -LiteralPath $RustupExe -PathType Leaf)) {
    # A rustup outside CARGO_HOME (e.g. a package manager) is used by its full path as well.
    $onPath = Get-Command rustup.exe -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($onPath) {
        $RustupExe = $onPath.Source
        $CargoBin = Split-Path -Parent $RustupExe
    } elseif (Get-Command cargo.exe -CommandType Application -ErrorAction SilentlyContinue) {
        Fail "[toolchain] cargo is on PATH but rustup is not (looked in $CargoBin and PATH); this script needs rustup to select toolchain $RustPin. Install rustup (https://rustup.rs) or remove the other Rust from PATH, then re-run. Nothing was built."
    } else {
        $init = Get-PinnedFile "https://static.rust-lang.org/rustup/archive/$RustupVersion/$Triple/rustup-init.exe" $RustupSha256 'rustup-init.exe'
        Log "installing rustup $RustupVersion into $CargoHome (no default toolchain, PATH not modified)"
        & $init -y --no-modify-path --default-toolchain none --profile minimal --default-host $Triple
        if ($LASTEXITCODE -ne 0) { Fail "[toolchain] rustup-init failed (exit $LASTEXITCODE). Nothing was built." }
        if (-not (Test-Path -LiteralPath $RustupExe -PathType Leaf)) { Fail "[toolchain] rustup-init reported success but $RustupExe does not exist (CARGO_HOME=$CargoHome). Nothing was built." }
    }
}
# This process (and the cargo/rustc/build scripts it starts) finds the proxies first.
$pathParts = @($env:Path -split ';' | Where-Object { $_ -and ($_.TrimEnd('\') -ne $CargoBin.TrimEnd('\')) })
$env:Path = (@($CargoBin) + $pathParts) -join ';'
Log "rustup: $RustupExe"

# Pinned cargo through the resolved rustup: '<rustup.exe> run <pin> cargo ...'. The toolchain is
# named explicitly, so RUSTUP_TOOLCHAIN or a rust-toolchain file elsewhere cannot change it.
function Test-PinnedCargo {
    $r = Invoke-Captured $RustupExe @('run', $RustPin, 'cargo', '--version')
    $ok = ($r.Code -eq 0) -and ($r.Text -match ('(?m)^cargo ' + [regex]::Escape($RustPin) + '\b'))
    return [pscustomobject]@{ Ok = $ok; Code = $r.Code; Text = $r.Text }
}
$check = Test-PinnedCargo
if (-not $check.Ok) {
    Log "toolchain $RustPin (installed once, reused afterwards)"
    # --no-self-update: rustup must not replace itself in the middle of this script.
    & $RustupExe toolchain install $RustPin --profile minimal --no-self-update
    if ($LASTEXITCODE -ne 0) { Fail "[toolchain] '$RustupExe toolchain install $RustPin' failed (exit $LASTEXITCODE). cargo was not run; Cargo.lock is not involved." }
    $check = Test-PinnedCargo
    if (-not $check.Ok) {
        # A rustup self-update replaces rustup.exe/proxies on Windows; give it a moment once.
        Start-Sleep -Seconds 3
        $check = Test-PinnedCargo
    }
}
if (-not $check.Ok) {
    Fail "[toolchain] the pinned cargo did not start: '$RustupExe run $RustPin cargo --version' exited $($check.Code) with: $($check.Text). cargo build was not run; Cargo.lock is not involved."
}
$CargoV = $check.Text
$rv = Invoke-Captured $RustupExe @('run', $RustPin, 'rustc', '-vV')
if ($rv.Code -ne 0) { Fail "[toolchain] '$RustupExe run $RustPin rustc -vV' failed: $($rv.Text). Nothing was built." }
$RustcV = ($rv.Text -split "`n")[0].Trim()
$RustcHost = ([regex]::Match($rv.Text, '(?m)^host:\s*(\S+)')).Groups[1].Value
if ($RustcHost -ne $Triple) { Fail "[toolchain] rustc $RustPin host is '$RustcHost', expected $Triple (MSVC). Install the MSVC toolchain ('$RustupExe toolchain install $RustPin-$Triple') or fix the rustup default host. Nothing was built." }
Log "toolchain ok: $CargoV; rustc host $RustcHost"
if ($env:CARGO_BUILD_TARGET -and $env:CARGO_BUILD_TARGET -ne $Triple) {
    Log "note: CARGO_BUILD_TARGET=$($env:CARGO_BUILD_TARGET) is overridden by the explicit --target $Triple"
}

# --- build --------------------------------------------------------------------------------
# --target is explicit, so a configured build.target / CARGO_BUILD_TARGET cannot produce a
# foreign binary under the windows-x64 label. The binary path is the "executable" cargo reports
# (honours CARGO_TARGET_DIR and build.target-dir).
Log "cargo build --release --locked --target $Triple"
Push-Location -LiteralPath $Root
try {
    $json = & $RustupExe run $RustPin cargo build --release --locked --target $Triple --message-format=json-render-diagnostics
    $rc = $LASTEXITCODE
    $lockState = $null
    if ($rc -ne 0) {
        # Phase split: was it the --locked lockfile check or the compilation? Ask cargo (offline,
        # nothing is written) whether Cargo.lock satisfies Cargo.toml.
        $meta = Invoke-Captured $RustupExe @('run', $RustPin, 'cargo', 'metadata', '--locked', '--offline', '--format-version', '1')
        if ($meta.Text -match 'lock file .*needs to be updated|--locked was passed') { $lockState = 'stale' }
    }
} finally {
    Pop-Location
}
if ($rc -ne 0) {
    if ($lockState -eq 'stale') { Fail "[lockfile] Cargo.lock does not match Cargo.toml and this build uses --locked (it never rewrites Cargo.lock). Update Cargo.lock in the source tree (cargo update -p <changed crate>), then re-run." }
    Fail "[cargo build] cargo build --release --locked failed (exit $rc, see the compiler output above). Cargo.lock matched Cargo.toml (or could not be checked offline); the toolchain check before the build passed."
}
$Bin = $null
$cargoFresh = $false
foreach ($line in $json) {
    if ($line -notmatch '"reason":"compiler-artifact"') { continue }
    $m = $line | ConvertFrom-Json
    if ($m.executable -and ($m.target.kind -contains 'bin') -and ([IO.Path]::GetFileName($m.executable) -eq "$Exe.exe")) {
        $Bin = $m.executable
        $cargoFresh = [bool]$m.fresh
    }
}
if (-not $Bin) { Fail "cargo did not report the $Exe.exe executable (no bin compiler-artifact message)" }
if (-not (Test-Path -LiteralPath $Bin)) { Fail "cargo succeeded but $Bin does not exist" }
if ($Bin -notmatch ('[\\/]' + [regex]::Escape($Triple) + '[\\/]release[\\/]')) { Fail "cargo reported $Bin, which is not a $Triple release build" }
# PE header check: machine must be AMD64 (0x8664), whatever the path says.
$fs = [IO.File]::OpenRead($Bin)
try {
    $hdr = New-Object byte[] 4096
    $n = $fs.Read($hdr, 0, $hdr.Length)
} finally { $fs.Close() }
$peOff = if ($n -ge 64) { [BitConverter]::ToInt32($hdr, 0x3C) } else { -1 }
$okPe = ($n -ge 64) -and ($hdr[0] -eq 0x4D) -and ($hdr[1] -eq 0x5A) -and ($peOff -gt 0) -and ($peOff + 6 -le $n) -and
    ($hdr[$peOff] -eq 0x50) -and ($hdr[$peOff + 1] -eq 0x45) -and ($hdr[$peOff + 2] -eq 0) -and ($hdr[$peOff + 3] -eq 0) -and
    ([BitConverter]::ToUInt16($hdr, $peOff + 4) -eq 0x8664)
if (-not $okPe) { Fail "$Bin is not an x64 (AMD64) PE executable: refusing to label it $Platform" }
$fresh = $null
if ((Get-Item -LiteralPath $Bin).LastWriteTimeUtc -gt $Start) {
    $fresh = 'built in this run'
} elseif ($cargoFresh) {
    $fresh = 'unchanged since the previous build (cargo verified it is up to date with the sources and Cargo.lock; cache reused)'
}
if (-not $fresh) { Fail "$Bin is older than this run and cargo did not report it up to date: refusing to package stale output" }
Log "binary: $Bin ($fresh)"

# --- runtime DLL imports (evidence, not assumption) ---------------------------------------
# No +crt-static is configured, so Rust's MSVC default links the C runtime dynamically:
# expected imports include VCRUNTIME140.dll (Visual C++ 2015-2022 Redistributable x64) and
# api-ms-win-crt-*.dll (Universal CRT, part of Windows 10/11 and Server 2016+). The actual list
# is read with dumpbin from the detected MSVC toolset and recorded in BUILD-INFO.txt.
$Imports = 'not determined (dumpbin not found in the MSVC toolset)'
$dumpbin = Get-ChildItem -Path (Join-Path $vs 'VC\Tools\MSVC\*\bin\Hostx64\x64\dumpbin.exe') -ErrorAction SilentlyContinue | Sort-Object FullName | Select-Object -Last 1
if ($dumpbin) {
    $d = Invoke-Captured $dumpbin.FullName @('/nologo', '/dependents', $Bin)
    $dlls = @(($d.Text -split "`n") | ForEach-Object { $_.Trim() } | Where-Object { $_ -match '^\S+\.dll$' })
    if ($d.Code -eq 0 -and $dlls.Count -gt 0) {
        $Imports = ($dlls -join ', ')
        Log "DLL imports: $Imports"
    } else {
        $Imports = "not determined (dumpbin exit $($d.Code))"
    }
}
$vcrt = if ($Imports -match '(?i)vcruntime140') { 'needs the Microsoft Visual C++ 2015-2022 Redistributable (x64) for VCRUNTIME140.dll' } elseif ($Imports -like 'not determined*') { 'pending verification (run: dumpbin /dependents rocksvision.exe)' } else { 'no VCRUNTIME import found' }

# --- identity -----------------------------------------------------------------------------
# Same definition as scripts/lib/common.sh rv_source_id: lines "<sha256>  <path>" over sorted
# src/** (bytewise, '/' separators) then Cargo.toml, Cargo.lock; id = SHA-256 of the lines.
# A CRLF checkout (git autocrlf) yields a different id than an LF checkout.
$rel = Get-ChildItem -LiteralPath (Join-Path $Root 'src') -Recurse -File | ForEach-Object { 'src/' + $_.FullName.Substring((Join-Path $Root 'src').Length + 1).Replace('\', '/') }
$sorted = [string[]]$rel
[Array]::Sort($sorted, [StringComparer]::Ordinal)
$manifest = New-Object Text.StringBuilder
foreach ($f in ($sorted + @('Cargo.toml', 'Cargo.lock'))) {
    $h = (Get-FileHash -LiteralPath (Join-Path $Root $f) -Algorithm SHA256).Hash.ToLower()
    [void]$manifest.Append("$h  $f`n")
}
$sha = [Security.Cryptography.SHA256]::Create()
$SourceId = (($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($manifest.ToString())) | ForEach-Object { $_.ToString('x2') }) -join '')
$vcs = 'none (no git repository; identity = source id)'
if ((Get-Command git -ErrorAction SilentlyContinue)) {
    $null = Invoke-Native { git -C $Root rev-parse --git-dir }
    if ($LASTEXITCODE -eq 0) {
        $rev = Invoke-Native { git -C $Root rev-parse HEAD }
        $dirty = if (Invoke-Native { git -C $Root status --porcelain }) { 'dirty' } else { 'clean' }
        $vcs = "git $rev ($dirty)"
    }
}

# --- dist ---------------------------------------------------------------------------------
# Assembled in dist\.staging-windows-x64\ and moved into place only when everything succeeded;
# on any error the staging dir is removed and the previous dist\windows-x64 stays untouched.
$FinalDist = Join-Path $Root "dist\$Platform"
$ZipName = "$Exe-$Version-$Platform.zip"
$Stage = Join-Path $Root "dist\.staging-$Platform"
if (Test-Path -LiteralPath $Stage) { Remove-Item -LiteralPath $Stage -Recurse -Force }
$Dist = Join-Path $Stage $Platform
$Zip = Join-Path $Stage $ZipName
try {
New-Item -ItemType Directory -Force -Path $Dist | Out-Null
Copy-Item -LiteralPath $Bin -Destination (Join-Path $Dist "$Exe.exe")
Copy-Item -LiteralPath (Join-Path $Root 'LICENSE') -Destination (Join-Path $Dist 'LICENSE')
Copy-Item -LiteralPath (Join-Path $Root 'THIRD-PARTY-LICENSES.md') -Destination (Join-Path $Dist 'THIRD-PARTY-LICENSES.md')
$utf8 = New-Object Text.UTF8Encoding $false
$info = @(
    "name: $Exe",
    "version: $Version",
    "platform: $Platform",
    "target: $Triple",
    "rustc: $RustcV",
    "cargo: $CargoV",
    "built (UTC): $((Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ'))",
    "build host: Windows $([Environment]::OSVersion.Version) $arch",
    "msvc: $vs",
    "source id (sha256 of src/**, Cargo.toml, Cargo.lock): $SourceId",
    "vcs: $vcs",
    "build: cargo build --release --locked --target $Triple; $fresh",
    "dll imports (dumpbin /dependents): $Imports",
    "runtime: MSVC C runtime linked dynamically (Rust default, no +crt-static); $vcrt; UCRT is part of Windows 10/11 and Server 2016+"
) -join "`n"
[IO.File]::WriteAllText((Join-Path $Dist 'BUILD-INFO.txt'), "$info`n", $utf8)
$names = [string[]](Get-ChildItem -LiteralPath $Dist -File | ForEach-Object { $_.Name })
[Array]::Sort($names, [StringComparer]::Ordinal)
$sums = $names | ForEach-Object { "$((Get-FileHash -LiteralPath (Join-Path $Dist $_) -Algorithm SHA256).Hash.ToLower())  $_" }
[IO.File]::WriteAllText((Join-Path $Dist 'SHA256SUMS'), (($sums -join "`n") + "`n"), $utf8)
Compress-Archive -LiteralPath $Dist -DestinationPath $Zip
$zipSha = (Get-FileHash -LiteralPath $Zip -Algorithm SHA256).Hash.ToLower()
[IO.File]::WriteAllText("$Zip.sha256", "$zipSha  $ZipName`n", $utf8)
# Publish: replace only this platform's directory and archive files.
$FinalZip = Join-Path $Root "dist\$ZipName"
foreach ($p in $FinalDist, $FinalZip, "$FinalZip.sha256") {
    if (Test-Path -LiteralPath $p) { Remove-Item -LiteralPath $p -Recurse -Force }
}
Move-Item -LiteralPath $Dist -Destination $FinalDist
Move-Item -LiteralPath $Zip -Destination $FinalZip
Move-Item -LiteralPath "$Zip.sha256" -Destination "$FinalZip.sha256"
} finally {
    if (Test-Path -LiteralPath $Stage) { Remove-Item -LiteralPath $Stage -Recurse -Force }
}
Write-Host "Built: $(Join-Path $FinalDist "$Exe.exe")"
Write-Host "Archive: $FinalZip"
Write-Host "SHA-256: $zipSha"
