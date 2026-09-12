#!/bin/sh
# herdr [[build]] step. Builds both binaries from source with cargo, then makes
# `hsm` and `agentmail` callable by name.
#
# A prebuilt-download fast path (release asset + SHA256SUMS, version-matched) can be
# added in front of the cargo build once releases exist; keep the fallback exactly
# this so installing never gets harder than "have cargo".
set -u

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)

# herdr may be launched without ~/.cargo/bin on PATH (GUI launch), so source the env
# file when present; the guard keeps a missing file from aborting the build.
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

if ! command -v cargo >/dev/null 2>&1; then
  echo "herdr-session-manager needs Rust 1.85+ to build, but cargo was not found. Install Rust from https://rustup.rs and re-run the install." >&2
  exit 1
fi

cd "$repo_root" && cargo build --release --bin hsm --bin agentmail || exit 1

# --- put the binaries on PATH ---------------------------------------------------
# Symlinks, not copies, so a rebuild updates them in place. ~/.local/bin is the
# conventional per-user bin dir; when it is not on PATH yet, one guarded line goes
# into the shell rc so new shells pick it up. The running shell cannot be changed
# from here, hence the notice at the end.
bin_dir="$HOME/.local/bin"
mkdir -p "$bin_dir"
for name in hsm agentmail; do
  ln -sf "$repo_root/target/release/$name" "$bin_dir/$name"
done

case ":$PATH:" in
  *":$bin_dir:"*) on_path=1 ;;
  *) on_path=0 ;;
esac

if [ "$on_path" -eq 0 ]; then
  case "$(basename "${SHELL:-sh}")" in
    zsh) rc="$HOME/.zshrc" ;;
    bash) rc="$HOME/.bashrc" ;;
    fish) rc="$HOME/.config/fish/config.fish" ;;
    *) rc="$HOME/.profile" ;;
  esac
  marker="# added by herdr-session-manager"
  if [ -f "$rc" ] && grep -q "$marker" "$rc"; then
    :
  else
    mkdir -p "$(dirname "$rc")"
    if [ "$(basename "$rc")" = "config.fish" ]; then
      printf '\n%s\nfish_add_path %s\n' "$marker" "$bin_dir" >> "$rc"
    else
      printf '\n%s\nexport PATH="%s:$PATH"\n' "$marker" "$bin_dir" >> "$rc"
    fi
    echo "herdr-session-manager: added $bin_dir to PATH in $rc. Open a new shell, or run: export PATH=\"$bin_dir:\$PATH\"" >&2
  fi
fi

echo "herdr-session-manager: hsm and agentmail are linked in $bin_dir" >&2
