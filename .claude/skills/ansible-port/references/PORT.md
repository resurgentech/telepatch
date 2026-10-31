# Phase 2: port (v2, built on `tp import`)

Start only when the doc's `Decision:` line exists. Port what it decided;
record deviations in `## Port notes`.

v1 applied the mapping table by hand. v2 starts from `tp import`, which
does the mechanical part, and leaves a `// TODO(port)` for everything it
will not guess. The porter's job is the TODOs, the structure, and the
proof. The mapping table stays: it is how you read what the emitter wrote
and how you write what it did not.

What is built: `#[role]`, `Host` and its facts, `h.pkg`/`h.yum`/`h.pip`,
`h.file`, `sh!`, `as_user`, `barrier`, `.reads/.writes/.allow_fail`,
the `plan` command, and the equivalence harness. Not built, and written
as a note in the role instead of faked: guards and `notify`, and the
`repo`, `svc`, `user`, `fetch`, `tpl!` and `git` primitives. Use `sh!`
with `unless`/`creates` for those and say in Port notes which primitive
replaces it later.

## Budget the work

The emitter is good on package lists, distro `include_tasks`, and
`register` + `rc != 0` pairs. It is poor on runtime state. Read the
`--report` before starting and budget from it:

- `rust_kernel`: 3 tasks, 0 TODO. The port is review plus structure
  (users, comments). Minutes.
- `dev_common`, `kernel_build`: 1-2 TODOs, all "unreachable branch".
  The port is deleting the dead branches, restructuring the flag chain
  into `if/else if`, deduplicating, and moving the understanding in.
- `src_repos`: `find`, `when` on a registered result, `apt` with
  `update_cache`. The port is a redesign of one op (see Resolving).
- `rust_build`: 28 of 29 tasks TODO (`set_fact`, `user_shell` Jinja,
  `get_url`, `when` on facts it computed). Treat the emitted file as a
  map of which YAML lines exist, not as a draft. The hand port is a
  fresh design, driven by the understanding doc's decision, with about
  a tenth of the YAML's tasks left.

A report with many `set_fact` or `when not translated` lines means the
role encodes a state machine in variables. Expect to replace it with
facts (`bin_exists`, `bin_version`) and an if/else, not to translate it.

## Steps

### 1. Run `tp import` into a scratch module

```
tp import <roles-dir>/<role> --roles-path <roles-dir> --out <scratch> --report
```

- The input is a role directory, a directory of roles, or a playbook.
  `--roles-path` says where roles live when it cannot be derived.
  `<scratch>` is outside `src/` (for example `build/import/<role>/`) so
  it is not compiled into the playbook repo. `--report` alone (no
  `--out`) prints only the report.
- The output is `lib.rs`, `flags.rs` (the `group_vars` distro flags as
  fns, each with its `file:line`), and one `<role>.rs` per role. It
  refers to `crate::vars::Vars`, which it does not write: your playbook
  repo already has `src/vars.rs`.
- Read the report first: per role, tasks, mapped, TODO, and each TODO
  with `file:line` and a reason. Skipped files are listed at the foot of
  `lib.rs` as TODOs too.
- To check the draft compiles, drop it into a throwaway crate beside a
  copy of `vars.rs`, or read it; you do not ship it. Never edit the
  scratch module in place and commit it.

### 2. Resolve each TODO

Take the TODOs in report order. For each: open the YAML at the cited
`file:line` (under `conversion/ansible/`), read the understanding doc's
Preserved knowledge and Decision for that task, then do what the row
says. The reasons the emitter produces, and only those:

| Reason in the report | What it means | Resolve by |
|---|---|---|
| `unreachable branch` `` `flag` `` | A distro flag no image reaches (`debian10_based`, `amazonlinux2_based`). The code is emitted anyway, under the flag | Confirm in the doc that it was dropped (Open question 3 default). Delete the branch and write one line in the module comment saying which YAML lines were dropped and why |
| `unmapped module` `` `x` `` | No emitter mapping (`find`, `get_url`, `uri`, `template`, `service`, `user`, `git`, ...). The task is a comment with its arguments | Read what the task is for in the understanding doc, then write the idiomatic form: a `sh!` with `unless`/`creates`; a `h.file(..)` op; or fold it away when the doc says it only fed a later `when` (`find` + `matched > 0` becomes a guard inside the op's `unless`). `get_url` + run script becomes one `curl ... \| sh` in a `( set -o pipefail && ... )` subshell |
| `unsupported arguments` | The module is mapped but an argument is not (`apt` with `cache_valid_time`, `update_cache`) | `update_cache` after a sources edit is the engine's apt cache rule: declare what the edit writes (step 3) and drop the task. Other arguments: decide against the doc; say which in Port notes |
| `when not translated` | A `when` the emitter could not turn into Rust. The statement is emitted inert, under `if false /* TODO */`, so it is never traced | Never leave `if false`. Decide what the condition means: a distro flag it did not know becomes a flag fn; a test on a registered command's output becomes a fact (`s.bin_exists(..).await`, `s.bin_version(..).await`, `h.exists(..).await`) and an `if`; `rc != 0` on a check the emitter could not fold (`check not foldable`: different user, several consumers) becomes `unless = ".."` on the acting op, spelled by hand |
| `Jinja not translated` | An expression other than `{{ var }}` of a known variable, `item`, or a loop var (`{{ user_shell }}` from a `set_fact`, filters, concatenations) | Replace with a Rust value (`h.vars.field`, a local `let`) interpolated as `{name}` in `sh!`, or remove it when the doc says the value is telepatch's job (the login shell is `bash -l` inside the persistent shell, so `{{ user_shell }} -lc` goes away) |
| `set_fact` | A runtime variable. No mechanical mapping | Find who reads it. Usually a derived flag for a later `when`: replace the pair with a fact read and an `if`; or a value from `getent`/`command -v`: compute it inside the op's shell line at run time. Do not mirror it as a Rust mutable |
| `task-level loop` | `loop:` on a task (not on an include). The statement is inert | Write a Rust `for` over a literal list or `h.vars` field; interpolate the loop variable with `{x}` (quoted). A loop over home directories is better as one shell line over the passwd home (see rust_build, `rm -rf` of `$home/.rustup`) |
| `loop not translated` | `loop:` on an `include_tasks`, expression not understood | Same: a `for` over the vars field. The emitter understands `admin_users` and literal lists |
| `guard` (`notify:`) | A handler is notified. Guards are not built | Keep the order, put `// TODO(guard): <handler>` where the restart goes, and list it in Port notes. Do not run the handler unconditionally |
| `handler` | A handler task. Same | Same: a comment only; the handler's command is in the comment |
| `delegate_to` | The task ran on another host. The emitter never guesses | Not portable into a per-host trace. Decide in the doc (Open question) or leave a comment and record it in Port notes as not ported |
| `failed_when` | The YAML's failure rule is not carried over | `.allow_fail()` when the YAML said "never fail"; otherwise write the rule into the shell line (`cmd \|\| [ $? -eq 2 ]`) |
| `changed_when` | A `changed_when` that is not `false` and not `'<text>' in reg.stdout|stderr` | Express as `changed_if = "regex"` on the text the tool really prints (read it on a live run: stderr and stdout are combined) |
| `galaxy_role`, `vars_prompt`, `unresolved_include`, `include_cycle`, `no_module`, `unparseable_file` | Constructs the loader skipped (kind names as listed at the foot of `lib.rs`) | Decide each against the doc. `unresolved_include` means a task file the loader could not find or resolve (a templated name): open it by hand and port it, since its tasks are missing from the output |

If a reason appears that is not in this table, resolve it from the
understanding doc and add a row to `## skill-feedback`.

The emitter also makes silent choices you must check, because they are
not TODOs:

- A `// ...` note above an op ("the YAML tested stderr; changed_if sees
  combined output", "subshell keeps pipefail out of the persistent
  shell"). Each is a decision; keep it as a code comment or fix it.
- `changed_if` is derived from `'text' in stdout`. The text the YAML
  tested may never have matched (`installing` where rustup prints
  `downloading component`). Run the command and check; when the YAML was
  wrong, the oracle cannot see it and the fix belongs in the port.
- Distro flags are emitted as if/if chains, one `if` per `include_tasks`.
  The YAML's branches are exclusive per image; collapse into one
  `if/else if` chain once you see it.
- Package lists are copied as written, duplicates included (`rsync`
  twice). Decide each duplicate against the doc.
- Task-level `when` on an include scope the emitter understood may still
  encode a precedence you have to read (`a and not b`).

### 3. Rewrite into idiomatic Rust in the playbook repo

The emitted file is a first draft. The result lives in the playbook
repo: `src/roles/<role>.rs` (declare it in `src/roles/mod.rs`) and the
playbook entry in `src/playbooks/<dir>.rs`, in YAML order. The
conventions the hand ports settled:

- **Flag fns.** One private fn per `group_vars` flag the role uses,
  copied from `flags.rs` with its `file:line` doc comment, so a flag
  reproduces `inventories/group_vars/all` exactly (Amazon outside
  `redhat_based`, `debian_based` testing the distribution, `arch_based`
  testing the family). Copy only the ones used; the emitted `flags.rs`
  holds all. If the same flag fn is wanted by two roles, a copy in each
  module is what the hand ports do; pull it into one shared module only
  when a third role needs it.
- **Branch per image.** `if debian_based {..} else if redhat_based {..}
  else if ..` with one comment citing the YAML lines. An image no flag
  matches gets nothing; that is the YAML's behaviour.
- **Users.** `admin_users` then `root`, each once, in that order:
  `fn users(vars: &Vars) -> Vec<String>` deduplicating (Ansible's
  two loops ran root twice when `admin_users` held it). Per user:
  `let s = h.as_user(&u);`. Ops on `s` run in a fresh login environment
  of `u`: `~` is `u`'s home, `~/.cargo/bin` is on PATH if the profile
  says so. Do not `su` or `sudo -u` by hand.
- **`sh!` quoting.** `{var}` interpolation shell-quotes the value, so
  interpolate a name, a path or a version, never a whole command or a
  fragment with operators. A command that needs shell syntax is a
  literal. Literal braces are doubled (`{{` `}}`, `${{HOME}}`).
- **`~` under `as_user`.** In `creates = "~/.cargo/bin/bindgen"` the
  tilde is left unquoted so `u`'s shell expands it. Do not rewrite to
  `/home/{u}`: the home comes from the passwd entry, not a guess.
- **Facts.** `s.bin_exists("cargo").await`, `s.bin_version("rustc").await`
  and `h.exists(p).await` are a round trip to the host. A fact read
  forces the ops it depends on to complete first (an implicit barrier),
  and the plan shows it as a blocking read. Prefer a literal `unless =
  "command -v x"` on the op when the check and the action are one
  decision; use a fact only when the branch picks between different
  ops (absent, broken, working).
- **`.writes([..])`.** A `sh!` op writes `*` unless narrowed, and an op
  that writes `*` fences everything around it (an implicit barrier) and
  triggers the apt cache refresh for the next apt op. When the YAML
  edits one known set of files, declare it: `.writes([Key::file(GLOB)])`
  keeps the refresh (the next apt op sees a changed sources file) and
  does not fence the package installs around it. Add `.reads([..])` when
  a shell line needs a package the plan must install first.
  `h.barrier()` only where order must hold across roles and no key
  expresses it.
- **Idempotency.** Keep what the YAML had. `creates`/`unless` the YAML
  did not have are an "after equivalence" change (the `uv` installer
  stays unconditional on the first port). Comment it, change it
  afterwards in its own change.
- **Comments are the understanding.** Each kept fact becomes one comment
  naming the reason, with the YAML `file:line` range (`debian.yml:1-50`),
  next to what implements it. Do not paste YAML.

### 4. Build and plan

```
cargo build
tp-basic plan <playbook> --distro <d>          # e.g. container/rust_build
```

`<playbook>` is the registered name (`container/<name>`). Read the plan:
role order, package sets and shell lines per window. Compare with
Outcomes. Then add the snapshot test: `tests/plans.rs` has one
`<name>_plans` test calling `plan_for` for each of the 13 distros
(`debian12 debian13 ubuntu2404 ubuntu2604 fedora43 fedora44 rockylinux9
rockylinux10 almalinux9 almalinux10 amazonlinux2023 arch omarchy`) and
`insta::assert_snapshot!`, with the snapshot path
`tests/plans/snapshots`. Accept the new `.snap` files only after reading
them against Outcomes (`INSTA_UPDATE=always cargo test --test plans`,
then `git diff` / read). A role whose behaviour depends on a fact
(`bin_exists`, `bin_version`) also gets one test per branch, with
`FakeExecutor` answering the facts (see `rust_build_with` in
`tests/plans.rs`).

When re-porting a role that already has snapshots, run the tests with
`INSTA_UPDATE=no`: any difference is a failure to explain, not a
snapshot to accept.

### 5. Equivalence on the images

```
ANSIBLE_SHA=<commit from conversion/SOURCE> just test-container <distro> <container>
```

`ANSIBLE_SHA` is the commit in `conversion/SOURCE`, not the default
ansible `master`: the oracle compares the Ansible run of that commit with
the telepatch run. Without it the script warns when `master` differs from
SOURCE; heed the warning. It needs docker, the base image
`resurgentech/base-<distro>:latest` built locally, and `build/tp-basic`
from `just musl` (rebuild after every code change). Run it for each
acceptance distro the doc names (every image family at least: one
Debian-family, one RedHat-family, Arch) and for `<container>` as named in
`../docker_images/docker-bake.hcl` (the container name is not always the
playbook name: `rust.yml` is `rust_build`).

Read both diffs:

- GATING (`ansible.state` vs `telepatch.state`: installed names and
  arch, pip, rustup, apt sources, `/usr/local/bin`). It must be empty.
  A line here is a port bug or a bug the doc did not record; do not
  accept it into a snapshot.
- VERSIONS (non-gating `.versions` side diff): package versions. Not a
  failure, but read it: it is where a pinned-versus-latest difference
  shows. State in Port notes what it showed and why it is fine.

Outputs are under `build/test-container/<container>-<distro>/`
(gitignored). No docker available: say so in Port notes, say which
distros were not run, and leave the step open; the plan snapshots alone
are not equivalence.

### 6. Present the diff

In `## Port notes` write: the code files; what each YAML task became (by
`file:line`); each TODO and how it was resolved; the plan snapshot files;
per distro run, the gating diff verbatim or "empty", and a one-line read
of the versions diff; each decision that was the agent's (agent-alone);
each "after equivalence" item left as a comment. A port with TODOs left
unresolved is not done: list them under Open.

### 7. After green

Move each kept fact from the doc into a comment beside the code that
implements it (step 3), delete the role's doc and the mirrored originals
no other doc cites, run `just check-conversion`. Then make the "after
equivalence" changes, each in its own change with its own oracle diff.

## Mapping

| Ansible | telepatch |
|---|---|
| `apt`/`dnf`/`pacman` with `name:` list | `h.pkg(["a", "b"])`: manager from `h.os.family` (Debian apt, RedHat dnf, Arch pacman) |
| `yum` | `h.yum([..])` |
| `apt` with `state: build-dep` | `h.pkg([..]).build_dep()` |
| `state: latest` | `h.pkg([..]).latest()` |
| `update_cache: yes/no` | `.update_cache(true)`; the engine already refreshes the apt cache once before the first install and again after anything that writes the sources, so drop per-task flags unless the YAML relies on `no` |
| `pip` | `h.pip([..])`, `.latest()` for `state: latest` |
| `lineinfile` | `h.file(p).line("..")`; with `regexp:` `h.file(p).line_re("^re", "..")` |
| `copy` with `content:` | `h.file(p).content("..")`, then `.mode(0o644)` and `.owner("u")` if the YAML sets them |
| `file` `state: directory` / `absent` | `h.file(p).dir()` / `.absent()` |
| `shell`/`command` | `sh!(h, "cmd {var}")`; `{var}` is quoted by the macro, `{{` and `}}` are literal braces |
| `args: creates:` | `sh!(h, "..", creates = "path")` |
| `register` + `changed_when: false` + `when: rc != 0` (check, then act) | one `sh!(h, "act", unless = "check")` |
| `changed_when: "<text>" in stdout` | `sh!(h, "..", changed_if = "regex")` |
| `ignore_errors: yes`, `failed_when: false` | `.allow_fail()` |
| task knows what it touches (a shell line that installs a package) | `.writes([Key::pkg("x")])`, `.reads([..])` |
| `loop: "{{ admin_users }}"` with `become_user: "{{ item }}"` | `for u in users(&h.vars) { let s = h.as_user(&u); sh!(s, "..") }`; ops on `s` run in a fresh login environment of `u` |
| `become: yes` (root, no user) | the host handle itself (the run is root) |
| `include_tasks`/`when:` on distro flags (`debian_based`, `redhat_based`, ...) | a flag fn per flag, then `if/else if`; or `match h.os.family { Family::Debian => .., .. }` when the YAML tests the family; `h.os.distro` for a single distro |
| `when: ansible_distribution_major_version == ..` | a `h.os.version.major()` test |
| `when:` on a command's output | `h.bin_version("cargo").await`, `h.exists(p).await`, `h.bin_exists(c).await`: facts are awaited, ops are not |
| `roles:` / `import_role` | one `#[role] async fn name(h: &Host<Vars>)` per role, called in order from the playbook fn |
| ordering that must hold across roles | `h.barrier()` |
| `vars`/`defaults` | fields of the vars struct behind `h.vars` |
| `notify` / `handlers` | not built: `// TODO(guard)` where the restart goes, order kept |
| `git`, `service`, `user`, `get_url`/`uri`, `template`, `find` | not built: `sh!` with `unless`/`creates`; say in Port notes which primitive replaces it later |

### Sketch

```rust
#[role]
pub async fn dev_common(h: &Host<Vars>) -> Result<(), TraceError> {
    if debian_based(&h.os) {
        h.pkg(["build-essential", "cmake"]);
    } else if redhat_based(&h.os) {
        h.pkg(["cmake", "gcc"]);
    } else if arch_based(&h.os) {
        h.pkg(["base-devel", "cmake"]);
    }
    sh!(
        h,
        "(set -o pipefail && curl -LsSf https://astral.sh/uv/install.sh | UV_INSTALL_DIR=/usr/local/bin sh)"
    );
    Ok(())
}
```

(The installer stays unconditional on the first port, as the YAML ran
it; a `creates = "/usr/local/bin/uv"` guard is an "after equivalence"
change.)
