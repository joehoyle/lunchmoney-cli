//! Incremental presentation of common chat Markdown. Raw messages remain in history.
//! Keep only ambiguous syntax between chunks, so ordinary prose streams immediately.
#[derive(Default)]
pub(crate) struct Markdown {
    pending: String,
    started: bool,
    line_start: bool,
    bold: bool,
    heading: bool,
    code: bool,
    fence: Option<char>,
    color: bool,
}

impl Markdown {
    pub fn new(color: bool) -> Self {
        Self {
            line_start: true,
            color,
            ..Self::default()
        }
    }

    fn style(&self) -> &'static str {
        if !self.color {
            ""
        } else if self.code || self.fence.is_some() {
            "\x1b[0;36m"
        } else if self.bold || self.heading {
            "\x1b[0;1m"
        } else {
            "\x1b[0m"
        }
    }

    pub fn push(&mut self, text: &str, finish: bool) -> String {
        self.pending.extend(
            text.chars()
                .filter(|c| !c.is_control() || matches!(c, '\n' | '\t')),
        );
        let mut output = String::new();
        while !self.pending.is_empty() {
            if self.line_start {
                let indent =
                    self.pending.len() - self.pending.trim_start_matches([' ', '\t']).len();
                let tail = &self.pending[indent..];
                if tail.is_empty() && !finish && self.pending.len() < 2048 {
                    break;
                }
                let first = tail.chars().next().unwrap_or(' ');
                if !finish && matches!(tail, "`" | "``" | "~" | "~~") {
                    break;
                }
                // Fence lines may include a language; it is decoration, not code.
                let fence = tail.starts_with("```") || tail.starts_with("~~~");
                if fence && (self.fence.is_none() || self.fence == Some(first)) {
                    let end = tail.find('\n');
                    if end.is_none() && !finish && self.pending.len() < 256 {
                        break;
                    }
                    if end.is_some() || finish {
                        let consumed = indent + end.map(|n| n + 1).unwrap_or(tail.len());
                        self.fence = if self.fence.is_some() {
                            None
                        } else {
                            Some(first)
                        };
                        self.pending.drain(..consumed);
                        output.push_str(self.style());
                        continue;
                    }
                }
                if self.fence.is_none() && !self.code {
                    let hashes = tail.chars().take_while(|c| *c == '#').count();
                    if !finish
                        && (tail.len() == hashes && hashes <= 6
                            || matches!(tail, "-" | "*" | "+" | ">" | "`" | "``" | "~" | "~~"))
                    {
                        break;
                    }
                    output.push_str(&self.pending[..indent]);
                    let prefix = if (1..=6).contains(&hashes) && tail[hashes..].starts_with(' ') {
                        self.heading = true;
                        output.push_str(self.style());
                        hashes + 1
                    } else if tail.starts_with("- ")
                        || tail.starts_with("* ")
                        || tail.starts_with("+ ")
                    {
                        output.push_str("• ");
                        2
                    } else if tail.starts_with("> ") {
                        output.push_str("│ ");
                        2
                    } else {
                        0
                    };
                    self.pending.drain(..indent + prefix);
                }
                self.line_start = false;
                continue;
            }
            let c = self.pending.chars().next().unwrap();
            if c == '\n' {
                output.push('\n');
                self.pending.drain(..1);
                self.line_start = true;
                if self.heading {
                    self.heading = false;
                    output.push_str(self.style());
                }
                continue;
            }
            if self.fence.is_none() {
                if c == '`' {
                    self.code = !self.code;
                    output.push_str(self.style());
                    self.pending.drain(..1);
                    continue;
                }
                if !self.code {
                    if matches!(c, '*' | '_') {
                        if self.pending.len() == 1 && !finish {
                            break;
                        }
                        if self.pending.starts_with("**") || self.pending.starts_with("__") {
                            self.bold = !self.bold;
                            output.push_str(self.style());
                            self.pending.drain(..2);
                            continue;
                        }
                    }
                    if c == '\\' {
                        if self.pending.len() == 1 && !finish {
                            break;
                        }
                        if self
                            .pending
                            .chars()
                            .nth(1)
                            .is_some_and(|c| c.is_ascii_punctuation())
                        {
                            self.pending.drain(..1);
                            let literal = self.pending.chars().next().unwrap();
                            output.push(literal);
                            self.pending.drain(..literal.len_utf8());
                            continue;
                        }
                    }
                    if c == '[' {
                        if let Some(label_end) = self.pending.find("](") {
                            let url_start = label_end + 2;
                            if let Some(url_end) = self.pending[url_start..].find(')') {
                                let end = url_start + url_end;
                                if self.color {
                                    output.push_str("\x1b[4m");
                                }
                                output.push_str(&self.pending[1..label_end]);
                                output.push_str(self.style());
                                output.push_str(" (");
                                output.push_str(&self.pending[url_start..end]);
                                output.push(')');
                                self.pending.drain(..end + 1);
                                continue;
                            }
                        }
                        // Never buffer an unbounded/unclosed link or a whole paragraph.
                        if !finish
                            && !self.pending.contains('\n')
                            && self.pending.len() < 2048
                            && (!self.pending.contains(']')
                                || self.pending.ends_with(']')
                                || self.pending.contains("]("))
                        {
                            break;
                        }
                    }
                }
            }
            self.started = true;
            output.push(c);
            self.pending.drain(..c.len_utf8());
        }
        self.started |= !output.is_empty();
        if finish && self.color && (self.started || !output.is_empty()) {
            output.push_str("\x1b[0m");
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_is_readable_and_identical_across_chunk_boundaries() {
        let input = "# October\nYou have **CA$1,490.70 left**.\n\n- **Budget:** CA$2,500\n  - Groceries: CA$655.73\n- `Spent`: CA$1,009.30\n[Food](https://example.com/food)\n```rust\n**literal code**\n```\nEscaped \\*star\\*, café.";
        let expected = "October\nYou have CA$1,490.70 left.\n\n• Budget: CA$2,500\n  • Groceries: CA$655.73\n• Spent: CA$1,009.30\nFood (https://example.com/food)\n**literal code**\nEscaped *star*, café.";
        for color in [false, true] {
            let whole = Markdown::new(color).push(input, true);
            if !color {
                assert_eq!(whole, expected);
            }
            let mut formatter = Markdown::new(color);
            let mut streamed = String::new();
            for c in input.chars() {
                streamed.push_str(&formatter.push(&c.to_string(), false));
            }
            streamed.push_str(&formatter.push("", true));
            assert_eq!(streamed, whole);
        }
    }

    #[test]
    fn prose_streams_immediately_and_unfinished_syntax_flushes() {
        let mut formatter = Markdown::new(false);
        assert_eq!(formatter.push("Hello café", false), "Hello café");
        assert_eq!(formatter.push("*", false), "");
        assert_eq!(formatter.push("", true), "*");
        assert_eq!(
            Markdown::new(false).push("[unfinished", true),
            "[unfinished"
        );
        assert_eq!(Markdown::new(false).push("\x1b\x07text", true), "text");
    }
}
