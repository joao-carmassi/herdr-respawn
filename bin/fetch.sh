#!/bin/sh
# Build step: puts bin/herdr-respawn in place. Prefers a local build, then the release binary, then cargo.
set -eu
cd "$(dirname "$0")/.."
out=bin/herdr-respawn

if [ -f target/release/herdr-respawn ]; then
  cp target/release/herdr-respawn "$out"; chmod 755 "$out"; exit 0
fi

ver=$(sed -n 's/^version = "\(.*\)"/\1/p' herdr-plugin.toml | head -n1)
case "$(uname -s)" in Linux) os=linux ;; Darwin) os=macos ;; *) echo "unsupported OS" >&2; exit 1 ;; esac
case "$(uname -m)" in x86_64|amd64) arch=x86_64 ;; aarch64|arm64) arch=aarch64 ;; *) echo "unsupported CPU" >&2; exit 1 ;; esac
name="herdr-respawn-$os-$arch"
url="https://github.com/joao-carmassi/herdr-respawn/releases/download/v$ver/$name"

get() { if command -v curl >/dev/null 2>&1; then curl -fsSL "$1" -o "$2"; else wget -q "$1" -O "$2"; fi; }
sha() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | cut -d' ' -f1
  else openssl dgst -sha256 "$1" | sed 's/.*= //'; fi
}

tmp="$out.download"
if get "$url" "$tmp" && get "$url.sha256" "$tmp.sha256"; then
  want=$(cut -d' ' -f1 "$tmp.sha256")
  if [ "$(sha "$tmp")" = "$want" ]; then
    mv "$tmp" "$out"; chmod 755 "$out"; rm -f "$tmp.sha256"; exit 0
  fi
  echo "checksum mismatch for $name" >&2
fi
rm -f "$tmp" "$tmp.sha256"

if command -v cargo >/dev/null 2>&1; then
  cargo build --release --locked
  cp target/release/herdr-respawn "$out"; chmod 755 "$out"; exit 0
fi
echo "could not download $url and cargo is missing; install Rust from https://rustup.rs" >&2
exit 1
