# UI prototype notes

Moved out of `ROADMAP.md` on 2026-11-05 at the user's request. This is the
ARCHITECT plan that was not wanted; it is kept as notes, not as a queue.
Nothing in it was built. The prototype in this directory does not follow it.

## Goal 10 — prototype: a Rust WASM page showing a run in Ghostty (T105–T109)

Asked by the user 2026-11-05: "I want to prototype a ui that is all rust
wasm. with ghostty showing a stream". This is a prototype, not the
product. It is the smallest thing that shows a real telepatch run's
output in a Ghostty terminal in a browser, with the page written in
Rust and compiled to WASM. Done when a `tp-basic run` against a `tp-*`
VM can be watched live in the browser, the same log can be replayed
afterwards, and the user has looked at it (T109). Planned 2026-11-05;
raw findings are in `tmp/agents/architect-20261106T0207Z.md`.

### Goal 10 decisions

- **Ghostty from Rust.** Ghostty's terminal core, `libghostty-vt`, is a
  Zig library with a C API. It is MIT-licensed and pre-1.0, so a minor
  release may break its API. It parses VT bytes and keeps the terminal
  state (screen, scrollback, and a "render state" for custom renderers).
  It draws nothing: the embedder renders. Ghostty builds it as
  `wasm32-freestanding` (Tier 1, CI-tested; about 0.8 MB ReleaseSmall).
  The safe Rust bindings `libghostty-vt` / `libghostty-vt-sys`
  (github.com/Uzaaft/libghostty-rs, MIT) support `wasm32-unknown-unknown`
  on stable Rust. At build time they need Zig 0.17.x and fetch Ghostty's
  source at a pinned commit (`GHOSTTY_SOURCE_DIR` builds offline). The
  embedder must register a secure random source
  (`sys::set_secure_random_source`). The README ports upstream's
  `wasm-vt` demo to Rust, but nobody here has built that path yet.
  `ghostty-web` (coder, MIT, 0.4.x) is an xterm.js-compatible
  **TypeScript** renderer over a patched Ghostty wasm build, so it is
  not Rust.
- **What "all Rust" means here (G10-1).** One wasm module built by
  rustc, with libghostty-vt linked in as a static library. The page's
  Rust code reads Ghostty's render state and draws cells on a Canvas 2D
  through `web-sys`. The only JS is the loader that `wasm-bindgen`
  generates, plus a one-line `<script type="module">` in `index.html`.
  No npm, no bundler, no TypeScript. T106 is a spike that proves the
  link before any page code is written. If it fails, the fallback
  (G10-1) is Ghostty's official `ghostty-vt.wasm` as a second module.
  Its only import is `env.log(ptr, len)` (upstream
  `example/wasm-vt/index.html`). About 20 lines of JS would instantiate
  it, and Rust would drive it through its exported
  `ghostty_wasm_alloc`, `ghostty_terminal_new` and
  `ghostty_terminal_vt_write`, still rendering from Rust.
- **The stream with no daemon.** The engine already writes a JSONL log
  (`run --log`, `executor.rs:42-48`, `Sink` at `:123-153`).
  `../tp_basic/scripts/test-vm.sh:406` already writes
  `$out/run<N>.jsonl` for every VM run. A new binary, `tp-view <log>`,
  serves the page and streams the log over Server-Sent Events (SSE: a
  one-way HTTP stream the browser reads with `EventSource`). It always
  reads from the first line and then follows the file until `run_end`.
  A finished log is a replay, and a growing one is a live tail: one
  code path, as the note asks (`docs/execution-model.html` ~line 287).
  It runs as a separate process next to the run, so the engine and the
  playbook binary gain nothing.
- **What the log lacks (T105, its own step).** `output` is lossy UTF-8
  per chunk (`executor.rs:406-409`), so a multibyte character split
  across two reads is corrupted and non-UTF-8 bytes are lost. There is
  no timestamp, no run id, no format version and no end marker, so a
  tail cannot know the run is over. Shells are pipes, not PTYs
  (`pool/mod.rs:128-134`), so output has bare LF. A terminal needs
  CR+LF, and the page must translate only for non-TTY ops, so
  `op_start` says `tty:false`. `stderr` is already merged
  (`pool/mod.rs:11`). The event types move to a small shared crate:
  the engine writes them and the page reads them, checked at compile
  time on both sides.
- **Crates.** New `crates/telepatch-stream` (event types and line
  decoder; serde, serde_json, base64 only; builds for wasm32). New
  `crates/tp-view` (the server; axum and tokio). New `crates/tp-web`
  (the page; `wasm-bindgen`, `web-sys`, `libghostty-vt`), kept **out of
  the root workspace** (G10-2) so `cargo test --workspace` needs no
  Zig. `telepatch` gains only a path dependency on `telepatch-stream`,
  whose external dependencies it already has (`crates/telepatch/Cargo.toml`).
  No HTTP, TLS or WebSocket dependency reaches the engine.
- **UI approach.** Plain `wasm-bindgen` + `web-sys`, no framework. The
  page is a host list, an op list and one canvas per op. Leptos, Yew
  and Dioxus add a reactive runtime this page does not need, and they
  add trunk. The build is `cargo build --target wasm32-unknown-unknown`
  then `wasm-bindgen --target web` (wasm-bindgen-cli pinned to the
  crate's exact version), driven by a `just` recipe.
- **Queued steps that change the event format.** T98 (`--check`, diff
  output, exit codes), T101 (timeouts reported as such), T102 (the
  reboot barrier, reconnect, `reboot pending`), and T72/T73 if warnings
  become events. None is edited here. Once T105 lands, the executor's
  `Sink` takes only `telepatch_stream::Event`. A new event then *must*
  be added to that enum, so those steps add it there as a matter of
  compiling. The format is v1. Additive changes (a new event or field)
  keep v1. The decoder keeps an unknown event as `Event::Unknown { ev,
  raw }`, which the page shows as a raw line in the host pane. Only a
  breaking change bumps `v`, and `tp-view` refuses a `v` it does not
  know. Order relative to Goals 7–9: question G10-4.
- **Secrets.** The stream is the log, byte for byte. The page and
  `tp-view` add nothing to it. Keeping secrets out of the log is T88's
  job (`<secret>` in events), and that rule then covers the page.
  `tp-view` binds 127.0.0.1 only. Fixtures come from test playbooks
  that use no `Secret`. The proof runs `dev_base`, which has no
  password var (`dev_desktop` does). Raw `output` bytes are what the
  host printed, the same as stdout today.
- **Where the prototype leaves room (not planned).** A daemon would add
  a socket target to `Sink` and serve the same SSE stream, and the page
  would not change. A PTY shell sets `tty:true` and the page stops
  translating LF. Stdin, keyboard input, launching runs and auth would
  be new endpoints on `tp-view` and new events. Ghostty's key encoder
  is already in the bindings. Op types stay off the wire: events carry
  `desc` and `role` strings, as today.

### Goal 10 order and gates

T105 → T106 → T107 → T108 → T109. T106 and T107 are independent after
T105. The gates are **E** (above) and **W** = `just web-test && just
web-build`. W runs in `crates/tp-web`: its native `cargo test`, which
links libghostty-vt for the host and so needs Zig 0.17.x, then the
wasm build. W is local (a unit test and a build). Nothing in Goal 10
starts a browser (G10-3). The only live gate is T109's **[VM]**.

### T105 — Stream log v1: typed events, raw bytes, timestamps, run id [todo]

**Touches:** new `crates/telepatch-stream/{Cargo.toml,src/lib.rs,tests/}`;
root `Cargo.toml` (member, workspace dep); `crates/telepatch/Cargo.toml`
(path dep); `crates/telepatch/src/executor.rs` (`Sink::event` takes
`telepatch_stream::Event`; every `json!` emission at `:403`, `:408`,
`:457`, `:634`, `:648`, `:691`, `:704`, `:709`, `:903`, `:907`, `:946`;
the module doc at `:42-48`); `crates/telepatch/tests/executor_local.rs`
(the kinds test at `:300`); new `crates/telepatch/tests/stream_log.rs`;
`docs/execution-model.html` (a short "Stream log" paragraph by the
stream bullet; not T71's decided-API rows); `justfile` (`wasm-check`).
**Must:**
- `telepatch-stream` depends on serde, serde_json and base64 only, and
  `just wasm-check` (`cargo check -p telepatch-stream --target
  wasm32-unknown-unknown`) passes. `just lint` runs it.
- `Event` is a serde enum tagged `ev`. Every kind and field written
  today is kept (`host_open`, `window`, `op_start`, `output`,
  `op_result`, `facts`, `host_status`, `will_install`, `will_create`,
  `will_change`). Decoding an unknown `ev` gives `Unknown { ev, raw }`,
  not an error. `parse_line(&str)` returns a `Record { t_ms, event }`.
- The first line is `{"ev":"run","v":1,"run":"<32 hex>","playbook":..,
  "started_unix_ms":..,"t":0}`. The run id is random per run. The last
  line is `run_end` with the exit code and each host's status. Every
  line carries `t`: milliseconds since the run started, from a
  monotonic clock.
- `output` carries `b64`, the exact bytes of the chunk, and no `data`.
  `op_start` adds `tty:false` and the principal's user name.
- `tests/stream_log.rs`: a local run of a test playbook whose `sh!` op
  prints an SGR colour, a `\r` progress line, a byte `0xff` and a
  multibyte character written in two halves. The decoded `output`
  bytes, concatenated per op, equal what the op printed, byte for byte.
  `t` never decreases. There is exactly one `run`, first, and one
  `run_end`, last. A second run gets a different run id. A user op's
  password hash still shows as `<hash>` in its events.
- A checked-in fixture `crates/telepatch-stream/tests/fixtures/run-v1.jsonl`
  is written by that test under `UPDATE_STREAM_FIXTURE=1` and decoded
  by a `telepatch-stream` test (format stability). It contains no home
  path and no hostname but `local`.
**Gate:** E, plus `just wasm-check`; in `../tp_basic`, `cargo test`
(snapshots unchanged: the plan output is untouched).
**Depends on:** none (see G10-4 for its place in the queue).

### T106 — Ghostty in a Rust wasm module: link spike and terminal wrapper [todo]

**Touches:** new `crates/tp-web/{Cargo.toml,Cargo.lock,src/lib.rs,src/term.rs,tests/}`
(its own `[workspace]`; listed under the root `Cargo.toml`'s
`exclude`); `justfile` (`web-test`, `web-build`, and a check that
`zig version` is 0.17.x with an install hint when it is not).
**Must:**
- `libghostty-vt` pinned to an exact version, and through it a fixed
  Ghostty commit. Prefer the render-state-only feature set if the
  bindings expose it.
- `term::Term` is a thin wrapper with `new(cols, rows)`, `write(&[u8])`,
  and `cells()` over the render state (rows of cells with char, fg, bg,
  bold).
- Native tests feed SGR red, then `\r` overwrites, a cursor-up, and a
  multibyte character split across two `write`s. They assert the plain
  text and the red cell's fg colour.
- `just web-build` compiles `tp-web` for `wasm32-unknown-unknown`, runs
  wasm-bindgen, and prints the `.wasm` size. The secure random source
  is registered from `crypto.getRandomValues`.
- If the wasm link or instantiation fails, the step stops. It records
  the exact error in its report and does not switch to the fallback on
  its own (G10-1).
**Gate:** W.
**Depends on:** T105 (the crate layout and the root `exclude`; no code
dependency).

### T107 — `tp-view`: serve the page and stream the log [todo]

**Touches:** new `crates/tp-view/{Cargo.toml,src/main.rs,tests/}`; root
`Cargo.toml` (member).
**Must:**
- `tp-view <log.jsonl> [--assets <dir>] [--port <n>]` listens on
  127.0.0.1 only; there is no flag to change the address. `--assets`
  defaults to `target/tp-web/`. A missing assets dir is an error that
  names `just web-build`.
- `GET /` and asset paths serve files from the assets dir, with the
  wasm MIME type for `.wasm`. A path that escapes the dir is a 404.
- `GET /events` is SSE: one `data:` per log line, from the first line.
  It then follows the file (poll about 100 ms), holds a partial last
  line until its newline, and ends the stream after `run_end`. It waits
  for a log that does not exist yet. Each line goes through
  `telepatch_stream::parse_line`: a bad line becomes an `event: error`
  and the stream goes on. A `run` line with an unknown `v` ends the
  stream with an error.
- Tests (tokio, temp dir, raw HTTP over `TcpStream`, no client crate):
  a log appended in three parts arrives complete and in order, then
  the stream closes after `run_end`. A log created after the request
  still arrives. Traversal gets a 404. Binding is checked to be
  loopback.
**Gate:** E.
**Depends on:** T105.

### T108 — The page: a Ghostty terminal per op, live and replay [todo]

**Touches:** `crates/tp-web/src/{lib.rs,model.rs,view.rs,render.rs}`,
`crates/tp-web/index.html`, `crates/tp-web/tests/`; `justfile`
(`web-build` writes `target/tp-web/`: `index.html`, the wasm and its
loader).
**Must:**
- `model` is native and has no web-sys. It routes records to host →
  op. It creates an op's `Term` at its first output (120 columns; rows
  grow with content up to 40, latest screen shown). It translates LF
  to CRLF when `tty` is false. It keeps the header `[role] #id desc`
  and the result from `op_result` (changed, ok, FAILED rc). It keeps a
  host pane of `window`, `will_*`, `host_status` and `Unknown` lines.
  `?speed=N` paces by `t` (default: as fast as it arrives).
- `view`/`render` (wasm only) open an `EventSource("/events")` and
  feed `model`. They draw each op's cells on its own canvas (Canvas 2D,
  monospace, fg/bg/bold). There is no keyboard input. The only JS is
  the wasm-bindgen loader and the `<script type="module">` line in
  `index.html`.
- Native tests feed the T105 fixture through `model`. They check the
  op list, results and host statuses match the fixture, and each op's
  plain-text screen equals its output with CRLF applied. An `Unknown`
  event shows up in the host pane.
**Gate:** W; and E (unchanged).
**Depends on:** T106, T107.

### T109 — Proof on a real run: live and replay [todo]

**Touches:** `README.md` (a "Watching a run" section: `just web-build`,
`tp-view <log>`, open `http://127.0.0.1:<port>/`); `justfile` (`view
log` recipe); `../tp_basic` only for its telepatch dependency, rebuilt
with `just musl`.
**Must:**
- `tp-view` is started on the path `test-vm.sh` will write
  (`../tp_basic/scripts/test-vm.sh:406`, `run1.jsonl`) before `just
  test-vm ubuntu2404 dev_base` starts. `curl -N /events` during the run
  shows lines arriving while ops run, not all at the end, and the
  stream closes at `run_end`.
- Afterwards, `tp-view` on the finished `run1.jsonl` replays it. The
  replayed line count equals the file's line count, and every
  `op_result` id has an `op_start`.
- The VM is deleted and `vmtool list` shows no `tp-*` VM.
- The user opens the page during a replay (and, if he likes, a live
  run) and says it shows the stream (G10-3). The step's report records
  his words.
**Gate:** E and W; **[VM]** `just test-vm ubuntu2404 dev_base` (1 GiB,
one VM, deleted).
**Depends on:** T108.

### Goal 10 out of scope

The daemon; the PTY shell; stdin from the page to an op; launching runs
from the page; auth; op types as a wire format; scrollback, selection,
search and copy in the page; a headless-browser test (unless G10-3 goes
the other way); ghostty-web, xterm.js, npm or a bundler; any change to
the Goal 7–9 steps.

### Goal 10 questions (open, 2026-11-05)

- **G10-1 Ghostty integration.** Default: link libghostty-vt into the
  Rust wasm module through the `libghostty-vt` crate, and draw from Rust
  (the only JS is wasm-bindgen's loader). If T106's spike fails, the
  fallback is Ghostty's official `ghostty-vt.wasm` as a second module,
  with about 20 lines of JS to load it and Rust still drawing. We do
  not use ghostty-web, because its renderer is TypeScript.
- **G10-2 Zig and the gate.** Default: `crates/tp-web` stays out of the
  root workspace, so `cargo test --workspace` never needs Zig. The page
  has its own local gate W, which needs Zig 0.17.x (via mise) and
  network for the first Ghostty source fetch.
- **G10-3 Browser check.** Default: no headless browser anywhere. The
  model and the terminal are tested natively, and the user looks at the
  page in his own browser for T109. The alternative is headless
  Chromium in a 1 GiB `tp-*` VM, which is likely tight on memory.
- **G10-4 Place in the queue.** Default: build T105–T109 next, ahead of
  the open Goal 7 step, because they touch only `executor.rs`'s `Sink`
  and new crates. T98, T101 and T102 would then add their events as
  typed `telepatch_stream::Event`s. The alternative is to build them
  after T104.
