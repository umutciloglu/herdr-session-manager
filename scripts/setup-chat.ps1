# Action launcher (Windows): run the agent chat setup checklist in a focused split.
$ErrorActionPreference = 'Stop'
$u = New-Object System.Text.UTF8Encoding($false); [Console]::OutputEncoding = $u; $OutputEncoding = $u
$herdr = if ($env:HERDR_BIN_PATH) { $env:HERDR_BIN_PATH } else { 'herdr' }
$root = if ($env:HERDR_PLUGIN_ROOT) { $env:HERDR_PLUGIN_ROOT } else { Join-Path $PSScriptRoot '..' }
if ($root.StartsWith('\\?\')) { $root = $root.Substring(4) }
$bin = Join-Path $root 'target\release\hsm.exe'
# The popup would inherit HERDR_PLUGIN_CONTEXT_JSON from herdr; a split pane runs a fresh shell,
# so hand the context over explicitly or "current pane" and "insert" have nothing to target.
$ctx = if ($env:HERDR_PLUGIN_CONTEXT_JSON) { $env:HERDR_PLUGIN_CONTEXT_JSON } else { '{}' }
$pane = (& $herdr pane split --current --direction right --focus --env "HERDR_PLUGIN_CONTEXT_JSON=$ctx" | ConvertFrom-Json).result.pane_id
& $herdr pane run $pane "& '$bin' setup-chat"
