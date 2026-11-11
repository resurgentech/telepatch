//! The page, in Dioxus: an app with menus and text ([`app`]) and a shell in
//! a frame below it ([`terminal`]).

mod app;
mod keys;
mod term;
mod terminal;

use std::rc::Rc;

use dioxus::prelude::*;

const APP_CSS: Asset = asset!("/assets/app.css");
/// The design note's fonts; its diagrams are sized for them.
const NOTE_FONTS: &str = "https://fonts.googleapis.com/css2?family=Newsreader:opsz,wght@6..72,500;6..72,600&family=IBM+Plex+Sans:wght@400;500;600&family=IBM+Plex+Mono:wght@400;500&display=swap";

fn main() {
    // Routes live in the URL hash. A link inside a page's own HTML
    // (`href="#/design/trace"`) then navigates within the document; loading
    // it again would start a new shell.
    let history = Rc::new(dioxus::web::HashHistory::new(false));
    dioxus::LaunchBuilder::web()
        .with_cfg(dioxus::web::Config::new().history(history))
        .launch(App);
}

#[component]
fn App() -> Element {
    rsx! {
        document::Title { "telepatch" }
        document::Stylesheet { href: NOTE_FONTS }
        // The design note's stylesheet; the pages are written against it.
        document::Stylesheet { href: "/content/style.css" }
        document::Stylesheet { href: APP_CSS }
        Router::<app::Route> {}
    }
}
