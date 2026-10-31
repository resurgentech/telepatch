# telepatch

<p align="center">
  <img src="docs/hero.svg" alt="A man with two fingers to his temple sends green waves to a retro computer whose screen reads telepatch" width="100%">
</p>

Configuration management in Rust, for people who are tired of waiting for
Ansible.

A role is an `async fn`. Running a playbook traces it into a per-host graph,
merges what can be merged, checks everything that can fail before touching a
host, and streams the result into a persistent shell. Nothing is installed on
the target.

**Status: experimental, 0.1.** The engine, the local and `docker exec`
transports, three primitives and the Ansible conversion tooling exist and are
tested. SSH, rollback, secrets and the rest of the primitive set are designed
but not built yet. See [What exists today](#what-exists-today) before you
depend on anything.

## Why

Ansible pays a full connect, copy module, run, collect, disconnect cycle per
task per host, buffers output until a task finishes, and cannot see that five
roles each want to `apt install` something and do it once. For a forty-task
playbook on a hundred hosts that is thousands of round trips, and you cannot
tell whether a slow task is working or hung.

telepatch keeps the vocabulary (roles, playbooks, vars, hosts) and changes the
execution model:

- **Code, not YAML.** Distro dispatch is a `match`, per-user work is a `for`,
  a condition is an `if`. The compiler checks it.
- **Trace, then plan.** Ops record themselves instead of running, so the
  engine sees the whole playbook before anything executes. Package installs
  across roles collapse into one package manager call per host.
- **Persistent shells.** One shell per host, plus a login shell per user the
  playbook touches, opened once. Ops are pipelined into them the way an
  operator would type them, and each reports a typed result.
- **Fail before you change anything.** Facts, package state and permissions
  are gathered up front. A host that cannot complete the plan is not started.
- **Live output.** Every op streams as it runs, prefixed by host, and the run
  is logged as JSONL for replay.

## A role

```rust
use serde::Deserialize;
use std::process::ExitCode;
use telepatch::facts::os::Family;
use telepatch::{Host, Playbook, PlaybookFuture, TraceError, role, sh};

#[derive(Deserialize, Default)]
struct Vars {
    #[serde(default)]
    admin_users: Vec<String>,
}

#[role]
async fn rust_build(h: &Host<Vars>) -> Result<(), TraceError> {
    match h.os.family {
        Family::Debian => h.pkg(["g++", "libssl-dev", "pkg-config", "clang", "musl-tools"]),
        Family::RedHat => h.pkg(["gcc-c++", "openssl-devel", "pkgconf-pkg-config", "clang"]),
        Family::Arch => h.pkg(["openssl", "pkgconf", "clang", "musl"]),
        Family::Other(_) => return Err(TraceError::new("unsupported distro")),
    };

    for u in h.vars.admin_users.iter().map(String::as_str).chain(["root"]) {
        let s = h.as_user(u);
        if !s.bin_exists("cargo").await {
            sh!(s, "curl -sSf https://sh.rustup.rs | sh -s -- -y");
        }
        for tool in ["cargo-deb", "cargo-generate-rpm"] {
            sh!(s, "cargo install --locked {tool}", creates = "~/.cargo/bin/{tool}");
        }
    }
    Ok(())
}

fn entry(h: Host<Vars>) -> PlaybookFuture {
    Box::pin(async move { rust_build(&h).await })
}

fn main() -> ExitCode {
    telepatch::cli::main(&[Playbook { name: "rust_build", run: entry }])
}
```

That is a complete playbook binary. Nothing in the role runs when it is
called: `h.pkg` and `sh!` append nodes to the host's trace and return
handles. A fact read such as `bin_exists` is answered from the trace when the
playbook itself wrote the answer, and from the host otherwise. Structured ops
are builders with typed options; anything that is really a shell line is
`sh!`, with shell-quoted interpolation and `unless`, `creates` and
`changed_if` so check-then-act is one line.

The binary it builds has three commands:

```
rust_build list                                  # playbook names
rust_build plan rust_build --distro rockylinux10 # the optimized plan, nothing applied
rust_build run  rust_build --local --vars v.toml # apply to this machine
```

`plan --distro` renders against an embedded os-release fixture for any of the
thirteen supported distros, so you can see what a host would get without
having one. `run --local` applies to the machine the binary is on, as root or
through `sudo -n`.

## How it runs

1. **Trace.** The playbook runs on the controller and records ops. Reads of
   state the playbook already wrote are answered from the trace; reads of
   pre-existing state cost one round trip. A read that needs a pending
   mutation's effect forces that mutation to apply first.
2. **Prepare.** Per host: gather the os, check package state, check what the
   plan will install, verify permissions. A host that fails here is not
   touched.
3. **Apply.** The per-host graph is ordered by effect (what each op reads and
   writes), same-kind ops are merged, and the windows are streamed into the
   host's shells. Each op runs in a subshell and ends with a nonce-tagged
   trailer carrying its typed result. A failure halts that host inside the
   shell itself; later roles on it are reported as not run.

The full design, including the parts not built yet, is in
[`docs/execution-model.html`](docs/execution-model.html).

## What exists today

| Area | State |
|---|---|
| Trace, resolver, engine loop, optimizer | Built, with snapshot tests and an adversarial plan suite |
| `pkg` (apt, dnf, yum, pacman, pip, apt build-dep) | Built; merged across roles; apt cache refreshed once when stale |
| `file` (content, line, line_re, dir, absent, mode, owner) | Built |
| `sh!` with `unless`, `creates`, `changed_if`; `raw`; `barrier` | Built |
| `as_user` | Built; each op runs in a fresh login shell of that user |
| Facts: `os`, `exists`, `bin_exists`, `bin_version` | Built; `os` from os-release across 13 distros |
| Transports | Local (`bash`, `sudo -n bash`) and `docker exec` |
| CLI, JSONL log, static musl binaries | Built |
| Guards (`.when(op.changed())`) | Traced and planned; the executor refuses them at prepare |
| SSH transport, per-user persistent shells | Designed, not built |
| `repo`, `svc`, `user`, `fetch`, `git`, `secret` | Designed, not built |
| Role rollback, templates, multi-host runs | Designed, not built |

The first real playbooks are in [`tp_basic`](https://github.com/resurgentech/tp_basic),
which ports a set of Ansible container-build playbooks and proves each port
equivalent by building the same image both ways and diffing the result.

## Porting from Ansible

The `tp` binary and the `ansible-port` skill turn an Ansible playbook into
telepatch code in two phases, understand then port:

```
tp classify playbooks/container/rust.yml   # which Ansible patterns each task uses
tp import roles/rust_build --report        # a Rust draft with TODO(port) where the mapping is not mechanical
tp check-understanding conversion/rust.md  # lint an understanding doc: levels, dated sources, a decision
```

The skill in [`.claude/skills/ansible-port/`](.claude/skills/ansible-port/)
drives the process, with a human reviewing each level or an agent alone. It
writes down what a playbook is for, whether its method is still current, and
what the port kept, dropped and fixed, then folds that into the code's
comments and deletes the YAML.

## Building and testing

Rust stable (pinned by `rust-toolchain.toml`), and `just`. Docker is needed
only for the live tests.

```
just test            # cargo test --workspace: the gate
just lint            # clippy with -D warnings, rustfmt check
just test-live-os    # read `os` through docker exec on all 13 images
just test-live-pkg   # the package playbook end to end on ubuntu:24.04 and rockylinux:10
just musl-example    # build examples/hello static for x86_64-unknown-linux-musl
```

Playbook binaries are meant to be built static and copied anywhere:
`tp_basic` bind-mounts one into a `docker build`.

## Layout

```
crates/telepatch         the engine: trace, resolver, optimizer, executor, shell pool, transports, ops, facts, cli
crates/telepatch-macros  sh! and #[role]
crates/tp                classify, import, check-understanding
.claude/skills/          the ansible-port skill
docs/execution-model.html
```

Playbooks and roles never live here. They are programs that depend on
`telepatch`, like `tp_basic`.

## Contributing

Issues and pull requests are welcome. The design note is the contract: a
change to the engine, a primitive or the shell protocol should match it or
change it. Keep `just test` and `just lint` green, and add a test first.

## License

MIT. See [LICENSE](LICENSE).
