#!/usr/bin/env bash
# Reports which build dependencies are missing, and what to run.
set -uo pipefail

# rustup installs here but only edits a shell profile if asked to, so a
# freshly installed toolchain is often present yet not on PATH.
[ -d "$HOME/.cargo/bin" ] && PATH="$HOME/.cargo/bin:$PATH"

missing=()        # system packages, installable with the package manager
toolchain=()      # things the package manager is the wrong way to install
ok=0

check_pkg_config () {
  if pkg-config --exists "$1" 2>/dev/null; then
    printf '  \033[32m✓\033[0m %-32s %s\n' "$1" "$(pkg-config --modversion "$1")"
    ok=$((ok + 1))
  else
    printf '  \033[31m✗\033[0m %-32s missing\n' "$1"
    missing+=("$2")
  fi
}

check_command () {
  if command -v "$1" >/dev/null 2>&1; then
    printf '  \033[32m✓\033[0m %-32s %s\n' "$1" "$($1 --version 2>&1 | head -1)"
    ok=$((ok + 1))
  else
    printf '  \033[31m✗\033[0m %-32s not on PATH\n' "$1"
    toolchain+=("$2")
  fi
}

echo "Toolchain"
# Never suggest installing Rust from the distribution's packages: they lag
# well behind, and this workspace needs a current stable toolchain.
check_command cargo "Rust (https://rustup.rs)"
check_command rustc "Rust (https://rustup.rs)"
check_command node  "Node 22.12+ (https://nodejs.org)"
check_command npm   "Node 22.12+ (https://nodejs.org)"

case "$(uname -s)" in
  Linux)
    echo
    echo "Linux system libraries (needed only for the desktop shell)"
    check_pkg_config webkit2gtk-4.1 libwebkit2gtk-4.1-dev
    check_pkg_config javascriptcoregtk-4.1 libwebkit2gtk-4.1-dev
    check_pkg_config gtk+-3.0 libgtk-3-dev
    check_pkg_config libsoup-3.0 libsoup-3.0-dev
    check_pkg_config dbus-1 libdbus-1-dev
    check_pkg_config librsvg-2.0 librsvg2-dev
    ;;
  Darwin)
    echo
    echo "macOS: Xcode command line tools"
    xcode-select -p >/dev/null 2>&1 \
      && printf '  \033[32m✓\033[0m %-32s %s\n' "xcode-select" "$(xcode-select -p)" \
      || { printf '  \033[31m✗\033[0m %-32s missing\n' "xcode-select"; missing+=("xcode-select --install"); }
    ;;
esac

echo
if [ ${#missing[@]} -eq 0 ] && [ ${#toolchain[@]} -eq 0 ]; then
  echo -e "\033[32mEverything needed is present.\033[0m"
  exit 0
fi

if [ ${#toolchain[@]} -gt 0 ]; then
  readarray -t unique_toolchain < <(printf '%s\n' "${toolchain[@]}" | sort -u)
  echo -e "\033[33mToolchain to install:\033[0m"
  for item in "${unique_toolchain[@]}"; do
    echo "  - $item"
  done
  if [ -d "$HOME/.cargo/bin" ]; then
    echo
    echo "  \$HOME/.cargo/bin exists but is not on PATH. Add it with:"
    echo "    . \"\$HOME/.cargo/env\""
  fi
  echo
fi

if [ ${#missing[@]} -gt 0 ]; then
  readarray -t unique < <(printf '%s\n' "${missing[@]}" | sort -u)
  echo -e "\033[33mSystem packages to install:\033[0m"
  if [ -f /etc/debian_version ]; then
    echo "  sudo apt update && sudo apt install -y ${unique[*]}"
  elif [ -f /etc/fedora-release ]; then
    echo "  See docs/DEVELOPMENT.md for the Fedora package names."
  else
    echo "  See docs/DEVELOPMENT.md for your distribution's package names."
  fi
  echo
  echo "These are needed only for the desktop shell. Without them you can still run:"
  echo "  cargo test --workspace --exclude relay-desktop"
  echo "  npm test"
fi
exit 1
