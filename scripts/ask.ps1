# Action launcher (Windows): herdr cannot spawn relative manifest pane commands here, so
# open a focused split and run the asker by absolute path instead of the popup.
$ErrorActionPreference = 'Stop'
$u = New-Object System.Text.UTF8Encoding($false); [Console]::OutputEncoding = $u; $OutputEncoding = $u
$herdr = if ($env:HERDR_BIN_PATH) { $env:HERDR_BIN_PATH } else { 'herdr' }
$root = if ($env:HERDR_PLUGIN_ROOT) { $env:HERDR_PLUGIN_ROOT } else { Join-Path $PSScriptRoot '..' }
if ($root.StartsWith('\\?\')) { $root = $root.Substring(4) }
$bin = Join-Path $root 'target\release\hsm.exe'
# The popup would inherit HERDR_PLUGIN_CONTEXT_JSON from herdr; a split pane runs a fresh shell,
# so hand the context over explicitly or the question has no sender and no "current pane".
$ctx = if ($env:HERDR_PLUGIN_CONTEXT_JSON) { $env:HERDR_PLUGIN_CONTEXT_JSON } else { '{}' }
$pane = (& $herdr pane split --current --direction right --focus --env "HERDR_PLUGIN_CONTEXT_JSON=$ctx" | ConvertFrom-Json).result.pane_id
& $herdr pane run $pane "& '$bin' ask --pane-mode"
