# Startup hook (Windows): snapshot herdr session refs and refresh the index.
$root = if ($env:HERDR_PLUGIN_ROOT) { $env:HERDR_PLUGIN_ROOT } else { Join-Path $PSScriptRoot '..' }
if ($root.StartsWith('\\?\')) { $root = $root.Substring(4) }
& (Join-Path $root 'target\release\hsm.exe') startup
