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
    /// Where on a rendered HTML page (`#id` or `under "Heading"`); its start and end are 0, as a page has no lines.
    #[serde(default)]
    pub at: String,
}

/// A ```choice block in a markdown document; `answer` is the label of the option picked, empty if none.
#[derive(Debug, Default, Deserialize)]
pub struct Choice {
    pub question: String,
    pub answer: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct Review {
    pub comments: Vec<Comment>,
    #[serde(default)]
    pub overall: String,
    #[serde(default)]
    pub choices: Vec<Choice>,
}

/// Returns the review as markdown for the agent to read.
pub fn to_markdown(name: &str, review: &Review) -> String {
    let overall = review.overall.trim();
    let mut comments: Vec<&Comment> = review.comments.iter().filter(|c| !c.text.trim().is_empty()).collect();
    if comments.is_empty() && overall.is_empty() && review.choices.iter().all(|c| c.answer.is_empty()) {
        return format!("# Review: {name}\nNo comments.\n");
    }
    comments.sort_by(|a, b| (&a.file, a.start, a.old).cmp(&(&b.file, b.start, b.old)));
    let mut out = match comments.len() {
        0 => format!("# Review: {name}\n"),
        1 => format!("# Review: {name} (1 comment)\n"),
        n => format!("# Review: {name} ({n} comments)\n"),
    };
    // The decisions first: they usually decide what happens next.
    for c in &review.choices {
        let answer = if c.answer.is_empty() { "No answer." } else { &c.answer };
        out += &format!("\n## Choice: {}\n{answer}\n", c.question);
    }
    for c in comments {
        let range = match (c.start, c.end) {
            (0, _) => String::new(),
            (s, e) if s == e => format!(":{s}"),
            (s, e) => format!(":{s}-{e}"),
        };
        let side = if c.old { " (old)" } else { "" };
        let at = if c.at.is_empty() { String::new() } else { format!(" ({})", c.at) };
        out += &format!("\n## {}{range}{side}{at}\n", c.file);
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
        Comment { file: file.into(), start, end, old, quote: quote.into(), text: text.into(), at: String::new() }
    }

    #[test]
    fn choices_come_first() {
        let choice = |question: &str, answer: &str| Choice { question: question.into(), answer: answer.into() };
        let review = Review {
            comments: vec![comment("plan.md", 3, 3, false, "x", "why?")],
            overall: String::new(),
            choices: vec![choice("Which approach?", "Snapshot"), choice("Ship now?", "")],
        };
        assert_eq!(
            to_markdown("plan.md", &review),
            "# Review: plan.md (1 comment)\n\
             \n## Choice: Which approach?\nSnapshot\n\
             \n## Choice: Ship now?\nNo answer.\n\
             \n## plan.md:3\n> x\n\nwhy?\n"
        );
        // A pick alone is an answer; unanswered choices alone are not.
        let picked = Review { choices: vec![choice("Which?", "B")], ..Review::default() };
        assert_eq!(to_markdown("p.md", &picked), "# Review: p.md\n\n## Choice: Which?\nB\n");
        let skipped = Review { choices: vec![choice("Which?", "")], ..Review::default() };
        assert_eq!(to_markdown("p.md", &skipped), "# Review: p.md\nNo comments.\n");
    }

    #[test]
    fn html_comments_have_no_line_numbers() {
        let mut c = comment("site/index.html", 0, 0, false, "Run it.", "say how");
        c.at = "#install".into();
        let plain = comment("site/index.html", 0, 0, false, "Plain", "ok");
        let review = Review { comments: vec![c, plain], ..Review::default() };
        assert_eq!(
            to_markdown("site/index.html", &review),
            "# Review: site/index.html (2 comments)\n\
             \n## site/index.html (#install)\n> Run it.\n\nsay how\n\
             \n## site/index.html\n> Plain\n\nok\n"
        );
    }

    #[test]
    fn empty_review() {
        assert_eq!(to_markdown("a.md", &Review::default()), "# Review: a.md\nNo comments.\n");
    }

    #[test]
    fn blank_comments_count_as_empty() {
        let review = Review {
            comments: vec![comment("a.md", 1, 1, false, "x", "  ")],
            overall: " ".into(),
            ..Review::default()
        };
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
            ..Review::default()
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
        let review = Review { comments: vec![comment("a.md", 1, 8, false, &quote, "t")], ..Review::default() };
        let md = to_markdown("a.md", &review);
        assert!(md.starts_with("# Review: a.md (1 comment)\n"), "{md}");
        assert!(md.contains("> 6\n> …\n"), "{md}");
        assert!(!md.contains("> 7"), "{md}");
    }
}
