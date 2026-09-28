# Action launcher (Windows): open the ask popup. Same context rules as ask.sh - the
# popup learns the invoking pane from HERDR_PLUGIN_CONTEXT_JSON, which is how the
# question knows who it is from.
$herdr = if ($env:HERDR_BIN_PATH) { $env:HERDR_BIN_PATH } else { 'herdr' }
& $herdr plugin pane open --plugin herdr-session-manager --entrypoint asker --placement popup
exit $LASTEXITCODE
