#!/bin/sh
# Action launcher: open the session browser popup. The popup process gets the invoking
# pane through HERDR_PLUGIN_CONTEXT_JSON, which is how "open in current pane" and
# "insert address" know where to type.
set -u
herdr_bin="${HERDR_BIN_PATH:-herdr}"
exec "$herdr_bin" plugin pane open --plugin herdr-session-manager --entrypoint browser --placement popup
