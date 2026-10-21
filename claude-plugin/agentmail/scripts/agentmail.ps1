# agentmail launcher for the Claude Code plugin, Windows half (see agentmail.cmd).
#
# Makes sure the release binary for the plugin's version is in
# $env:CLAUDE_PLUGIN_DATA\bin\<version>\agentmail.exe, downloading it and verifying it
# against the release's SHA256SUMS the first time, and prints its path. It does not
# run the binary: agentmail.cmd does, because cmd.exe hands the MCP stdio pipes to its
# child untouched, while PowerShell would re-encode the server's output line by line.
#
# Same layout, lock and checks as the POSIX `agentmail` script beside it, which Git
# Bash runs for the hooks; either may be the one that downloads.
#
# Overridable: AGENTMAIL_BIN, AGENTMAIL_PLUGIN_VERSION, AGENTMAIL_REPO, AGENTMAIL_BASE_URL.
$ErrorActionPreference = 'Stop'
# Windows PowerShell 5.1 renders a progress bar per downloaded chunk, which makes a
# download many times slower, and may still default to TLS versions GitHub refuses.
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

function Fail([string]$message) {
    [Console]::Error.WriteLine("agentmail plugin: $message")
    exit 1
}

if ($env:AGENTMAIL_BIN) {
    Write-Output $env:AGENTMAIL_BIN
    exit 0
}

$repo = if ($env:AGENTMAIL_REPO) { $env:AGENTMAIL_REPO } else { 'umutciloglu/herdr-session-manager' }
$baseUrl = if ($env:AGENTMAIL_BASE_URL) { $env:AGENTMAIL_BASE_URL } else { "https://github.com/$repo/releases/download" }
$releases = "https://github.com/$repo/releases"

if (-not $env:CLAUDE_PLUGIN_DATA) {
    Fail 'CLAUDE_PLUGIN_DATA is not set; this launcher is meant to be run by Claude Code.'
}

$manifest = Join-Path $PSScriptRoot '..\.claude-plugin\plugin.json'
$version = $env:AGENTMAIL_PLUGIN_VERSION
if (-not $version) {
    try {
        $version = (Get-Content -Raw -Path $manifest | ConvertFrom-Json).version
    } catch {
        $version = $null
    }
}
if (-not $version) { Fail "could not read the plugin version from $manifest" }

# A 32-bit PowerShell on 64-bit Windows reports x86 here and the real machine in
# PROCESSOR_ARCHITEW6432.
$arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
$triple = switch ($arch) {
    'AMD64' { 'x86_64-pc-windows-msvc' }
    default { $null }
}
if (-not $triple) {
    Fail "no prebuilt agentmail for Windows/$arch. Releases: $releases. Build it with ``cargo install --git https://github.com/$repo agentmail`` and set AGENTMAIL_BIN to the result."
}

$dir = Join-Path $env:CLAUDE_PLUGIN_DATA "bin\$version"
$bin = Join-Path $dir 'agentmail.exe'

# Only a verified binary is ever renamed into place, so existing means ready.
if (Test-Path -LiteralPath $bin) {
    Write-Output $bin
    exit 0
}

New-Item -ItemType Directory -Path $dir -Force | Out-Null

# Two sessions starting together would both download. CreateNew fails when the lock
# file exists, so exactly one wins and the rest wait for it; the POSIX launcher takes
# the same file the same way. A lock still held after a minute is treated as left
# behind by a killed launcher and ignored; the install is a rename of a verified file,
# never a partial write, so going ahead is safe.
$lock = Join-Path $dir '.lock'
$locked = $false
for ($try = 0; $try -lt 60; $try++) {
    try {
        [System.IO.File]::Open($lock, 'CreateNew').Close()
        $locked = $true
        break
    } catch {
        if (Test-Path -LiteralPath $bin) {
            Write-Output $bin
            exit 0
        }
        Start-Sleep -Seconds 1
    }
}

$tmp = Join-Path $dir (".download." + [guid]::NewGuid().ToString('N'))
try {
    # The winner may have finished between our last check and taking the lock.
    if (-not (Test-Path -LiteralPath $bin)) {
        $asset = "agentmail-$triple.exe"
        [Console]::Error.WriteLine("agentmail plugin: downloading $asset v$version")
        New-Item -ItemType Directory -Path $tmp -Force | Out-Null

        $sums = Join-Path $tmp 'SHA256SUMS'
        try {
            Invoke-WebRequest -Uri "$baseUrl/v$version/SHA256SUMS" -OutFile $sums -UseBasicParsing
        } catch {
            Fail "could not download SHA256SUMS for v$version. Releases: $releases"
        }
        $file = Join-Path $tmp $asset
        try {
            Invoke-WebRequest -Uri "$baseUrl/v$version/$asset" -OutFile $file -UseBasicParsing
        } catch {
            Fail "no prebuilt $asset for v$version. Releases: $releases"
        }

        # coreutils writes `hash  name`, binary mode writes `hash *name`; accept either.
        $pattern = "^([0-9a-f]{64}) [ *]" + [regex]::Escape($asset) + "$"
        $expected = Get-Content $sums | Where-Object { $_ -match $pattern } | ForEach-Object { $Matches[1] } | Select-Object -First 1
        if (-not $expected) { Fail "no checksum listed for $asset in v$version" }
        $actual = (Get-FileHash -Path $file -Algorithm SHA256).Hash.ToLower()
        if ($actual -ne $expected) { Fail "checksum mismatch for $asset v$version; nothing was installed" }

        try {
            [System.IO.File]::Move($file, $bin)
        } catch {
            # Another launcher that ignored a stale lock got there first.
            if (-not (Test-Path -LiteralPath $bin)) { Fail "could not install agentmail into ${dir}: $($_.Exception.Message)" }
        }
        [Console]::Error.WriteLine("agentmail plugin: installed $asset v$version, verified SHA-256")
    }
} finally {
    Remove-Item -Recurse -Force -LiteralPath $tmp -ErrorAction SilentlyContinue
    if ($locked) { Remove-Item -Force -LiteralPath $lock -ErrorAction SilentlyContinue }
}

Write-Output $bin
exit 0
