#!/bin/sh
# Action launcher: open the ask popup. Same context rules as browse.sh — the
# popup learns the invoking pane from HERDR_PLUGIN_CONTEXT_JSON, which is how
# the question knows who it is from.
set -u
herdr_bin="${HERDR_BIN_PATH:-herdr}"
exec "$herdr_bin" plugin pane open --plugin herdr-session-manager --entrypoint asker --placement popup
