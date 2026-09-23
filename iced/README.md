# Ringboard iced

<a href="https://crates.io/crates/clipboard-history-iced">![Crates.io Version](https://img.shields.io/crates/v/clipboard-history-iced)</a>

This binary is a Ringboard client that provides a GUI built with
[iced](https://github.com/iced-rs/iced), using [native-theme-iced](https://docs.rs/native-theme-iced)
to match your system theme.

## Suggested workflow

To reduce startup latency, closing the application sends it to the background rather than killing
it. Thus, it is suggested to bind a shortcut that executes the following command for fast clipboard
launches:

```shell
# Run this command to generate the command that goes in the shortcut
bash -c 'echo $(which ringboard-iced) toggle'
```

## Usage instructions

- Type to search; the search box is focused automatically whenever the window is focused, so you can start typing right away.
- Hover an entry (or highlight it with the keyboard) to reveal its delete and show-details buttons; the favorite star is always visible.

## Keyboard shortcuts

| Keys | Action |
| --- | --- |
| <kbd>Type</kbd> | search the clipboard history |
| <kbd>Enter</kbd> / <kbd>Ctrl</kbd>+<kbd>V</kbd> | paste the highlighted entry |
| <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>V</kbd> | paste it as plain text |
| <kbd>Ctrl</kbd>+<kbd>0</kbd>-<kbd>9</kbd> | paste the Nth recent entry |
| <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>0</kbd>-<kbd>9</kbd> | paste the Nth favorite |
| <kbd>Up</kbd> / <kbd>Down</kbd> | move the highlight |
| <kbd>Home</kbd> / <kbd>End</kbd> | jump to the first or last entry |
| <kbd>PgUp</kbd> / <kbd>PgDn</kbd> | move by a page |
| <kbd>Left</kbd> / <kbd>Right</kbd> | collapse or expand Favorites, or open or close the details of the highlighted entry |
| <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Up</kbd>/<kbd>Down</kbd> | move a favorite earlier or later |
| <kbd>Ctrl</kbd>+<kbd>D</kbd> | toggle details |
| <kbd>Delete</kbd> | delete the highlighted entry |
| <kbd>Ctrl</kbd>+<kbd>R</kbd> | reload the database |
| <kbd>Ctrl</kbd>+<kbd>Tab</kbd> / <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Tab</kbd> | cycle tabs |
| <kbd>Alt</kbd>+<kbd>1</kbd>-<kbd>5</kbd> | jump to a tab: All, Text, Images, Favorites, Settings |
| <kbd>Alt</kbd>+<kbd>X</kbd> / <kbd>Alt</kbd>+<kbd>M</kbd> | cycle the search kind: plain, RegEx, MIME |
| <kbd>?</kbd> | toggle this list |
| <kbd>Esc</kbd> | clear the search, close details, then close the window |

`src/shortcuts.rs` renders this list in the app, and tests that every row matches this section.

## Settings tab

The Settings tab surfaces the same knobs the `ringboard` CLI already exposes, without dropping to
a terminal:

- **Server limits** (`ringboard configure server`): the max main-ring and favorites-ring entry
  counts, read from and written to the server's `config.toml`. As with the CLI, the server needs
  restarting for a change to take effect.
- **Maintenance** (`ringboard gc`): trigger a garbage-collection pass with a given "max wasted
  bytes" threshold (0 forces a full compaction and duplicate cleanup).

Wiping the database and viewing detailed fragmentation stats are still CLI-only for now
(`ringboard wipe` / `ringboard debug stats`) — wiping in particular tears down the server and the
directory this client has open, which isn't something to do safely from a running GUI session
without more surgery than a first pass warrants.

## Performance

Unlike a fixed-interval redraw loop, this client only wakes up in response to real events:
keyboard/window events and messages pushed from the background controller thread each deliver
their own wakeup, so the app does no work at all while sitting idle (no background polling
timer). This keeps idle CPU usage effectively at zero.

Like the [egui client](../egui), by default this client resumes from a resident background
process instead of exiting fully when closed: closing the window hides it, and a subsequent
`ringboard-iced toggle` re-shows the same still-running process instantly rather than starting a
new one. Set `RINGBOARD_NO_DAEMON=1` to opt out and have the window really exit on close — useful
when iterating locally, since otherwise re-running the binary without `toggle` still leaves the
previous instance's window alive in the background until its lock is taken over.
