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

## 2026-11-11 — T95 `git` checkout

- `h.git(url).depth(n).rev(r).latest().into(path)` clones a checkout,
  including through `as_user`. Prepare probes `rev-parse HEAD` and the
  origin URL. No `.rev` leaves a clean checkout of the same remote
  alone. `.rev` checks out that commit. `.latest` fast-forwards the
  branch. A different remote, a dirty tree, or a path that is not a
  checkout blocks the host before the window changes it. The trailer
  records `created` and `was_rev` for T98.
- `just docker-vm test-live-git` on `tp-docker-20261112-071509`: 1
  passed, 0 failed, 26.80s (a bare repo inside ubuntu:24.04). Peak
  memory 320 MiB of 961, swap 0. VM deleted. Not an OOM. Gate E green
  (`cargo test --workspace`, `just lint`).
- Commits: telepatch f74daad

## 2026-11-11 — T96 Typed facts and assertions

- `h.user_info(u)` (home, shell, exists), `h.stat(p)` (mode, owner,
  size, exists), `h.pkg_installed(n)` (`Some(version)` or `None`) and
  `h.check(cmd)` (rc, stdout) are facts. Every fact polled at one point
  is one `read_facts` round trip per host. `h.assert(cond, msg)` blocks
  that host in prepare with `msg` before the window's ops run.
- `cargo test --workspace` and `just lint` green. No VM.
- Commits: telepatch aa9b0ee

## 2026-11-11 — T97 Author-defined resources and `cmd!`

- `h.resource(T)` for a `Resource` (`KIND`, `key`, `probe`, `plan`,
  `apply`, optional `inverse`). `cmd!` quotes like `sh!` and produces a
  `Script`, not an op. `Probe::stdout().missing_is_none()`, `Probed`,
  `Change::{None, will}`, `Script::stdin`. `Key::custom`. A resource op
  keeps `Eq`/`Debug` by kind, key and rendered scripts and never merges.
  No inverse is recorded as irreversible. `plan_against` runs `plan` on
  recorded probe output. The decided `Terminfo` resource compiles an
  entry with `tic`.
- `just docker-vm test-live-resource` on `tp-docker-20261112-080408`: 1
  passed, 0 failed, 12.07s (ubuntu:24.04; run 2 unchanged). Peak memory
  296 MiB of 961, swap 0. VM deleted. Not an OOM. Gate E green
  (`cargo test --workspace`, `just lint`).
- Commits: telepatch c8830be

## 2026-11-12 — T97a Retire the production acceptance runners

- `just test-acceptance`, `just test-matrix` and `just test-container` are
  gone, and so are `scripts/test-container.sh` and `scripts/test-matrix.sh`.
  The three capture cases (equal, package missing, version-only) and
  `selftest_intended` run from `scripts/lib/compare.sh --self-test`.
  `dev_base`'s `PB_ORACLE` is 0; `test-vm.sh` does not read
  `build/test-container/`. Gate: `just test-oracle-selftest` green. No VM.
- Commits: tp_basic 5715776

## 2026-11-12 — T97b Ansible edge playbook, reviewed against the edge list

- One play, `hosts: all`, roles `edge` then `edge_extra`. Debian installs
  `www-browser`/`w3m` and Docker's apt repo then `docker-ce-cli`; Red Hat
  except Amazon installs `mailx`/`s-nail` and Docker's fedora or rhel
  `.repo` then `docker-ce-cli`; Amazon is the same pair and no repo; Arch
  requests `sh`/`bash` and no repo. The marker file is `content: "edge\n"`.
  The jq fetch carries `sha256:5942c9b0…` from the jq 1.7.1 `sha256sum.txt`
  line for `jq-linux-amd64`. `rust-src` is only in `edge_extra`. Pin:
  `ansible-core==2.22.0` (PyPI's newest stable that day). `just check-edge`
  green, including `ansible-playbook --syntax-check`. No VM.
- Commits: tp_basic 2b9416e

## 2026-11-12 — T97c Apt counts a provided name as installed

- An apt Present name is not missing when dpkg says `installed` or an
  installed package Provides it (`tp_apt_present` in the install
  script). Pin, latest, absent and a `.deb` file install keep the
  status check. dnf stays on `rpm -q --whatprovides` and pacman on
  `pacman -T`. 44 snapshots moved, and only in that present-check
  text: 16 in telepatch, 28 Debian-family plans in tp_basic. Red Hat
  and Arch plans are byte-identical. tp_basic's lock also records
  telepatch's `sha2` dependency from T94.
- Gate E green (`cargo test --workspace`, `just lint`, tp_basic
  `cargo test`). No VM.
- Commits: telepatch 47d4c45, tp_basic 3e79742

## 2026-11-12 — T97d Port the edge playbook

- Playbook `edge` runs `edge` then `edge_extra` and nothing else. One
  `pkg` installs curl, ca-certificates and the family pair
  (`w3m`/`www-browser`, `s-nail`/`mailx`, `bash`/`sh`). Then Docker's apt
  repo (debian or ubuntu) or dnf `.repo` (fedora or rhel) and
  `docker-ce-cli`. Amazon and Arch get no repo. The `edge\n` marker,
  `edge-jq` fetched by sha256, and one `pkg[rustup]` stable that carries
  `rust-src` from `edge_extra`. 13 new `edge__*` snapshots, and
  `plans_are_byte_identical` over every playbook. Close-out checked the
  port against the T97b table and Ansible tasks: no mismatch. Gate B
  green. No VM.
- Commits: tp_basic 04d1a5e

## 2026-11-12 — T97g Debian edge pair becomes libz-dev / zlib1g-dev

- The Debian-family edge pair is `zlib1g-dev` (provider) and `libz-dev`
  (virtual name), replacing `w3m`/`www-browser`, which has 25 providers
  on ubuntu 24.04. Changed the apt task, `check-edge-playbook.sh`,
  `src/roles/edge.rs` and the two edge tests. Only op #1 of the four
  Debian-family `edge__*` snapshots moved; every other snapshot is
  byte-identical. T97e's uncommitted harness was left as it was.
  Telepatch unchanged. Gate `just check-edge && cargo test && just
  test-oracle-selftest` green. No VM.
- Commits: tp_basic 6ad1160

## 2026-11-12 — T97h Prepare veritech04 as the tp-* VM host

- New `scripts/vm-host.sh` in tp_basic. `check` is read-only, `setup`
  is idempotent, and `--self-test` runs 51 stub cases with no network.
  `scripts/lib/vm.sh` gains only the `TP_VM_HOST` / `TP_VM_HOST_DIR` /
  `TP_VM_POOL` settings block.
- Setup ran for real on veritech04.labtwo:
  - One sudo, `apt-get install --no-install-recommends golang-1.24-go
    libvirt-dev`, after a clean simulation (3 new packages, no upgrade,
    no removal).
  - vmtool 465d48e built there with go1.24.13.
  - Pool `tp` at `/var/lib/libvirt/images/tp`.
  - `ubuntu2404.qcow2` uploaded, sha256 72ecb307… verified.
- The live run found two bugs, fixed test-first:
  - qemu-img's JSON has a nested `virtual-size`.
  - `virsh vol-download` to stdout adds a newline.
- veritech04's VM list and networks are unchanged, and `dpkg -l` differs
  only by the three packages. No VM created.
- Gate green: self-test, `check`, `just test-oracle-selftest`.
- Commits: tp_basic 46a06fe

## 2026-11-12 — T97i vm.sh drives vmtool on the VM host

- With `TP_VM_HOST` remote (default veritech04.labtwo), `scripts/lib/vm.sh`
  works as follows:
  - It runs `./vmtool` in `~/tp-vmtool` over ssh, one `bash -c`
    argument, so zsh cannot split it. Create carries `--pool tp`.
  - The tcp/22 wait is probed from the host.
  - The guest is reached from here with a per-run key and `ProxyJump`.
    An ssh on the host, with vmtool's key in place, installs the run
    key's `.pub`.
  - `VM_SSH_OPTS` is the one option set for `sshv` and the
    docker-socket forward.
  - `vm_record` and `vm_delete` refuse any name but `tp-[a-z0-9-]+`.
    Delete verifies both `vmtool list` and `.machines/<name>` on the
    host.
- `docker-vm.sh` runs `vm-host.sh check` first and names the host in its
  end line. A failed final list is a FAIL, never "no tp-* VM".
- `test-vm.sh` and `test-vm-oracle.sh` refuse a remote host until T97j.
  Local argv is unchanged.
- Live: `tp-docker-20261112-113216` ran on veritech04 and showed its
  label. Peak memory was 216 MiB of 961, with no swap used. The VM was
  deleted and verified gone. veritech04's other VMs and this
  workstation's VM list are unchanged.
- Gate green.
- Commits: tp_basic 2dd2fd0

## 2026-11-12 — T97e ubuntu2404 edge gate

- `just test-edge` runs the local half, then one ubuntu2404 cell via
  `just docker-vm` on veritech04.labtwo. The local half is telepatch
  `cargo test --workspace`, `cargo test --test plans`, the self-test and
  `just musl`.
- The cell: Ansible's `edge` from a throwaway venv, and `tp-basic run
  edge --local` twice, both from `ubuntu:24.04` pulled in the VM.
  - Captures identical, with no intended-differences file.
  - Marker bytes are `edge\n` and `edge-jq` is checksummed on both
    sides.
  - Run 2 `CHANGED 0`.
  - Cell 167 s; VM peak 698 MiB of 961, 184 MiB swap.
  - VM `tp-docker-20261112-113710` deleted and verified gone.
- The harness was resumed unchanged from the blocked run; the README now
  documents where the VMs run.
- The libz-dev / zlib1g-dev pair installed on both sides.
- Observation for a follow-up: run 2's advisory prepare line still lists
  `libz-dev` as "will install" (it does not apply T97c's Provides rule).
- Gate green.
- Commits: tp_basic 4fcf3cb

## 2026-11-12 — T97f manual edge matrix, proved on one dnf distro

- `test-edge.sh cell` takes every `tests/plans.rs` distro with a stock
  image, with a bootstrap per family (apt, dnf, dnf5, pacman). The local
  half and `just test-edge` stay ubuntu2404 only.
- New `scripts/test-edge-matrix.sh` and `just test-edge-matrix`: manual,
  not a goal gate. One VM, the cells one after another, `SKIP omarchy`,
  a failed cell does not stop the rest. It refuses when the VM host lists
  a `tp-*` VM. The README says Jared starts it.
- `rockylinux10` maps to `rockylinux/rockylinux:10`, because Docker's
  library `rockylinux` has no `10` tag.
- Live: `just docker-vm test-edge-cell rockylinux10` PASS on veritech04.
  - Captures identical, marker and `edge-jq` ok on both sides.
  - Run 2 `CHANGED 0`.
  - Cell 139 s; VM peak 649 MiB of 961, 306 MiB swap.
  - VM `tp-docker-20261112-202245` deleted and verified gone.
- Gate green.
- Commits: tp_basic 8cd9b69

## 2026-11-12 — T97k the prepare summary uses the install check's own test

- `pkg_probe` prints `present` (the Present install script's test) and
  `removable` (the absent script's test) per name. The test text is
  defined once in `ops/pkg.rs` and reused: `tp_apt_present`, rpm, and
  pacman with its `-Qqg` group half.
- A provided-only name is no longer "will install" (apt `libz-dev`) or
  "will remove" (apt, dnf `mailx`, pacman `sh`). An installed pacman
  group is no longer announced.
- Rendered scripts are byte-identical; no plan snapshot moved in either
  repo.
- Four stubbed-manager tests, including script/probe agreement for apt,
  dnf and pacman.
- Gate E green. No VM.
- Commits: telepatch 35d12ca

## 2026-11-12 — T97l reach the docker VM's containers through ssh forwards

- `docker-vm.sh` exports `TP_DOCKER_VM_PUBLISH`, `TP_DOCKER_VM_ADDR` and
  `TP_DOCKER_VM_PORTS` (default 42201-42208, a bad override exits 2
  before any VM). On a remote host both addresses are 127.0.0.1, and the
  socket tunnel also carries one `-L 127.0.0.1:P:127.0.0.1:P` per port.
  `TP_VM_HOST=local` keeps the VM's address, `TP_DOCKER_VM_IP` and
  today's tunnel argv.
- The live tests read the endpoint (`tests/live/endpoint.rs`, duplicated
  in the openbao test). They publish `<PUBLISH>:P:<port>`, dial
  `<ADDR>:P`, and move on when docker says a port is taken. `docker port`
  is gone.
- The first live run had no forwards (`export_endpoint` ran after the
  tunnel argv was built). Fixed, and the self-test now checks the order.
- Live on veritech04: `test-live-ssh` 5/5 (VM `tp-docker-20261112-204658`)
  and `test-live-openbao` 1/1 (VM `tp-docker-20261112-205127`). Each VM
  was deleted and verified gone.
- Gate green.
- Commits: tp_basic 73f4833, telepatch fc68029

## 2026-11-12 — T97j `test-vm.sh` and `test-vm-oracle.sh` on the VM host

- T97i's refusal is gone from both scripts. `tp-basic run --host` takes
  its ssh arguments from `tpb_ssh_args`: `--ssh-key`, the known_hosts
  options and, on a remote VM host, `--ssh-option
  ProxyJump=$TP_VM_HOST`. The vmtool preflight is local-only.
- `write_inventory` puts `-o ProxyJump=<host>` inside the one
  `ansible_ssh_common_args` value on a remote host. It still names only
  the key's path.
- Every guest command in `test-vm.sh` (`su_tpdev`, journal, systemctl)
  already went through `sshv`, which jumps since T97i. Unchanged.
- `TP_VM_HOST=local`: the argv and the inventory are byte-identical,
  proved by self-test. The jump through telepatch's ssh master was
  checked offline with `ssh -G`: no engine change.
- No live run (T103 is the first). Gate `just test-oracle-selftest`
  green. No tp-* VM listed on veritech04 or here afterwards.
- Commits: tp_basic 3c0f51e

## 2026-11-12 — T98 `--check`, `--diff` and coverage

- `run --check` traces and prepares every host and applies nothing.
  File ops are probed read-only; a line edit runs on a temp copy.
  `--diff` (needs `--check`) adds a unified diff per file. A secret file
  shows no diff and its body is never read back.
- Exit 0 nothing to change, 2 changes pending, 1 a host blocked.
- At the first implicit barrier the check stops: `check stopped at
  <file:line>: this read needs the changes above` (G7-4). Fact reads now
  record their call site for it.
- One `[coverage] <role>: n ops, m reversible` line per role, from
  `OpSpec::has_inverse` (G7-1). file, unit, repo, user and sh count as
  irreversible: their trailers capture no inverse yet.
- 14 tests in `tests/check.rs`, 2 in `tests/cli.rs`. No VM (T104).
- Gate E green; no plan snapshot moved in either repo.
- Commits: telepatch 294f3ac, tp_basic d021e3d (Cargo.lock)

## 2026-11-12 — T99 inventory and `--limit`

- New `telepatch::inventory` reads `inventory.toml`: `[vars]`,
  `[groups.*]` with `hosts`/`vars`, and `[hosts."dest"]` with
  `groups`/`vars`. Errors name the file and line.
- Precedence: host, groups (in file order; a later group beats an
  earlier one), inventory `[vars]`, `--vars`, playbook defaults,
  `Default`. Within each, a role section beats a top-level key.
- `run --inventory f [--limit a,b]`: group or host names; an unknown
  name is a usage error. `--host` is kept, and it is exclusive with
  `--inventory`. `h.in_group(..)`. `HostSpec.groups` (66 literals
  moved).
- tp_basic's secret-literal gate reads `inventory*` with the inventory
  loader.
- Live on veritech04 (`just docker-vm test-live-inventory`, VM
  `tp-docker-20261112-213806`): two sshd containers on 42201 and 42202,
  each reached by its own forward, with a throwaway known-hosts file.
  - The group var reached one host only. Run 2 `ok`. `--limit gpu`
    touched one host.
  - Peak 351 MiB of 961. The VM was deleted and verified gone.
  - The taken-port retry did not trigger live.
- Gate green. One retry of the commit gate after a flaky,
  untouched `shell_pool::shell_death_fails_in_flight_ops`.
- Commits: telepatch c949f93, tp_basic bece6a2

## 2026-11-12 — T100 `--serial`, `--max-fail`, `--only`

- `run --serial n` runs n hosts at a time, in the order given (a fair
  semaphore). `--serial 0` is a usage error.
- `run --max-fail n`: once more than n hosts end failed, blocked or in
  error, hosts not yet started are reported `skipped` (`max-fail n
  reached`), and the exit code is 1. Started hosts finish. Without
  `--serial` every host has already started.
- `run --only a,b`: `#[role]` skips the body of a top-level role that is
  not named. Its marks stay in the trace, and the report lists `skipped
  by --only`. A role called inside a named role runs with it. A name
  the playbook did not trace is an error listing the roles it did.
- `executor::run_with` + `RunControls`; `run`/`run_check` unchanged.
  All three work under `--check`.
- 16 tests in `tests/run_controls.rs`. Gate E green; no snapshot moved.
- Commits: telepatch d57755f

## 2026-11-12 — T110 Set the rejected T101 aside; commit the T100 LOG entry

- The rejected T101 build (15 tracked files and the untracked
  `ops/wait.rs`) is saved twice: `tmp/t101-rejected/wip.patch` plus
  `wait.rs`, and the stash "T101 rejected: bash -c wrapper
  (2026-11-12)". Both checked equal; the patch applies on the new
  HEAD. Nothing popped or dropped; T113 drops the stash.
- The T100 entry committed alone on a clean tree. Gate green.
- Commits: telepatch 736458c

## 2026-11-12 — T111 No script on a command line

- The `as_user` login bash (`env -i … bash -l /dev/fd/9
  9<<'__tp_S_<nonce>_<id>'`) and the `h.check` probe (`bash /dev/fd/9
  9<<'__tp_C_<digest>_<n>'`) read their script from a quoted here-doc,
  not argv: no 128 KiB cap, nothing in `/proc/*/cmdline`. A script
  line equal to the delimiter is `RenderError::ScriptHasDelimiter`.
- `tests/no_argv.rs`: 200 KiB file byte-exact (plain, `as_user`) and a
  cmdline sweep (plain, `as_user`, check). Second audit pass found no
  third production site.
- Gate E green; no snapshot moved in either repo.
- Commits: telepatch 8ba9bb2

## 2026-11-12 — T112 Start marker, one process group per op, and the kill proof

- The persistent shell runs under `set -m`: each op subshell is its own
  process group (pgid = `$BASHPID`). Proved locally in
  `tests/op_kill.rs`: a group kill from a second shell clears the op
  (TERM, then KILL for TERM-ignorers), a `setsid` child survives, and
  the shell runs the next op. No `/proc` fallback was needed.
- An op prints a start line `__tpS_<nonce> <id> <pid> <shell_pid>`,
  which becomes `Event::Started` and is never output; guard and halt
  skips print none. `trap 'exit 143' TERM`; the parent's stderr goes to
  /dev/null around each op, so a signalled op's job notice (the whole
  script) never reaches the stream. A killed op still trips
  `__tp_halt`.
- Finding for T101: under load a TERM can be lost; the watchdog must
  re-send or escalate.
- 12 tests in `tests/op_kill.rs`. Gate E green. 42 telepatch snapshots
  re-accepted (wrapper lines only); none moved in tp_basic.
- Commits: telepatch 540ceac

## 2026-11-12 — T101 `.timeout`: the controller's watchdog

- `.timeout(d)` on every op (zero is a trace error; a second call
  replaces the first) and per-kind defaults: pkg 30 min; sh, raw,
  resource, git 10 min; file, repo 5 min; svc 2 min; unit, group, user
  1 min. Part of the merge keys; the plan shows only an explicit limit.
- Enforced by the controller, not on the host: `Watch` over the shell
  (clock from the start line, or from when the op becomes current), a
  root second shell opened once per host on first expiry, TERM to the
  op's process group once a second for 5 s, then KILL; the kill script
  goes through the second shell's stdin. Reported `FAILED (timed out
  after d)`; PID gone with no trailer is `ended without a result`; no
  start line drops the shell (`op never started`); a host with no
  second shell is dropped (`no second shell to stop it`).
- `Shell::abandon`/`abort` in the pool. Note amended (controller clock,
  `set -m`, start line, repeated TERM, Run controls). Live ssh case
  written, run by T104.
- Gate E green: 816 passed; tp_basic unchanged.
- Commits: telepatch dd71572

## 2026-11-12 — T113 `.retry`, `wait_for` and the fetch limits

- `.retry(n)` by the controller: a retried op ends its run; on a
  failure or timeout with retries left the executor waits 1, 2, 4 .. 30
  s and re-sends the gated script (halt cleared) with a new start line
  and clock. One `attempt k of n+1 failed (..); retrying in Ns` line per
  failed attempt; the last attempt is reported and read by guards.
- `h.wait_for(Port::tcp | Wait::path | Wait::unit)` (`ops/wait.rs`):
  own deadline (exit 124), controller backstop d + 15 s, default 5 min,
  no change, not run in `--check`, skipped without systemd.
- Fetch: `.retry`/`.timeout` merged by max; default 10 min per attempt
  (RT-4 still open). Each attempt bounded by `curl --max-time` /
  `wget --timeout` (no coreutils `timeout`); the prepare fetch probe
  runs under T101's watchdog with the summed deadline.
- Note: `wait_for` row, handle methods and Run controls marked built.
  The T110 stash "T101 rejected: bash -c wrapper (2026-11-12)" dropped;
  `tmp/t101-rejected/` kept.
- Gate E green: 853 passed; tp_basic unchanged.
- Commits: telepatch ec67ee1

## 2026-11-12 — T114 `as_user` ops run in the user's persistent login shell

- One login shell per user per host, fed over stdin like every other
  shell; an `as_user` op is a plain `( … )` subshell in it. No per-op
  `bash -l`, `env -i` or here-doc. Shells keyed by `RunAs`.
- `as_user("root")`: a second transport root shell turned into
  `exec env -i HOME=… USER=… LOGNAME=… SHELL=… PATH=… bash -l` (values
  from `getent passwd`), then a hello; `init_shell` and the T101
  watchdog cover it. Non-root `as_user` on local and docker stays
  deferred (OS-2 default). No passwd entry blocks the host.
- `__tp_f`, `__tp_dr`, `__tp_run` set in place; guards across shells
  are constants; the daemon-reload flag is carried across shells.
- Staleness rules 1-6 as written (OS-3 default; `OpNode::stales`),
  lazy reopen (`   login shell u reopened: <rule> after #N`, event
  `login_reopened`), an op that can stale its own shell ends its run,
  `h.as_user(u).relogin()` (plan `relogin u`). The group clause of rule
  1 is inert: `group` ops carry no members.
- tests/no_argv.rs adapted; tests/login_shell.rs (17); note amended.
- Gate E green: 884 passed; lint clean; tp_basic 48 passed, unchanged.
- Commits: telepatch 7be0643

## 2026-11-12 — T105 The shell-death test races its own send

- The test was wrong, not the pool. `shell_death_fails_in_flight_ops`
  accepts `Ok` or `Err(Died)` from a send after the kill; `wait` still
  says `Died`. New `send_to_a_dead_shell_is_the_ops_failure`
  (deterministic: waits for the process to be gone, since stdout's EOF
  can precede the stdin pipe closing). New `tests/shell_death.rs`: a
  `--local` window whose first op kills the shell (`kill -9 $$
  $BASHPID`) fails the host at that op; the next op is not run.
- Loop on top of T114: each binary 1000 runs, 8 at a time, 0 failures;
  `cargo test --workspace` 5 times green (886 passed); lint clean;
  tp_basic unchanged. No file under `src/`.
- Commits: telepatch 257d92e

## 2026-11-12 — T102 Reboot marks and the playbook barrier

- `.needs_reboot()` on every op handle; `h.reboot().if_pending()` records
  `Node::Reboot` (not `Node::Barrier`). Plan: `needs_reboot` flag, `reboot
  if pending` line. Windows never span a reboot node; nothing hoists across.
- A marked member that changed (per member for a merged `pkg`) is appended
  to `${TP_RUN_DIR:-/run/telepatch}/reboot-pending` through the root shell
  after its run is collected, before a failure is acted on.
- Barrier: apply everything before it, probe the mark (an earlier run's
  counts). Over SSH: `systemctl reboot`, drop every shell, poll a fresh
  root shell until `${TP_BOOT_ID:-/proc/sys/kernel/random/boot_id}`
  changes (deadline 10 min), login and killer shells reopen lazily,
  confirmed facts forgotten. `--local`, docker, `--reboot never`: host
  stops with `reboot pending at barrier` (status `reboot pending`, exit 1).
- End of run lists `reboot pending`; `--reboot auto` reboots once more.
  `--reboot` conflicts with `--check`; a check stops at a barrier that may
  be pending. Note: reboot row and `#d-reboot` marked built.
- Gate E green: 915 passed; lint clean; tp_basic 48 passed, unchanged.
- Commits: telepatch fec9bd6

## 2026-11-12 — T103 VM proof of reboot

- tp_basic: test-only playbook `reboot_vm` (a file op marked
  `.needs_reboot()`, `h.reboot().if_pending()`, a file op after it; vars
  `[reboot_vm] marked`, `after`), plan snapshots on 13 distros.
- `just test-vm ubuntu2404 reboot_vm` on veritech04, one 1 GiB `tp-*` VM,
  PASS: run 1 crossed the barrier (boot id changed, down 8s, after-op
  applied, mark gone); run 2 CHANGED 0, no reboot; run 3 with a fresh mark
  and `--reboot never` exit 1, `reboot pending at barrier`, after-op not
  run, mark present; run 4 rebooted from that mark and applied the
  after-op; run 5 CHANGED 0. VM deleted, none left.
- test-vm.sh: two-word STATUS parse; VM names map `_` to `-` (the
  T97j name guard refused `tp-reboot_vm-…`, and would have refused
  services_vm and dev_*).
- Gate B green: tp_basic cargo test 50 passed, check-conversion ok,
  test-oracle-selftest ok, test-vm.sh --self-test ok.
- Commits: tp_basic fcc1643

## 2026-11-13 — T115 `h.check` runs share one bash

- A run (adjacent `check` reads in one fact round, same `RunAs`) is one
  probe op and one fresh bash; a run of one is today's case. OS-1
  settled by the user: "T115 agreed on bash per group", no bash kept
  across awaits.
- Each check is `tp_cN='…'`, run as `( eval "$tp_cN" ) >"$d/N"
  2>/dev/null </dev/null`; rc, length and stdout per check; temp dir
  removed before return. Script still on fd 9 from a quoted here-doc.
- `facts` event gains `"probes": n`; answers stay one per query.
- tests/facts.rs (6 new), tests/no_argv.rs run-of-three sweep; note
  amended (facts paragraph and section).
- Gate E green: 924 passed; lint clean; tp_basic 50 passed, unchanged.
- tp_basic: stale `bash -l` comment in rust_build.rs corrected.
- Commits: telepatch 9adc138; tp_basic 46c7aaf

## 2026-11-13 — T106 Inverse capture for `file` and `unit`

- Every file and unit form records `existed`, `was_type`, `was_mode`,
  `was_owner`, `created_top` and `backup` (stage path) in `FileResult`.
  Copied forms `cp -a`; `.absent()` and `.unpack`'s old tree move
  (renames); `.from(local_dir)` copies replaced files plus a `created`
  manifest; mode/owner-only changes save no bytes; unchanged ops save
  nothing.
- Backups on the target's own filesystem (FU-Q3): `tp_fsroot` device
  walk, `TP_FS_ROOTS` test seam; `<root>/.telepatch/tmp/<run>/undo/
  <user>/<id>` linked from `<stage>/<run>/undo/…`; `fsroots` index.
  Mount points and nested mounts refused before a move. No fallback to
  the stage's filesystem. Only a uid-0 shell creates `.telepatch` at a
  walked root.
- Root-shell preparation op before each capturing `as_user` run; cleanup
  on every host end (fresh root shell if the old one is gone), kept
  across a reboot barrier; stale sweep of dead runs older than 24 h,
  younger ones warned (FU-Q4 default).
- `has_inverse` true for file and unit; note cells and "Roles as
  transactions" row updated.
- tp_basic: test-only playbook `undo_fs` and its `test-vm.sh` setup
  (64 MiB ext4 loop mount at `/srv/tpfs`), written here, run in T104.
- Snapshots: telepatch 13, tp_basic 117 moved (capture lines only,
  checked by script), 13 new undo_fs plans.
- Gate E green: telepatch 975 passed, lint clean; tp_basic 52 passed,
  check-conversion ok, test-oracle-selftest ok.
- Commits: telepatch 4c1ff3b; tp_basic 7611fef

## 2026-11-13 — T107 Inverse capture for `repo` and `user`

- `RepoResult`: `key_created`, `file_created`, `backup`; an existing apt
  source or `.dnf` body is backed up by T106's `tp_save` on its own
  filesystem before the rewrite (seam test with the link); a backup
  failure fails the op before the rewrite.
- `UserResult`: `was_shell` (null when `usermod -s` did not run),
  `groups_added` (only the groups the account lacked); no hash, shadow
  field or checksum in any output (tested).
- `has_inverse`: user true, repo true unless `RepoKind::Unset`; a repo
  window sets `undo_used`; note cells and transactions rows updated.
- Snapshots: telepatch 4, tp_basic 49, capture lines only.
- Gate E green: telepatch 987 passed, lint clean; tp_basic 52 passed.
- Commits: telepatch f9a1024; tp_basic 18c5fba

## 2026-11-13 — T108 `sh` `.undo(..)` and "sh without undo"

- `.undo(cmd)` on the `sh!` handle stores the command (`ShellSpec.undo`);
  nothing runs or captures it. `has_inverse(Shell)` is `undo.is_some()`;
  `Raw` stays false. `.undo` on a non-sh op is a trace error.
- `[coverage] r: n ops, m reversible, k sh without undo` when k > 0
  (k: ops of kind `sh` with no inverse, `Raw` included); unchanged when
  k = 0. The `sh!` warning still fires. Note's `sh` row: `.undo` built.
- No plan text or snapshot moved in either repo.
- Gate E green: telepatch 990 passed, lint clean; tp_basic 52 passed.
- Commits: telepatch c108ab1

## 2026-11-13 — T109 `--check`: the note's counts, and file probes after earlier changes

- `[prepare] N will change, K ok, B blocked`, in trace ops (merged pkg
  members once each): N pending, K probed and settled, run-time ops in
  neither with their reason; exit status unchanged (still from findings).
- File lines after an earlier window's change add `(as the host is now;
  the earlier windows' changes may differ)`; after an earlier op in the
  window writing the same path or a parent, `(as the host is now; this
  window's earlier ops may differ)`. `*` writes do not count.
- `CheckInfo` gains `will_ops`, `ok_ops`; the `[prepare]` check event
  carries them. Module doc updated. No snapshot moved.
- Gate E green: telepatch 996 passed, lint clean; tp_basic 52 passed.
  One gate-hook run hit a load flake in
  `op_kill::a_termed_op_still_prints_its_trailer_with_rc_143` (5 s
  wait); five isolated reruns ok; the retry passed.
- Commits: telepatch 4b78b5c

## 2026-11-13 — T124 No test touches the real stage, cache, run dir or a real filesystem root

- `.cargo/config.toml` in both repos: `[env] TP_SANDBOX_REQUIRED = "1"`.
  Under it `tp_sbx` (`src/sandbox.rs`) refuses a stage, cache, run dir
  or `<root>/.telepatch` not under `$TP_SANDBOX`: `init_shell` fails the
  shell open; `tp_undo`, `tp_pf`, `tp_rmrun`, the sweep, `LOCATE`,
  `stage_vars` and the reboot scripts refuse before touching anything.
  `LocalTransport::open` refuses controller-side (Q-H2: `cargo run
  --local`).
- `telepatch::testing::sandbox()` / `Sandbox::at(dir)`,
  `LocalTransport::sandboxed(_at)`, `SshTransport::sandboxed(_at)`;
  every shell-opening test moved; `tempfile` is a normal dependency.
- `tests/sandbox.rs` (7 proofs); `just check-sandbox` (read-only
  inventory before/after both repos' tests).
- Snapshots: additions only, four `tp_sbx` line shapes (telepatch 15
  files +66; tp_basic 140 files +1273); T126 moves them into init.
- Gate E green: telepatch+tp_basic 1058 passed, lint clean;
  check-sandbox unchanged.
- Commits: telepatch c1bb04f; tp_basic e947ef8

## 2026-11-13 — T127 tp_basic: fix four clippy errors and add `just lint`

- `admin_users.rs`: a doc line no longer starts with `>` (two
  doc-quote errors); the doc comment's tab is written `<TAB>`.
  `rust_kernel.rs`: unused `sh` import dropped. No behaviour change.
- tp_basic `just lint`: `cargo fmt --check` and `cargo clippy
  --all-targets -- -D warnings`. `cargo fmt` reformatted `docker.rs`,
  `golang.rs`, `tests/hacks.rs`, `tests/plans.rs` (formatting only),
  which the recipe's `fmt --check` needed.
- Gates E and B now include tp_basic `just lint` (roadmap Common gates).
- Gate B green: tp_basic 52 passed, plan snapshots unchanged, lint,
  check-conversion and oracle self-tests ok.
- Commits: tp_basic 4c4ac61

## 2026-11-13 — T125 The flaky `op_kill::a_termed_op_still_prints_its_trailer_with_rc_143`

- Engine at fault (the step's rule): `Watch::seen` stopped at a
  signalled op's trailer while group members remained. Now the trailer
  is held and TERM/KILL continue until the killer reports the group
  gone; later ops' events are replayed after the result, in order.
- Engine test `limits::a_child_that_outlives_the_ops_trailer_is_still_killed_after_the_grace`
  (executor_local.rs): RED stopped at 1.03 s with the child alive.
- The test's own race fixed too: `term_until_end` TERMs until the group
  is gone; the op starts `sleep 300 &` before `echo up`.
- RED under load: 1 failure in 65 runs (8 parallel, 16 CPU burners).
  GREEN: 1000 runs, 0 failures; `cargo test --workspace` 5x green
  (1007 passed each); lint, tp_basic test+lint, check-sandbox ok.
- Commits: telepatch 4c502d0

## 2026-11-13 — T126 Undo and fetch helpers defined once per shell, not in every op script

- `init_shell` defines `ops::undo::HELPERS` (incl. T124's `tp_sbx`) and
  sets `__tp_hv` (FNV-1a 64 of the text) on every shell: root, login
  and its reopens, post-reboot root, cleanup's fresh root, the killer.
- File, unit, repo and the undo preparation op start with one guard
  line; a stale hash or missing `tp_save` ends the op with rc 125, a
  trailer and `telepatch: shell helpers missing or stale (want <hash>)`.
- Plans with a capturing op show `shell helpers: undo <hash8>` once
  per host. `trailer::with_helpers` for hand-run scripts. Trailer
  PRELUDE stays per op (Q-H1). The note describes the helpers once.
  Fetch `place`/`unpack` carried no copy (they run inside the file op).
- Snapshots audited line by line: telepatch −1580 helper lines (20 ops
  × 79), +20 guard, +9 header; tp_basic −32153 (407 × 79), +407 guard,
  +140 header; nothing else.
- Gate E green: telepatch 1014 passed, lint; tp_basic 52 passed, lint;
  check-sandbox unchanged.
- Commits: telepatch c2e6aea; tp_basic 509b077

## 2026-11-13 — T128 Sandbox gaps: login shells' `env -i`, and the live tests

- Confirmed: `as_user("root")`'s login shell (`exec env -i … bash -l`)
  lost `TP_SANDBOX_REQUIRED`, `TP_SANDBOX` and every path seam. No test
  had hit the real stage only by luck (no login-shell op overwrote an
  existing file; login_shell.rs's profile exported the seams).
- `sandbox::CARRY` + `carry_words()`: the `env -i` line carries each of
  ten variables the root shell has set, as `${N+N="$N"}`; none in
  production. Note updated.
- Tests: `login_shell::the_login_shell_keeps_the_sandbox_and_the_path_seams`
  (RED: all unset at HEAD), a capture test, a unit test for the words.
- `tests/live/inventory.rs`'s local factory is sandboxed; the other
  live files open no local shell. `--ignored --list` compiles (44).
- `check-sandbox.sh`: non-fixed mount points listed only when they
  hold a `.telepatch` (host docker mounts made it fail falsely).
- Gate E and check-sandbox green.
- Commits: telepatch bce318b

## 2026-11-13 — T119 Downloads land on their target's filesystem

- An item follows its first consumer's target (`tp_fsroot` from the
  nearest existing ancestor). On another root R: download into
  `R/.telepatch/tmp/<run>/fetch/`, verified, renamed into
  `R/.telepatch/cache/sha256/<sum>`; `<stage>/<run>/f-<sum>` (or
  `u-<hash>`) links to it; R indexed in `fsroots`. Lookup: R's cache,
  main cache, download onto R. Unwritable R blocks the host
  (`cannot stage on <R>`), never a fallback to the stage.
- New paths checked by `tp_sbx`; prepare script starts with the guard
  line; nothing joined HELPERS. Op lookup stays one line and notes
  `staged on <R>, target now on <R2>`. Stale sweep also runs at the
  first fetching window (T106 A14). binary_release stages on its
  install dir. A pkg-from-file consumer keeps the item on the stage (A1).
- Tests: 9 in fetch.rs, 1 in binary_release.rs (red 8+1, green 48/21).
  tp_basic `undo_fs` gains the VM fetch case (written; runs in T104).
- Snapshots: tp_basic edge 13 lookup lines; undo_fs 13 files for the
  new ops; telepatch none. Gate E and check-sandbox green.
- Commits: telepatch 14e1987; tp_basic 573f0e6

## 2026-11-13 — T121 Cache registry, usage record and 90-day expiry

- `tp_one` reports `fuse <sum> <dir>` per hit or download and registers
  a new per-fs cache in `<main>/../caches` (lock `.caches.lock`).
- End-of-run root-shell probe before `cleanup_stage`: per directory,
  `flock -x -w 30`, merge uses into `.telepatch-usage` (v1, sum, time,
  owners), adopt unrecorded files, drop gone lines, tmp+mv, then remove
  lowercase-64-hex regular files unused > 90 days. Corrupt record set
  aside, nothing removed. Only the main cache or
  `*/.telepatch/cache/sha256` lines are processed. `--check` and a dead
  root shell record nothing. Shared run lock in the root shell's init.
- Tests: tests/cache_usage.rs (15). Red 12/14 (impl.), FOREMAN red
  reruns; green. No snapshot moved. Gate E and check-sandbox green.
- Commits: telepatch 2fe54c4

## 2026-11-13 — T122 A playbook owns what it caches; what it stops fetching is removed

- Items a run of P used get P in their owners. On an entitled run (real,
  no `--only`, engine Ok, host ok/changed, record lock held), an item P
  owns whose sum is not in F (P's whole trace on this host) loses P;
  with no owner left it is removed (`no longer fetched by P`). Other
  runs say `cache: ownership not checked (<reason>)` once.
- A playbook name with a tab, comma or newline (or empty) is a trace
  error. Note: "Cache ownership".
- Tests: cache_usage.rs 28 (10 new red then green). By-design test
  changes in binary_release.rs/fetch.rs (an older tarball is removed).
  Gate E and check-sandbox green; no snapshot moved.
- Commits: telepatch 7a24837

## 2026-11-13 — T123 `cache usage` and `cache purge` over an inventory

- `tp-basic cache usage|purge` with run's targets; one root shell and
  one inline script per host (`src/cache_cmd.rs`, scripts in
  `ops/fetch.rs`, table/JSON in `report.rs`).
- usage: items, bytes, owners, oldest/newest per directory, totals,
  `--json`; read-only. purge: `--yes` required (else a dry listing),
  exclusive run lock without waiting (`in use by a running run`),
  removes 64-hex items, records, locks, empty per-fs `.telepatch`;
  never `.telepatch/tmp`; `--owner P`. Exit 0/1/2.
- Tests: 14 in cli.rs (red 12/13, green). tp_basic test-vm.sh VM case
  written (runs in T104), self-test green. Gate E and check-sandbox
  green; no snapshot moved.
- Commits: telepatch fbc8725; tp_basic bd1cefd

## 2026-11-13 — T116 `--max-fail` needs `--serial`; an unknown `--only` role is a usage error

- `--max-fail` without `--serial`: exit 2 before anything opens.
- `#[role]` registers its name in a `linkme` slice (via
  `telepatch::__private`); `telepatch::roles()` lists them sorted.
  `run --only x` with no such role: exit 2, `roles: …`, closest (edit
  distance <= 2) first, checked before inventory or any transport.
- A real role a host's trace never reaches: `<host>: --only x: not
  traced on this host`, one closing warning if no host traced it, exit
  unchanged. T100's run_controls tests adapted.
- Tests: cli.rs 2 new, run_controls.rs 8 new/adapted, tp_basic
  roles_registered.rs. Red then green (FOREMAN rerun red). Gate E and
  check-sandbox green; no snapshot moved.
- Commits: telepatch 30611ac; tp_basic affd0a3

## 2026-11-13 — T117 Rocky 9 test image: `rockylinux/rockylinux:9`

- tp_basic `scripts/test-edge.sh` `edge_image` (the live map; not in the
  step's Touches), the matrix self-test map and README.md move to
  `rockylinux/rockylinux:9`. Self-test red (map) then green.
- Live: `just docker-vm test-edge-cell rockylinux9` on veritech04, one
  1 GiB VM, deleted and verified gone. FAIL on the Ansible side at the
  edge Red Hat package task: `No package mailx available` and `curl`
  vs the image's `curl-minimal`. Both are EL9-wide, so the old image
  fails the same way (inferred; no old-image cell was ever run). The
  telepatch side is unproven on Rocky 9.
- Gate B green.
- Commits: tp_basic ccb71bd

## 2026-11-13 — T118 Reboot reporting: a host that did not come back fails, and a stop says why

- No new boot id by the deadline, or a failed `systemctl reboot`:
  `Failed`, op `h.reboot().if_pending() at <site>`, rc -1, counted by
  `--max-fail` (supersedes T102 A2).
- Mark lines `subject\trole\tfile:line`; old tabless lines read as
  "marked by an earlier run"; dedup by subject.
- Barrier stop block (why / barrier / marked by / not run / to finish)
  per cause never, local, docker; events gain `barrier_site`, `cause`,
  item objects `{subject, role, site}`. End-of-run list matches.
- Tests: executor_local.rs (5 new), executor_ssh.rs (3 new, old adapted).
  Red by FOREMAN against stashed src; green. Gate E and check-sandbox
  green; no snapshot moved.
- Commits: telepatch abf7c9c

## 2026-11-13 — T120 `h.as_user(u)` on the local transport

- `LocalTransport::open` serves `Principal::User(u)`: the calling user
  (`.user()` seam, else `id -un`) gets T114's `env -i … bash -l` with
  T128's carry words and no sudo (shared `executor::start_login`);
  another user gets `sudo -n -iu u bash`, no runuser. A refusal or a
  missing sudo is `Limit::SudoRefused` with ssh's text adapted
  (`LocalTransport::message()`). The controller-side sandbox refusal
  covers the User path. Note: "one mechanism" paragraph.
- Tests: tests/local_user.rs (16; red 14/16 rerun by FOREMAN, green),
  shell_pool.rs and executor_ssh.rs adapted. No real sudo or second user.
- tp_basic: test-only `local_user`/`local_user_nologin`, the
  `test-vm.sh` case (written; runs in T104), self-test green.
- Gate E and check-sandbox green; no snapshot moved.
- Commits: telepatch 8f55549; tp_basic 81799b1

## 2026-11-13 — T134 The timing bound in two `executor_ssh` timeout tests

- No engine fault: phase trace under T125's load (8 copies, 16 burners)
  worst 1.84 s end to end; only TERM→gone grows (host-side kill script,
  median 372 ms loaded); controller re-polls at once. Table in the report.
- The load failures were cross-kills: `Reaper("sleep 4.31")` killed other
  tests' live sleeps. Now `sleep 300.<pid><n>` with anchored patterns, a
  60 s hang guard in place of `< 3800 ms`, and the op's sleep checked gone.
- Red (old, loaded) 20/40 failed; green 0/40; loop 2000 runs, 0 failures;
  E 5/5 green; check-sandbox unchanged.
- Commits: telepatch aba83ca

## 2026-11-13 — T131 A package file follows the same staging rule

- `Item.pkg_file` removed: an item lands on its first consumer's
  filesystem in window order; a pkg-file consumer counts as the stage's.
  The pkg op reads via the `f-<sum>` stage link, else the main cache
  (`fetch::LOOKUP_PKG`). Note `d-fetch` updated.
- Tests: tests/fetch.rs, 3 new (shared URL with a file op first, staged
  on fsB with usage recorded there; pkg alone; pkg first). Red by
  FOREMAN with src stashed (download on the stage, not fsB); green.
- No snapshot holds a pkg-from-file script: none moved in either repo.
  Gate E and check-sandbox green. T130 (held by XO) not built.
- Commits: telepatch 559e2b2

## 2026-11-13 — T133 The edge cell on EL9: rockylinux9 passes, both sides

- EL9 (RedHat, major 9) asks for `/usr/bin/curl` (curl-minimal provides
  it; `curl` conflicts) and `/bin/mailx` (EL9's s-nail provides it;
  nothing provides `mailx`), in the Ansible task and the role alike;
  EL10, Fedora, Amazon keep `curl`/`mailx` (Q-E1 at its default).
  Evidence from `dnf repoquery` in both EL9 images (one probe VM, deleted).
- telepatch: `installs_downloader` also takes an absolute name whose file
  name is curl/wget, so the EL9 window still splits; test in fetch.rs,
  red then green; note updated.
- tp_basic: plans.rs and check-edge red then green; snapshots edge
  rockylinux9/almalinux9, names only. Gate B, E half, check-sandbox green.
- Live: rockylinux9 PASS (149 s), almalinux9 PASS (237 s), each its own
  1 GiB VM, second run CHANGED 0, slices identical; VMs deleted.
- Commits: telepatch 6b8770b; tp_basic bac7591

## 2026-11-13 — T137 The last shared `sleep 4.31`/`4.32` patterns in `executor_ssh`

- `a_timeout_after_the_barrier_is_still_killed_through_a_second_shell_of_the_new_boot`
  and its playbook use T134's `long_sleep` (`300.<pid>3`, `300.<pid>4`,
  anchored `Reaper`s); the 8 s bound is `HANG_GUARD`; both sleeps are
  checked gone. `grep 'sleep 4\.3[12]' tests/` finds nothing.
- Loop (executor_ssh binary filtered to the three timeout tests, 200 runs,
  8 at a time): old 183/200 failed, new 0/200.
- Gate E and check-sandbox green.
- Commits: telepatch 368a177

## 2026-11-13 — T135 The cache without locks or records: mtime is the last access, owner directories say who uses an item

- Resumed from the parked branch `t135-wip` (7c416c6), folded into one
  commit with the amendment; branch deleted, tp_basic stash dropped.
- No locks and no record: prepare's hit or download touches the item's
  mtime and creates `owners/<owner_dir(playbook)>/<sum>`; the registry
  is appended without `flock` and never rewritten by a run. `RUN_LOCK`,
  `USAGE`, `TP_OWN` and the record tests are deleted.
- End-of-run probe: legacy record and lock files removed on sight,
  90-day expiry by mtime with the markers, ownership cleanup by marker
  on entitled runs.
- Owner directories are the playbook name percent-encoded on the
  controller (`owner_dir`/`owner_name`); `/` stays legal, so tp_basic's
  `container/*` names are unchanged. Encoded name at most 200 bytes.
- An op whose cached download vanished (`.from`, `.unpack`, pkg file,
  binary_release) fails with the gone-file message.
- src: 8 files +247 −289. Snapshots: telepatch 1 (2 lines), tp_basic
  39 (65 lines), each the gone-file check. Gate E and check-sandbox green.
- Commits: telepatch faa3985; tp_basic 9ef74a5

## 2026-11-13 — T136 `cache usage` is a disk-space report; `cache purge` without locks

- `cache usage`: per host, each cache directory's `du -sb` (owner
  markers included), one `working` line (stage plus every filesystem's
  `.telepatch/tmp`, dead runs' roots from `<stage>/*/fsroots`), host
  total, inventory total. `--json` in bytes only. No items, owners or
  last-used times.
- `cache purge --yes`: no lock, no record; removes items, `owners/`,
  legacy record and lock files; rmdirs an emptied per-filesystem tree;
  registry and working directories untouched. `--owner P` removes
  `owners/<owner_dir(P)>/` and only items no other owner marks.
- Deleted: `tp_recload`, `tp_pown`, `tp_odel`, every `flock` line, the
  "in use" outcome, `utc_minute`, per-item reporting. src 4 files
  +232 −413. `flock|telepatch-usage|RUN_LOCK` in src: 3 hits, all
  remove-on-sight of the legacy record.
- Gate E and check-sandbox green. tp_basic unchanged.
- Commits: telepatch 7800867

## 2026-11-14 — T104 Goals 8 and 9 proofs (run; step kept open)

- Every proof ran on veritech04, 1 GiB tp-* VMs, one at a time. Passed:
  `test-live-ssh` 6/6, incl. T101's timeout kill over ssh + `sudo -n -H`;
  the other 12 telepatch `test-live-*`; tp_basic `test-edge` (CHANGED 0,
  identical); `test-services`; `test-vm local_user`; `test-vm undo_fs`
  with the T136 cache case; `test-vm-oracle dev_desktop_kept` (identical,
  3 intended); `test-vm dev_desktop` with `--check` 0 / `--check --diff` 2.
- Fixed on the way (test or harness faults, no engine change): live
  user test expected the pre-T88 line form; undo_fs read root-only paths
  without sudo and expected run 2 CHANGED 0 despite its listing sh!; the
  oracle's VM names held `_`; test-vm.sh gained the T136 cache checks and
  the dev_desktop --check proof.
- Not proved: the main-cache `cache usage` line (no main cache exists in
  the undo_fs case; GAP); one unexplained ~10 min hang in a docker-
  transport run (`unit_rocky`, not reproduced; suspect `pool/mod.rs:416`
  close vs docker exec stdin EOF). openbao failed once in the shared
  batch VM (`docker create` exit 1, stderr not captured), passed fresh.
  The step stays queued. veritech04 vt-* set unchanged; no tp-* left.
- Commits: telepatch 293866f; tp_basic 2dd7ccd

## 2026-11-14 — T138 A shell close that cannot wait forever; the openbao test shows its stderr

- `Shell::close` types `exit`, drops stdin, waits at most 10 s for the local
  child, then `start_kill()` with a 5 s grace; returns whether the bound was
  hit. `set_close_bound` (tests only), `argv0()`.
- `close_shell` in executor.rs: one warning line and one `shell_close_killed`
  event for root, login <u>, killer, the probe shell, the reboot discard and
  the cache shell; host status unchanged. Other closes are bounded silently.
- openbao live test: status, stderr and stdout in every failing assert.
- Tests: tests/shell_close.rs (3), executor unit test. Red: HEAD's close
  hung on the EOF-lost stub until the test's 8 s bound. Gate E and
  check-sandbox green.
- Commits: telepatch 3827fbd
