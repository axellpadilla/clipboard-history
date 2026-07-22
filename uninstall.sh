#!/usr/bin/env bash

systemctl --user disable ringboard-server --now
systemctl --user disable ringboard-x11 --now
systemctl --user disable ringboard-wayland --now
systemctl --user disable ringboard.slice --now
systemctl --user daemon-reload

rm ~/.config/systemd/user/ringboard*
rm ~/.local/share/applications/ringboard*
rm ~/.local/share/icons/hicolor/1024x1024/ringboard*
rm -r ~/.local/share/clipboard-history/

rm -f ~/.cargo/bin/ringboard \
  ~/.cargo/bin/ringboard-server \
  ~/.cargo/bin/ringboard-x11 \
  ~/.cargo/bin/ringboard-wayland \
  ~/.cargo/bin/ringboard-tui \
  ~/.cargo/bin/ringboard-egui \
  ~/.cargo/bin/ringboard-iced \
  ~/.cargo/bin/wayland-interface-check
