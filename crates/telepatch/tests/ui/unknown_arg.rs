use telepatch::{Host, sh};
fn f(h: &Host<()>) {
    sh!(h, "echo hi", onlyif = "test -e /x");
}
fn main() {}
