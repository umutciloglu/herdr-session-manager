# herdr [[build]] step (Windows).
#
# Fast path: download the prebuilt hsm.exe and agentmail.exe for this platform from the
# GitHub release matching the version this source declares, verify both against the
# release's SHA256SUMS, and put them in target\release.
# Fallback: on ANY miss - no release for this version, no network, a checksum mismatch,
# an unmapped platform - say why and build from source with cargo, exactly as before.
#
# Overridable for testing: HSM_REPO, HSM_BASE_URL, HSM_CARGO_TOML, HSM_OUT_DIR.
$ErrorActionPreference = 'Stop'
# Windows PowerShell 5.1 renders a progress bar per downloaded chunk, which makes a
# download many times slower, and may still default to TLS versions GitHub refuses.
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path

$repo = if ($env:HSM_REPO) { $env:HSM_REPO } else { 'umutciloglu/herdr-session-manager' }
$cargoToml = if ($env:HSM_CARGO_TOML) { $env:HSM_CARGO_TOML } else { Join-Path $RepoRoot 'Cargo.toml' }
$outDir = if ($env:HSM_OUT_DIR) { $env:HSM_OUT_DIR } else { Join-Path $RepoRoot 'target\release' }
$baseUrl = if ($env:HSM_BASE_URL) { $env:HSM_BASE_URL } else { "https://github.com/$repo/releases/download" }

function Build-FromSource {
    # herdr may run without ~/.cargo/bin on PATH (started before rustup finished).
    $cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
    if ((Test-Path $cargoBin) -and -not (($env:Path -split ';') -contains $cargoBin)) {
        $env:Path = "$cargoBin;$env:Path"
    }
    $cargo = Get-Command cargo -ErrorAction SilentlyContinue
    if (-not $cargo) {
        [Console]::Error.WriteLine("herdr-session-manager needs Rust 1.85+ to build, but cargo was not found. Install Rust from https://rustup.rs and re-run the install.")
        exit 1
    }
    Set-Location $RepoRoot
    & cargo build --release --bin hsm --bin agentmail
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}

# True only when both verified binaries are in place.
function Get-Prebuilt {
    # A 32-bit PowerShell on 64-bit Windows reports x86 here and the real machine in
    # PROCESSOR_ARCHITEW6432.
    $arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
    $triple = switch ($arch) {
        'AMD64' { 'x86_64-pc-windows-msvc' }
        default { $null }
    }
    if (-not $triple) {
        [Console]::Error.WriteLine("herdr-session-manager: no prebuilt binary for Windows/$arch - building from source instead.")
        return $false
    }

    # The version this source declares, not the newest release: a binary whose version
    # differs from this checkout is never installed silently.
    $match = Select-String -Path $cargoToml -Pattern '^version *= *"([^"]+)"' | Select-Object -First 1
    if (-not $match) {
        [Console]::Error.WriteLine("herdr-session-manager: could not read the version from $cargoToml - building from source instead.")
        return $false
    }
    $version = $match.Matches.Groups[1].Value

    $tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("hsm-" + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp -Force | Out-Null
    try {
        $sums = Join-Path $tmp 'SHA256SUMS'
        try {
            Invoke-WebRequest -Uri "$baseUrl/v$version/SHA256SUMS" -OutFile $sums -UseBasicParsing
        } catch {
            throw "no published checksums for v$version"
        }
        $lines = Get-Content $sums

        # Verify both before installing either: half an install is worse than none.
        foreach ($name in 'hsm', 'agentmail') {
            $asset = "$name-$triple.exe"
            $dest = Join-Path $tmp $asset
            try {
                Invoke-WebRequest -Uri "$baseUrl/v$version/$asset" -OutFile $dest -UseBasicParsing
            } catch {
                throw "no prebuilt $asset for v$version"
            }
            $pattern = "^([0-9a-f]{64}) [ *]" + [regex]::Escape($asset) + "$"
            $expected = $lines | Where-Object { $_ -match $pattern } | ForEach-Object { $Matches[1] } | Select-Object -First 1
            if (-not $expected) { throw "no checksum listed for $asset" }
            $actual = (Get-FileHash -Path $dest -Algorithm SHA256).Hash.ToLower()
            if ($actual -ne $expected) { throw "checksum mismatch for $asset" }
        }

        New-Item -ItemType Directory -Path $outDir -Force | Out-Null
        foreach ($name in 'hsm', 'agentmail') {
            Move-Item -Force -Path (Join-Path $tmp "$name-$triple.exe") -Destination (Join-Path $outDir "$name.exe")
        }
        [Console]::Error.WriteLine("herdr-session-manager: installed prebuilt v$version ($triple), verified SHA-256.")
        return $true
    } catch {
        [Console]::Error.WriteLine("herdr-session-manager: $($_.Exception.Message) - building from source instead.")
        return $false
    } finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }
}

if (-not (Get-Prebuilt)) { Build-FromSource }

# Opt-out for people who manage PATH themselves: the plugin still works, herdr runs
# the binaries by their manifest paths; only `hsm` and `agentmail` by name are lost.
if ($env:HSM_NO_PATH -eq '1') { exit 0 }

# --- put the binaries on PATH ---------------------------------------------------
# Not the release directory itself: herdr builds a GitHub install inside a temporary
# checkout and moves it into place afterwards, so $outDir is gone once this returns.
# Each launcher resolves the current plugin root when it runs: the herdr-provided root
# inside plugin invocations, else the newest GitHub install, else the directory this
# build ran in (the `plugin link` case).
#
# The launchers are .cmd files, so they are for typing `hsm` and `agentmail` in a
# terminal. Nothing that spawns a binary uses them: hsm and agentmail find each other
# next to themselves, and `agentmail setup` writes its real .exe path into hooks.
$binDir = Join-Path $env:USERPROFILE '.local\bin'
New-Item -ItemType Directory -Path $binDir -Force | Out-Null

# cmd.exe reads a batch file in the OEM code page, so a non-ASCII build path would be
# garbled; such a path is left out and the launcher relies on the other lookups.
$buildRootLine = ''
if ($RepoRoot -match '^[\x20-\x7e]+$') {
    $buildRootLine = "if not defined exe if exist `"$RepoRoot\target\release\%name%.exe`" set `"exe=$RepoRoot\target\release\%name%.exe`""
}

foreach ($name in 'hsm', 'agentmail') {
    # No parenthesised blocks around expanded paths: a `)` in a path such as
    # `Program Files (x86)` would end the block early.
    $launcher = @"
@echo off
rem herdr-session-manager launcher (generated by scripts/fetch-or-build.ps1)
setlocal
set "name=$name"
set "exe="
set "plugins="
if defined XDG_CONFIG_HOME set "plugins=%XDG_CONFIG_HOME%\herdr\plugins\github"
if not defined plugins set "plugins=%APPDATA%\herdr\plugins\github"
if defined HERDR_PLUGIN_ROOT if exist "%HERDR_PLUGIN_ROOT%\target\release\%name%.exe" set "exe=%HERDR_PLUGIN_ROOT%\target\release\%name%.exe"
if not defined exe for /f "delims=" %%d in ('dir /b /ad /o-d "%plugins%\herdr-session-manager-*" 2^>nul') do if not defined exe if exist "%plugins%\%%d\target\release\%name%.exe" set "exe=%plugins%\%%d\target\release\%name%.exe"
$buildRootLine
if not defined exe goto missing
"%exe%" %*
exit /b %errorlevel%
:missing
echo %name%: no herdr-session-manager build found; reinstall with: herdr plugin install umutciloglu/herdr-session-manager 1>&2
exit /b 127
"@
    [System.IO.File]::WriteAllText((Join-Path $binDir "$name.cmd"), ($launcher -replace "`r?`n", "`r`n"), [System.Text.Encoding]::ASCII)
}

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not (($userPath -split ';') -contains $binDir)) {
    $newPath = if ($userPath) { "$binDir;$userPath" } else { $binDir }
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    [Console]::Error.WriteLine("herdr-session-manager: added $binDir to the user PATH. Open a new terminal to use hsm and agentmail by name.")
}
[Console]::Error.WriteLine("herdr-session-manager: hsm and agentmail launchers are in $binDir")
exit 0
