# Work log

Append-only history of what shipped from `ROADMAP.md`. FOREMAN appends
an entry per landed step or tranche. Newest at the bottom. Corrections
are appended, never edited in; a hook refuses rewrites.

```
## YYYY-MM-DD — <step id and title>

- What shipped
- Commits: <shas>
```

---

## 2026-10-30 — T0 orch set up

- `/orch init`: `.orch.json`, `docs/LOG.md`, `AGENTS.md`, the
  `CLAUDE.md` lines, `.grok/agents/`; `ROADMAP.md` and `tmp/` ignored.

## 2026-10-30 — T1 Workspace skeleton

- Workspace with `telepatch`, `telepatch-macros` (re-exported), `tp`
  (`--help`); edition 2024; smoke test per crate; `just test`/`lint`;
  orch T0 files committed alongside.
- Commits: ff9b1e8, bca4116

## 2026-10-30 — T13 `../tp_basic`: originals in `conversion/`

- 31 byte copies from `../ansible` 79d1ca1 (4 container playbooks, 5
  roles, `group_vars/all`) under `conversion/ansible/`; `SOURCE` with
  repo, commit, sha256 per file; `just check-conversion`.
- Commits: tp_basic ff4e150

## 2026-10-30 — T5 `os` fact; T3 trace core and plan rendering

- `Os { family, distro, version }` from os-release; 13 upstream fixtures
  plus Pop!_OS and an omarchy host; `just os-fixtures`.
- `Node`/`Key`/`Guard`/`RunAs`, `Host<V>` with `as_user` scopes,
  `barrier()`, role boundaries, plan renderer, `testing::trace` harness.
- Commits: 37144b1

## 2026-10-30 — T2 Ansible corpus loader and `tp classify`

- saphyr loader with file:line through roles/includes/blocks; classifier
  for the eight patterns + module category + proposed primitive;
  Markdown `## Current method` section and `--json`; 10 fixtures with
  snapshots; out-of-scope skipped; `just classify-corpus`.
- Commits: 5a2f8af

## 2026-10-30 — T4 `sh!`/`#[role]`; T7 resolver, facts, engine loop

- `sh!` with quoted interpolation, `unless`/`creates`/`changed_if`,
  `.writes()`, `.allow_fail()`; `#[role]` boundaries; trybuild cases.
- `exists`/`bin_exists`/`bin_version` facts; resolver Confirmed →
  Postcondition → Block; engine loop with deferred mutations and
  implicit barriers; fake executor window snapshots.
- Commits: 3155452

## 2026-10-30 — T6 Primitive templates; T14 `dev_base` understanding doc

- T6: per-op subshell with EXIT-trap trailer and typed results; `pkg`
  over apt/dnf/yum/pacman/pip with Ansible's flags; apt cache pass;
  `file` builders; trace-time errors fail before apply.
- T14: `../tp_basic/conversion/dev_base.md` (all levels, dated sources,
  local image probes, agent-alone decision, skill-feedback);
  check-conversion checks headings and source dates.
- Commits: e21c7e1; tp_basic 5e37c21

## 2026-10-30 — T8 optimizer; T9 shell pool and local transport; T15 skill v1

- T8: edges, pkg merge with `hoist_is_safe`, per-host schedule, apt rule
  after merging; `testing::plan`; adversarial snapshots.
- T9: pipelined shell core with trailer splitter; `Transport::open`;
  local bash / `sudo -n bash`; login-env wrapper; op stdin from
  /dev/null; ETXTBSY-safe test helpers.
- T15: `.claude/skills/ansible-port/` (SKILL, UNDERSTAND, PORT,
  template); `tp check-understanding`; `just skill-check`; tp_basic
  check-conversion calls it.
- Commits: f124e39, 0c31c28; tp_basic c3107cc

## 2026-10-30 — T10 `docker exec` transport; live `os`

- `DockerTransport` over the shell core; `read_os`; ignored live test
  over the 13 upstream images; `just test-live-os` green.
- Commits: 14a2ab5

## 2026-10-30 — T11 Executor, prepare and error semantics

- `HostExec` over local/docker shells; prepare (users, package state);
  host-side halt on failure; skipped roles; status table first; live
  stream + JSONL log; build-dep template syntax fix; local e2e and
  `just test-live-pkg` (ubuntu:24.04, rockylinux:10) green.
- Commits: 5772233

## 2026-10-30 — T12 Runner library, testing API, musl build

- `telepatch::cli::main` (list/plan/run, TOML vars, --log);
  embedded os fixtures and `testing::plan_for`; `examples/hello`;
  `just musl-example` (static-pie).
- Commits: 0b0e3cf

## 2026-10-30 — T16 `../tp_basic` cargo workspace and `tp-basic`

- `Vars` + `vars/container.toml`; `tp-basic` via `telepatch::cli::main`;
  `just musl` (static). Path dependency on telepatch until pushed.
- Commits: tp_basic 5a97172

## 2026-10-30 — T17 `../tp_basic`: port `dev_common`, `container/dev_base`

- Flag dispatch as in group_vars, YAML package lists, pip and uv as
  written; plan snapshots on 13 distros; Port notes in `dev_base.md`.
- Commits: tp_basic 708b919

## 2026-10-30 — T22 `tp import`, the emitter

- Emitter over the T2 loader with the mechanical mappings and
  TODO(port) for the rest; per-role report; emitted code for the five
  roles compiles as `crates/tp/tests/emitted`; `just import-diff`.
- Commits: bc200ff

## 2026-10-30 — T23 Skill v2 and blind re-port trial (one item left)

- PORT.md v2 on `tp import`; blind re-port of `rust_kernel` equals the
  hand port's plans on all 13 distros; ten findings folded into PORT.md.
- Left: remove the untracked `../tp_basic/tests/trial/` (recursive
  delete needs human approval in this session).
- Commits: c15b436, 35e2ec8

## 2026-10-30 — T18 docker equivalence harness; `dev_base` equal

- `test-container` (two chains from one pinned base, Ansible Dockerfiles
  unmodified, bind-mounted static tp-basic), `image-state.sh`, self-test.
- `dev_base` identical on ubuntu2404 and rockylinux10 (gating and
  versions) against ansible 79d1ca1; understanding folded into
  `dev_common.rs`; `dev_base.md` and the playbook mirror removed.
- Commits: tp_basic 12bfe98, a7dbfd9

## 2026-10-30 — T19 `kernel_build.yml`; T20 `rust.yml`

- Docs (dated research, image probes), ports with 13-distro plan
  snapshots; `kernel_build` and `rust_build` identical on ubuntu2404 and
  rockylinux10 against ansible 79d1ca1; understanding folded into
  `src_repos.rs`, `kernel_build.rs`, `rust_build.rs`; docs and playbook
  mirrors removed. Playbook renamed `container/rust` (oracle naming).
- Commits: tp_basic 875b5da, 1a5d208, cf00682, 1af5940

## 2026-10-30 — T21 `latest_kernel_build.yml`; goal 1 closes

- `rust_kernel` port (`changed_if` on what rustup prints, `creates` for
  bindgen); `just test-acceptance` green end to end (7/7 identical);
  `conversion/` holds no ported role; `just test-matrix`: 34/40 pass,
  debian13 diff (`sq`, merge alternative, OQ4), fedora43/44 error
  (dnf5 rejects `@Development Tools` by name).
- Commits: tp_basic 875b5da, b84bb7f, 18e28ab

## 2026-10-31 — ansible-port skill v3: understand the task before the method

- Review of the `dev_base` port found the uv installer reproduced as an
  unguarded, unpinned `sh!` because nobody had asked what the task was.
  Skill v3 adds a `## Task intent` level (one row per task: intent,
  shape, adequate, primitive), the `sh!`-is-a-hack rule with
  `// HACK(<primitive>)` markers, primitive gaps and built-primitive
  defects lists, and "build a primitive first" as a normal phase-1
  outcome. `tp classify --intent-skeleton` fills the factual columns;
  `tp check-understanding --playbook` checks the judgment columns and
  the row count. Two blind trials (`dev_base.yml`, `rust.yml`) in fresh
  contexts; 22 skill-feedback items folded into v3.1 and v3.2.
- Trial 2 recommends a `tool` primitive (installer shape: binary,
  installed version, wanted version, source, install vs update) before
  `rust_build` is re-ported; `tool` is not yet in the design note.
- tp_basic 01ae9aa: uv guarded (`HACK(tool)`), rsync once, dnf5 group id
  per distro; 7/7 acceptance identical, second pass CHANGED 0;
  `conversion/rust.md` holds the trial-2 case.
- Commits: 1cf4461, ffd6776, a9bd000, 03c0199

## 2026-10-31 — T30 design note: the source axis on `pkg`

- `docs/execution-model.html` pkg row: sources (Distro default, Pip,
  Cargo, Rustup, Script), one method per source producing one `PkgSpec`,
  `wanted` (Present, `.pin(v)`, `.latest()`), the five questions, latest
  resolved on the host; Facts in bulk: non-distro probes join the batch.
- Commits: 6d7092a

## 2026-10-31 — T31 `Source` and `Wanted` on `PkgSpec`, no behaviour change

- `PkgSpec { source, names, wanted, mode, update_cache }`; `Manager`
  loses Pip, `PkgMode` loses Latest; one `PkgHandle` with `.pin`/`.latest`
  (distro: trace error); Cargo/Rustup/Script "source not built yet";
  merge key adds source and wanted, Script never merges. No snapshot moved.
- Commits: 95c8ec5

## 2026-10-31 — T32 `Script` source; non-distro probes in prepare and the report

- `h.script(name, ScriptSpec{bin, version, latest, install, update})`:
  probe `[ -x bin ]` + version token, install/update with `TP_WANT`,
  re-probe and fail on unmet; `Latest::{Command, GithubRelease}` on the
  host. Writes `*`, post `pkg:script:<user>:<name>` and `file:<bin>`,
  kind `pkg[script]`. Prepare probes in the op's principal, same batch;
  `WillChange` lines and the `will_change` event.
- Commits: 8d61630

## 2026-10-31 — T33 `Cargo` source (per user)

- `h.cargo`/`s.cargo`: `cargo install --list` probe, one install for the
  missing, `.pin` one name (`--version`), `.latest` via `cargo search` on
  the host; keys `pkg:cargo:<user>:<name>`; adjacent ops merge, pinned
  never; report lines join names with equal installed/wanted.
- Commits: eae9f1a

## 2026-10-31 — T34 `Rustup` source

- `h.rustup(tc)` with `.targets/.components/.profile/.set_default/.installer`:
  installer, toolchain install, default, `rustup check`/`update` for
  Latest, then targets and components; keys `pkg:rustup:<user>:<tc>…`.
  "update available" matched in either case (rustup 1.29 lower-cases it).
- Commits: a0e24e5

## 2026-10-31 — T37 skill and `tp classify`: installers are `pkg` sources

- `tool` retired from the skill and the classifier; marker for a missing
  source is `HACK(pkg: no <Source> source yet)`; PORT's built list and
  dev_common sketch use the sources.
- Commits: dd977fc

## 2026-11-01 — T28 `../tp_basic`: base-state capture and `just test-idempotent`

- `test-container.sh` also writes `base.state` of the pinned base (all
  7 acceptance cells have one). `just test-idempotent <distro>
  <dev_base|rust>`: two runs in one container of the pinned base, pass on
  `CHANGED 0`. ubuntu2404 dev_base passes; rust baseline: only
  `rustup update` changes. Acceptance 7/7 identical.
- Commits: tp_basic 2e734f2

## 2026-11-01 — T35 `../tp_basic`: uv as `pkg` with a `Script` source

- dev_common's uv is `h.script("uv", ScriptSpec…)`, wanted Present, the
  YAML's install line; `HACK(tool)` gone. 54 plan snapshots, only the uv
  op block. Acceptance 7/7 identical; dev_base second run `CHANGED 0`,
  first run reports `pkg[script] root uv: absent -> present`.
- Commits: tp_basic e2cd213

## 2026-11-01 — T36 `../tp_basic`: `rust_build` on the Rustup and Cargo sources

- Per user: `s.rustup("stable").targets([musl]).set_default().latest()`,
  `s.cargo(["cargo-deb", "cargo-generate-rpm"])`; repair branch and its
  two fact reads dropped; no `sh!` in the block. `conversion/rust.md`
  folded and removed with the mirrored originals (`conversion/` back to
  its T21 state). 26 plan snapshots, `blocking reads` 2 -> 0.
  Acceptance 7/7 identical; `test-idempotent ubuntu2404 rust` and
  `dev_base` `CHANGED 0`. Goal 4 is done.
- Commits: tp_basic 6929037, 0d0fbc2

## 2026-11-01 — T24 SSH transport: the root shell over one connection

- `SshTransport` (`transport/ssh.rs`): the user's `ssh`, one ControlMaster
  per host kept as a child (private `/tmp/tp-<pid>-<n>` socket dir,
  `ControlPersist=no`), channels on it; `-T -o BatchMode=yes` always; key via
  `-i … -o IdentitiesOnly=yes`, extra `-o`s; root via `id -u`, `sudo -n true`,
  `exec sudo -n bash` (`Shell::exec`); Limits from the master's stderr, the
  ssh text via `message(host)`; `-O exit` and kill on close or drop. Stub
  gate tests; `just test-live-ssh` against an sshd container.
- Commits: b557db0

## 2026-11-01 — T25 SSH transport: several shells, MaxSessions, User shells

- A channel refused for MaxSessions opens one more master; `User(u)` is a
  channel running `sudo -n -iu <u> bash` (`SudoRefused` with sudo's text).
  Channels pass `ProxyCommand=false` so a refused mux session cannot fall
  back to an untracked direct connection (seen live). Live: `MaxSessions 1`
  want 3 got 3 over 3 masters.
- Commits: 85c6032

## 2026-11-01 — T26 Executor: SSH target and per-principal shells

- `Target::Ssh`; root shell plus one shell per user, opened in prepare after
  `getent` and before any op of the window; ops, source probes and fact
  reads go to their principal's shell; a change of principal waits for the
  previous run; a failure stops sends on every shell; user shells, root
  shell, then connections close at host end. Local/docker block a user
  window with the transport's limit. Root-only windows unchanged.
- Commits: ec7775f

## 2026-11-01 — T27 CLI: `run --host` over SSH

- `run <name> (--local | --host <dest>...) [--ssh-key p] [--ssh-option K=V]...`;
  hosts in parallel, dest verbatim as the report name; `run_with_args` takes
  an ssh transport factory for tests.
- Commits: 3f31576

## 2026-11-02 — T29 `../tp_basic`: `just test-vm`, dev_base on a VM over SSH, transport proof

- Gates on run 1 exit 0, run 2 `STATUS ok`/`CHANGED 0`, and the VM gone from
  `vmtool list` after the trap's delete. The oracle comparison is a
  non-gating report (containers and VMs differ in structure); no oracle
  record skips it with a note. Live green; the report shows the known
  `- packages|binfmt-support amd64`.
- Commits: tp_basic 23bc8f3

## 2026-11-02 — T38 `../tp_basic`: `just test-vm-oracle`, Ansible versus telepatch on a VM

- Two fresh VMs, one at a time: the ported Ansible commit (git archive,
  ansible-playbook as bot with become) and `tp-basic run --host`; states
  diffed with test-container.sh's `compare_captures` (byte-identical copy),
  pre-states compared non-gating. VM lifecycle moved to `scripts/lib/vm.sh`,
  sourced by test-vm.sh. On demand. Live: state, versions and pre-states
  identical; `just test-vm` still green after the move.
- Commits: tp_basic 7082914

## 2026-11-03 — `just test-vm-oracle` timed: two passes per side

- Direct change at the user's request (no roadmap step). Each side now
  runs twice on its VM, both passes timed from this machine, the second
  pass's changed count recorded in `timings.txt`; states captured after
  the second pass. Rerun bug fixed (`ansible-src/` from a previous run).
- Live, ubuntu2404 dev_base: ansible 4m04.9s then 7.0s (changed=1: the
  original's unguarded uv installer); telepatch 3m45.6s then 1.3s
  (CHANGED 0). States, versions and pre-states identical.
- Commits: tp_basic 112975d

## 2026-11-03 — T39 `Mise` pkg source, system layer

- `h.mise([..])`: tools into mise's system layer as root, no sudo.
  `/etc/mise/config.toml` entries via `mise config set --system` only when
  they differ (the file is created empty first; mise refuses a missing one
  as untrusted), `mise install --system` then `mise reshim --system`;
  installed versions from the system data dir only. Present / Pinned /
  Latest, root only, keys `pkg:mise:root:<n>`, kind `pkg[mise]`. Present
  probe needs no network (checked with the container's network cut).
  Live `mise_system_layer` (`just test-live-mise`) green.
- Commits: c5be4d5

## 2026-11-03 — T40 `../tp_basic`: oracle intended differences, one `compare_captures`, a mise capture

- `scripts/lib/compare.sh` holds the only `compare_captures`; an optional
  `tests/oracle/intended/<container>.txt` (`+section|line`,
  `-section|line`) is applied to the ansible side before the unchanged
  `diff -u`. `image-state.sh` gains a gated `== mise ==` section.
  `just test-oracle-selftest`. Live: ubuntu2404 dev_base identical with no
  file (run with the pinned `ANSIBLE_SHA`); `just test-vm` still green.
- Commits: tp_basic 7f90178

## 2026-11-03 — T41 `../tp_basic`: `dev_common` installs mise system-wide and `just` through it

- After uv: mise (Script source, mise.run into `/usr/local/bin/mise`),
  `/etc/profile.d/mise.sh` (system shims on login PATH), `h.mise(["just"])`.
  First intended divergence: six lines per container in
  `tests/oracle/intended/`. `test-vm.sh` gates on bot's `bash -lc "just
  --version"` via the shim. Live: `test-acceptance` 7/7 identical (6
  intended differences applied); `test-idempotent ubuntu2404 dev_base`
  CHANGED 0; `test-vm` green (just 1.59.0 as bot); `test-vm-oracle`
  green, second passes ansible 6.7s (changed=1), telepatch 1.3s (0).
  tp_basic's git dependency needs c5be4d5 pushed for a fresh clone.
- Commits: tp_basic bfbaa2a

## 2026-11-04 — Goal 5a housekeeping (direct, at the user's request)

- `pkg[mise]` reshims whenever a wanted tool's installed binary has no
  system shim, not only right after an install, so a run that died between
  install and reshim heals on the rerun; a reshim counts as a change. Stub
  test `mise_reshims_when_an_installed_tool_has_no_shim`; the stub mise now
  materialises install bin dirs and shims. 4 op snapshots and tp_basic's 52
  plan snapshots regenerated (diff: the reshim block only). Live
  `test-live-mise` green.
- tp_basic `just test-container` pins `ported_ansible` like test-acceptance
  (a bare run resolved Ansible master, which already has `just`).
- README timing tables updated to the 2026-11-03 run.
- Commits: telepatch (this), tp_basic (next)

## 2026-11-04 — T42 `../tp_basic`: mirror dev_desktop; dev_desktop and admin_users understanding docs

- Mirror of `playbooks/dev_desktop.yml`, `inventories/group_vars` and the
  nine roles at 79d1ca1 (37 sha256 lines in `conversion/SOURCE`;
  `default_password` is `""`). `dev_desktop.md` (rows nvm_node, rust_tools,
  tmux; tmux keep, nvm_node and rust_tools drop, Node in its own role) and
  `admin_users.md` (keep, port as is). Probes correct the roadmap:
  `@includedir` ships on Ubuntu, Debian and Arch, not Rocky; Ansible leaves a
  new account locked (no `NP`); group `sudo` is missing on Arch and AL2023.
  Not yet reviewed by the user.
- Commits: tp_basic 98098cc

## 2026-11-04 — T43 `../tp_basic`: docker, golang, vm_server understanding docs

- Dated vendor sources and container probes: Docker's deb822/armored-key
  apt method and `docker-ce.repo` (rhel path for Rocky/Alma), daemon start
  per family, podman/runc coexist on Rocky 9/10; go.dev VERSION and dl JSON,
  godoc deprecated for pkgsite; Ubuntu's `qemu-kvm` is virtual
  (`qemu-system-x86`), the `libvirt` group's package per family. Decisions:
  docker rewrite, golang rewrite, vm_server shrink. Not yet reviewed.
- Commits: tp_basic 68abd44

## 2026-11-04 — T44 `Os::codename()`

- `UBUNTU_CODENAME`, else `VERSION_CODENAME`, else `None`; all 15 fixtures
  asserted (Pop!_OS `noble`).
- Commits: 1846b57

## 2026-11-04 — T45 `file(..).validate(cmd)`

- Content and line edits build a candidate beside the target at its final
  mode and owner, run the validator on it (`%s`), and move it only on exit
  0. No `%s` is a trace error. Templates without `.validate` unchanged.
- Commits: 4953049

## 2026-11-04 — T46 `group` primitive

- `h.group(n)` / `.system()`, root only, `getent` then `groupadd`; prepare
  line `group n: will create`. Live `just test-live-user` (ubuntu:24.04,
  rockylinux:10, rerun `changed 0`).
- Commits: 796007d

## 2026-11-04 — T47 `user` primitive

- `h.user(n).shell(..).groups_add([..]).password_hash(..)`: present only,
  root only; shell enforced and groups appended on an existing user; the
  hash applied only on create (`chpasswd -e` on stdin, never when empty)
  and shown as `<hash>` everywhere a reader sees the script. Writes
  `user:n`, reads `group:g`. Live in `just test-live-user`.
- Commits: 8fa92ad

## 2026-11-04 — T48 users the run creates, then `as_user`

- An op addressed to `User(u)` after an op writing `user:u` starts the
  next window, so prepare checks and opens u after u exists; a fact read as
  u waits for the user op. No existing plan moved. Live case in `just
  test-live-ssh`.
- Commits: e7c81cf

## 2026-11-04 — T49 `repo` primitive

- `h.repo(n).apt(AptRepo{..})` (deb822 `.sources`, armored key in
  `/etc/apt/keyrings`), `.dnf_url(url)`, `.dnf(DnfRepo{..})`; fetched files
  only when absent, written files compared byte for byte; `optimize`
  keeps distro `pkg` ops on their side of a repo op; the next apt op
  updates the cache. Live `just test-live-repo` installs `docker-ce-cli`
  from Docker's repo on ubuntu:24.04 and rockylinux:10, rerun `changed 0`.
- Commits: 17c7cc9
