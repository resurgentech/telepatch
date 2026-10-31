//! `tp check-understanding`: mechanical checks on an understanding doc.
//!
//! Rules, one problem per output line:
//! - every level heading is present as an exact line, in order;
//!   `## skill-feedback` is optional, but when present it comes last
//!   (`Options::require_feedback` makes it required);
//! - every line containing `http://` or `https://` carries a `YYYY-MM-DD`;
//! - no hedge word outside fenced code blocks and the generated
//!   `## Current method` section;
//! - the `## Recommendation and decision` section has a `Decision:` line.

use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

/// Level headings every doc carries, in review order.
pub const REQUIRED_HEADINGS: [&str; 9] = [
    "## Purpose",
    "## Outcomes",
    "## Task intent",
    "## Current method",
    "## Preserved knowledge",
    "## Is the method current",
    "## Recommendation and decision",
    "## Port notes",
    "## Review",
];

/// Heading of the trial-only section.
pub const FEEDBACK_HEADING: &str = "## skill-feedback";

const CURRENT_METHOD: &str = "## Current method";
const TASK_INTENT: &str = "## Task intent";
/// Columns of the Task intent table the agent must fill; the tool's
/// skeleton leaves them empty.
const JUDGMENT_COLUMNS: [&str; 4] = ["Intent", "Shape", "Adequate", "Primitive"];
const DECISION_SECTION: &str = "## Recommendation and decision";

/// Words that state doubt instead of a fact (whole word, any case).
pub const HEDGE_WORDS: [&str; 12] = [
    "probably",
    "might",
    "maybe",
    "perhaps",
    "seems",
    "likely",
    "could be",
    "possibly",
    "arguably",
    "somewhat",
    "presumably",
    "apparently",
];

#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Require `## skill-feedback` (a trial of the skill).
    pub require_feedback: bool,
}

fn hedge_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let alt: Vec<String> = HEDGE_WORDS.iter().map(|w| w.replace(' ', r"\s+")).collect();
        Regex::new(&format!(r"(?i)\b({})\b", alt.join("|"))).unwrap()
    })
}

fn date_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(^|[^0-9])[0-9]{4}-[0-9]{2}-[0-9]{2}($|[^0-9])").unwrap())
}

/// Check `text`; `name` prefixes each problem. Empty result means pass.
pub fn check(name: &str, text: &str, opts: Options) -> Vec<String> {
    let mut problems = Vec::new();
    let mut fenced = false;
    let mut section = String::new();
    // heading -> line number of first occurrence outside fences
    let mut headings: Vec<(String, usize)> = Vec::new();
    let mut has_decision = false;
    // Task intent table: (column index, column name) of the judgment columns,
    // taken from the header row; None until the header is seen.
    let mut intent_cols: Option<Vec<(usize, String)>> = None;

    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if !fenced && line.starts_with("## ") {
            section = line.trim_end().to_string();
            headings.push((section.clone(), n));
        }
        if !fenced && section == TASK_INTENT {
            if line.starts_with('|') {
                let cells: Vec<&str> = line
                    .trim()
                    .trim_matches('|')
                    .split('|')
                    .map(str::trim)
                    .collect();
                match &intent_cols {
                    None => {
                        intent_cols = Some(
                            cells
                                .iter()
                                .enumerate()
                                .filter(|(_, c)| JUDGMENT_COLUMNS.contains(c))
                                .map(|(i, c)| (i, (*c).to_string()))
                                .collect(),
                        );
                    }
                    Some(cols) if !cells.iter().all(|c| c.chars().all(|ch| ch == '-')) => {
                        for (i, col) in cols {
                            if cells.get(*i).is_none_or(|c| c.is_empty()) {
                                problems.push(format!(
                                    "{name}:{n}: Task intent row has an empty '{col}' cell"
                                ));
                            }
                        }
                    }
                    Some(_) => {}
                }
            } else if line.trim() == "Primitive gaps:" {
                problems.push(format!(
                    "{name}:{n}: 'Primitive gaps:' says nothing (write the gaps, or 'none')"
                ));
            }
        }
        if !fenced
            && section == DECISION_SECTION
            && line.starts_with("Decision:")
            && line.trim() != "Decision:"
        {
            has_decision = true;
        }
        // A URL in backticks is a command or a cell, not a source; the
        // source-line rule applies to URLs outside backticks.
        let outside_ticks: String = line.split('`').step_by(2).collect::<Vec<_>>().join(" ");
        if !fenced
            && (outside_ticks.contains("http://") || outside_ticks.contains("https://"))
            && !date_re().is_match(line)
        {
            problems.push(format!("{name}:{n}: source line without a YYYY-MM-DD date"));
        }
        if !fenced && section != CURRENT_METHOD {
            for m in hedge_re().find_iter(line) {
                problems.push(format!(
                    "{name}:{n}:{}",
                    m.as_str()
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .to_lowercase()
                ));
            }
        }
    }

    let feedback = opts.require_feedback || headings.iter().any(|(t, _)| t == FEEDBACK_HEADING);
    let mut last = 0usize;
    for h in REQUIRED_HEADINGS
        .iter()
        .chain(feedback.then_some(&FEEDBACK_HEADING))
    {
        match headings.iter().find(|(t, _)| t == h) {
            None => problems.push(format!("{name}: missing heading '{h}'")),
            Some((_, n)) if *n < last => {
                problems.push(format!("{name}:{n}: heading '{h}' is out of order"))
            }
            Some((_, n)) => last = *n,
        }
    }
    if !has_decision {
        problems.push(format!(
            "{name}: no 'Decision:' line in '{DECISION_SECTION}'"
        ));
    }
    problems.sort_by_key(|p| {
        // stable file order: line-numbered problems first by line
        p.split(':')
            .nth(1)
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(usize::MAX)
    });
    problems
}

/// Number of data rows in the `## Task intent` table (header and rule
/// excluded), for the row-count check against the playbook.
pub fn intent_row_count(text: &str) -> usize {
    let mut fenced = false;
    let mut in_section = false;
    let mut rows = 0usize;
    let mut seen_header = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        if line.starts_with("## ") {
            in_section = line.trim_end() == TASK_INTENT;
            seen_header = false;
            continue;
        }
        if in_section && line.starts_with('|') {
            if !seen_header {
                seen_header = true;
            } else if !line
                .trim()
                .trim_matches('|')
                .chars()
                .all(|c| c == '-' || c == '|' || c == ' ')
            {
                rows += 1;
            }
        }
    }
    rows
}

/// Read and check a file.
pub fn check_file(path: &Path, opts: Options) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(check(&path.display().to_string(), &text, opts))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "# t\n\n## Purpose\n\nx\n\n## Outcomes\n\nx\n\n## Task intent\n\nx\n\n## Current method\n\ng\n\n## Preserved knowledge\n\nx\n\n## Is the method current\n\nx\n\n## Recommendation and decision\n\nDecision: take option 1.\n\n## Port notes\n\n## Review\n\nx\n";

    fn run(t: &str) -> Vec<String> {
        check("d.md", t, Options::default())
    }

    #[test]
    fn good_passes() {
        assert_eq!(run(GOOD), Vec::<String>::new());
    }

    const TABLE: &str = "| Task | Intent | Shape | Method | Signals | Adequate | Primitive |\n|---|---|---|---|---|---|---|\n| `a.yml:1` x | x present | package | `apt` | | yes | pkg |\n\nPrimitive gaps: none.\n";

    #[test]
    fn filled_intent_table_passes() {
        let t = GOOD.replace(
            "## Task intent\n\nx\n",
            &format!("## Task intent\n\n{TABLE}"),
        );
        assert_eq!(run(&t), Vec::<String>::new());
    }

    #[test]
    fn empty_judgment_cell_is_reported() {
        let table = TABLE.replace("| x present | package |", "| x present |  |");
        let t = GOOD.replace(
            "## Task intent\n\nx\n",
            &format!("## Task intent\n\n{table}"),
        );
        assert_eq!(
            run(&t),
            ["d.md:15: Task intent row has an empty 'Shape' cell"]
        );
    }

    #[test]
    fn empty_primitive_gaps_is_reported() {
        let table = TABLE.replace("Primitive gaps: none.", "Primitive gaps:");
        let t = GOOD.replace(
            "## Task intent\n\nx\n",
            &format!("## Task intent\n\n{table}"),
        );
        assert_eq!(run(&t).len(), 1);
        assert!(run(&t)[0].contains("Primitive gaps"));
    }

    #[test]
    fn missing_heading_is_reported() {
        let t = GOOD.replace("## Outcomes\n", "## Outcome\n");
        assert_eq!(run(&t), ["d.md: missing heading '## Outcomes'"]);
    }

    #[test]
    fn out_of_order_heading_is_reported() {
        let t = GOOD
            .replace("## Outcomes", "## Tmp")
            .replace("## Purpose", "## Outcomes")
            .replace("## Tmp", "## Purpose");
        let p = run(&t);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].contains("out of order"), "{p:?}");
    }

    #[test]
    fn feedback_optional_unless_required() {
        assert!(run(GOOD).is_empty());
        let p = check(
            "d.md",
            GOOD,
            Options {
                require_feedback: true,
            },
        );
        assert_eq!(p, ["d.md: missing heading '## skill-feedback'"]);
        let with = format!("{GOOD}\n## skill-feedback\n\n1. x\n");
        assert!(
            check(
                "d.md",
                &with,
                Options {
                    require_feedback: true
                }
            )
            .is_empty()
        );
    }

    #[test]
    fn undated_source_is_reported_dated_passes() {
        let bad = GOOD.replace("x\n\n## Outcomes", "see https://a.example/x\n\n## Outcomes");
        assert_eq!(run(&bad), ["d.md:5: source line without a YYYY-MM-DD date"]);
        let ok = GOOD.replace(
            "x\n\n## Outcomes",
            "see https://a.example/x (accessed 2026-10-30)\n\n## Outcomes",
        );
        assert!(run(&ok).is_empty());
        let longdigits = GOOD.replace(
            "x\n\n## Outcomes",
            "https://a.example/x 12026-10-301\n\n## Outcomes",
        );
        assert_eq!(run(&longdigits).len(), 1);
    }

    #[test]
    fn hedge_words_whole_word_case_insensitive() {
        let t = GOOD.replacen("x\n", "It Probably works; it could  be fine.\n", 1);
        assert_eq!(run(&t), ["d.md:5:probably", "d.md:5:could be"]);
        let t = GOOD.replacen("x\n", "mighty maybes and likelihood\n", 1);
        assert!(run(&t).is_empty());
    }

    #[test]
    fn hedge_words_skipped_in_fences_and_current_method() {
        let t = GOOD.replace("g\n", "g might\n```\nmaybe\n```\n");
        assert!(run(&t).is_empty());
        let t = GOOD.replacen(
            "x\n\n## Outcomes",
            "```\nmaybe https://u.example\n```\n\n## Outcomes",
            1,
        );
        assert!(run(&t).is_empty());
    }

    #[test]
    fn decision_line_required_in_its_section() {
        let t = GOOD.replace("Decision: take option 1.", "We take option 1.");
        assert_eq!(
            run(&t),
            ["d.md: no 'Decision:' line in '## Recommendation and decision'"]
        );
        let t = GOOD.replace("Decision: take option 1.", "Decision:");
        assert_eq!(run(&t).len(), 1);
    }
}
