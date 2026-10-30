use std::rc::Rc;

use telepatch::facts::os::Os;
use telepatch::testing::FixtureHost;

pub fn fixture(host: &str, os: &str) -> FixtureHost<()> {
    let p = format!(
        "{}/tests/fixtures/os-release/{os}",
        env!("CARGO_MANIFEST_DIR")
    );
    let os = Os::parse(&std::fs::read_to_string(p).unwrap()).unwrap();
    FixtureHost::new(host, os, Rc::new(()))
}

pub fn ubuntu() -> FixtureHost<()> {
    fixture("web1", "ubuntu2404")
}
