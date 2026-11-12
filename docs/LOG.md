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

## 2026-11-04 — T50 desktop vars, `dev_desktop` playbook, `admin_users` role (tp_basic)

- `Vars.default_password` (serde default ""); `vars/desktop.toml`
  (`admin_users = ["bot","tpdev"]`); `dev_desktop` (admin_users,
  dev_common, rust_build) and `dev_desktop_kept` registered. `admin_users`
  ports the intent: `^[#@]includedir` → `@includedir` (bug 1), the
  distro's own `secure_path` with `/usr/local` first (bug 2), `wheel`
  where no `sudo` group exists (bug 4), every sudoers write `visudo -cf`.
  13 `dev_desktop` snapshots; 52 container snapshots unchanged.
- Commits: tp_basic 0b09a8a (docs accepted), 37af8c8; telepatch f100c1a
  (skill: port the intent).

## 2026-11-04 — T51 tmux and Node roles (tp_basic)

- `node`: `h.mise(["node"])` where nvm_node ran (nvm_node dropped; merged
  by the optimizer into dev_common's mise op). `tmux`: `tmux` + `tmuxp` on
  the Debian family and Arch, `tmux` alone elsewhere. `dev_desktop_kept` =
  admin_users + tmux; 13 + 13 snapshots.
- Commits: tp_basic 42e6705

## 2026-11-04 — T52 `docker` role on Docker's repository (tp_basic)

- `repo` + `docker-ce`, `docker-ce-cli`, `containerd.io`,
  `docker-buildx-plugin`, `docker-compose-plugin` on Ubuntu/Pop!_OS
  (`ubuntu`), Debian (`debian`, suite = codename), Fedora (`fedora`),
  Rocky/Alma (`rhel`); distro `docker` on Arch/Omarchy and Amazon; any
  other distro a trace error. `HACK(svc)` `systemctl enable --now docker`
  (unless active) on Fedora, EL and Arch. `group docker`, each admin user
  appended. No `get.docker.com`.
- Commits: tp_basic a2794fa

## 2026-11-04 — T53 `golang` as a `Script` source (tp_basic)

- `h.script("go", ..).latest()`: latest from `go.dev/VERSION?m=text` on
  the host; INSTALL maps `uname -m`, fetches the tarball and
  `?mode=json`, checks sha256 (awk, no jq) before replacing
  `/usr/local/go`. `/etc/profile.d/go.sh` appends `/usr/local/go/bin`.
  godoc, GOPATH dropped. Shell tests against a fake curl; docker check on
  ubuntu:24.04: run 1 changed 2, run 2 changed 0 (go1.27.3).
- Commits: tp_basic 18f3d6f

## 2026-11-04 — T54 `vm_server` shrunk to KVM/libvirt (tp_basic)

- Distro KVM/libvirt/virt-install/bridge-utils per family
  (`qemu-system-x86` on the Debian family, not the virtual `qemu-kvm`);
  `HACK(svc)` socket start on Amazon (`virtqemud.socket`) and Arch
  (`libvirtd.socket`); each admin user appended to `libvirt`. Vagrant,
  `ansible`, `ruby*` dropped. Playbook in YAML order.
- Commits: tp_basic 2a3185a

## 2026-11-04 — T55 `just test-vm ubuntu2404 dev_desktop` (tp_basic)

- `test-vm.sh <distro> <playbook>` (allowlist dev_base, dev_desktop);
  `vm_up` takes memory and disk; per-user working checks for bot and
  tpdev. Live: dev_desktop run 1 865s, run 2 9s `CHANGED 0`, 16/16 checks
  (go1.27.3, node v26.13.0, just 1.59.0, tmux 3.4, virsh 10.0.0,
  hello-world); dev_base rerun `CHANGED 0`. No `tp-*` VM left. Finding:
  root shell `sudo -n bash` lacks `-H`, so with the drop-in's
  `!env_reset` root's mise writes bot's `~/.cache/mise` (not fixed).
- Commits: tp_basic 979ef3f

## 2026-11-04 — T56 VM oracle for the kept roles, `dev_desktop_kept` (tp_basic)

- `test-vm-oracle.sh ubuntu2404 dev_desktop_kept`: a two-role playbook
  (admin_users, tmux) written into the script's own ansible export;
  `image-state.sh --users` adds account and sudoers sections only when
  asked (docker captures byte-identical). Intended: admin_users bugs 1
  and 2 (3 sudoers lines). Live: identical (3 intended differences
  applied); dev_base oracle still identical (6). No `tp-*` VM left.
- Commits: tp_basic a11010c

## 2026-11-04 — T57 conversion cleanup and the docker oracle rerun (tp_basic)

- Kept facts moved into the role comments; the five understanding docs
  and every mirrored original removed; `conversion/SOURCE` is header
  only (`check-conversion` ok, empty list). `just test-acceptance` 7/7
  identical (6 intended differences), 85m. Goal 5b's steps are done.
- Commits: tp_basic da57d5a

## 2026-11-04 — Root shell opens with `sudo -n -H` (telepatch, tp_basic)

- T55's finding fixed: the root shell is `sudo -n -H bash` on SSH and
  locally, so `HOME` is root's even under the admin drop-in's
  `!env_reset`; before, root's mise wrote `/home/bot/.cache/mise`. Stub
  sudos in three test suites demand `-H` and set `HOME=/root`; the
  root-shell tests assert the argv and `$HOME`. `test-vm.sh` gates on no
  mise warning and no foreign-owned file under each admin home. Live
  `just test-vm ubuntu2404 dev_desktop`: run 2 `CHANGED 0`, 20/20
  checks, no `tp-*` VM left.
- Commits: see git log (telepatch, tp_basic)

## 2026-11-04 — T58 `svc` primitive (telepatch)

- `h.svc(unit).enabled().started()` (`ops/svc.rs`): is-enabled/enable,
  is-active/start with daemon-reload when `NeedDaemonReload` says yes;
  key `svc:<unit>` (`.service` appended), root only, nothing wanted is a
  trace error. Without a running systemd the enable is applied offline
  (plain `systemctl enable` works, probed ubuntu:24.04 and rockylinux:10)
  and the start skipped; no `systemctl` skips the op; a skip is never a
  change. Prepare probe after the repo probe: `will enable, will start`,
  `will enable; no running systemd: start skipped`, `no systemctl:
  skipped`. 21 PATH-fake tests; `just test-live-svc` 2/2 (run 2
  `CHANGED 0`); tp_basic's 78 plan snapshots byte for byte.
- Commits: telepatch (this commit)

## 2026-11-04 — T62 intended differences scoped to distros (tp_basic)

- `[distro,distro]+section|line`: applied only on the listed cells,
  ignored elsewhere, malformed scope fails with file:line; unscoped
  lines unchanged. 12 new self-tests; `just test-oracle-selftest` 57 ok.
- Commits: tp_basic 8bef5fe

## 2026-11-04 — T60 `svc` in the docker and vm_server roles (tp_basic)

- docker: `h.svc("docker").enabled().started()` on every family;
  vm_server: one socket per kit (`libvirtd.socket` Debian/Arch,
  `virtqemud.socket` RedHat/Amazon). No `HACK(svc)` left; the `sh`
  barriers go, so on Fedora/EL and Arch the libvirt packages merge into
  docker's pkg op. 13 `dev_desktop` snapshots move; 65 others byte for
  byte.
- Commits: tp_basic efe666b

## 2026-11-04 — T61 svc proofs: Ubuntu VM and init-less containers (tp_basic, telepatch)

- `desktop_services` playbook (curl, ca-certificates, docker, vm_server;
  13 snapshots) and `just test-services <distro>` (plain container, two
  runs). ubuntu2404 and rockylinux10: run 2 `CHANGED 0`, docker and the
  libvirt socket `enabled`; rockylinux10's svc docker enabled offline.
- `just test-vm ubuntu2404 dev_desktop`: run 1 ok; `disable --now
  docker`; run 2 `CHANGED 1`, the svc op (`will enable, will start`);
  run 3 `CHANGED 0`; 24 checks incl. is-enabled/is-active of docker and
  libvirtd.socket; no `tp-*` VM left.
- Engine fixes found by the runs: prepare no longer promises `no
  systemctl: skipped`, or `will enable` for a not-found unit, when a pkg,
  sh or file op precedes the window's svc ops (`…yet: decided at run
  time, after this window's earlier ops`).
- Commits: tp_basic 4f71d29; telepatch 9ad103c, b451246

## 2026-11-04 — T63 dev_common's bugs of intent (tp_basic)

- pip asks for `PyYAML`, not `pyaml`, on the RedHat family and Amazon:
  declared `-pip|pyaml` and `-/usr/local/bin|pyaml` (its console script),
  scoped to those seven distros. uv's installer runs with
  `UV_NO_MODIFY_PATH=1`; not oracle-visible (the installer already skips
  `env`/rc edits when the install dir is on PATH, as in the image
  build), so the planned `-env` lines were dropped. Live `test-container
  rockylinux10 dev_base` identical (8 intended), `ubuntu2404 dev_base`
  identical (6 intended). 65 snapshots move (uv line; pip op on 35).
- Commits: tp_basic 21aef75

## 2026-11-04 — T64 rust_kernel's libclang on every family (tp_basic)

- `clang-devel` on the RedHat family and Amazon (probe: only
  `libclang.so.<N>` from `clang-libs` there; Arch's `clang-libs` ships
  `libclang.so`). Not oracle-visible. 7 `latest_kernel_build` snapshots
  gain the op.
- Commits: tp_basic bfaa4f9

## 2026-11-04 — T65 Intended differences and Open choices (tp_basic)

- Comments only: the "After equivalence" lists in dev_common,
  rust_build, kernel_build, rust_kernel (and src_repos' one line) become
  "Intended differences" (T63/T64's fixes, bug and oracle lines) and
  "Open choices" (the G6-2 rows, accepted defaults, oracle impact); dnf
  group and `which` items removed with a reason.
- Commits: tp_basic ffe725b

## 2026-11-04 — T66 docker oracle rerun with the declared fixes (tp_basic)

- tp_basic bfaa4f9: `just test-acceptance` 7/7 identical (ubuntu cells 6
  intended differences, rockylinux10 cells 8: mise plus `-pip|pyaml`,
  `-/usr/local/bin|pyaml`); `test-idempotent ubuntu2404 dev_base` and
  `rust` `CHANGED 0`.
- Commits: none (outputs under build/)

## 2026-11-04 — T67 the oracle's Ansible side from a pinned docker_images commit (tp_basic)

- `ported_docker_images := 5df316b` passed as `DI_SHA`; one `git
  archive` export per run into `build/docker_images/<sha>/` (atomic,
  marker, reused); test-container, test-matrix, test-idempotent and
  test-services read the hcl, `Dockerfiles/ansible.*` and context from
  it; a non-commit is exit 2. Live `test-container ubuntu2404 dev_base`
  identical (6 intended).
- Commits: tp_basic 3328f2d

## 2026-11-04 — T68 a tp-basic dist with provenance (tp_basic)

- `just dist`: build/dist/{tp-basic, vars/container.toml, PROVENANCE}
  (tp_basic_url, tp_basic_hash, telepatch_hash with `_dirty`,
  tp_basic_sha256, rustc); the recipe prints the file and fails on a
  missing field, junk line or home path (`--self-test` 9 ok).
- Commits: tp_basic 1d41d92

## 2026-11-04 — T69 docker_images builds with telepatch (docker_images)

- `Dockerfiles/telepatch.{debian,redhat,arch}` (tp-basic from the bake
  named context `tp` = tp_basic's `build/dist`, `TP_DIST` overrides; the
  history step reads the dist's PROVENANCE); hcl `family`, tp playbook
  names, no ansible variables; `ansible.*` deleted (5df316b is the last
  Ansible-based commit). `check-dist.sh` (17 ok); push refuses a missing
  or `_dirty` dist before anything (`bake.sh --check-push`, first line
  of `just push`). bake needs `--allow fs.read=$TP_DIST`.
- `just check` no `ansible`; `just build ubuntu2404` and `just build
  dev_base-arch` ok; the dev_base history names tp_basic 1d41d92,
  telepatch 0fc183e and the binary's sha256, as PROVENANCE. Not pushed.
- Commits: docker_images bc767fb, 174b5b9

## 2026-11-04 — T70 the oracle's telepatch side on docker_images' Dockerfiles (tp_basic)

- `scripts/test-container.sh` builds the telepatch side from
  `../docker_images/Dockerfiles/telepatch.<family>` (working tree) with
  `--build-context tp=build/dist`; `scripts/lib/dist-input.sh`
  (`dist_require`, `preamble_check`); test-idempotent and test-services
  run from `build/dist` and check their preamble against the shipped
  Dockerfile; tp_basic's own `Dockerfiles/` deleted.
- `just test-idempotent ubuntu2404 dev_base` `CHANGED 0`; `just
  test-acceptance` 7/7 identical (ubuntu cells 6 intended differences,
  rockylinux10 cells 8), run with a `_dirty` dist built before the
  commit. The Ansible side came from the pinned export.
- Entry appended by XO in review: a20b1d6's message names T70 but
  carried only the T69 entry.
- Commits: tp_basic 52013f1

## 2026-11-04 — `just` on the image PATH (docker_images, tp_basic)

- The telepatch-built images had `just` only on login shells' PATH (mise's
  shims through `/etc/profile.d/mise.sh`); `docker run IMAGE just` failed
  (`just: not found`). `Dockerfiles/telepatch.*` now set `ENV PATH` to
  Docker's default with `/usr/local/share/mise/shims` first, a fixed
  value so layers do not stack it. Built by XO on the user's "fix just".
- `ORG=tp-fix just build` of `rust_build-ubuntu2404`,
  `dev_base-rockylinux10`, `dev_base-arch`: `docker run IMAGE just
  --version` prints `just 1.59.0` in all four images, also as uid 1000.
  `just test-container` ubuntu2404 and rockylinux10 `dev_base` still
  identical. A uid with no writable home gets a mise cache warning on
  stderr; the README says so.
- Commits: docker_images fdf38ca, tp_basic a456c04

## 2026-11-06 — T70a docker workloads run in a disposable tp-* VM (tp_basic, telepatch, docker_images)

- `scripts/docker-vm.sh run [--disk N] [--keep] -- <cmd>` (tp_basic): a
  1 GiB ubuntu2404 VM with docker.io and docker-buildx, its daemon
  labelled `tp-docker-vm=<name>`, the socket forwarded over ssh with
  vmtool's key in place; the VM deleted and checked gone afterwards.
  `just docker-vm <recipe>` in all three repos.
- `scripts/lib/docker-guard.sh` fails closed (exit 2) unless that label
  is on the daemon `DOCKER_HOST` reaches; first line of every docker
  recipe in the three repos and in the scripts; `require_docker_vm()` in
  telepatch's live tests. Its `--self-test` (stub docker) runs all 23
  recipes and scripts: exit 2, no docker call.
- No local bind mounts: `image-state.sh` on stdin, the dist by `docker
  cp`; bases built in the VM from the pinned export with bake (`di_base`).
  sshd publishes on `$TP_DOCKER_VM_IP`. A non-ignored live test that ran
  `docker exec` on this machine now uses a stub. `vm_up` survives
  vmtool's disk-grow race with sshd.
- `just docker-vm test-container ubuntu2404 dev_base` identical (6
  intended differences); `just docker-vm test-live-ssh` 5/5; `just
  docker-vm test-live-os` 11/13: arch and omarchy fixtures predate
  `archlinux:latest`'s current build date (not refreshed: it moves 8
  tp_basic snapshots).
- Commits: tp_basic c0d741a, telepatch 91a63cb, docker_images c65be41

## 2026-11-06 — T70b every VM at 1 GiB, with swap (tp_basic)

- `vm_up` creates every tp-* VM with 1 GiB and refuses any other size;
  a 4 GiB `/tp-swap` with `swapon` only; default disk = image + swap;
  peak memory and swap printed before each delete; on a failed run an
  `Out of memory`/`oom-kill` kernel line is printed with the killed
  process and the step, kept in `build/`, exit 3.
- `just test-vm ubuntu2404 dev_base` PASS (CHANGED 0; peak 387 MiB,
  swap 24 MiB); `just test-vm ubuntu2404 dev_desktop` PASS (run 3
  CHANGED 0, working checks; peak 669 MiB, swap 488 MiB; run 1 1203 s).
- Commits: tp_basic 563f9a8

## 2026-11-06 — T71 the design note states the decided API (telepatch)

- `docs/execution-model.html`: "The decided API", the 24 entries with a
  built/designed status each; the held and unruled list in one place;
  a Removed list; the contradicting sections rewritten. README's table
  matches. `tests/note.rs` guards removed forms, headings and statuses.
- Commits: telepatch f1af534

## 2026-11-06 — T72 trace errors with a location; policy errors (telepatch)

- `TraceError` carries message, role chain and caller `file:line`
  (`#[track_caller]` through every op builder) and prints the decided
  form. `Policy { UnitViaFile, UnguardedRestart, Unverified }`,
  `.allow(Policy, reason)` (empty reason a hard error); `UnitViaFile`
  checked below `/etc/systemd/system`; allowances in the plan and the
  run report. No tp_basic snapshot moved.
- Commits: telepatch 9dddbb6

## 2026-11-06 — T73 sh! is a reported warning (telepatch, tp_basic)

- Each `sh!` records its site; plan and run report end with `warning:
  sh! in a role body at <file>:<line>` per site; exit status unchanged.
  tp_basic `tests/hacks.rs` requires a `// HACK(<primitive>): …` line
  above every `sh!`; the three sites carry one. 17 tp_basic snapshots
  (kernel_build debian family, latest_kernel_build) and 7 telepatch
  test snapshots gain warning lines, additions only.
- Commits: telepatch 3893696, tp_basic 13d6828

## 2026-11-06 — T70c a rolling distro's version is `rolling` (telepatch, tp_basic)

- `Os::parse`: `BUILD_ID=rolling` makes the version `rolling` whatever
  `VERSION_ID` says; other os-release unchanged. The 14 arch/omarchy
  tp_basic snapshots move only their header line. `just docker-vm
  test-live-os` 13/13.
- Commits: telepatch cc4ce96, tp_basic 9ae5225

## 2026-11-06 — T70d test-acceptance prunes each cell's images in the VM (tp_basic)

- `test-container.sh --prune` (passed by `test-acceptance`) removes
  exactly the images a cell built after its compare, guard checked
  again first, bases kept, `docker builder prune -f`; disk before and
  after printed. Guard self-test drops the docker_images cases (XO
  amendment: the user reverted that guard) and adds `test-live-unit`.
  `just docker-vm test-acceptance` 7/7 identical in one 60 GB VM.
- Commits: tp_basic da91c38

## 2026-11-06 — T74 guards and change triggers (telepatch)

- `.changed()`/`.ok()` on every handle; `Guard` gains `Or`/`And` with
  `| & !`. The executor evaluates guards in the shell (variables for a
  source in the same run, constants for one already reported); a false
  guard is `Outcome::Skipped`. Equal guarded ops merge with guards
  or-joined; merged pkg member rule; `.at_end()` window.
- Commits: telepatch e10176d

## 2026-11-06 — T75 service states (telepatch)

- svc `.disabled()`, `.masked()`, `.stopped()` (exclusive sets); one
  `systemctl show` probe per window with an offline fallback; offline
  enable/disable/mask, start/stop skipped; was_enabled/was_active/
  was_masked in the result. `just docker-vm test-live-svc` 4/4.
- Commits: telepatch 5ab910f

## 2026-11-06 — T76 restart and reload carry a guard (telepatch)

- `.restart()`, `.reload()`, `.restart_on(&h)`; unguarded is
  `UnguardedRestart`, allowed prints `will restart, every run; allowed
  unguarded-restart: <reason>`; guarded restarts merge or-joined; a
  restart of a unit the run starts is skipped; `will restart if pkg
  <name> changes`; a second run is CHANGED 0.
- Commits: telepatch b8c3910

## 2026-11-06 — T77 h.unit: unit files and drop-ins (telepatch)

- `h.unit(n).content(..).mode(..)`, `.drop_in(..)`, `.absent()` through
  the file op's candidate path with `systemd-analyze verify`; one
  daemon-reload before the next svc op after a changed unit (booted);
  `.restart_on(&unit)`; recipe `test-live-unit` 2/2 in a docker VM.
- Commits: telepatch b5d69c3

## 2026-11-06 — T77a owned handles, exact restart suppression, reload flush (telepatch)

- Every builder returns an owned handle (`op_builders!`), so `let daemon =
  h.file(..).content(..);` binds; a restart is dropped only by a start
  that started the unit (`SvcResult.started`, `!started(#n)`); a changed
  unit with no svc op after it in its window gets one daemon-reload at
  the window's end, reported and in prepare. `just docker-vm
  test-live-svc test-live-unit` 4/4 and 2/2.
- Commits: telepatch 0232dde

## 2026-11-06 — T80 cargo().locked() and one rustup op per toolchain (telepatch)

- `.locked()` passes `--locked` and is part of the cargo source (locked
  and unlocked never merge); rustup ops of one user and toolchain merge
  where the hoist is safe (targets and components unioned, set_default
  or-ed); a conflicting profile or installer is a trace error naming
  both sites; a later window asks only for what earlier ones lacked.
- Commits: telepatch 9ae95fc

## 2026-11-06 — T78 one pkg, every source chained (telepatch, tp_basic)

- `h.pkg(names)` is the distro manager; `.yum()`, `.pip()`, `.cargo()`,
  `.mise()`, `.rustup()`, `.installer_script(InstallerScript)` switch the
  source (a second call is a trace error); `h.pip`/`h.cargo`/`h.mise`/
  `h.rustup`/`h.yum`/`h.script` and `ScriptSpec` removed; the importer
  emits the chained form; tp_basic's nine call sites moved, plans
  unchanged. `just docker-vm test-acceptance` 7/7 identical.
- Commits: telepatch 2bb5c17, tp_basic cefe938

## 2026-11-06 — T79 pin and latest on the distro manager (telepatch)

- `.pin(v)` on apt (`name=v`), dnf/yum (`name-v`), checked by version
  through the manager; a pacman pin that is neither the repo's nor the
  installed version blocks the host in prepare; `.latest()` upgrades only
  names whose candidate is newer (apt `--only-upgrade` with one refresh
  per window, dnf `upgrade`, pacman `-S --needed`); pinned ops never
  merge. Zypper not built (no zypper manager exists). `just docker-vm
  test-live-pkg` 9/9 on ubuntu2404, rockylinux10, arch.
- Commits: telepatch 8536933

## 2026-11-06 — T77b state and restart on one svc chain (telepatch)

- `h.svc(u).enabled().started().restart_on(&x)` (and `.restart().when(g)`,
  `.reload().when(g)` after a state want) traces the action as a second
  svc op and returns its handle: the same plan, prepare lines, report,
  merges and suppression as the two-call form. `.when(g)` guards the op
  the handle names when called; a state after the action, and restart
  with reload, stay trace errors. `just docker-vm test-live-svc` 4/4.
- Commits: telepatch ac3d877

## 2026-11-06 — T83 vars per role, four layers (telepatch, tp_basic)

- `telepatch::vars` loads `V` as a struct of role sections: the role's
  `[section]`, then a top-level key of the same name, then the playbook's
  defaults (TOML text, `cli::main_with_defaults`), then the role's own
  `Default`; field names from a probing serde deserializer; an unknown
  key, a top-level key no role has, or a wrong type fails at load naming
  file and line. tp_basic: one vars struct per role; `rustup_version` and
  `cargo_version` dropped; the 91 plan snapshots unchanged.
- Commits: telepatch 3a546df, tp_basic 764c52d

## 2026-11-06 — T81 fetch, prepare-phase downloads (telepatch, tp_basic)

- `h.fetch(url).sha256(sum)` is a value; `h.file(p).from(&f)` places it,
  `.unpack(&f).strip(n)` unpacks tar (gz, xz, zst) or zip and swaps the
  directory in. Prepare downloads on the host to `/var/tmp/telepatch/<run>/`,
  verifies and caches under `/var/cache/telepatch/sha256/<sum>`; a cache
  hit makes no call; any failure, or no curl/wget, blocks the host. No
  checksum is `Unverified`. `just docker-vm test-live-fetch` 2/2 (ubuntu,
  rocky; red first on rocky's missing `cmp`, fixed in 3d8a500).
- Commits: telepatch e4dd4ff, 3d8a500; tp_basic a88d80b

## 2026-11-06 — T82 binary_release source (telepatch)

- `h.pkg([n]).binary_release(BinaryRelease::new()...)`: `{version}` and
  `{arch}` resolved on the host in prepare (present, pin, latest by
  `Latest::text`/`github_release`, or by published checksum with no
  `{version}`); `Sum::json|sidecar|value`; download and verify through
  T81's staging; a tarball swapped into `unpack_to`, a binary over `bin`;
  no checksum source is `Unverified`. A merged op carries every member's
  allowances, each tagged with its member. `just docker-vm
  test-live-fetch` 4/4 (ubuntu, rocky).
- Commits: telepatch 42d920a

## 2026-11-06 — T83a no cmp on the host; a fetch after the op that installs curl (telepatch, tp_basic)

- File content, line candidates, unit and repo files compare by
  `sha256sum`; no script needs `cmp` or `diff`. A window whose distro
  `pkg` op installs curl or wget before a fetch or binary release carries
  a split candidate, printed in the plan; when prepare finds neither tool
  the engine applies the head through that op, then prepares the rest as
  the next window; with a downloader on the host the run is unchanged.
  tp_basic: 82 plan snapshots move on the comparison line only. `just
  docker-vm test-live-fetch` 6/6 (ubuntu, rocky; split seen live).
- Commits: telepatch 0c8db33, cd349ec (the split covers wget: tests); tp_basic 28b9083

## 2026-11-06 — T84 rust_kernel without shell (tp_basic)

- `rust-src` through `s.pkg(["stable"]).rustup().components([..])`,
  `bindgen-cli` through `.cargo().locked()`; both HACK comments go;
  `src_repos`'s `sh!` is the one left. The component op does not merge
  into `rust_build`'s (Present vs Latest in the merge key; a `*` writer
  between): reported. 13 `latest_kernel_build` snapshots move. `just
  docker-vm test-container ubuntu2604 latest_kernel_build` identical;
  `test-idempotent ubuntu2404 rust` CHANGED 0.
- Commits: tp_basic f8c59b2

## 2026-11-06 — T85 golang on binary_release (tp_basic)

- The decided go example on `binary_release` (`Sum::json`, `Latest::text`,
  `unpack_to("/usr/local/go").strip(1)`); `INSTALL`, `LATEST` and
  `tests/golang.rs` go. 13 `dev_desktop` snapshots move: the go op becomes
  `pkg[binary_release]`, and 7 show T83a's split candidate after
  `dev_common`'s curl/wget op. `just test-vm ubuntu2404 dev_desktop` PASS
  at 1 GiB (run 3 CHANGED 0; `go version go1.27.3` as each admin user).
- Commits: tp_basic b5bd710

## 2026-11-06 — T86 VM proof of service states, units and guarded restarts (tp_basic)

- Test-only playbook `services_vm`: a unit with a drop-in, enabled, started
  and restarted on its change; a file-guarded reload; a disabled and
  stopped unit; a masked unit. `just test-vm ubuntu2404 services_vm` PASS
  at 1 GiB (run 1 restart and reload suppressed by the start; run 2
  CHANGED 0, both shown skipped; run 3, a changed Description var, one
  restart, start timestamp moved, one daemon-reload in the journal).
  `just docker-vm test-services ubuntu2404` PASS with it init-less.
- Commits: tp_basic 5debb31, 7583ccb

## 2026-11-06 — T86b Playbook defaults per playbook, typed

- `Playbook::new(name, run)` (const fn) replaces the struct literal (143
  literals moved); `.defaults(fn() -> V)` with `V: Serialize` on that
  method only, serialized to TOML as layer 3 for that playbook; an
  unserializable value fails at load as `playbook <name> defaults: ..`.
  `plan` and `run` take the named playbook's defaults;
  `main_with_defaults` and `run_with_args_and_defaults` removed;
  `testing::plan_for_playbook`. tp_basic: `Vars` derives `Serialize`,
  `services_vm` supplies its probe description as a playbook default;
  all 104 snapshots unchanged.
- Commits: telepatch 95d13c2, tp_basic a00b7db

## 2026-11-06 — T86a One rustup op per toolchain across roles

- Rustup's `wanted` leaves the merge key (the merged op wants Latest
  over Present); a rustup member joins an earlier head of equal key when
  nothing between is a barrier, writes `*` or a `file:` key, or touches
  the keys it adds (`rustup_hoist_is_safe`); set_default or Latest onto
  a Present head is checked as `*`. Cargo writes `pkg:cargo:<u>:<name>`
  plus new `Key::Home(u)` (overlaps `file:`, `bin:`) instead of `*`, and
  never crosses a distro `pkg` op. Four adversarial tests. tp_basic:
  `rust_kernel`'s `rust-src` folds into `rust_build`'s op in all 13
  `latest_kernel_build` plans; 39 plans move on cargo's `writes`; no
  distro op moves; `latest_kernel_build_has_one_rustup_op_per_toolchain`.
- Commits: telepatch c66bf6e, tp_basic cb812ab

## 2026-11-06 — T86c `plan` output is byte-stable

- The staging subdir is `"$tp_sd"/"$__tp_run"`, a shell variable the
  executor sets once in each shell it opens (root and login) from the
  per-process id; a login op passes it through `env -i`; `run_id()` is
  private. New snapshots of a binary release plus a no-sum fetch plan,
  and a scan of every snapshot for a `<hex>-<pid>` id. tp_basic's
  normaliser goes; the 13 `dev_desktop` plans move on the one staging
  line; two `plan` processes print identical text.
- Commits: telepatch ec68cac, tp_basic 5e9620e

## 2026-11-11 — T86e wget everywhere a download happens, proven live

- `Latest::github_release` uses the same curl-else-wget form as
  `Latest::Text` (`wget -q -O - --`). A PATH farm with only a fake wget
  resolves a github_release latest. Live `just docker-vm test-live-fetch`
  8/8: the six T83a cases, plus wget-only on ubuntu:24.04 (GNU Wget
  1.21.4) and rockylinux:10 (GNU Wget2 2.2.0). Rocky 10 has no
  `wget2-wget`; the test installs EPEL `wget2` and wraps it with
  `--no-local-db` so libwget does not leave `/tmp` lock files. The
  engine line is not changed.
- Commits: telepatch 0d9b14f (engine line, mixed with unrelated
  ui-prototype files; not rewritten), fb897bf (the live-test follow-up)

## 2026-11-11 — T86d Every daemon-reload is reported; the guarded reload fires on the VM

- Each daemon-reload telepatch runs is one stream line (`systemd
  daemon-reload: <reason>`), one `daemon_reload` event (`reason`, `op`)
  and one `HostReport` entry. A skipped one prints nothing and is not
  counted in OK or CHANGED. Svc snapshots move on the trailer JSON and
  the `__tp_drn` lines only. A newline in a string default survives the
  vars re-emitter, so `probe_conf`'s default `probe=1\n` does not move
  the conf op.
- tp_basic: `services_vm.probe_conf`; 39 plans with a svc op gain the
  `NeedDaemonReload` `__tp_drn` lines only. `just test-vm ubuntu2404
  services_vm` PASS at 1 GiB (run 3 one `daemon_reload` event and
  journal delta 1; run 4 the conf file and the guarded reload, MainPID
  unchanged, zero daemon-reloads; run 5 CHANGED 0). `just docker-vm
  test-services ubuntu2404` PASS, init-less, no reload line.
- Commits: telepatch a916c60, tp_basic 3db6094

## 2026-11-11 — T86f Second-run proof for `rust_kernel`

- `test-idempotent` accepts `latest_kernel_build`. `just docker-vm
  test-idempotent ubuntu2604 latest_kernel_build` second run CHANGED 0
  (`/tmp/t86f-idem2.log`, IDEM_EXIT:0); one `pkg[rustup]` op carrying
  `rust-src`. Peak 634 MiB, swap 397 MiB, no OOM. VM deleted.
- Ubuntu kernel extras request `libbabeltrace-dev` only. The virtual
  name `libbabeltrace-ctf-dev` stayed changed forever: apt's present
  check does not see Provides. Four Ubuntu plan snapshots move on that
  name. The first live gate (`/tmp/t86f-idem.log`) was CHANGED 1 on it.
- Commits: tp_basic 009c780

## 2026-11-11 — T87 close Goal 7 on the evidence already in hand

- No new proof run. The user stopped `just docker-vm test-acceptance`
  (exit 143). VM `tp-docker-20261112-002818` was already deleted. The
  partial log is neither a pass nor a fail: six of seven cells gated
  identical (`/tmp/t87-accept.log`); `ubuntu2604 latest_kernel_build`
  was killed before its gate, during the telepatch image. Every cell's
  dist stamp is tp_basic `3db6094` dirty, not clean `009c780`.
- Confirmed, not re-run: `/tmp/t87-plans` 104 plan pairs identical
  (PLAN_FAIL=0; the 104 `.err` sidecars match too); `/tmp/t87-ws.log`
  WS_EXIT:0 (`tests/note.rs` 7 passed). Each
  `container/latest_kernel_build` plan has one `pkg[rustup]` op carrying
  `rust-src`. T86e `/tmp/t86e-live3.log` 8/8; T86d `/tmp/t86d-vm.log`
  and `/tmp/t86d-services.log` PASS; T86f `/tmp/t86f-idem2.log` CHANGED 0
  (tp_basic `009c780`, LOG `f33c2de`). Status rows, the design-note pkg
  status and PORT.md `Playbook::new` stay `cc84358`. Acceptance scripts
  not deleted.
- Commits: telepatch (this LOG commit); `cc84358` unchanged

## 2026-11-11 — T88 Secret, references and the dependency-free backends

- `Secret` deserializes from a literal or `{ bao, field }`, `{ env }`,
  `{ file }`, `{ prompt }`. Display and Debug are `<secret>`. The bytes
  zeroize. Prepare resolves every reference the trace used, one read per
  path, before any host change; a miss names the var and the role; an
  unknown scheme names the scheme. `plan` resolves nothing. The user op
  takes a `Secret`; plan, log, report and events show `<secret>`; the
  host compares the shadow field by checksum. `secret_literals` fails
  tp_basic when a tracked vars or inventory file gives a `Secret` a
  literal.
- tp_basic `default_password` is `Option<Secret>`. `test-vm.sh` passes
  the throwaway hash through `{ env = "TP_TEST_PW_HASH" }` and does not
  print it. The first VM run applied the hash (`user tpdev` changed,
  run 3 CHANGED 0, working checks passed) but `su_tpdev` treated the pty
  closing as failure before it reaped the child. The checker now reaps,
  covered by `--self-test`. Retry `just test-vm ubuntu2404 dev_desktop`
  PASS (`/tmp/t88-vm.log`, VM_EXIT 0, run 3 CHANGED 0). VM deleted. No
  OOM. dev_desktop's idempotent pass is run 3; run 2 is the docker
  enable-then-start, CHANGED 1.
- Commits: telepatch d39168c, tp_basic 09363d7

## 2026-11-11 — T89 Templates with askama

- `.content` holds bytes or an askama template and renders when the
  script is built. Prepare resolves the secret references that render
  formatted, one read per path, before any host change. Plan, Debug,
  the log and the report omit a secret file's body and mark the path
  `(secret)`. The host script is where a `Secret` prints its value. A
  missing template field fails to compile (trybuild).
- tp_basic `sudoers_dropin` is `templates/sudoers_dropin`. The rendered
  bytes match the previous format string, including the seven spaces
  and the final newline. Snapshots unchanged.
- Gate E: `cargo test --workspace` and `just lint` green. Gate B:
  tp_basic `cargo test`, `just check-conversion`, `just
  test-oracle-selftest` green. No VM.
- Commits: telepatch d87ffe9, tp_basic 0de5db5

## 2026-11-11 — T90 telepatch-openbao

- New crate `telepatch-openbao`: `backend()`, scheme `bao`, token from
  `BAO_TOKEN` then `VAULT_TOKEN`, address from `BAO_ADDR` then
  `VAULT_ADDR`, no token file. KV version per mount from
  `/v1/sys/internal/ui/mounts/<path>`, then one read per path (`/data/`
  for v2). A missing field lists names only. Skip-verify prints a
  prepare warning. Token expiry comes from `auth/token/lookup-self`.
- Stub HTTP tests on `127.0.0.1` only: v1, v2, missing path, missing
  field, 403, no token, skip-verify, expired token. No real server.
- `just lint` fails if `cargo tree -p telepatch` contains `ureq` or
  `rustls`. Gate E green (`cargo test --workspace`, `just lint`,
  tp_basic `cargo test`).
- Commits: telepatch 44e98e3

## 2026-11-11 — T91 OpenBao live test and the playbook binary

- `tp-basic` registers `telepatch_openbao::backend()`. The ignored test
  starts a plain `debian:13` container with `/bao server -dev`, copies
  the local `bao` binary in with `docker cp`, and adds no capability.
  It writes one v2 and one v1 field on scratch mounts and the backend
  reads both at `http://$TP_DOCKER_VM_IP:<port>`.
- `just test-live-openbao` unsets inherited `BAO_*` and `VAULT_*`
  first. Nothing reads `.env`. The throwaway token is not recorded.
- `just docker-vm test-live-openbao` VM_EXIT 0. Pass line:
  `PASS openbao v2 and v1`. VM `tp-docker-20261112-051239` deleted.
  The server started without a capability. Gate E and B green before
  the VM.
- Commits: telepatch de0293a, tp_basic 0e0284b

## 2026-11-11 — T92 Package removal

- `.absent()` removes only names the manager reports installed.
  `.purge()` is apt-only (`apt-get purge`, then `dpkg --purge` if the
  name is still `installed` or `config-files`); any other manager is a
  trace error. An absent op posts `Absent`, does not merge with an
  install, and keeps trace order against one. Prepare says `pkg: will
  remove cmake`. The names removed are the inverse, on
  `HostReport.removed`.
- A purge that leaves dpkg status `not-installed` is done. Ubuntu keeps
  that stanza, and `dpkg-query -W` still exits 0. The live check accepts
  `not-installed` or a missing record, and still requires the conffiles
  and the binary to be gone.
- `just docker-vm test-live-pkg` on `tp-docker-20261112-060014`: 12
  passed, 0 failed, 115.52s. VM deleted. Not an OOM. Two earlier VMs
  failed the old `dpkg-query -W` exit check and were deleted. Gate E
  green (`cargo test --workspace`, `just lint`).
- Commits: telepatch 1875375

## 2026-11-11 — T93 Package from a fetched file

- `h.pkg([name]).from(deb).pin(v)` installs one package through apt or
  dnf when the manager does not already report that name and version.
  The file is the prepare-phase checksum cache, installed as `./x.deb`
  or `./x.rpm` from the staging directory. The symlink is removed in
  the op. Any other manager is a trace error. No pin, or more than one
  name, fails the trace. No checksum is `Unverified`.
- `just docker-vm test-live-fetch` on `tp-docker-20261112-061654`: 9
  passed, 0 failed, 75.32s, including `fetch_ubuntu_pkg_from_a_deb`
  (a deb built with `dpkg-deb`, run 2 unchanged, nothing left in
  `/tmp`). VM deleted. Not an OOM. Gate E green (`cargo test
  --workspace`, `just lint`).
- Commits: telepatch 7593e84

## 2026-11-11 — T94 Controller-side files and commands

- `local_file`, `local_dir` and `local([argv])` are read in prepare.
  A file or tree is compared by sha256 and streamed only where it
  differs, in 256 KiB chunks, each its own shell script. Nothing on
  the host is removed by a directory copy. `~` is the operator's
  home; a relative path is beside the vars file. A missing file or
  a failed command blocks the host before that window changes it.
  `.line(&pubkey)` takes a `local_file`. `.via_controller()` downloads
  with curl or wget on the controller and streams the same way.
- `cargo test --workspace` and `just lint` green. The local executor
  test copies a 20 MiB binary (interior NULs; run 2 does not rewrite).
  No VM.
- Commits: telepatch d7083e7
