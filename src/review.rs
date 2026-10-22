use serde::Deserialize;

const MAX_QUOTE_LINES: usize = 6;

#[derive(Debug, Default, Deserialize)]
pub struct Comment {
    pub file: String,
    pub start: u32,
    pub end: u32,
    /// Anchored to the old side of a diff (a deleted line).
    #[serde(default)]
    pub old: bool,
    pub quote: String,
    pub text: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct Review {
    pub comments: Vec<Comment>,
    #[serde(default)]
    pub overall: String,
}

/// Returns the review as markdown for the agent to read.
pub fn to_markdown(name: &str, review: &Review) -> String {
    let overall = review.overall.trim();
    let mut comments: Vec<&Comment> = review.comments.iter().filter(|c| !c.text.trim().is_empty()).collect();
    if comments.is_empty() && overall.is_empty() {
        return format!("# Review: {name}\nNo comments.\n");
    }
    comments.sort_by(|a, b| (&a.file, a.start, a.old).cmp(&(&b.file, b.start, b.old)));
    let mut out = format!("# Review: {name} ({} comments)\n", comments.len());
    for c in comments {
        let range = if c.start == c.end { c.start.to_string() } else { format!("{}-{}", c.start, c.end) };
        let side = if c.old { " (old)" } else { "" };
        out += &format!("\n## {}:{range}{side}\n", c.file);
        let lines: Vec<&str> = c.quote.lines().collect();
        for line in lines.iter().take(MAX_QUOTE_LINES) {
            out += &format!("> {line}\n");
        }
        if lines.len() > MAX_QUOTE_LINES {
            out += "> …\n";
        }
        out += &format!("\n{}\n", c.text.trim());
    }
    if !overall.is_empty() {
        out += &format!("\n## Overall\n{overall}\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comment(file: &str, start: u32, end: u32, old: bool, quote: &str, text: &str) -> Comment {
        Comment { file: file.into(), start, end, old, quote: quote.into(), text: text.into() }
    }

    #[test]
    fn empty_review() {
        assert_eq!(to_markdown("a.md", &Review::default()), "# Review: a.md\nNo comments.\n");
    }

    #[test]
    fn blank_comments_count_as_empty() {
        let review = Review { comments: vec![comment("a.md", 1, 1, false, "x", "  ")], overall: " ".into() };
        assert_eq!(to_markdown("a.md", &review), "# Review: a.md\nNo comments.\n");
    }

    #[test]
    fn sorted_ranges_old_side_and_overall() {
        let review = Review {
            comments: vec![
                comment("b.py", 3, 3, true, "-gone", "why removed?"),
                comment("a.py", 10, 12, false, "l1\nl2\nl3", " split this "),
            ],
            overall: "Looks good.".into(),
        };
        assert_eq!(
            to_markdown("diff", &review),
            "# Review: diff (2 comments)\n\
             \n## a.py:10-12\n> l1\n> l2\n> l3\n\nsplit this\n\
             \n## b.py:3 (old)\n> -gone\n\nwhy removed?\n\
             \n## Overall\nLooks good.\n"
        );
    }

    #[test]
    fn long_quote_is_truncated() {
        let quote = (1..=8).map(|i| i.to_string()).collect::<Vec<_>>().join("\n");
        let review = Review { comments: vec![comment("a.md", 1, 8, false, &quote, "t")], overall: String::new() };
        let md = to_markdown("a.md", &review);
        assert!(md.contains("> 6\n> …\n"), "{md}");
        assert!(!md.contains("> 7"), "{md}");
    }
}
