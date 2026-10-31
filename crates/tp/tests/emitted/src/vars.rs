//! Playbook variables, mirroring `inventories/group_vars/all` for the parts
//! the five corpus roles use. Hand-written (a copy of tp_basic's `Vars`);
//! `tp import` does not emit it.

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Vars {
    pub admin_users: Vec<String>,
    pub rustup_version: String,
    pub cargo_version: String,
    pub rustc_version: String,
}

impl Default for Vars {
    fn default() -> Self {
        Vars {
            admin_users: vec!["root".to_string()],
            rustup_version: "1.29.0".to_string(),
            cargo_version: "1.94.0".to_string(),
            rustc_version: "1.94.0".to_string(),
        }
    }
}
