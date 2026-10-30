# telepatch

Configuration management for people who are tired of waiting for Ansible.

Roles and tasks are plain Rust. Running a playbook traces it into a per-host
graph, merges what can be merged, checks everything that can fail before
touching a host, and then streams the result into persistent shells over SSH.
Nothing is installed on the targets.

**Status: design.** There is no code yet. The design is in
[`docs/execution-model.html`](docs/execution-model.html) and this README is a
summary of it.

## Why

Ansible pays a full connect, copy module, run, collect, disconnect cycle per
task per host, buffers output until a task finishes, and has no way to see
that five roles each want to `apt install` something and do it once. For a
forty-task playbook on a hundred hosts that is thousands of round trips and
tens of `apt` invocations per host, and you cannot tell whether a slow task is
working or hung.

telepatch keeps the vocabulary (variables, roles, tasks, playbooks, inventory)
and changes the execution model:

- **Code, not YAML.** A role is an `async fn`. Distro dispatch is a `match`,
  per-user work is a `for`, parsing stdout is `.split(':')`.
- **Trace, then plan.** Ops record themselves instead of running. The engine
  sees the whole playbook before anything executes, so package installs
  across all roles collapse into one `apt` call per host.
- **Persistent shells.** One SSH connection per host, a root shell and a
  login shell per user the playbook touches, opened once. Ops are pipelined
  into them the way an operator would type them.
- **A terminal per op.** Ops that a human might watch get a PTY; the raw
  stream is logged with timestamps and replayable in a TUI or web UI.
- **Fail before you change anything.** Secrets, downloads, template
  rendering, facts and permission checks all run and must pass on every host
  before the first mutation.
- **Roles roll back.** Each primitive captures what it is about to change.
  A failed role on a host is undone on that host, and the plan says up front
  which ops are not reversible.

## What a role looks like

```rust
#[role]
async fn rust_build(h: &Host) {
    match h.os.family {
        Debian => h.pkg(["build-essential", "libclang-dev"]),
        RedHat => h.pkg(["gcc", "clang-devel"]),
        Arch   => h.pkg(["base-devel", "clang"]),
    }.installed();

    for u in h.vars.admin_users.iter().chain(["root"]) {
        let s = h.as_user(u);
        let want = &h.vars.rust_version;
        sh!(s, "curl -sSf https://sh.rustup.rs | sh -s -- -y", unless = "which cargo");
        sh!(s, "rustup update", unless = "rustc --version | grep -q {want}");
        for tool in ["cargo-deb", "cargo-generate-rpm"] {
            sh!(s, "cargo install --locked {tool}", creates = "~/.cargo/bin/{tool}");
        }
    }
}
```

Structured ops (`pkg`, `file`, `git`, `fetch`, `svc`, `user`) are builders
with typed options and known inverses. Anything that is really a shell line is
`sh!`, with shell-quoted interpolation and `unless` / `creates` / `changed_if`
so check-then-act is one line.

## How it runs

1. **Trace.** The playbook runs on the controller and records ops. Fact reads
   about state the playbook itself wrote are answered from the trace; reads of
   pre-existing state sync with the host. Conditionals on whether an op
   changed something are guards on the dependent op, not control flow.
2. **Prepare.** Per host, in parallel: resolve secrets, fetch downloads to the
   host and verify checksums, render templates, gather facts in bulk, check
   `sudo`. Any failure stops the run with nothing changed.
3. **Apply.** The per-host graph is optimized (effect-based ordering, merge
   of same-kind ops, hosts in parallel) and streamed into the host's shells.
   Each op reports a typed result in a nonce-tagged trailer.
4. **Roll back** a role on a host if one of its ops fails, from an undo log
   built as the role ran.

## Primitives

Sized to the roles in the existing Ansible repo this replaces.

| Primitive | Replaces |
|---|---|
| `pkg` | apt, dnf, yum, pacman, build-dep, pip |
| `repo` | apt_key, apt_repository, the keyring + sources.list dance |
| `file` | file, template, copy, lineinfile, replace |
| `sh` | shell, command, and the hand-rolled check-then-act around them |
| `svc` | systemd, every `shell: systemctl …`, daemon-reload handlers |
| `user` | user, authorized_key |
| `as_user` | become_user + `getent passwd` + `$SHELL -lc` |
| `secret` | vars_prompt, `bao read` via delegate_to |
| `fetch` | get_url, uri, local_action get_url |
| `git` | `shell: git clone …` with `creates:` |

## Documentation

- [`docs/execution-model.html`](docs/execution-model.html): the design note.
  Tracing and the fact resolver, shells over SSH, the engine loop and
  optimizer, the Ansible corpus survey, primitives, syntax, prepare phase,
  transactions, error handling, and what to prototype first.

## Non-goals, for now

- Running anything other than a shell on the target.
- Non-Linux targets beyond what their shell happens to support.
- Resuming a run after connection loss (it rolls back instead).
- Speculative planning across runs. The design leaves room for it; the
  measured count of blocking fact reads decides whether it is ever built.
