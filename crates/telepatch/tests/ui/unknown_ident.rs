use telepatch::{Host, sh};
fn f(h: &Host<()>) {
    sh!(h, "echo {nope}");
}
fn main() {}
