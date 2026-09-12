# herdr [[build]] step (Windows). Builds both binaries from source with cargo, then
# adds the release directory to the user's PATH so `hsm` and `agentmail` resolve by
# name in new terminals. Symlinks need privileges on Windows, so the directory itself
# goes on PATH instead.
$ErrorActionPreference = 'Stop'
$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$cargo = Get-Command cargo -ErrorAction SilentlyContinue
if (-not $cargo) {
    [Console]::Error.WriteLine("herdr-session-manager needs Rust 1.85+ to build, but cargo was not found. Install Rust from https://rustup.rs and re-run the install.")
    exit 1
}
Set-Location $RepoRoot
& cargo build --release --bin hsm --bin agentmail
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$binDir = Join-Path $RepoRoot 'target\release'
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not (($userPath -split ';') -contains $binDir)) {
    [Environment]::SetEnvironmentVariable('Path', "$binDir;$userPath", 'User')
    [Console]::Error.WriteLine("herdr-session-manager: added $binDir to the user PATH. Open a new terminal to use hsm and agentmail by name.")
}
exit 0
