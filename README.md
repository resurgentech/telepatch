# telepatch

<p align="center">
  <img src="docs/hero.svg" alt="A man with two fingers to his temple sends green waves to a retro computer whose screen reads telepatch" width="100%">
</p>

Configuration management in Rust, for people who are tired of waiting for
Ansible.

A role is an `async fn`. Running a playbook traces it into a per-host graph,
merges what can be merged, checks everything that can fail before touching a
host, and streams the result into a persistent shell over SSH. Nothing is
installed on the target: no Python, no agent, no module copies. The playbook
is one static binary.

**Status: 0.1, experimental, and already doing real work.** The engine, the
local, `docker exec` and SSH transports, the package, file and shell
primitives and the Ansible conversion tooling are built and tested. The
first production playbooks live in
[`tp_basic`](https://github.com/resurgentech/tp_basic), where every ported
role is proven equivalent to its Ansible original by building the same
image both ways and diffing the result. See
[What exists today](#what-exists-today) for the edges.

## Why

Ansible pays a full connect, copy module, run, collect, disconnect cycle per
task per host, buffers output until a task finishes, and cannot see that five
roles each want to `apt install` something and do it once. For a forty-task
playbook on a hundred hosts that is thousands of round trips, and you cannot
tell whether a slow task is working or hung.

telepatch keeps the vocabulary (roles, playbooks, vars, hosts) and changes the
execution model:

- **Code, not YAML.** Distro dispatch is a `match`, per-user work is a `for`,
  a condition is an `if`. The compiler checks it, and your editor can
  navigate it.
- **Trace, then plan.** Ops record themselves instead of running, so the
  engine sees the whole playbook before anything executes. Package installs
  across roles collapse into one package manager call per host. `plan` shows
  you exactly what a host would get, for any supported distro, with no host.
- **Persistent shells.** One root shell per host, plus a login shell per
  user the playbook touches, opened once and multiplexed over a single SSH
  connection. Ops are pipelined into them the way an operator would type
  them, and each reports a typed result.
- **Fail before you change anything.** Facts, package state and permissions
  are gathered up front. A host that cannot complete the plan is not started,
  and the report says why.
- **Converged means idle.** A second run reads the facts once and sends
  nothing. No task replay, no "changed" from a shell task that could not
  tell.
- **Live output.** Every op streams as it runs, prefixed by host, and the run
  is logged as JSONL for replay.
- **One binary.** Playbooks compile to a static musl executable. Copy it to a
  build container, a laptop or a jump host and run it there, or point it at
  hosts over SSH.

### Measured

The `dev_base` playbook from `tp_basic` (a 37-package development toolchain
plus the uv installer), applied by the original Ansible playbook and by
telepatch to two fresh Ubuntu 24.04 VMs from the same image, timed from the
controller with SSH included. Both left the VMs in an identical state, down
to package versions.

| | First run | Second run | Still "changed" on the second run |
|---|---|---|---|
| Ansible | 4m05s | 7.0s | 1 task |
| telepatch | 3m46s | 1.3s | 0 ops |

The first run is apt's download either way. The second run is the model:
Ansible replays every task and still reports a change from an unguarded
installer; telepatch probes once, finds nothing to do, and returns.

## A role

```rust
use serde::Deserialize;
use std::process::ExitCode;
use telepatch::facts::os::Family;
use telepatch::{Host, Playbook, PlaybookFuture, TraceError, role};

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
        s.rustup("stable")
            .targets(["x86_64-unknown-linux-musl"])
            .set_default()
            .latest();
        s.cargo(["cargo-deb", "cargo-generate-rpm"]);
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
called: `h.pkg`, `s.rustup` and `s.cargo` append nodes to the host's trace
and return handles. Packages have a *source* (the distro manager, pip,
cargo, rustup, or a vendor install script) and a *wanted* state (present,
pinned to a version, or latest), and every source knows how to probe, install
and update, so an installer is never a bare shell line with a `creates`
guard. Anything that really is a shell line is `sh!`, with shell-quoted
interpolation and `unless`, `creates` and `changed_if` so check-then-act is
one line.

The binary it builds has three commands:

```
rust_build list                                     # playbook names
rust_build plan rust_build --distro rockylinux10    # the optimized plan, nothing applied
rust_build run  rust_build --local --vars v.toml    # apply to this machine
rust_build run  rust_build --host bot@10.0.0.7 --host bot@10.0.0.8 --vars v.toml --log run.jsonl
```

`plan --distro` renders against an embedded os-release fixture for any of
the fifteen supported distro releases, so you can see what a host would get
without having one. `run --local` applies to the machine the binary is on, as
root or through `sudo -n`. `run --host` applies over SSH to each destination
in parallel, with your ssh config, agent and keys, or `--ssh-key` and
`--ssh-option K=V` when you want to be explicit.

## How it runs

1. **Trace.** The playbook runs on the controller and records ops. Reads of
   state the playbook already wrote are answered from the trace; reads of
   pre-existing state cost one round trip. A read that needs a pending
   mutation's effect forces that mutation to apply first.
2. **Prepare.** Per host: gather the os, probe every package source, check
   what the plan will install or update, verify sudo for each user the
   playbook touches. A host that fails here is not touched. The report lists
   what will change before it changes.
3. **Apply.** The per-host graph is ordered by effect (what each op reads and
   writes), same-kind ops are merged, and the windows are streamed into the
   host's shells. Each op runs in a subshell and ends with a nonce-tagged
   trailer carrying its typed result. A failure halts that host inside the
   shell itself; later roles on it are reported as not run.

Over SSH, telepatch opens one multiplexed connection per host and runs every
shell as a channel on it: a root shell through `sudo -n`, and a login shell
per user through `sudo -n -iu`. When the server's session limit is hit it
opens another connection rather than failing. Authentication, host keys and
proxies are the user's ssh binary's business, exactly as they are for `ssh`
itself.

The full design, including the parts not built yet, is in
[`docs/execution-model.html`](docs/execution-model.html).

## What exists today

| Area | State |
|---|---|
| Trace, resolver, engine loop, optimizer | Built, with snapshot tests and an adversarial plan suite |
| `pkg` from the distro (apt, dnf, yum, pacman, apt build-dep) | Built; merged across roles; apt cache refreshed once when stale |
| `pkg` from pip, cargo, rustup, vendor scripts (`h.pip`, `h.cargo`, `h.rustup`, `h.script`) | Built; present, pinned or latest; latest resolved on the host by the source's own command |
| `file` (content, line, line_re, dir, absent, mode, owner) | Built |
| `sh!` with `unless`, `creates`, `changed_if`; `raw`; `barrier` | Built |
| `as_user` | Built; a persistent login shell per user over SSH, a fresh login environment per op elsewhere |
| Facts: `os`, `exists`, `bin_exists`, `bin_version` | Built; `os` from os-release across 15 distro releases |
| Transports | Local (`bash`, `sudo -n bash`), `docker exec`, SSH (multiplexed, root and per-user shells) |
| CLI (`list`, `plan`, `run --local`, `run --host …`), JSONL log, static musl binaries | Built |
| Guards (`.when(op.changed())`) | Traced and planned; the executor refuses them at prepare |
| `repo`, `svc`, `user`, `fetch`, `git`, `secret` | Designed, not built |
| Role rollback, templates, reconnect, PTY | Designed, not built |

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
comments and deletes the YAML. Installer tasks (`curl | sh`, rustup, `cargo
install`) become package ops with the right source, not shell lines.

`tp_basic` is the worked example: five roles and four container playbooks
ported this way, each proven equivalent to the Ansible original on Ubuntu
and Rocky by building the image both ways and diffing the installed state,
and on a VM over SSH by the same diff against a live `ansible-playbook` run.

## Building and testing

Rust stable (pinned by `rust-toolchain.toml`), and `just`. Docker and ssh
are needed only for the live tests.

```
just test            # cargo test --workspace: the gate
just lint            # clippy with -D warnings, rustfmt check
just test-live-os    # read `os` through docker exec on all the upstream images
just test-live-pkg   # the package playbook end to end on ubuntu:24.04 and rockylinux:10
just test-live-ssh   # the SSH transport against an sshd container: root and user shells, auth and sudo failures
just musl-example    # build examples/hello static for x86_64-unknown-linux-musl
```

Playbook binaries are meant to be built static and copied anywhere:
`tp_basic` bind-mounts one into a `docker build` and scp's nothing to its
VMs.

## Layout

```
crates/telepatch           the engine: trace, resolver, optimizer, executor, prepare, shell pool, transports, ops, facts, cli
crates/telepatch/examples  hello: the smallest playbook binary
crates/telepatch-macros    sh! and #[role]
crates/tp                  classify, import, check-understanding
.claude/skills/            the ansible-port skill
docs/execution-model.html  the design note; the contract for the engine, the primitives and the shell protocol
```

Playbooks and roles never live here. They are programs that depend on
`telepatch`, like `tp_basic`.

## Contributing

Issues and pull requests are welcome. The design note is the contract: a
change to the engine, a primitive or the shell protocol should match it or
change it. Keep `just test` and `just lint` green, and add a test first.

## License

MIT. See [LICENSE](LICENSE).
