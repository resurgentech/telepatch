# ui-prototype

A web app written in Rust with Dioxus and compiled to WASM, with menus,
text and a shell in a frame that can go full screen, served by a Rust
backend. Standalone: its own Cargo workspace, no code shared with the
telepatch crates. Its text is the repo's `../docs` folder, read when a
page is opened.

    just setup   # once: wasm32 target, the Dioxus CLI (dx) into .tools/
    just run     # builds the page with dx, serves http://127.0.0.1:7681/
    just test

- `server/`: axum.
  - `main.rs` serves the bundle `dx build` leaves under `target/dx/` and
    gives each WebSocket (`/ws`) its own
    `$SHELL` on a PTY. PTY output goes out as binary frames; binary frames
    coming in are written to the PTY; a text frame `"<cols> <rows>"`
    resizes it. Listens on 127.0.0.1 only and refuses a WebSocket opened
    by a page it did not serve. The shell runs as you.
  - `content.rs` turns `../docs` into menus (`/content/menus`, JSON) and
    pages (`/content/page/<id>`, HTML): the design note cut at each
    `<h2>`, `LOG.md`, and each file in `reports/`. The note's stylesheet
    is served as `/content/style.css`.
- `web/`: the page, a Dioxus app. Dioxus is pinned to `0.8.0-alpha.3`,
  a pre-release: the `dx` in `.tools/` must be the same version, and the
  next alpha may need code changes.
  - `main.rs` launches it with the router's routes kept in the URL hash,
    so a link inside a page's HTML does not reload the document (which
    would start a new shell).
  - `app.rs` has the routes, the menu bar and the content page.
  - `terminal.rs` has the frame as a component (size label, full-screen
    button, key and resize handlers) and the shell inside it: socket,
    terminal, canvas drawing. The grid is as many cells as fit the frame.
  - `term.rs` turns bytes into cells; `keys.rs` maps keys to bytes.
  - `assets/app.css` is the app's own styles. There is no hand-written
    JavaScript or HTML; `dx` generates the loader and `index.html`.

The page loads the design note's fonts from Google Fonts, as the note
itself does. Keys go to the shell while its frame has the focus (click
it). In full screen the browser takes Escape to leave; on Chromium the
page asks for the key, so a press reaches the shell and holding it
leaves. No scrollback, selection, paste or mouse in the terminal.

## Ghostty

Not in yet. `web/src/term.rs` uses the pure-Rust `vt100` crate as a
stand-in. The Ghostty Rust bindings (`libghostty-vt`) build for
wasm32-unknown-unknown only from the unreleased main branch of
https://github.com/Uzaaft/libghostty-rs (0.2.2 on crates.io stops with
"unsupported Rust target for vendored build"), and need Zig 0.17.x, which
fetches and compiles Ghostty's source at build time. The swap is
`term.rs` plus the dependency line in `web/Cargo.toml`.

`notes.md` is the earlier, larger plan that was moved out of `ROADMAP.md`.
