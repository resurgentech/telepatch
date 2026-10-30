//! Adversarial plan snapshots: the optimizer's merge rules, one case per
//! file, rendered through `testing::plan` (the windows the executor gets).

mod barrier;
mod build_dep;
mod common;
mod implicit_barrier;
mod merge_roles;
mod narrowed_sh;
mod pip_apt;
mod same_fact;
mod sh_blocks;
mod two_hosts;
