//! `src_repos`: imported by `tp import`. Generated; do not edit.
//! Regenerate with `just import-regen`, then resolve the `TODO(port)` lines.

use telepatch::facts::os::Distro;
use telepatch::{Host, TraceError, role, sh};

use crate::flags::*;
use crate::vars::Vars;

#[role]
pub async fn src_repos(h: &Host<Vars>) -> Result<(), TraceError> {
    // roles/src_repos/tasks/main.yml:4 Look for deb822 style apt sources
    // TODO(port): roles/src_repos/tasks/main.yml:4 unmapped module `find`: no emitter mapping
    // find: {"paths":"/etc/apt/sources.list.d","patterns":"*.sources"}

    // roles/src_repos/tasks/main.yml:11 include apt_sources_deb822.yml
    // TODO(port): roles/src_repos/tasks/main.yml:11 when not translated: debian_based|bool and apt_deb822_sources.matched > 0 (comparison not understood)
    if false /* TODO(port): when debian_based|bool and apt_deb822_sources.matched > 0 (comparison not understood) */ {
        // roles/src_repos/tasks/apt_sources_deb822.yml:7 Turn on source repos in /etc/apt/sources.list.d/*.sources
        sh!(
            h,
            "if grep -qs '^Types:.*deb-src' /etc/apt/sources.list.d/*.sources; then\n  exit 0\nfi\nsed -i 's/^Types: deb$/Types: deb deb-src/' /etc/apt/sources.list.d/*.sources\necho \"SOURCES_CHANGED\"",
            changed_if = "SOURCES_CHANGED",
        );

        // roles/src_repos/tasks/apt_sources_deb822.yml:17 Update apt package cache again
        // TODO(port): roles/src_repos/tasks/apt_sources_deb822.yml:17 unsupported arguments: `cache_valid_time`, `update_cache`
        // apt: {"cache_valid_time":1,"update_cache":"yes"}
    }

    // roles/src_repos/tasks/main.yml:14 include apt_sources_list.yml
    // TODO(port): roles/src_repos/tasks/main.yml:14 when not translated: debian_based|bool and apt_deb822_sources.matched == 0 (comparison not understood)
    if false /* TODO(port): when debian_based|bool and apt_deb822_sources.matched == 0 (comparison not understood) */ {
        // roles/src_repos/tasks/apt_sources_list.yml:4 Turn on source repos to /etc/apt/sources.list
        sh!(
            h,
            "cp /etc/apt/sources.list /etc/apt/sources.list.ansible\nif [ -z \"$( cat /etc/apt/sources.list | grep deb-src )\" ]; then\n  sed -Ei 's/^deb /deb-src /' /etc/apt/sources.list.ansible-src\n  cat /etc/apt/sources.list.ansible-src | grep deb-src >> /etc/apt/sources.list.ansible\n  rm /etc/apt/sources.list.ansible-src\nelse\n  # Some distros have src repos commented out\n  sed -Ei 's/^# deb-src /deb-src /' /etc/apt/sources.list.ansible\nfi\nif [ -z \"$(diff /etc/apt/sources.list /etc/apt/sources.list.ansible)\" ]; then\n  rm /etc/apt/sources.list.ansible\nelse\n  mv /etc/apt/sources.list.ansible /etc/apt/sources.list\n  echo \"SOURCES_LIST_CHANGED\"\nfi",
            changed_if = "SOURCES_LIST_CHANGED",
        );
    }
    Ok(())
}
