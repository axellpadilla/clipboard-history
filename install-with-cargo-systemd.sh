#!/usr/bin/env bash
set -e

# Fork/branch to install from. Override with e.g. `RINGBOARD_REPO=someone/else RINGBOARD_BRANCH=main curl ... | bash`.
RINGBOARD_REPO="${RINGBOARD_REPO:-axellpadilla/clipboard-history}"
RINGBOARD_BRANCH="${RINGBOARD_BRANCH:-patched}"
RAW_BASE="https://raw.githubusercontent.com/$RINGBOARD_REPO/$RINGBOARD_BRANCH"
RELEASE_BASE="https://github.com/$RINGBOARD_REPO/releases/latest/download"

# Which GUI client to install as the desktop launcher/`toggle` target.
# Override with e.g. `RINGBOARD_CLIENT=iced curl ... | bash`.
RINGBOARD_CLIENT="${RINGBOARD_CLIENT:-egui}"
if [ "$RINGBOARD_CLIENT" != "egui" ] && [ "$RINGBOARD_CLIENT" != "iced" ]; then
  echo "Unknown RINGBOARD_CLIENT: $RINGBOARD_CLIENT (expected 'egui' or 'iced')" >&2
  exit 1
fi
RINGBOARD_OTHER_CLIENT="egui"
if [ "$RINGBOARD_CLIENT" = "egui" ]; then
  RINGBOARD_OTHER_CLIENT="iced"
fi

# Release binaries are built per target triple (see .github/workflows/cid.yml).
# Override with e.g. `RINGBOARD_TARGET=aarch64-unknown-linux-gnu curl ... | bash`.
if [ -z "$RINGBOARD_TARGET" ]; then
  case "$(uname -m)" in
    x86_64) RINGBOARD_TARGET=x86_64-unknown-linux-gnu ;;
    aarch64 | arm64) RINGBOARD_TARGET=aarch64-unknown-linux-gnu ;;
    riscv64) RINGBOARD_TARGET=riscv64gc-unknown-linux-gnu ;;
    *)
      echo "Unsupported architecture: $(uname -m). Set RINGBOARD_TARGET explicitly." >&2
      exit 1
      ;;
  esac
fi

mkdir -p ~/.cargo/bin
# install_bin below downloads straight to ~/.cargo/bin instead of going
# through `cargo install`, so unlike upstream we can't assume cargo has
# already put it on PATH (e.g. via ~/.profile). Export it ourselves so the
# `which ringboard-*` calls below actually resolve.
export PATH="$HOME/.cargo/bin:$PATH"

# Downloads $1 to the curl args in $2.., failing loudly (instead of silently
# writing an empty/error-page file) if the request doesn't succeed.
fetch() {
  local url="$1"
  shift
  if ! curl -sSfL "$url" "$@"; then
    echo "Failed to download $url" >&2
    exit 1
  fi
}

# Downloads a release binary asset for the current target and installs it as
# a `ringboard-*` binary on PATH. Downloads to a temp file first and only
# chmods/moves it into place once the download fully succeeds, so a failed
# or partial download (e.g. a 404 HTML page) never becomes a broken live
# binary, and the atomic rename means a currently-running process keeps
# executing its old binary safely instead of being corrupted mid-read.
install_bin() {
  local bin="$1"
  local tmp
  tmp="$(mktemp ~/.cargo/bin/."$bin".XXXXXX)"
  if ! curl -sSfL "$RELEASE_BASE/$RINGBOARD_TARGET-$bin" -o "$tmp"; then
    echo "Failed to download $bin from $RELEASE_BASE/$RINGBOARD_TARGET-$bin" >&2
    rm -f "$tmp"
    exit 1
  fi
  chmod +x "$tmp"
  mv -f "$tmp" ~/.cargo/bin/"$bin"
}

fetch "$RAW_BASE/ringboard.slice" --create-dirs -O --output-dir ~/.config/systemd/user/

install_bin ringboard-server
fetch "$RAW_BASE/server/ringboard-server.service" --create-dirs -O --output-dir ~/.config/systemd/user/
sed -i "s|ExecStart=ringboard-server|ExecStart=$(which ringboard-server)|g" ~/.config/systemd/user/ringboard-server.service

install_bin ringboard

install_bin ringboard-$RINGBOARD_CLIENT
fetch "$RAW_BASE/$RINGBOARD_CLIENT/ringboard-$RINGBOARD_CLIENT.desktop" --create-dirs -O --output-dir ~/.local/share/applications/
fetch "$RAW_BASE/logo.jpeg" -o ringboard.jpeg --create-dirs -O --output-dir ~/.local/share/icons/hicolor/1024x1024/
sed -i "s|Exec=ringboard-$RINGBOARD_CLIENT|Exec=$(echo $(which ringboard-$RINGBOARD_CLIENT) toggle)|g" ~/.local/share/applications/ringboard-$RINGBOARD_CLIENT.desktop
sed -i "s|Icon=ringboard|Icon=$HOME/.local/share/icons/hicolor/1024x1024/ringboard.jpeg|g" ~/.local/share/applications/ringboard-$RINGBOARD_CLIENT.desktop

# Replace a previously installed different GUI client so there's only ever
# one Ringboard launcher entry/binary active, instead of leaving a second,
# stale icon around that still (mis)launches the old client.
killall ringboard-$RINGBOARD_OTHER_CLIENT 2> /dev/null || true
rm -f ~/.local/share/applications/ringboard-$RINGBOARD_OTHER_CLIENT.desktop
rm -f ~/.cargo/bin/ringboard-$RINGBOARD_OTHER_CLIENT

# Stop existing watchers in case user is switching between X11 and Wayland
systemctl --user disable ringboard-x11 --now 2> /dev/null || true
systemctl --user disable ringboard-wayland --now 2> /dev/null || true

if [ "$XDG_SESSION_TYPE" = "wayland" ]; then
  install_bin wayland-interface-check
  if [ "$XDG_CURRENT_DESKTOP" != "COSMIC" ] && ! wayland-interface-check ext_data_control_manager_v1; then
    export XDG_SESSION_TYPE=x11
  fi
fi

install_bin ringboard-$XDG_SESSION_TYPE
fetch "$RAW_BASE/$XDG_SESSION_TYPE/ringboard-$XDG_SESSION_TYPE.service" --create-dirs -O --output-dir ~/.config/systemd/user/
sed -i "s|ExecStart=ringboard-$XDG_SESSION_TYPE|ExecStart=$(which ringboard-$XDG_SESSION_TYPE)|g" ~/.config/systemd/user/ringboard-$XDG_SESSION_TYPE.service

killall ringboard-egui ringboard-iced ringboard-tui 2> /dev/null || true

# The service won't exist yet on a first install.
systemctl --user stop ringboard-server 2> /dev/null || true
systemctl --user daemon-reload
systemctl --user start ringboard-server
systemctl --user enable ringboard-$XDG_SESSION_TYPE --now

echo
echo "--- DONE ---"
echo
echo "Consider reading the $RINGBOARD_CLIENT docs:"
echo "https://github.com/$RINGBOARD_REPO/blob/$RINGBOARD_BRANCH/$RINGBOARD_CLIENT/README.md"

if [ "$XDG_SESSION_TYPE" = "x11" ]; then
  echo
  echo "If you use a password manager and wish to exclude passwords from the clipboard, read the docs:"
  echo "https://github.com/$RINGBOARD_REPO/blob/$RINGBOARD_BRANCH/x11/README.md#password-manager-integration"
fi

if [ "$XDG_CURRENT_DESKTOP" = "COSMIC" ] && [ ! -f /etc/profile.d/clipboard.sh ]; then
  echo
  echo "COSMIC_DATA_CONTROL_ENABLED must be set, which requires sudo."
  echo "Please reboot after letting the following command run:"
  echo "$ sudo sh -c 'echo \"export COSMIC_DATA_CONTROL_ENABLED=1\" > /etc/profile.d/clipboard.sh; chmod 644 /etc/profile.d/clipboard.sh'"
  sudo sh -c 'echo "export COSMIC_DATA_CONTROL_ENABLED=1" > /etc/profile.d/clipboard.sh; chmod 644 /etc/profile.d/clipboard.sh'
fi
