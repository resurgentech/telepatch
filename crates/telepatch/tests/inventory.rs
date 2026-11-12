//! `inventory.toml`: hosts, groups, and vars per host and group, layered
//! over the vars file (see `telepatch::inventory`).

use serde::Deserialize;
use telepatch::Host;
use telepatch::inventory::{Inventory, secret_literals};
use telepatch::vars::VarLayer;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct Docker {
    admin_users: Vec<String>,
    channel: String,
}
impl Default for Docker {
    fn default() -> Self {
        Docker {
            admin_users: vec!["root".into()],
            channel: "stable".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RustBuild {
    admin_users: Vec<String>,
    rustc_version: String,
    jobs: u32,
}
impl Default for RustBuild {
    fn default() -> Self {
        RustBuild {
            admin_users: vec!["root".into()],
            rustc_version: "1.94.0".into(),
            jobs: 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct Vars {
    docker: Docker,
    rust_build: RustBuild,
}

fn users(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|s| s.to_string()).collect()
}

fn inv(text: &str) -> Inventory {
    Inventory::parse("inv.toml", text).unwrap()
}

fn inv_err(text: &str) -> String {
    match Inventory::parse("inv.toml", text) {
        Ok(_) => panic!("expected an error"),
        Err(e) => e,
    }
}

fn vars_of(i: &Inventory, host: &str, file: &str, defaults: &str) -> Vars {
    let layer = VarLayer::parse("vars.toml", file).unwrap();
    let h = i.hosts().iter().find(|h| h.name == host).unwrap();
    i.vars_for::<Vars>(h, Some(&layer), defaults).unwrap()
}

const FULL: &str = r#"[vars]
admin_users = ["inv"]
channel = "inv-chan"

[vars.docker]
channel = "inv-docker"

[groups.builders]
hosts = ["bot@b1", "bot@b2"]
vars = { rust_build = { rustc_version = "1.1.0" }, jobs = 8 }

[groups.gpu]
hosts = ["bot@g1"]
vars = { jobs = 16 }

[hosts."bot@g1"]
groups = ["builders"]
vars = { admin_users = ["bot", "ml"] }
"#;

#[test]
fn hosts_groups_and_membership_from_both_directions() {
    let i = inv(FULL);
    let seen: Vec<(&str, Vec<&str>)> = i
        .hosts()
        .iter()
        .map(|h| {
            (
                h.name.as_str(),
                h.groups.iter().map(String::as_str).collect(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        vec![
            ("bot@b1", vec!["builders"]),
            ("bot@b2", vec!["builders"]),
            // gpu by `[groups.gpu].hosts`, builders by `[hosts].groups`;
            // groups come in the order their tables are listed.
            ("bot@g1", vec!["builders", "gpu"]),
        ]
    );
    assert_eq!(i.group_names(), vec!["builders", "gpu"]);
}

#[test]
fn a_host_only_in_the_hosts_table_exists() {
    let i = inv("[hosts.\"a@solo\"]\nvars = { jobs = 3 }\n");
    assert_eq!(i.hosts().len(), 1);
    assert_eq!(i.hosts()[0].name, "a@solo");
    assert!(i.hosts()[0].groups.is_empty());
}

#[test]
fn ssh_url_hosts_keep_their_port() {
    let i = inv("[groups.a]\nhosts = [\"ssh://tp@127.0.0.1:2201\", \"ssh://tp@127.0.0.1:2202\"]\n");
    let names: Vec<&str> = i.hosts().iter().map(|h| h.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["ssh://tp@127.0.0.1:2201", "ssh://tp@127.0.0.1:2202"]
    );
}

#[test]
fn precedence_host_group_inventory_file_defaults() {
    let i = inv(FULL);
    // file sets everything at the lowest place; defaults below it.
    let file = "jobs = 2\n[docker]\nchannel = \"file\"\n[rust_build]\nrustc_version = \"file\"\n";
    let defaults = "[docker]\nadmin_users = [\"def\"]\n";

    let b1 = vars_of(&i, "bot@b1", file, defaults);
    // admin_users: no host/group value; the inventory's [vars] top-level key
    // beats the file and the default, for both sections.
    assert_eq!(b1.docker.admin_users, users(&["inv"]));
    assert_eq!(b1.rust_build.admin_users, users(&["inv"]));
    // channel: the inventory's [vars.docker] section beats its top-level key
    // and the file's [docker].
    assert_eq!(b1.docker.channel, "inv-docker");
    // group beats inventory and file.
    assert_eq!(b1.rust_build.rustc_version, "1.1.0");
    assert_eq!(b1.rust_build.jobs, 8);

    // g1: host vars beat groups; the later group (gpu) beats builders.
    let g1 = vars_of(&i, "bot@g1", file, defaults);
    assert_eq!(g1.docker.admin_users, users(&["bot", "ml"]));
    assert_eq!(g1.rust_build.admin_users, users(&["bot", "ml"]));
    assert_eq!(g1.rust_build.jobs, 16);
    assert_eq!(g1.rust_build.rustc_version, "1.1.0");
}

#[test]
fn file_and_defaults_apply_below_the_inventory() {
    let i = inv("[groups.a]\nhosts = [\"x@h\"]\n");
    let file = "jobs = 2\n[docker]\nchannel = \"file\"\n";
    let v = vars_of(
        &i,
        "x@h",
        file,
        "[rust_build]\nrustc_version = \"def\"\njobs = 99\n",
    );
    assert_eq!(v.rust_build.jobs, 2);
    assert_eq!(v.docker.channel, "file");
    assert_eq!(v.rust_build.rustc_version, "def");
    assert_eq!(v.docker.admin_users, users(&["root"]));
}

#[test]
fn no_vars_file_is_fine() {
    let i = inv(FULL);
    let h = &i.hosts()[0];
    let v = i.vars_for::<Vars>(h, None, "").unwrap();
    assert_eq!(v.rust_build.jobs, 8);
    assert_eq!(v.docker.channel, "inv-docker");
}

#[test]
fn within_a_place_the_role_section_beats_a_top_level_key() {
    let i = inv("[groups.a]\nhosts = [\"x@h\"]\n\
         vars = { admin_users = [\"top\"], docker = { admin_users = [\"sec\"] } }\n");
    let v = vars_of(&i, "x@h", "", "");
    assert_eq!(v.docker.admin_users, users(&["sec"]));
    assert_eq!(v.rust_build.admin_users, users(&["top"]));
}

#[test]
fn later_group_wins_over_earlier() {
    let i = inv("[groups.a]\nhosts = [\"x@h\"]\nvars = { jobs = 1 }\n\
         [groups.b]\nhosts = [\"x@h\"]\nvars = { jobs = 2 }\n");
    assert_eq!(vars_of(&i, "x@h", "", "").rust_build.jobs, 2);
}

#[test]
fn vars_tables_in_header_form() {
    let i = inv(
        "[groups.a]\nhosts = [\"x@h\"]\n[groups.a.vars.rust_build]\njobs = 5\n\
         [hosts.\"x@h\".vars]\nchannel = \"edge\"\n",
    );
    let v = vars_of(&i, "x@h", "", "");
    assert_eq!((v.rust_build.jobs, v.docker.channel.as_str()), (5, "edge"));
}

// ---- errors name the file and the line ----

#[test]
fn parse_error_names_the_file() {
    let e = inv_err("[groups.a\n");
    assert!(e.starts_with("cannot parse inventory inv.toml:"), "{e}");
}

#[test]
fn unknown_top_level_key() {
    let e = inv_err("[vars]\n\n[grups.a]\nhosts = []\n");
    assert!(e.starts_with("inv.toml:3: unknown key `grups`"), "{e}");
}

#[test]
fn unknown_key_in_a_group_and_a_host() {
    let e = inv_err("[groups.a]\nhosts = [\"x@h\"]\nhost = 1\n");
    assert!(
        e.starts_with("inv.toml:3: unknown key `host` in [groups.a]"),
        "{e}"
    );
    let e = inv_err("[hosts.\"x@h\"]\nhosts = []\n");
    assert!(
        e.starts_with("inv.toml:2: unknown key `hosts` in [hosts.\"x@h\"]"),
        "{e}"
    );
}

#[test]
fn wrong_types_name_the_line() {
    let e = inv_err("[groups.a]\nhosts = \"x@h\"\n");
    assert!(
        e.starts_with("inv.toml:2: `hosts` of group `a` must be an array of strings"),
        "{e}"
    );
    let e = inv_err("[groups.a]\nhosts = [\"x@h\", 3]\n");
    assert!(e.starts_with("inv.toml:2: "), "{e}");
    let e = inv_err("[groups.a]\nhosts = [\"x@h\"]\nvars = 4\n");
    assert!(
        e.starts_with("inv.toml:3: `vars` of group `a` must be a table"),
        "{e}"
    );
    let e = inv_err("vars = 4\n");
    assert!(e.starts_with("inv.toml:1: `vars` must be a table"), "{e}");
}

#[test]
fn unknown_group_on_a_host() {
    let e = inv_err("[groups.a]\nhosts = [\"x@h\"]\n[hosts.\"x@h\"]\ngroups = [\"a\", \"nope\"]\n");
    assert!(
        e.starts_with("inv.toml:4: host `x@h` names group `nope`, which is not defined; groups: a"),
        "{e}"
    );
}

#[test]
fn unknown_vars_key_names_file_and_line() {
    let i = inv("[groups.a]\nhosts = [\"x@h\"]\n\nvars = { nope = 1 }\n");
    let e = i.check_vars::<Vars>().unwrap_err();
    assert!(e.starts_with("inv.toml:4: unknown key `nope`"), "{e}");
    let i = inv("[vars]\njobs = 1\nbogus = 2\n");
    let e = i.check_vars::<Vars>().unwrap_err();
    assert!(e.starts_with("inv.toml:3: unknown key `bogus`"), "{e}");
    let i = inv("[hosts.\"x@h\"]\nvars = { docker = { nope = 1 } }\n");
    let e = i.check_vars::<Vars>().unwrap_err();
    assert!(
        e.starts_with("inv.toml:2: unknown key `nope` in [docker]"),
        "{e}"
    );
}

#[test]
fn wrong_type_in_vars_names_file_and_line() {
    let i = inv("[groups.a]\nhosts = [\"x@h\"]\nvars = { jobs = \"many\" }\n");
    let h = &i.hosts()[0];
    let e = i.vars_for::<Vars>(h, None, "").unwrap_err();
    assert!(e.starts_with("inv.toml:3: "), "{e}");
}

#[test]
fn the_vars_file_still_reports_its_own_file_and_line() {
    let i = inv("[groups.a]\nhosts = [\"x@h\"]\n");
    let layer = VarLayer::parse("vars.toml", "\njobs = \"many\"\n").unwrap();
    let e = i
        .vars_for::<Vars>(&i.hosts()[0], Some(&layer), "")
        .unwrap_err();
    assert!(e.starts_with("vars.toml:2: "), "{e}");
}

// ---- --limit ----

#[test]
fn limit_by_group_by_host_and_both() {
    let i = inv(FULL);
    let names = |l: &[&str]| -> Vec<String> {
        i.limit(&l.iter().map(|s| s.to_string()).collect::<Vec<_>>())
            .unwrap()
            .iter()
            .map(|h| h.name.clone())
            .collect()
    };
    assert_eq!(names(&["gpu"]), vec!["bot@g1"]);
    assert_eq!(names(&["builders"]), vec!["bot@b1", "bot@b2", "bot@g1"]);
    assert_eq!(names(&["bot@b2"]), vec!["bot@b2"]);
    // a union, once each, in inventory order
    assert_eq!(
        names(&["bot@g1", "builders", "gpu"]),
        vec!["bot@b1", "bot@b2", "bot@g1"]
    );
}

#[test]
fn limit_with_an_unknown_name_is_an_error_listing_what_exists() {
    let i = inv(FULL);
    let e = i.limit(&["bulders".to_string()]).map(|_| ()).unwrap_err();
    assert!(
        e.starts_with("unknown --limit name `bulders`")
            && e.contains("groups: builders, gpu")
            && e.contains("hosts: bot@b1, bot@b2, bot@g1"),
        "{e}"
    );
}

#[test]
fn limit_to_an_empty_group_selects_nothing_and_says_so() {
    let i = inv("[groups.a]\nhosts = []\n[groups.b]\nhosts = [\"x@h\"]\n");
    let e = i.limit(&["a".to_string()]).map(|_| ()).unwrap_err();
    assert!(e.contains("selects no hosts"), "{e}");
}

// ---- in_group ----

#[test]
fn host_in_group() {
    let os = telepatch::facts::os::Os::fixture("ubuntu2404").unwrap();
    let h = Host::new("h", os.clone(), std::rc::Rc::new(()));
    assert!(!h.in_group("gpu"));
    let h = h.with_groups(vec!["builders".into(), "gpu".into()]);
    assert!(h.in_group("gpu") && h.in_group("builders") && !h.in_group("web"));
    assert!(h.clone().in_group("gpu"));
}

// ---- secrets ----

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct Svc {
    token: Option<telepatch::Secret>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct SVars {
    svc: Svc,
}

#[test]
fn secret_literals_in_an_inventory_are_found_by_place_and_reference_is_not() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("inventory.toml");
    std::fs::write(
        &p,
        "[vars]\ntoken = { env = \"TP_T99_OK\" }\n\
         [groups.a]\nhosts = [\"x@h\"]\nvars = { svc = { token = \"hunter2\" } }\n\
         [hosts.\"x@h\"]\nvars = { token = \"also-literal\" }\n",
    )
    .unwrap();
    let hits = secret_literals::<SVars>(std::slice::from_ref(&p)).unwrap();
    assert_eq!(hits.len(), 2, "{hits:?}");
    assert!(
        hits.iter().all(|h| h.starts_with(&p.display().to_string())),
        "{hits:?}"
    );
    assert!(
        hits.iter()
            .all(|h| !h.contains("hunter2") && !h.contains("also-literal")),
        "{hits:?}"
    );
}
