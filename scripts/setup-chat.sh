#!/bin/sh
# Action launcher: open the agent chat setup checklist in a popup.
set -u
herdr_bin="${HERDR_BIN_PATH:-herdr}"
exec "$herdr_bin" plugin pane open --plugin herdr-session-manager --entrypoint chat-setup --placement popup
