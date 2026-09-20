#!/bin/sh
# Install the jevnql command (no Rust needed):
#   curl -fsSL https://raw.githubusercontent.com/Adityakhalkar/JevNQL/main/install.sh | sh
set -eu

repo="Adityakhalkar/JevNQL"
bin_dir="${JEVNQL_BIN_DIR:-$HOME/.local/bin}"

case "$(uname -s) $(uname -m)" in
  "Darwin arm64")  target=aarch64-apple-darwin ;;
  "Darwin x86_64") target=x86_64-apple-darwin ;;
  "Linux x86_64")  target=x86_64-unknown-linux-gnu ;;
  "Linux aarch64"|"Linux arm64") target=aarch64-unknown-linux-gnu ;;
  *) echo "No prebuilt jevnql for $(uname -s) $(uname -m). Build it with: cargo install --git https://github.com/$repo jevnql-cli" >&2; exit 1 ;;
esac

url="https://github.com/$repo/releases/latest/download/jevnql-$target.tar.gz"
echo "Downloading jevnql ($target)…"
mkdir -p "$bin_dir"
curl -fsSL "$url" | tar -xz -C "$bin_dir"
chmod +x "$bin_dir/jevnql"

echo "Installed $bin_dir/jevnql"
case ":$PATH:" in
  *":$bin_dir:"*) echo "Try it:  jevnql your-data/*.csv" ;;
  *) echo "Add it to your PATH:  export PATH=\"$bin_dir:\$PATH\"" ;;
esac
