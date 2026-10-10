#!/usr/bin/env bash
# Builds the RTX VSR app (app/) and installs it for the current user.
#
#   install/install_app.sh
#
# Needs a Rust toolchain (https://rustup.rs) and nothing else: the app loads
# the NVIDIA driver's libraries at run time and offers to download NVIDIA's
# VSR runtime itself. Run `rtx-vsr` afterwards; its first start adds it to the
# applications menu.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
bin_dir=${XDG_BIN_HOME:-$HOME/.local/bin}

if ! command -v cargo >/dev/null; then
    echo "cargo not found. Install Rust first:" >&2
    echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" >&2
    exit 1
fi

(cd "$here/../app" && cargo build --release --locked)
install -Dm755 "$here/../app/target/release/rtx-vsr" "$bin_dir/rtx-vsr"

echo "Installed $bin_dir/rtx-vsr"
case ":$PATH:" in
    *":$bin_dir:"*) echo "Start it with: rtx-vsr" ;;
    *) echo "Start it with: $bin_dir/rtx-vsr   ($bin_dir is not on your PATH)" ;;
esac
