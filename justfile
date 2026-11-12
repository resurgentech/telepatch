# The gate.
test:
    cargo test --workspace

lint:
    cargo clippy --workspace --all-targets -- -D warnings
    cargo fmt --all --check
    # The engine crate stays free of HTTP and TLS. ureq and rustls live in
    # telepatch-openbao only.
    ! cargo tree -p telepatch --prefix none -e normal,build | grep -E '^(ureq|rustls) '

# tp check-understanding passes the good fixture and fails the bad one.
skill-check:
    cargo run -q -p tp -- check-understanding crates/tp/tests/fixtures/understanding/good.md
    ! cargo run -q -p tp -- check-understanding crates/tp/tests/fixtures/understanding/bad.md

# Classify the container playbooks in ../ansible (prints; not a gate).
classify-corpus:
    for p in dev_base kernel_build rust latest_kernel_build; do \
        cargo run -q -p tp -- classify ../ansible/playbooks/container/$p.yml; echo; \
    done

# Regenerate crates/telepatch/tests/fixtures/os-release/<distro> from the
# upstream images named in ../docker_images/docker-bake.hcl (needs docker and
# network; not part of the gate). omarchy's base image is stock Arch plus a
# pacman repo, so its os-release is Arch's. pop-os and omarchy-host are
# hand-kept and not touched here.
os-fixtures:
    #!/usr/bin/env bash
    set -euo pipefail
    bash ../tp_basic/scripts/lib/docker-guard.sh
    out=crates/telepatch/tests/fixtures/os-release
    mkdir -p "$out"
    sed -nE 's/^ *([a-z0-9]+) += \{ root = "([^"]+)".*/\1 \2/p' ../docker_images/docker-bake.hcl |
    while read -r distro image; do
        echo "$distro <- $image"
        docker run --rm "$image" sh -c 'cat /etc/os-release 2>/dev/null || cat /usr/lib/os-release' > "$out/$distro"
    done

# Live: start each upstream image in docker, read `os` through the docker
# transport, compare with the committed fixtures. Needs docker; may pull.
test-live-os:
    bash ../tp_basic/scripts/lib/docker-guard.sh
    cargo test -p telepatch --test live -- --ignored os_

# Live: the package playbook through the executor on ubuntu:24.04 and
# rockylinux/rockylinux:10 via docker exec; second run must change nothing.
test-live-pkg:
    bash ../tp_basic/scripts/lib/docker-guard.sh
    cargo test -p telepatch --test live -- --ignored --nocapture pkg_

# Live: the mise source's system layer in a fresh ubuntu:24.04 container as
# root (mise installed with an InstallerScript): run 2 changed 0 with the network
# cut, root-owned config, system shim, a plain user runs it, pin, latest.
# Needs docker and the network.
test-live-mise:
    bash ../tp_basic/scripts/lib/docker-guard.sh
    cargo test -p telepatch --test live -- --ignored --nocapture mise_

# Live: the SSH transport against an ubuntu:24.04 sshd container (root
# shell via sudo, read_os, AuthFailed, ConnectRefused, SudoRefused). Needs
# docker, ssh, ssh-keygen and the network for apt.
test-live-ssh:
    bash ../tp_basic/scripts/lib/docker-guard.sh
    cargo test -p telepatch --test live -- --ignored --nocapture ssh_

# Live: `run --inventory` over two sshd containers in the docker VM, each on
# its own port of TP_DOCKER_VM_PORTS (named in one inventory as
# ssh://tp@addr:port, a throwaway known-hosts file); a group var reaches one
# only. Needs docker, ssh, ssh-keygen and the network for apt.
test-live-inventory:
    bash ../tp_basic/scripts/lib/docker-guard.sh
    cargo test -p telepatch --test live -- --ignored --nocapture inventory::

# Live: the group primitive (and, from T47, the user primitive) on
# ubuntu:24.04 and rockylinux/rockylinux:10 via docker exec; second run
# must change nothing. Needs docker.
test-live-user:
    bash ../tp_basic/scripts/lib/docker-guard.sh
    cargo test -p telepatch --test live -- --ignored --nocapture group:: user::

# Live: the repo primitive with Docker's real repositories on ubuntu:24.04
# (apt, deb822 + keyring) and rockylinux/rockylinux:10 (dnf .repo), each
# installing docker-ce-cli from it in the same run; second run must change
# nothing. Needs docker and the network.
test-live-repo:
    bash ../tp_basic/scripts/lib/docker-guard.sh
    cargo test -p telepatch --test live -- --ignored --nocapture repo_

# Live: the svc primitive on ubuntu:24.04 and rockylinux/rockylinux:10 in
# plain containers with no init: skipped without systemctl; with systemd
# installed, the unit is enabled offline and the start is skipped; second
# run must change nothing. Needs docker and the network (apt/dnf).
test-live-svc:
    bash ../tp_basic/scripts/lib/docker-guard.sh
    cargo test -p telepatch --test live -- --ignored --nocapture svc_

# Live: the unit primitive on ubuntu:24.04 and rockylinux/rockylinux:10 in
# plain containers with no init: systemd installed by a pkg op, then a unit
# and a drop-in written and checked by systemd-analyze verify, the unit
# enabled offline (no daemon-reload without a running systemd); second run
# must change nothing; a unit verify refuses is not written. Needs docker
# and the network (apt/dnf).
test-live-unit:
    bash ../tp_basic/scripts/lib/docker-guard.sh
    cargo test -p telepatch --test live -- --ignored --nocapture unit_

# Live: the fetch value with .from(..) and .unpack(..).strip(1) on
# ubuntu:24.04 and rockylinux/rockylinux:10 in plain containers with no init:
# a python3 http.server on 127.0.0.1 inside the container serves a file and a
# tar.gz; run 1 downloads, verifies, caches by sha256, places and unpacks;
# the server is then stopped and run 2 must change nothing and call no
# network; a wrong checksum blocks before apply. Needs docker and the network
# (apt/dnf for python3, and curl where the image has none).
test-live-fetch:
    bash ../tp_basic/scripts/lib/docker-guard.sh
    cargo test -p telepatch --test live -- --ignored --nocapture fetch_

# Live: git checkout on ubuntu:24.04. The remote is a bare repo created
# inside the container. Needs docker and the network (apt install git).
test-live-git:
    bash ../tp_basic/scripts/lib/docker-guard.sh
    cargo test -p telepatch --test live -- --ignored --nocapture git_

# Live: the Terminfo resource on ubuntu:24.04. Run 1 compiles an entry with
# tic; run 2 changes nothing. Needs docker and the network (apt install
# ncurses-bin).
test-live-resource:
    bash ../tp_basic/scripts/lib/docker-guard.sh
    cargo test -p telepatch --test live -- --ignored --nocapture resource_

# Live: OpenBao dev server in a plain debian:13 container on the docker VM.
# Inherited BAO_* and VAULT_* are dropped here; the test sets its own.
# Nothing reads .env. Needs docker (the VM's daemon) and the network.
test-live-openbao:
    #!/usr/bin/env bash
    set -euo pipefail
    bash ../tp_basic/scripts/lib/docker-guard.sh
    while IFS='=' read -r name _; do
        case $name in
            BAO_*|VAULT_*) unset "$name" ;;
        esac
    done < <(env)
    cargo test -p telepatch-openbao --test live -- --ignored --nocapture live_

# Run a docker recipe of this justfile against the daemon of a fresh 1 GiB
# tp-* VM, deleted afterwards (../tp_basic/scripts/docker-vm.sh). Every docker
# recipe refuses to run outside it (exit 2). `just docker-vm test-live-os`
docker-vm +recipe:
    bash ../tp_basic/scripts/docker-vm.sh run -- just {{recipe}}

# Build examples/hello for musl and fail unless the binary is static; then
# smoke-test it.
musl-example:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --release --example hello --target x86_64-unknown-linux-musl -p telepatch
    bin=target/x86_64-unknown-linux-musl/release/examples/hello
    desc=$(file -b "$bin")
    echo "file: $desc"
    ldd_out=$(ldd "$bin" 2>&1 || true)
    echo "ldd:  $ldd_out"
    if echo "$desc" | grep -Eq 'statically linked|static-pie linked' \
       || echo "$ldd_out" | grep -Eq 'not a dynamic executable|statically linked'; then
        echo "static: yes"
    else
        echo "static: NO" >&2; exit 1
    fi
    "$bin" plan hello --distro ubuntu2404 | head -5

# Regenerate crates/tp/tests/emitted/src from the corpus roles (then review
# the diff; the test `checked_in_emitted_code_is_current` guards it).
import-regen:
    UPDATE_EMITTED=1 cargo test -q -p tp --test import checked_in_emitted_code_is_current

# Informative: diff what `tp import` emits for each corpus role against the
# hand port in ../tp_basic. Always exits 0.
import-diff:
    #!/usr/bin/env bash
    for r in dev_common src_repos kernel_build rust_build rust_kernel; do
        echo "=== $r"
        diff -u "crates/tp/tests/emitted/src/$r.rs" "../tp_basic/src/roles/$r.rs" || true
    done
    exit 0
