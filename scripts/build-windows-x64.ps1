# One command: builds dist\windows-x64\rocksvision.exe on Windows 10/11 x64 (MSVC), plus
# BUILD-INFO.txt, THIRD-PARTY-LICENSES.md, SHA256SUMS and
# dist\rocksvision-<version>-windows-x64.zip (+ .sha256). See docs/BUILD.md.
#
# Usage (Windows PowerShell 5.1 or PowerShell 7, from any directory; paths with spaces are fine):
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\build-windows-x64.ps1
# (-ExecutionPolicy Bypass applies to this one process only; no system setting is changed.)
#
# Prerequisites it installs when missing:
# - Visual Studio Build Tools 2022, workload "C++ build tools" (MSVC + Windows SDK) from the pinned
#   fixed-version bootstrapper below. The installer asks for administrator rights (UAC prompt).
#   If it reports that a reboot is required, the script stops: reboot yourself, then re-run.
#   The script never reboots and never disables security settings.
# - rustup from the pinned rustup-init.exe (SHA-256 checked), toolchain from rust-toolchain.toml.
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

$Start = (Get-Date).ToUniversalTime()
Start-Sleep -Seconds 1
$Root = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
# Every required input is checked BEFORE any install or build; missing ones are listed together.
$missing = @('Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', 'THIRD-PARTY-LICENSES.md', 'src\main.rs') | Where-Object { -not (Test-Path -LiteralPath (Join-Path $Root $_) -PathType Leaf) }
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
if ([Environment]::OSVersion.Version.Major -lt 10) { Fail 'Windows 10 or 11 is required' }

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

# --- Rust ---------------------------------------------------------------------------------
$cargoBin = if ($env:CARGO_HOME) { Join-Path $env:CARGO_HOME 'bin' } else { Join-Path $env:USERPROFILE '.cargo\bin' }
if (-not (Get-Command rustup -ErrorAction SilentlyContinue) -and (Test-Path -LiteralPath (Join-Path $cargoBin 'rustup.exe'))) {
    $env:Path = "$cargoBin;$env:Path"
}
if (-not (Get-Command rustup -ErrorAction SilentlyContinue)) {
    if (Get-Command cargo -ErrorAction SilentlyContinue) {
        Fail "cargo is on PATH but rustup is not; this script needs rustup to select toolchain $RustPin. Install rustup (https://rustup.rs) or remove the other Rust from PATH, then re-run."
    }
    $init = Get-PinnedFile "https://static.rust-lang.org/rustup/archive/$RustupVersion/$Triple/rustup-init.exe" $RustupSha256 'rustup-init.exe'
    Log "installing rustup $RustupVersion (no default toolchain, PATH not modified)"
    & $init -y --no-modify-path --default-toolchain none --profile minimal --default-host $Triple
    if ($LASTEXITCODE -ne 0) { Fail 'rustup-init failed' }
    $env:Path = "$cargoBin;$env:Path"
    if (-not (Get-Command rustup -ErrorAction SilentlyContinue)) { Fail "rustup installed but not found in $cargoBin" }
}
Log "toolchain $RustPin (installed once, reused afterwards)"
& rustup toolchain install $RustPin --profile minimal
if ($LASTEXITCODE -ne 0) { Fail "rustup could not install toolchain $RustPin" }

# --- build --------------------------------------------------------------------------------
# The binary path is the "executable" cargo reports for the bin artifact (honours
# CARGO_TARGET_DIR and build.target-dir).
Log 'cargo build --release --locked'
Push-Location -LiteralPath $Root
try {
    $json = & rustup run $RustPin cargo build --release --locked --message-format=json-render-diagnostics
    $rc = $LASTEXITCODE
    $RustcV = (& rustup run $RustPin rustc -V)
    $CargoV = (& rustup run $RustPin cargo -V)
} finally {
    Pop-Location
}
if ($rc -ne 0) { Fail 'cargo build failed (see above). If cargo says the lock file needs to be updated, Cargo.lock is stale for Cargo.toml: this build uses --locked and never rewrites it.' }
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
$fresh = $null
if ((Get-Item -LiteralPath $Bin).LastWriteTimeUtc -gt $Start) {
    $fresh = 'built in this run'
} elseif ($cargoFresh) {
    $fresh = 'unchanged since the previous build (cargo verified it is up to date with the sources and Cargo.lock; cache reused)'
}
if (-not $fresh) { Fail "$Bin is older than this run and cargo did not report it up to date: refusing to package stale output" }
Log "binary: $Bin ($fresh)"

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
    "build: cargo build --release --locked; $fresh",
    'runtime: no extra DLLs (Rust std links the MSVC runtime statically; UCRT is part of Windows 10/11)'
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
