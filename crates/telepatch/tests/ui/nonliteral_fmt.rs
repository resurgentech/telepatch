use telepatch::{Host, sh};
fn f(h: &Host<()>) {
    let cmd = "echo hi";
    sh!(h, cmd);
}
fn main() {}
