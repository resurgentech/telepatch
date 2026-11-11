//! The app around the terminal: a menu bar built from the server's list of
//! pages, and a content pane showing the page the route names.
//!
//! Routes (in the URL hash): `/` is the page `home`, `/log` is `log`,
//! `/design/<section>` and `/reports/<name>` are pages with a two-part id.
//! `/design/<section>/<anchor>` is that page, scrolled to an element.

use dioxus::prelude::*;
use serde::Deserialize;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

use crate::terminal::Terminal;

#[derive(Clone, Debug, PartialEq, Routable)]
#[rustfmt::skip]
pub enum Route {
    #[layout(Frame)]
        #[route("/")]
        Home {},
        #[route("/log")]
        Log {},
        #[route("/design/:section")]
        Design { section: String },
        #[route("/design/:section/:anchor")]
        DesignAnchor { section: String, anchor: String },
        #[route("/reports/:name")]
        Report { name: String },
        #[route("/:..path")]
        NotFound { path: Vec<String> },
}

impl Route {
    /// The route that shows content page `id`.
    fn for_page(id: &str) -> Route {
        match id.split_once('/') {
            None if id == "home" => Route::Home {},
            None if id == "log" => Route::Log {},
            Some(("design", section)) => Route::Design {
                section: section.to_string(),
            },
            Some(("reports", name)) => Route::Report {
                name: name.to_string(),
            },
            _ => Route::NotFound {
                path: id.split('/').map(str::to_string).collect(),
            },
        }
    }

    /// The content page this route shows.
    fn page(&self) -> String {
        match self {
            Route::Home {} => "home".to_string(),
            Route::Log {} => "log".to_string(),
            Route::Design { section } | Route::DesignAnchor { section, .. } => format!("design/{section}"),
            Route::Report { name } => format!("reports/{name}"),
            Route::NotFound { path } => path.join("/"),
        }
    }
}

/// One entry of the menu bar. With no `items` it is a link to page `id`.
#[derive(Clone, Deserialize, PartialEq)]
struct Menu {
    title: String,
    id: Option<String>,
    items: Vec<Item>,
}

/// One line of a menu: a link to page `id`, or with no `id` a group label.
#[derive(Clone, Deserialize, PartialEq)]
struct Item {
    title: String,
    id: Option<String>,
}

/// The body of `url`; `None` when the server has no such thing.
async fn fetch(url: &str) -> Result<Option<String>, String> {
    let error = |e: wasm_bindgen::JsValue| format!("{e:?}");
    let window = web_sys::window().ok_or("no window")?;
    let response: web_sys::Response = JsFuture::from(window.fetch_with_str(url))
        .await
        .map_err(error)?
        .dyn_into()
        .map_err(error)?;
    if !response.ok() {
        return Ok(None);
    }
    let text = JsFuture::from(response.text().map_err(error)?).await.map_err(error)?;
    Ok(text.as_string())
}

/// The menu bar, the content pane and the shell. The shell is part of the
/// frame, not of a page, so it stays as it is across navigation.
#[component]
fn Frame() -> Element {
    rsx! {
        div { class: "app-bar",
            Link { class: "app-title", to: Route::Home {}, "telepatch" }
            Menus {}
        }
        main { class: "app-content", id: "pane", Outlet::<Route> {} }
        Terminal {}
    }
}

#[component]
fn Menus() -> Element {
    let menus = use_resource(|| async {
        let json = fetch("/content/menus").await?.unwrap_or_default();
        serde_json::from_str::<Vec<Menu>>(&json).map_err(|e| e.to_string())
    });
    // The index of the menu that is open.
    let mut open = use_signal(|| None::<usize>);
    let current = use_route::<Route>().page();

    let menus = match &*menus.read() {
        Some(Ok(menus)) => menus.clone(),
        Some(Err(e)) => return rsx! { span { class: "app-error", "menus: {e}" } },
        None => Vec::new(),
    };
    rsx! {
        div { class: "app-menus",
            // Covers the page under an open menu: a click outside closes it.
            if open().is_some() {
                div { class: "menu-backdrop", onclick: move |_| open.set(None) }
            }
            for (i, menu) in menus.into_iter().enumerate() {
                if let (Some(id), true) = (&menu.id, menu.items.is_empty()) {
                    Link { class: "menu-link", to: Route::for_page(id), "{menu.title}" }
                } else {
                    div {
                        class: "menu",
                        class: if menu.items.iter().any(|item| item.id.as_ref() == Some(&current)) { "current" },
                        button {
                            r#type: "button",
                            aria_expanded: open() == Some(i),
                            onclick: move |_| open.set(if open() == Some(i) { None } else { Some(i) }),
                            "{menu.title}"
                        }
                        if open() == Some(i) {
                            ul {
                                for item in menu.items.iter() {
                                    if let Some(id) = &item.id {
                                        li {
                                            Link {
                                                to: Route::for_page(id),
                                                onclick: move |_| open.set(None),
                                                "{item.title}"
                                            }
                                        }
                                    } else {
                                        li { class: "menu-group", "{item.title}" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Content page `id`, scrolled to the element `anchor` when there is one.
#[component]
fn Page(id: ReadSignal<String>, anchor: ReadSignal<Option<String>>) -> Element {
    let html = use_resource(move || async move { fetch(&format!("/content/page/{id}")).await });

    // Once the page's HTML is in the document, go where the link pointed.
    use_effect(move || {
        if html.read().is_none() {
            return;
        }
        let Some(document) = web_sys::window().and_then(|w| w.document()) else {
            return;
        };
        match anchor().and_then(|id| document.get_element_by_id(&id)) {
            Some(target) => target.scroll_into_view(),
            None => {
                if let Some(pane) = document.get_element_by_id("pane") {
                    pane.set_scroll_top(0);
                }
            }
        }
    });

    match &*html.read() {
        None => rsx! {},
        Some(Ok(Some(html))) => rsx! {
            div { class: "page", dangerous_inner_html: "{html}" }
        },
        Some(Ok(None)) => rsx! {
            div { class: "page",
                section { class: "prose",
                    h2 { "Not found" }
                    p { "There is no page " code { "{id}" } "." }
                }
            }
        },
        Some(Err(e)) => rsx! {
            div { class: "page",
                section { class: "prose",
                    h2 { "Could not load" }
                    p { "{e}" }
                }
            }
        },
    }
}

#[component]
fn Home() -> Element {
    rsx! { Page { id: "home", anchor: None::<String> } }
}

#[component]
fn Log() -> Element {
    rsx! { Page { id: "log", anchor: None::<String> } }
}

#[component]
fn Design(section: String) -> Element {
    rsx! { Page { id: "design/{section}", anchor: None::<String> } }
}

#[component]
fn DesignAnchor(section: String, anchor: String) -> Element {
    rsx! { Page { id: "design/{section}", anchor } }
}

#[component]
fn Report(name: String) -> Element {
    rsx! { Page { id: "reports/{name}", anchor: None::<String> } }
}

#[component]
fn NotFound(path: Vec<String>) -> Element {
    rsx! { Page { id: path.join("/"), anchor: None::<String> } }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_and_its_route_name_each_other() {
        for id in ["home", "log", "design/trace", "reports/2026-11-06-T73-x"] {
            assert_eq!(Route::for_page(id).page(), id);
        }
        let anchored = Route::DesignAnchor {
            section: "decided".into(),
            anchor: "d-pkg".into(),
        };
        assert_eq!(anchored.page(), "design/decided");
        assert_eq!(Route::for_page("nope").page(), "nope");
    }

    #[test]
    fn routes_are_the_hashes_the_server_writes_into_links() {
        assert_eq!(Route::for_page("home").to_string(), "/");
        assert_eq!(Route::for_page("design/trace").to_string(), "/design/trace");
        assert_eq!(
            "/design/decided/d-pkg".parse::<Route>().ok(),
            Some(Route::DesignAnchor {
                section: "decided".into(),
                anchor: "d-pkg".into()
            })
        );
        assert_eq!("/log".parse::<Route>().ok(), Some(Route::Log {}));
    }

    #[test]
    fn menus_parse_from_the_servers_json() {
        let menus: Vec<Menu> = serde_json::from_str(
            r#"[{"title":"Design","id":null,"items":[{"title":"Overview","id":"home"},{"title":"2026-01-02","id":null}]},
                {"title":"Log","id":"log","items":[]}]"#,
        )
        .unwrap();
        assert_eq!(menus[0].items[0].id.as_deref(), Some("home"));
        assert_eq!(menus[0].items[1].id, None);
        assert!(menus[1].items.is_empty());
    }
}
