# Action launcher (Windows): open the session browser popup. Same context rules as
# browse.sh - the popup learns the invoking pane from HERDR_PLUGIN_CONTEXT_JSON, which is
# how "open in current pane" and "insert address" know where to type.
$herdr = if ($env:HERDR_BIN_PATH) { $env:HERDR_BIN_PATH } else { 'herdr' }
& $herdr plugin pane open --plugin herdr-session-manager --entrypoint browser --placement popup
exit $LASTEXITCODE
