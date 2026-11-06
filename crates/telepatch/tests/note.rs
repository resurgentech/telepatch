//! The design note is the contract: it states the decided API, says what
//! is built and what is designed, and keeps the removed forms only in its
//! "Removed" list.

use std::path::PathBuf;

/// Tokens of the API that the decided design removed. Each may appear in
/// the note only inside the section with `id="removed"`.
const FORBIDDEN: &[&str] = &[
    "tpl!",
    "h.pip(",
    "h.script(",
    "h.cargo(",
    "h.mise(",
    "h.rustup(",
    "h.yum(",
    "ScriptSpec",
    "secret(\"",
];

/// One heading per entry of the 2026-11-05 design review, in its order.
const DECIDED_HEADINGS: &[&str] = &[
    "No shell in a role body",
    "Policy errors",
    "Author-defined resources",
    "Service state",
    "Restart and reload carry a guard",
    "Change triggers",
    "Unit files and drop-ins",
    "Reboot",
    "One pkg, every source chained",
    "Package removal",
    "Package from a file or URL",
    "cargo and rustup options",
    "Fetch and unpack",
    "Controller-side files and commands",
    "Git checkout",
    "Templates",
    "Vars per role",
    "Secrets",
    "The telepatch-openbao crate",
    "Facts and assertions",
    "Dry run with diffs",
    "Inventory",
    "Run controls",
    "Held, and out",
];

fn note_html() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/execution-model.html");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The text a reader sees: tags dropped, the common entities decoded.
fn text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

/// The byte range of the element that carries `id="<id>"`, from its
/// opening `<` to the end of its closing tag. The element must not nest
/// another element of the same tag name.
fn element_with_id(html: &str, id: &str) -> Option<std::ops::Range<usize>> {
    let attr = format!("id=\"{id}\"");
    let at = html.find(&attr)?;
    let start = html[..at].rfind('<')?;
    let name: String = html[start + 1..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect();
    let close = format!("</{name}>");
    let end = at + html[at..].find(&close)? + close.len();
    Some(start..end)
}

/// The note with its "Removed" section cut out, and that section alone.
fn split_removed(html: &str) -> (String, String) {
    match element_with_id(html, "removed") {
        Some(r) => (
            format!("{}{}", &html[..r.start], &html[r.end..]),
            html[r].to_string(),
        ),
        None => (html.to_string(), String::new()),
    }
}

/// The text of every `<h2>` and `<h3>` heading.
fn headings(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    for level in ["h2", "h3"] {
        let open = format!("<{level}");
        let close = format!("</{level}>");
        let mut rest = html;
        while let Some(i) = rest.find(&open) {
            let after = &rest[i..];
            let Some(gt) = after.find('>') else { break };
            let Some(end) = after.find(&close) else { break };
            out.push(text(&after[gt + 1..end]).trim().to_string());
            rest = &after[end + close.len()..];
        }
    }
    out
}

#[test]
fn removed_forms_appear_only_in_the_removed_list() {
    let html = note_html();
    let (outside, removed) = split_removed(&html);
    let outside = text(&outside);
    let removed = text(&removed);
    let mut hits = Vec::new();
    for token in FORBIDDEN {
        for (n, line) in outside.lines().enumerate() {
            if line.contains(token) {
                hits.push(format!(
                    "{token:?} outside \"Removed\", text line {}: {}",
                    n + 1,
                    line.trim()
                ));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "removed API in the note:\n{}",
        hits.join("\n")
    );
    assert!(
        !removed.is_empty(),
        "the note has no section with id=\"removed\""
    );
    for token in FORBIDDEN {
        assert!(
            removed.contains(token),
            "the \"Removed\" list does not name {token:?}"
        );
    }
}

#[test]
fn every_decided_entry_has_its_heading() {
    let html = note_html();
    let found = headings(&html);
    let missing: Vec<&str> = DECIDED_HEADINGS
        .iter()
        .copied()
        .filter(|h| !found.iter().any(|f| f == h))
        .collect();
    assert!(
        missing.is_empty(),
        "decided headings missing from the note: {missing:?}"
    );
    for h in DECIDED_HEADINGS {
        let n = found.iter().filter(|f| f == h).count();
        assert_eq!(n, 1, "heading {h:?} appears {n} times");
    }
}

#[test]
fn every_primitive_row_says_built_or_designed() {
    let html = note_html();
    let r =
        element_with_id(&html, "primitives-table").expect("a table with id=\"primitives-table\"");
    let table = &html[r];
    let body = &table[table.find("<tbody>").expect("tbody")..];
    let rows: Vec<&str> = body.split("<tr>").skip(1).collect();
    assert!(rows.len() >= 10, "only {} primitive rows", rows.len());
    for row in rows {
        let name = text(row.split("</td>").next().unwrap_or(""))
            .trim()
            .to_string();
        let status = row
            .find("<td class=\"status\">")
            .map(|i| {
                let cell = &row[i..];
                text(&cell[..cell.find("</td>").unwrap_or(cell.len())])
                    .trim()
                    .to_string()
            })
            .unwrap_or_else(|| panic!("row {name:?} has no status cell"));
        assert!(
            status.starts_with("built") || status.starts_with("designed"),
            "row {name:?}: status {status:?} says neither built nor designed"
        );
    }
}

#[test]
fn the_unruled_and_held_list_is_in_one_place() {
    let html = note_html();
    let r = element_with_id(&html, "held").expect("a section with id=\"held\"");
    let held = text(&html[r]);
    for needle in [
        "mounts and filesystems",
        "firewall",
        "udev",
        "time zone",
        "system upgrade",
        "package groups",
        "h.kernel_args()",
        "per-user systemd",
        ".force()",
        "--allow-destructive",
    ] {
        assert!(
            held.contains(needle),
            "the held list does not mention {needle:?}"
        );
    }
}

#[test]
fn text_strips_tags_and_entities() {
    assert_eq!(
        text("<code>h.pip(</code>&lt;x&gt; &amp; <span class=\"w\">\"a\"</span>"),
        "h.pip(<x> & \"a\""
    );
}

/// Builders return an owned handle, so the note's examples bind a chain
/// in one statement, and the note says so where it describes handles.
#[test]
fn the_examples_bind_a_chain_and_handles_are_owned() {
    let html = note_html();
    let (outside, _) = split_removed(&html);
    let t = text(&outside);
    for bound in [
        "let daemon = h.file(\"/etc/docker/daemon.json\").content(",
        "let unit = h\n    .unit(\"minikube.service\")\n    .content(",
        "let toolkit = h.pkg(",
    ] {
        assert!(t.contains(bound), "the note has no example {bound:?}");
    }
    // the two-statement form a `&Self` builder forced is gone
    for split in [
        "let daemon = h.file(\"/etc/docker/daemon.json\");",
        "let unit = h.unit(\"minikube.service\");",
    ] {
        assert!(!t.contains(split), "the note still binds first: {split:?}");
    }
    let r = element_with_id(&html, "trace").expect("a section with id=\"trace\"");
    let trace = text(&html[r]);
    for needle in [
        "Every builder on every handle returns the handle, owned",
        "s.reload(); s.when(g);",
        "Fetched",
    ] {
        assert!(
            trace.contains(needle),
            "the trace section does not say {needle:?}"
        );
    }
}
