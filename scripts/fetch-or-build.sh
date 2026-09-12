#!/bin/sh
# herdr [[build]] step.
#
# Fast path: download the prebuilt `hsm` and `agentmail` for this platform from the
# GitHub release that matches the version this source declares, verify both against the
# release's SHA256SUMS, and install them into target/release.
# Fallback: on ANY miss — no release for this version, no network, a checksum mismatch,
# an unmapped platform, no curl/wget — say why and build from source with cargo, exactly
# as before. Installing never gets harder than "have cargo".
#
# Overridable for testing: HSM_REPO, HSM_BASE_URL, HSM_CARGO_TOML, HSM_OUT_DIR.
set -u

repo="${HSM_REPO:-umutciloglu/herdr-session-manager}"

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
cargo_toml="${HSM_CARGO_TOML:-$repo_root/Cargo.toml}"
out_dir="${HSM_OUT_DIR:-$repo_root/target/release}"
base_url="${HSM_BASE_URL:-https://github.com/$repo/releases/download}"

have() { command -v "$1" >/dev/null 2>&1; }

download() { # download <url> <dest>
  # Quiet: a missing release is an expected outcome here, not an error to shout about.
  if have curl; then
    curl -fsSL -o "$2" "$1" 2>/dev/null
  elif have wget; then
    wget -q -O "$2" "$1" 2>/dev/null
  else
    return 127
  fi
}

sha256_of() { # prints the hex digest of file $1
  if have sha256sum; then
    sha256sum "$1" | awk '{print $1}'
  elif have shasum; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    return 127
  fi
}

build_from_source() {
  # herdr may be launched without ~/.cargo/bin on PATH (GUI launch), so source the env
  # file when present; the guard keeps a missing file from aborting the build.
  [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
  if ! command -v cargo >/dev/null 2>&1; then
    echo "herdr-session-manager needs Rust 1.85+ to build, but cargo was not found. Install Rust from https://rustup.rs and re-run the install." >&2
    exit 1
  fi
  cd "$repo_root" && cargo build --release --bin hsm --bin agentmail || exit 1
}

# Prints why it gave up and returns 1; the caller then builds from source.
fetch_prebuilt() {
  miss() {
    echo "herdr-session-manager: $1 — building from source instead." >&2
    [ -n "${tmpdir:-}" ] && rm -rf "$tmpdir"
    return 1
  }

  os=$(uname -s 2>/dev/null || echo unknown)
  arch=$(uname -m 2>/dev/null || echo unknown)
  triple=""
  case "$os" in
    Darwin)
      case "$arch" in
        arm64|aarch64) triple="aarch64-apple-darwin" ;;
        x86_64|amd64)  triple="x86_64-apple-darwin" ;;
      esac
      ;;
    Linux)
      case "$arch" in
        x86_64|amd64)  triple="x86_64-unknown-linux-musl" ;;
      esac
      ;;
  esac
  [ -n "$triple" ] || { miss "no prebuilt binary for $os/$arch"; return 1; }

  # The version this source declares, not the newest release: a binary whose version
  # differs from this checkout is never installed silently.
  version=$(grep -E '^version *= *"' "$cargo_toml" 2>/dev/null | head -n 1 | sed -E 's/^version *= *"([^"]+)".*/\1/')
  [ -n "$version" ] || { miss "could not read version from $cargo_toml"; return 1; }

  tmpdir=$(mktemp -d 2>/dev/null) || { echo "herdr-session-manager: could not create a temp dir — building from source instead." >&2; return 1; }

  sums="$tmpdir/SHA256SUMS"
  download "$base_url/v$version/SHA256SUMS" "$sums" || { miss "no published checksums for v$version"; return 1; }

  # Download and verify both binaries before installing either: half an install is
  # worse than none.
  for name in hsm agentmail; do
    asset="$name-$triple"
    download "$base_url/v$version/$asset" "$tmpdir/$asset" || { miss "no prebuilt $asset for v$version"; return 1; }
    # coreutils writes `hash  name`, binary mode writes `hash *name`; accept either.
    expected=$(grep -E "^[0-9a-f]{64} [ *]$asset\$" "$sums" 2>/dev/null | awk '{print $1}' | head -n 1)
    [ -n "$expected" ] || { miss "no checksum listed for $asset"; return 1; }
    actual=$(sha256_of "$tmpdir/$asset") || { miss "no sha-256 tool (sha256sum/shasum) available"; return 1; }
    [ "$actual" = "$expected" ] || { miss "checksum mismatch for $asset"; return 1; }
  done

  mkdir -p "$out_dir" || { miss "could not create $out_dir"; return 1; }
  for name in hsm agentmail; do
    chmod +x "$tmpdir/$name-$triple"
    mv -f "$tmpdir/$name-$triple" "$out_dir/$name" || { miss "could not install $name into $out_dir"; return 1; }
  done

  rm -rf "$tmpdir"
  echo "herdr-session-manager: installed prebuilt v$version ($triple), verified SHA-256." >&2
  return 0
}

fetch_prebuilt || build_from_source

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
