use telepatch::{Host, sh};
fn f(h: &Host<()>) {
    let x = 1;
    sh!(h, "echo {x:?}");
}
fn main() {}
