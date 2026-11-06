//! Vars per role: four layers, merged at the TOML level per section.

use serde::Deserialize;
use telepatch::vars::{load, load_file};

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

fn v(file: &str, defaults: &str) -> Vars {
    load("t.toml", file, defaults).unwrap()
}

fn e(file: &str, defaults: &str) -> String {
    load::<Vars>("t.toml", file, defaults).unwrap_err()
}

fn users(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|s| s.to_string()).collect()
}

#[test]
fn role_default_is_the_last_layer() {
    assert_eq!(v("", ""), Vars::default());
}

#[test]
fn playbook_default_beats_role_default() {
    let r = v("", "admin_users = [\"d\"]\n[rust_build]\njobs = 9\n");
    assert_eq!(r.docker.admin_users, users(&["d"]));
    assert_eq!(r.rust_build.admin_users, users(&["d"]));
    assert_eq!(r.rust_build.jobs, 9);
    assert_eq!(r.rust_build.rustc_version, "1.94.0");
}

#[test]
fn top_level_key_beats_playbook_default() {
    let r = v(
        "admin_users = [\"top\"]\n",
        "admin_users = [\"d\"]\n[rust_build]\nadmin_users = [\"dsec\"]\n",
    );
    // the file's top-level key wins over the defaults' section key too
    assert_eq!(r.docker.admin_users, users(&["top"]));
    assert_eq!(r.rust_build.admin_users, users(&["top"]));
}

#[test]
fn section_key_beats_top_level_key_for_that_section_only() {
    let r = v(
        "admin_users = [\"top\"]\n[rust_build]\nadmin_users = [\"own\"]\n",
        "",
    );
    assert_eq!(r.docker.admin_users, users(&["top"]));
    assert_eq!(r.rust_build.admin_users, users(&["own"]));
}

#[test]
fn a_top_level_key_shared_by_two_sections_reaches_both() {
    let r = v("admin_users = [\"a\", \"b\"]\n", "");
    assert_eq!(r.docker.admin_users, users(&["a", "b"]));
    assert_eq!(r.rust_build.admin_users, users(&["a", "b"]));
    // a key only one section has reaches only that one
    let r = v("rustc_version = \"2.0\"\n", "");
    assert_eq!(r.rust_build.rustc_version, "2.0");
    assert_eq!(r.docker, Docker::default());
}

#[test]
fn layers_win_per_field_not_per_section() {
    let r = v(
        "[rust_build]\nrustc_version = \"3\"\n",
        "[rust_build]\nrustc_version = \"2\"\njobs = 7\n",
    );
    assert_eq!(r.rust_build.rustc_version, "3");
    assert_eq!(r.rust_build.jobs, 7);
    assert_eq!(r.rust_build.admin_users, users(&["root"]));
}

#[test]
fn top_level_key_no_role_has_is_an_error_naming_file_and_line() {
    let m = e("[docker]\nchannel = \"x\"\n\nbogus = 1\n", "");
    assert!(m.starts_with("t.toml:4:") && m.contains("bogus"), "{m}");
    // before any table header too
    let m = e("# c\nbogus = 1\n", "");
    assert!(m.starts_with("t.toml:2:"), "{m}");
}

#[test]
fn unknown_key_in_a_section_is_an_error_naming_file_and_line() {
    let m = e(
        "admin_users = [\"a\"]\n\n[docker]\nchannel = \"x\"\nrustup = \"1\"\n",
        "",
    );
    assert!(
        m.starts_with("t.toml:5:") && m.contains("rustup") && m.contains("[docker]"),
        "{m}"
    );
}

#[test]
fn wrong_type_is_an_error_naming_file_and_line() {
    let m = e("\n\n[rust_build]\njobs = \"many\"\n", "");
    assert!(
        m.starts_with("t.toml:4:") && m.contains("invalid type"),
        "{m}"
    );
    let m = e("admin_users = \"root\"\n", "");
    assert!(m.starts_with("t.toml:1:"), "{m}");
    // a section given a scalar
    let m = e("x = 1\n", "");
    assert!(m.starts_with("t.toml:1:"), "{m}");
    let m = e("docker = 1\n", "");
    assert!(m.starts_with("t.toml:1:") && m.contains("table"), "{m}");
}

#[test]
fn syntax_error_names_the_file() {
    let m = e("jobs = \n", "");
    assert!(m.starts_with("cannot parse vars file t.toml:"), "{m}");
}

#[test]
fn bad_playbook_defaults_name_the_defaults() {
    let m = e("", "bogus = 1\n");
    assert!(m.starts_with("playbook defaults (line 1):"), "{m}");
    let m = e("", "jobs = \"x\"\n");
    assert!(m.contains("playbook defaults"), "{m}");
}

#[test]
fn no_file_equals_an_empty_file() {
    let none: Vars = load_file(None, "").unwrap();
    assert_eq!(none, v("", ""));
    let d = "admin_users = [\"d\"]\n";
    let none: Vars = load_file(None, d).unwrap();
    assert_eq!(none, v("", d));
}

#[test]
fn load_file_reads_a_path_and_reports_a_missing_one() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("v.toml");
    std::fs::write(&p, "[docker]\nchannel = \"edge\"\n").unwrap();
    let r: Vars = load_file(Some(&p), "").unwrap();
    assert_eq!(r.docker.channel, "edge");
    let m = load_file::<Vars>(Some(&dir.path().join("no.toml")), "").unwrap_err();
    assert!(m.starts_with("cannot read vars file"), "{m}");
    std::fs::write(&p, "[docker]\nnope = 1\n").unwrap();
    let m = load_file::<Vars>(Some(&p), "").unwrap_err();
    assert!(m.starts_with(&format!("{}:2:", p.display())), "{m}");
}

// An optional section and a flat (pass-through) field.
#[derive(Debug, PartialEq, Default, Deserialize)]
#[serde(default)]
struct Flat {
    dir: String,
    extra: Option<Docker>,
    count: Option<u32>,
}

#[test]
fn optional_and_scalar_sections() {
    let r: Flat = load(
        "t.toml",
        "dir = \"/x\"\ncount = 3\n[extra]\nchannel = \"c\"\n",
        "",
    )
    .unwrap();
    assert_eq!(r.dir, "/x");
    assert_eq!(r.count, Some(3));
    assert_eq!(r.extra.unwrap().channel, "c");
    let m = load::<Flat>("t.toml", "dir = \"/x\"\nzzz = 1\n", "").unwrap_err();
    assert!(m.starts_with("t.toml:2:"), "{m}");
}

// A section named like a field of another section: `admin_users` is both.
#[derive(Debug, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct Accounts {
    admin_users: Vec<String>,
    password: String,
}
#[derive(Debug, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct Shared {
    admin_users: Accounts,
    docker: Docker,
}

#[test]
fn a_section_named_like_a_field_elsewhere() {
    // the array is the field for both; the table is only the section
    let r: Shared = load("t.toml", "admin_users = [\"a\"]\n", "").unwrap();
    assert_eq!(r.admin_users.admin_users, users(&["a"]));
    assert_eq!(r.docker.admin_users, users(&["a"]));
    let r: Shared = load("t.toml", "[admin_users]\npassword = \"p\"\n", "").unwrap();
    assert_eq!(r.admin_users.password, "p");
    assert_eq!(r.docker, Docker::default());
}
