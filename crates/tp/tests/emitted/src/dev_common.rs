//! `dev_common`: imported by `tp import`. Generated; do not edit.
//! Regenerate with `just import-regen`, then resolve the `TODO(port)` lines.

use telepatch::facts::os::Distro;
use telepatch::{Host, TraceError, role, sh};

use crate::flags::*;
use crate::vars::Vars;

#[role]
pub async fn dev_common(h: &Host<Vars>) -> Result<(), TraceError> {
    // roles/dev_common/tasks/main.yml:4 include debian.yml
    if debian_based(&h.os) {
        // roles/dev_common/tasks/debian.yml:2 Debian Install python
        h.pkg(["python3-apt", "python3-pip"]);

        // roles/dev_common/tasks/debian.yml:7 Debian Common development tools
        h.pkg([
            "build-essential",
            "cmake",
            "clang",
            "llvm",
            "lld",
            "git",
            "rsync",
            "gcc",
            "gnupg",
            "rpm",
            "cppcheck",
            "autoconf",
            "asciidoc",
            "bc",
            "cpio",
            "bison",
            "flex",
            "gawk",
            "gettext",
            "gzip",
            "hostname",
            "m4",
            "net-tools",
            "openssl",
            "rsync",
            "xmlto",
            "zstd",
            "diffstat",
            "sshpass",
            "btop",
            "acl",
            "curl",
        ]);

        // roles/dev_common/tasks/debian.yml:44 Ubuntu2404 python deps
        h.pkg([
            "python3-jinja2",
            "python3-git",
            "python3-yaml",
            "python3-hvac",
        ]);
    }

    // roles/dev_common/tasks/main.yml:8 include redhat.yml
    if redhat_based(&h.os) {
        // roles/dev_common/tasks/redhat.yml:2 RedHat Common development tools
        h.pkg([
            "@Development Tools",
            "cmake",
            "clang",
            "llvm",
            "lld",
            "bison",
            "flex",
            "git",
            "rsync",
            "gcc",
            "zlib-devel",
            "gnupg",
            "bc",
            "newt",
            "slang",
            "xmlto",
            "net-tools",
            "rpm-build",
            "dpkg-devel",
            "dpkg",
            "dpkg-dev",
            "zstd",
            "python3",
            "python3-devel",
            "python3-docutils",
            "python3-pip",
        ]);
    }

    // roles/dev_common/tasks/main.yml:12 include amazonlinux2.yml
    // TODO(port): roles/dev_common/tasks/main.yml:12 unreachable branch: `amazonlinux2_based`: no image reaches it; the design doc drops it
    if amazonlinux2_based(&h.os) {
        // roles/dev_common/tasks/amazonlinux2.yml:2 AmazonLinux2 Common development tools
        h.yum([
            "@Development Tools",
            "bc",
            "newt",
            "slang",
            "xmlto",
            "rsync",
            "cmake",
            "net-tools",
            "rpm-build",
            "python2",
            "python2-pip",
            "python3",
            "python3-pip",
            "zstd",
            "wget",
        ]);
    }

    // roles/dev_common/tasks/main.yml:16 include amazonlinux2023.yml
    if amazonlinux2023_based(&h.os) {
        // roles/dev_common/tasks/amazonlinux2023.yml:2 AmazonLinux2023 Common development tools
        h.pkg([
            "@Development Tools",
            "cmake",
            "clang",
            "llvm",
            "lld",
            "git",
            "rsync",
            "gcc",
            "zlib-devel",
            "bc",
            "newt",
            "slang",
            "xmlto",
            "net-tools",
            "rpm-build",
            "zstd",
            "wget",
            "python3",
            "python3-devel",
            "python3-pip",
        ]);
    }

    // roles/dev_common/tasks/main.yml:20 include archlinux.yml
    if arch_based(&h.os) {
        // roles/dev_common/tasks/archlinux.yml:2 Arch Common development tools
        h.pkg([
            "base-devel",
            "cmake",
            "clang",
            "llvm",
            "lld",
            "git",
            "rsync",
            "gnupg",
            "bc",
            "cpio",
            "xmlto",
            "zstd",
            "wget",
            "python",
            "python-pip",
            "python-yaml",
            "python-jinja",
            "python-gitpython",
            "python-pytest",
        ]);
    }

    // roles/dev_common/tasks/main.yml:26 Various python dependancies
    if !((debian_based(&h.os) || arch_based(&h.os))) {
        h.pip(["jinja2", "GitPython", "pyaml"]);
    }

    // roles/dev_common/tasks/main.yml:34 Installing uv for python
    // subshell keeps `pipefail` out of the persistent shell
    sh!(h, "(set -o pipefail && curl -LsSf https://astral.sh/uv/install.sh | UV_INSTALL_DIR=/usr/local/bin sh)");
    Ok(())
}
