# AGENTS.md

## Core Practices

### Evidence-Based Development

Verify against executable sources of truth, not prose. Always favor official third-party docs, source code, and best practices over LLM memory or stale prose. In this repo that means:

- **Lint config**: The real `rustfmt` and `clippy` flags live in the `supercilex-tests` crate source, not in any repo config file. Read it when unsure about a lint rule.
- **Golden files**: `client-sdk/api.golden`, `core/api.golden`, and `cli/command-reference*.golden` are the authoritative snapshot of the public API surface. They are the contract — if you change a public signature, the golden must be updated.
- **CI is the test**: There is no `.github/`, no `rustfmt.toml`, no Makefile. `tests/tidy.rs` **is** the CI. If a rule isn't enforced there, it doesn't exist. Never create config files the test doesn't expect.
- **Third-party APIs**: When interacting with framework or library code (iced >0.14.0, `image` >0.25, `regex`, `rustix`, `tokio`, etc.), consult the upstream crate docs and source before assuming behavior. Iced >0.14's `text_input` lacking `on_focus` and `window::resize` corrupting text are examples of version-specific behavior that only the real docs/code reveal.

### Design by Contract (DbC)

The `Command` → `Message` protocol in `client-sdk/src/ui_actor.rs` defines formal contracts between GUI clients and the background controller thread:

| Command | Pre-condition | Post-condition (success) | Post-condition (failure) |
|---|---|---|---|
| `Paste(id)` | Entry exists, paste server reachable | `Message::Pasted` | `Message::Error(CommandError)` |
| `PasteText(id)` | Entry exists, paste server reachable | `Message::Pasted` (forced `text/plain`) | `Message::Error(CommandError)` |
| `Swap(id1, id2)` | Both IDs exist in the ring | `Message::Swapped` | `SwapResponse { error1, error2 }` → `CommandError::Core` |
| `Favorite(id)` / `Unfavorite(id)` | Entry exists | `Message::FavoriteChange(id)` | `CommandError::Core` |
| `Delete(id)` | Entry exists | `Message::Deleted(id)` | `CommandError::Core` |

New `Command` variants must define their pre/post conditions explicitly and follow the same `Result<Option<Message>, CommandError>` pattern.

### Fail-Fast Operational Standard

The controller's `handle_command()` in `client-sdk/src/ui_actor.rs` returns `Result<Option<Message>, CommandError>`. Every operation reports failure immediately at the interface boundary — no silent error swallowing:

- Failed socket connections → `ClientError` returned, cached connection invalidated (`*server = None` / `*paste_server = None`).
- Missing entries → `CoreError::IdNotFound(IdNotFoundError)` propagated.
- Invalid regex in search → `CommandError::Regex(regex::Error)`.
- Corrupt image data → `CommandError::Image(ImageError)`.

The UI thread receives these as `Arc<ControllerMessage>` containing `Message::Error` or `Message::FatalDbOpen` — never a half-baked state.

### Railway Oriented Programming (ROP)

The `controller()` function in `client-sdk/src/ui_actor.rs` runs a two-track data flow:

- **Success track**: `Command` → `handle_command()` → `Ok(Some(Message::Pasted/Swapped/...))` → pushed to UI via channel.
- **Failure track**: `Command` → `handle_command()` → `Err(CommandError)` → wrapped as `Message::Error` or `Message::FatalDbOpen` → pushed to UI.

The GUI's `Message` enum (`iced/src/message.rs`) already has both tracks: `Controller(Arc<ControllerMessage>)` for success/failure messages from the controller, plus its own `WindowEvent`/`KeyEvent`/user-action variants. Never mix the tracks — let `Result` compose cleanly.

### Result Pattern (Type Safety)

The codebase uses `Result<T, E>` exclusively for fallible operations. Key error types (all in `client-sdk`):

- `ClientError` — protocol version mismatch, invalid server responses.
- `CommandError` — aggregates `CoreError`, `ClientError`, `regex::Error`, `ImageError`.
- `CoreError` (from `ringboard-core`) — `IdNotFound`, `IoErr`, ring-level failures.

No `null`, no `panic!` for flow control, no unchecked exceptions. Every call site that can fail returns `Result` and forces the caller to handle both outcomes.

### Compaction-Justified DRY

Reuse is mandatory where patterns repeat. Concrete examples in this codebase:

- **All GUI clients share one controller**: `ui_actor::controller()`, `Command`, and `Message` live once in `client-sdk`. New paste/swap/delete behavior belongs in the SDK — never copy-pasted per-client.
- **`ImagePipeline` in `iced/src/app.rs`**: One type handles both thumbnail and full-resolution image decode/cache pipelines. The comment in the source says it explicitly: "keeping the mechanics in one type means fixes to requesting/eviction can't miss a twin copy."
- **`UiEntryCache` enum** (`client-sdk/src/ui_actor.rs`): A single type represents all entry display states (text, highlighted text, image, binary, error) — no parallel type hierarchies per client.

### Repo-Specific Constraints

- **Server is the single writer**: All mutations go through the server over Unix domain sockets (`io_uring` + `mmap` backed). Clients are read-only views. Never bypass the server to write to the DB directly.
- **Nightly edition 2024 is baseline**: The workspace uses `edition = "2024"` and nightly-only features (`#![feature(core_io_borrowed_buf)]`). Stable Rust will not compile this project.
- **Environment-driven behaviour**: `RINGBOARD_NO_DAEMON` switches between resident daemon mode and dev-mode. The iced client detects existing instances at boot to avoid duplicate socket ownership.

## Lint & format (DO NOT use bare `cargo fmt` or `cargo clippy`)

The CI uses extra config that plain `cargo fmt/clippy` silently omit. The one command to run is:

```sh
cargo test -p lint --test tidy
```

This calls `supercilex-tests::fmt()` and `::clippy()` with the full config. To auto-fix:

```sh
UPDATE_EXPECT=1 cargo test -p lint --test tidy
```

The hidden `rustfmt` config applied: `style_edition=2024, imports_granularity=Crate, group_imports=StdExternalCrate, wrap_comments=true, format_strings=true, overflow_delimited_expr=true, format_code_in_doc_comments=true, format_macro_bodies=true, format_macro_matchers=true, use_field_init_shorthand=true`.

The hidden `clippy` config: `-- -W clippy::all,pedantic,nursery,cargo,float_cmp_const,empty_structs_with_brackets` with a handful of commonly-suppressed lints (see `supercilex-tests` source for the exact suppression list).

There is no `rustfmt.toml` and no `.github/` workflows — the lint test **is** the CI.

## Build & run

- Edition **2024** — requires nightly Rust.
- Use `cargo run -p clipboard-history-iced` (not `iced` — that's a directory, not a package name). Same pattern for all clients.
- `RINGBOARD_NO_DAEMON=1` prevents the GUI from staying resident; window actually closes. Use it for dev iteration.
- `ringboard-iced toggle` re-shows a background instance. Without the toggle arg, a second `cargo run` will likely error because the previous process still holds the window.

## Project structure

```
(workspace root = lint meta-package, publish=false)
cli/            → crate clipboard-history,    binary ringboard
client-sdk/     → crate clipboard-history-client-sdk  (library; features: ui, search, deduplication, watcher-utils)
core/           → crate clipboard-history-core         (library; shared types + ring buffer logic)
egui/           → crate clipboard-history-egui, binary ringboard-egui
iced/           → crate clipboard-history-iced, binary ringboard-iced
server/         → crate clipboard-history-server        (binary ringboard-server)
tui/            → crate clipboard-history-tui,  binary ringboard-tui
wayland/        → crate clipboard-history-wayland
wayland-interface-check/  (compile-time check; no binary)
x11/
```

**Naming note**: crate names are `clipboard-history-*`, binaries are `ringboard-*`.

## Architecture

- **Client-server**: All writes go through the server (Unix domain sockets, custom binary DB using `io_uring` + `mmap`). The server is the single writer.
- **Background controller thread**: The `client-sdk` crate provides `ui_actor::controller()` which runs a separate thread accepting `Command` values and pushing `Message` values back. All GUI clients use this.
- **Paste flow**: `send_paste_buffer()` in `client-sdk/src/api.rs` takes `mime_override: Option<MimeType>` — `Command::Paste` passes `None` (original MIME), `Command::PasteText` passes `Some(text/plain)`.
- **Favorites reorder**: `Command::Swap(id1, id2)` → `SwapRequest::response()` on main server socket → `Message::Swapped`.

## Golden / snapshot files

These are compared in test and must be kept in sync:

- `client-sdk/api.golden` — public API surface of the SDK
- `core/api.golden` — public API surface of the core crate
- `cli/command-reference.golden`, `cli/command-reference-short.golden` — CLI help output

## Fuzz

```sh
cd client-sdk/fuzz && cargo fuzz run <target>
```

## Only one test file

`tests/tidy.rs` (fmt + clippy). Per-crate API golden tests live in `client-sdk/tests/api.rs`, `core/tests/api.rs`, and `cli/src/main.rs` (as `#[test]` modules).
