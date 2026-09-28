# Action launcher (Windows): open the agent chat setup checklist in a popup.
$herdr = if ($env:HERDR_BIN_PATH) { $env:HERDR_BIN_PATH } else { 'herdr' }
& $herdr plugin pane open --plugin herdr-session-manager --entrypoint chat-setup --placement popup
exit $LASTEXITCODE
