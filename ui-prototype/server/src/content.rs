//! The app's text: a `docs/` folder turned into menus and pages.
//!
//! - `execution-model.html` (the design note) is cut at each `<h2>` into one
//!   page per section, `design/<section id>`; its header and contents list
//!   are `home`; its stylesheet is served as the content stylesheet.
//! - `LOG.md` is the page `log`.
//! - `reports/*.md` are the pages `reports/<file stem>`, newest first.
//!
//! Everything is read from disk on each request, so the app shows the
//! folder as it is now.

use std::collections::HashMap;
use std::path::PathBuf;

use pulldown_cmark::{Event, Options, Parser, html};
use serde::Serialize;

const NOTE: &str = "execution-model.html";
const LOG: &str = "LOG.md";
const REPORTS: &str = "reports";

/// One entry of the menu bar. With no `items` it is a link to page `id`.
#[derive(Debug, Serialize, PartialEq)]
pub struct Menu {
    pub title: String,
    pub id: Option<String>,
    pub items: Vec<Item>,
}

/// One line of a menu: a link to page `id`, or with no `id` a group label.
#[derive(Debug, Serialize, PartialEq)]
pub struct Item {
    pub title: String,
    pub id: Option<String>,
}

pub struct Docs(PathBuf);

struct Note {
    style: String,
    home: String,
    /// `(section id, title, html)` in document order.
    sections: Vec<(String, String, String)>,
}

impl Docs {
    pub fn new(root: impl Into<PathBuf>) -> Docs {
        Docs(root.into())
    }

    fn read(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.0.join(name)).ok()
    }

    fn note(&self) -> Option<Note> {
        self.read(NOTE).map(|src| parse_note(&src))
    }

    /// `(file stem, title)` of every report, newest first.
    fn reports(&self) -> Vec<(String, String)> {
        let Ok(dir) = std::fs::read_dir(self.0.join(REPORTS)) else {
            return Vec::new();
        };
        let mut stems: Vec<String> = dir
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter_map(|name| name.strip_suffix(".md").map(str::to_string))
            .collect();
        stems.sort_by(|a, b| b.cmp(a));
        stems
            .into_iter()
            .map(|stem| {
                let text = self.read(&format!("{REPORTS}/{stem}.md")).unwrap_or_default();
                let title = text
                    .lines()
                    .find_map(|l| l.strip_prefix("# "))
                    .map(|t| t.replace('`', ""))
                    .unwrap_or_else(|| stem.clone());
                (stem, title)
            })
            .collect()
    }

    pub fn menus(&self) -> Vec<Menu> {
        let mut menus = Vec::new();
        if let Some(note) = self.note() {
            let mut items = vec![Item {
                title: "Overview".into(),
                id: Some("home".into()),
            }];
            items.extend(note.sections.into_iter().map(|(id, title, _)| Item {
                title,
                id: Some(format!("design/{id}")),
            }));
            menus.push(Menu {
                title: "Design".into(),
                id: None,
                items,
            });
        }
        if self.read(LOG).is_some() {
            menus.push(Menu {
                title: "Log".into(),
                id: Some("log".into()),
                items: Vec::new(),
            });
        }
        let reports = self.reports();
        if !reports.is_empty() {
            // Grouped by the date the file name starts with.
            let mut items: Vec<Item> = Vec::new();
            let mut day = String::new();
            for (stem, title) in reports {
                let date = stem.get(..10).unwrap_or("").to_string();
                if date != day {
                    items.push(Item {
                        title: date.clone(),
                        id: None,
                    });
                    day = date;
                }
                items.push(Item {
                    title,
                    id: Some(format!("{REPORTS}/{stem}")),
                });
            }
            menus.push(Menu {
                title: "Reports".into(),
                id: None,
                items,
            });
        }
        menus
    }

    /// The HTML of page `id`, or `None` when there is no such page.
    pub fn page(&self, id: &str) -> Option<String> {
        if id == "home" {
            return self.note().map(|n| n.home);
        }
        if id == "log" {
            return self.read(LOG).map(|md| markdown(&md));
        }
        if let Some(section) = id.strip_prefix("design/") {
            let (_, _, html) = self.note()?.sections.into_iter().find(|(s, ..)| s == section)?;
            return Some(html);
        }
        if let Some(stem) = id.strip_prefix("reports/") {
            // Only a name the folder listing produced is ever opened.
            let (stem, _) = self.reports().into_iter().find(|(s, _)| s == stem)?;
            return self.read(&format!("{REPORTS}/{stem}.md")).map(|md| markdown(&md));
        }
        None
    }

    /// The design note's stylesheet; the pages are written against it.
    pub fn style(&self) -> String {
        self.note().map(|n| n.style).unwrap_or_default()
    }
}

/// The text between the first `open` and the next `close`, and the rest.
fn between<'a>(src: &'a str, open: &str, close: &str) -> Option<(&'a str, &'a str)> {
    let start = src.find(open)? + open.len();
    let len = src[start..].find(close)?;
    Some((&src[start..start + len], &src[start + len + close.len()..]))
}

/// `open` through `close`, tags included, and the rest.
fn element<'a>(src: &'a str, open: &str, close: &str) -> Option<(&'a str, &'a str)> {
    let start = src.find(open)?;
    let end = start + src[start..].find(close)? + close.len();
    Some((&src[start..end], &src[end..]))
}

fn strip_tags(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">")
}

/// Every `id="..."` value in `html`.
fn ids(html: &str) -> Vec<&str> {
    html.split(" id=\"")
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .collect()
}

fn parse_note(src: &str) -> Note {
    let style = between(src, "<style>", "</style>").map_or("", |(css, _)| css);
    let (header, rest) = element(src, "<header>", "</header>").unwrap_or(("", src));
    let (toc, body) = element(rest, "<nav class=\"toc\"", "</nav>").unwrap_or(("", rest));

    // A `<section>` holding an `<h2>` starts a page; what follows it up to
    // the next such section (figures, further sections) belongs to it.
    let mut sections: Vec<(String, String, String)> = Vec::new();
    for (i, chunk) in body.split("<section").enumerate() {
        if i == 0 {
            continue;
        }
        let chunk = format!("<section{chunk}");
        match between(&chunk, "<h2>", "</h2>") {
            Some((h2, _)) => {
                let title = strip_tags(h2);
                let tag_end = chunk.find('>').unwrap_or(0);
                let id = ids(&chunk[..tag_end])
                    .first()
                    .map_or_else(|| format!("section-{}", sections.len() + 1), |id| id.to_string());
                sections.push((id, title, chunk));
            }
            None => {
                if let Some((.., html)) = sections.last_mut() {
                    html.push_str(&chunk);
                }
            }
        }
    }
    // The note's closing `</div>` ends up on the last page.
    if let Some((.., html)) = sections.last_mut()
        && let Some(end) = html.rfind("</section>")
    {
        html.truncate(end + "</section>".len());
    }

    // The note links by `#id` within one document. Here an id lives on one
    // of several pages, so a link names the page too.
    let mut owner: HashMap<String, String> = HashMap::new();
    for (page, _, html) in &sections {
        for id in ids(html) {
            owner.entry(id.to_string()).or_insert_with(|| page.clone());
        }
    }
    let relink = |html: &str| {
        let mut out = String::with_capacity(html.len());
        let mut parts = html.split("href=\"#");
        out.push_str(parts.next().unwrap_or(""));
        for part in parts {
            let (id, rest) = part.split_once('"').unwrap_or((part, ""));
            match owner.get(id) {
                Some(page) if page == id => out.push_str(&format!("href=\"#/design/{id}\"")),
                Some(page) => out.push_str(&format!("href=\"#/design/{page}/{id}\"")),
                None => out.push_str(&format!("href=\"#{id}\"")),
            }
            out.push_str(rest);
        }
        out
    };
    let home = relink(&format!("{header}\n{toc}"));
    for (.., html) in &mut sections {
        *html = relink(html);
    }
    Note {
        style: style.to_string(),
        home,
        sections,
    }
}

/// Markdown as a page. HTML written inside the Markdown is shown as text,
/// not passed through.
fn markdown(src: &str) -> String {
    let events = Parser::new_ext(src, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH).map(
        |event| match event {
            Event::Html(raw) | Event::InlineHtml(raw) => Event::Text(raw),
            other => other,
        },
    );
    let mut out = String::from("<section class=\"prose md\">");
    html::push_html(&mut out, events);
    out.push_str("</section>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTE_SRC: &str = r##"<title>t</title>
<style>
h2 { color: red; }
</style>
<div class="page">
<header><h1>Note</h1></header>
<nav class="toc" aria-label="Sections"><ol><li><a href="#one">One</a></li></ol></nav>

<section class="prose" id="one">
<h2>One &amp; <code>only</code></h2>
<p>See <a href="#two">two</a> and <a href="#deep">deep</a> and <a href="#nowhere">x</a>.</p>
</section>

<figure><svg id="fig"></svg></figure>

<section class="prose">
<p>Still one.</p>
</section>

<section class="prose" id="two">
<h2>Two</h2>
<section class="prose" id="deep"><h3>Deep</h3></section>
</section>
</div>
"##;

    /// A docs folder of its own for the test `name`.
    fn docs(name: &str) -> (Docs, PathBuf) {
        let dir = std::env::temp_dir().join(format!("ui-prototype-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("reports")).unwrap();
        std::fs::write(dir.join(NOTE), NOTE_SRC).unwrap();
        std::fs::write(dir.join(LOG), "# Log\n\n## Entry\n\n- `a` <script>x</script>\n").unwrap();
        std::fs::write(dir.join("reports/2026-01-01-T1-first.md"), "# T1 — `first`\n\nbody\n").unwrap();
        std::fs::write(dir.join("reports/2026-01-02-T2-second.md"), "no heading\n").unwrap();
        std::fs::write(dir.join("reports/notes.txt"), "ignored").unwrap();
        (Docs::new(&dir), dir)
    }

    #[test]
    fn the_note_is_cut_at_each_h2() {
        let note = parse_note(NOTE_SRC);
        let titles: Vec<_> = note.sections.iter().map(|(id, t, _)| (id.as_str(), t.as_str())).collect();
        assert_eq!(titles, [("one", "One & only"), ("two", "Two")]);
        let one = &note.sections[0].2;
        assert!(one.contains("<svg id=\"fig\">") && one.contains("Still one."));
        assert!(!one.contains("<h2>Two</h2>"));
        assert!(note.sections[1].2.trim_end().ends_with("</section>"));
        assert_eq!(note.style.trim(), "h2 { color: red; }");
    }

    #[test]
    fn links_name_the_page_that_holds_the_target() {
        let note = parse_note(NOTE_SRC);
        assert!(note.home.contains("<h1>Note</h1>"));
        assert!(note.home.contains("href=\"#/design/one\""));
        let one = &note.sections[0].2;
        assert!(one.contains("href=\"#/design/two\""));
        assert!(one.contains("href=\"#/design/two/deep\""));
        assert!(one.contains("href=\"#nowhere\""));
    }

    #[test]
    fn menus_list_the_folder() {
        let (docs, dir) = docs("menus");
        let menus = docs.menus();
        let titles: Vec<_> = menus.iter().map(|m| m.title.as_str()).collect();
        assert_eq!(titles, ["Design", "Log", "Reports"]);
        let design: Vec<_> = menus[0].items.iter().map(|i| i.id.as_deref()).collect();
        assert_eq!(design, [Some("home"), Some("design/one"), Some("design/two")]);
        assert_eq!(menus[1].id.as_deref(), Some("log"));
        let reports: Vec<_> = menus[2]
            .items
            .iter()
            .map(|i| (i.title.as_str(), i.id.as_deref()))
            .collect();
        assert_eq!(
            reports,
            [
                ("2026-01-02", None),
                ("2026-01-02-T2-second", Some("reports/2026-01-02-T2-second")),
                ("2026-01-01", None),
                ("T1 — first", Some("reports/2026-01-01-T1-first")),
            ]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn every_menu_entry_has_a_page_and_nothing_else_does() {
        let (docs, dir) = docs("pages");
        for menu in docs.menus() {
            let ids = menu.id.into_iter().chain(menu.items.into_iter().filter_map(|i| i.id));
            for id in ids {
                assert!(docs.page(&id).is_some_and(|html| !html.is_empty()), "{id}");
            }
        }
        for id in ["", "design/none", "reports/notes", "reports/../LOG", "../Cargo.toml"] {
            assert!(docs.page(id).is_none(), "{id}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn html_inside_markdown_is_shown_as_text() {
        let (docs, dir) = docs("markdown");
        let log = docs.page("log").unwrap();
        assert!(log.contains("<h2>Entry</h2>") && log.contains("<code>a</code>"));
        assert!(log.contains("&lt;script&gt;") && !log.contains("<script>"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
