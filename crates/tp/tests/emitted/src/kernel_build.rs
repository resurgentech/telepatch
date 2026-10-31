//! `kernel_build`: imported by `tp import`. Generated; do not edit.
//! Regenerate with `just import-regen`, then resolve the `TODO(port)` lines.

use telepatch::facts::os::Distro;
use telepatch::{Host, TraceError, role, sh};

use crate::flags::*;
use crate::vars::Vars;

#[role]
pub async fn kernel_build(h: &Host<Vars>) -> Result<(), TraceError> {
    // roles/kernel_build/tasks/main.yml:4 include ubuntu.yml
    if ubuntu_based(&h.os) {
        // roles/kernel_build/tasks/ubuntu.yml:2 Ubuntu Kernel Build Dependencies
        h.pkg(["linux"]).build_dep();

        // roles/kernel_build/tasks/ubuntu.yml:7 Ubuntu Kernel Build Dependencies Dependencies
        h.pkg([
            "libperl-dev",
            "libzstd-dev",
            "libpython3-dev",
            "binutils-dev",
            "equivs",
            "dwarves",
            "systemtap-sdt-dev",
            "libbabeltrace-dev",
            "libbabeltrace-ctf-dev",
            "python3-pip",
            "python3-jinja2",
            "python3-yaml",
            "python3-git",
            "python3-lxml",
        ]);
    }

    // roles/kernel_build/tasks/main.yml:8 include debian10.yml
    // TODO(port): roles/kernel_build/tasks/main.yml:8 unreachable branch: `debian10_based`: no image reaches it; the design doc drops it
    if debian10_based(&h.os) {
        // roles/kernel_build/tasks/debian10.yml:2 Debian Kernel Build Dependencies Dependencies
        h.pkg([
            "libncurses-dev",
            "libssl-dev",
            "libelf-dev",
            "libudev-dev",
            "libpci-dev",
            "libiberty-dev",
            "equivs",
            "dwarves",
            "dkms",
        ]);

        // roles/kernel_build/tasks/debian10.yml:16 Debian Kernel Build Dependencies
        h.pkg(["linux"]).build_dep();
    }

    // roles/kernel_build/tasks/main.yml:12 include debian.yml
    if debian_based(&h.os) && !(ubuntu_based(&h.os)) {
        // roles/kernel_build/tasks/debian.yml:2 Debian Kernel Build Dependencies Dependencies
        h.pkg([
            "libncurses-dev",
            "libssl-dev",
            "libelf-dev",
            "libudev-dev",
            "libpci-dev",
            "libiberty-dev",
            "equivs",
            "dwarves",
            "dkms",
        ]);

        // roles/kernel_build/tasks/debian.yml:16 Debian Kernel Build Dependencies
        h.pkg(["linux"]).build_dep();
    }

    // roles/kernel_build/tasks/main.yml:16 include redhat.yml
    if redhat_based(&h.os) {
        // roles/kernel_build/tasks/redhat.yml:2 RedHat Update pip3
        h.pip(["pip"]).latest();

        // roles/kernel_build/tasks/redhat.yml:7 RedHat Kernel Build Dependencies
        h.pkg([
            "rpmdevtools",
            "rpm-build",
            "redhat-rpm-config",
            "audit-libs-devel",
            "binutils-devel",
            "elfutils-devel",
            "elfutils-libelf-devel",
            "java-devel",
            "libcap-devel",
            "ncurses-devel",
            "newt-devel",
            "numactl-devel",
            "openssl-devel",
            "pciutils-devel",
            "libnl3-devel",
            "xz-devel",
            "zlib-devel",
            "perl-interpreter",
            "perl-devel",
            "perl-generators",
            "perl-Carp",
            "perl(ExtUtils::Embed)",
            "python3-sphinx",
            "dwarves",
            "kabi-dw",
            "libbabeltrace-devel",
            "libbpf-devel",
            "libcap-ng-devel",
            "libmnl-devel",
            "bpftool",
        ]);

        // roles/kernel_build/tasks/redhat.yml:43 Various python dependancies
        h.pip(["jinja2", "GitPython", "pyaml"]);
    }

    // roles/kernel_build/tasks/main.yml:20 include amazonlinux2.yml
    // TODO(port): roles/kernel_build/tasks/main.yml:20 unreachable branch: `amazonlinux2_based`: no image reaches it; the design doc drops it
    if amazonlinux2_based(&h.os) {
        // roles/kernel_build/tasks/amazonlinux2.yml:2 AmazonLinux2 installing epel
        h.yum([
            "https://dl.fedoraproject.org/pub/epel/epel-release-latest-7.noarch.rpm",
        ]);

        // roles/kernel_build/tasks/amazonlinux2.yml:8 AmazonLinux2 Kernel Build Dependencies Dependencies
        h.yum([
            "yum-utils",
            "python3",
            "python3-pip",
            "python3-docutils",
            "python3-sphinx",
            "hostname",
            "openssl",
            "asciidoc",
            "openssl-devel",
            "elfutils-devel",
            "binutils-devel",
            "newt-devel",
            "python-devel",
            "perl(ExtUtils::Embed)",
            "audit-libs-devel",
            "numactl-devel",
            "pciutils-devel",
            "pesign",
            "hmaccalc",
            "ncurses-devel",
            "cpan",
            "dpkg",
            "dpkg-dev",
        ]);

        // roles/kernel_build/tasks/amazonlinux2.yml:36 AmazonLinux2 Kernel Build Dependencies
        sh!(h, "yumdownloader --source kernel");

        // roles/kernel_build/tasks/amazonlinux2.yml:39 AmazonLinux2 python3 dependencies
        sh!(h, "python3 -m pip install pyaml; python3 -m pip install jinja2");
    }

    // roles/kernel_build/tasks/main.yml:24 include amazonlinux2023.yml
    if amazonlinux2023_based(&h.os) {
        // roles/kernel_build/tasks/amazonlinux2023.yml:2 AmazonLinux2023 Kernel Build Dependencies
        h.pkg([
            "rpmdevtools",
            "rpm-build",
            "audit-libs-devel",
            "binutils-devel",
            "elfutils-devel",
            "elfutils-libelf-devel",
            "libcap-devel",
            "ncurses-devel",
            "newt-devel",
            "numactl-devel",
            "openssl-devel",
            "pciutils-devel",
            "xz-devel",
            "zlib-devel",
            "perl-interpreter",
            "perl-devel",
            "perl-generators",
            "perl-Carp",
            "perl(ExtUtils::Embed)",
            "python3-sphinx",
            "dwarves",
            "libbpf-devel",
            "libcap-ng-devel",
            "libmnl-devel",
            "bpftool",
        ]);

        // roles/kernel_build/tasks/amazonlinux2023.yml:33 AmazonLinux2023 python dependancies
        h.pip(["jinja2", "GitPython", "pyaml"]);
    }

    // roles/kernel_build/tasks/main.yml:28 include archlinux.yml
    if arch_based(&h.os) {
        // roles/kernel_build/tasks/archlinux.yml:4 Arch Kernel Build Dependencies
        h.pkg([
            "devtools",
            "pacman-contrib",
            "bc",
            "pahole",
            "rust",
            "rust-bindgen",
            "rust-src",
        ]);
    }
    Ok(())
}
