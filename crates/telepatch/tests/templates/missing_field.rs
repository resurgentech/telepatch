use askama::Template;

#[derive(Template)]
#[template(source = "hi {{ name }}", ext = "txt")]
struct Hi {
    other: String,
}

fn main() {}
