//! Adversarial plan snapshots: the optimizer's merge rules, one case per
//! file, rendered through `testing::plan` (the windows the executor gets).

mod barrier;
mod build_dep;
mod cargo_does_not_cross_distro_pkg;
mod cargo_locked;
mod common;
mod exists_after_cargo_is_a_barrier;
mod implicit_barrier;
mod merge_roles;
mod narrowed_sh;
mod pip_apt;
mod rustup_conflict;
mod rustup_does_not_hoist_across;
mod rustup_merge;
mod rustup_present_joins_latest;
mod rustup_windows;
mod same_fact;
mod sh_blocks;
mod two_hosts;
