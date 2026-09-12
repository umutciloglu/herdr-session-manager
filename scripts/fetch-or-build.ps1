# herdr [[build]] step (Windows).
#
# Fast path: download the prebuilt hsm.exe and agentmail.exe for this platform from the
# GitHub release matching the version this source declares, verify both against the
# release's SHA256SUMS, and put them in target\release.
# Fallback: on ANY miss — no release for this version, no network, a checksum mismatch —
# say why and build from source with cargo, exactly as before.
#
# Overridable for testing: HSM_REPO, HSM_BASE_URL, HSM_CARGO_TOML, HSM_OUT_DIR.
$ErrorActionPreference = 'Stop'
$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path

$repo = if ($env:HSM_REPO) { $env:HSM_REPO } else { 'umutciloglu/herdr-session-manager' }
$cargoToml = if ($env:HSM_CARGO_TOML) { $env:HSM_CARGO_TOML } else { Join-Path $RepoRoot 'Cargo.toml' }
$outDir = if ($env:HSM_OUT_DIR) { $env:HSM_OUT_DIR } else { Join-Path $RepoRoot 'target\release' }
$baseUrl = if ($env:HSM_BASE_URL) { $env:HSM_BASE_URL } else { "https://github.com/$repo/releases/download" }
$triple = 'x86_64-pc-windows-msvc'

function Build-FromSource {
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
        Invoke-WebRequest -Uri "$baseUrl/v$version/SHA256SUMS" -OutFile $sums -UseBasicParsing -ErrorAction Stop
        $lines = Get-Content $sums

        # Verify both before installing either: half an install is worse than none.
        foreach ($name in 'hsm', 'agentmail') {
            $asset = "$name-$triple.exe"
            $dest = Join-Path $tmp $asset
            Invoke-WebRequest -Uri "$baseUrl/v$version/$asset" -OutFile $dest -UseBasicParsing -ErrorAction Stop
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

# --- put the binaries on PATH ---------------------------------------------------
# Symlinks need privileges on Windows, so the release directory itself goes on PATH.
$binDir = $outDir
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not (($userPath -split ';') -contains $binDir)) {
    [Environment]::SetEnvironmentVariable('Path', "$binDir;$userPath", 'User')
    [Console]::Error.WriteLine("herdr-session-manager: added $binDir to the user PATH. Open a new terminal to use hsm and agentmail by name.")
}
exit 0
