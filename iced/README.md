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

- Type to search; the search box is focused automatically whenever the window is focused, so you
  can start typing right away.
  - Use <kbd>Alt</kbd> + <kbd>X</kbd> or <kbd>Alt</kbd> + <kbd>M</kbd> to cycle the search kind
    (plain text → RegEx → MIME type).
- Press <kbd>Enter</kbd> to paste the highlighted entry.
  - Use <kbd>Ctrl</kbd> + <kbd>N</kbd> to paste the `N`<sup>th</sup> entry.
- Use <kbd>Up</kbd>/<kbd>Down</kbd> to move the highlight, and <kbd>Left</kbd>/<kbd>Right</kbd> to
  collapse/expand the Favorites section.
- Hover an entry (or highlight it with the keyboard) to reveal its delete and show-details
  buttons; the favorite star is always visible.
- Use <kbd>Ctrl</kbd> + <kbd>D</kbd> to toggle details for the highlighted entry.
- Use <kbd>Alt</kbd> + <kbd>1</kbd>-<kbd>5</kbd> to jump directly to a tab
  (All/Text/Images/Favorites/Settings), or <kbd>Ctrl</kbd> + <kbd>Tab</kbd> /
  <kbd>Ctrl</kbd> + <kbd>Shift</kbd> + <kbd>Tab</kbd> to cycle through them.
- Use <kbd>Ctrl</kbd> + <kbd>R</kbd> to manually reload the database.
- Press <kbd>Escape</kbd> to clear the current search, or to close the app if the search is
  already empty.

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
