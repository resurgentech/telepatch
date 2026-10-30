# The gate.
test:
    cargo test --workspace

lint:
    cargo clippy --workspace --all-targets -- -D warnings
    cargo fmt --all --check

# tp check-understanding passes the real trial doc and fails the bad fixture.
skill-check:
    cargo run -q -p tp -- check-understanding ../tp_basic/conversion/dev_base.md
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
    cargo test -p telepatch --test live -- --ignored os_
