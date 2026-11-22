//! A pattern that behaves like the JavaScript regexes the rule set is written
//! in.
//!
//! Rust's `regex` crate deliberately has no lookaround, and 18 of the 541
//! patterns in the embedded rule set use a negative lookahead. Dropping those
//! patterns would be a silent behaviour change — the counters that count
//! `ls` items or `ps` processes would quietly read zero — so they are
//! translated instead.
//!
//! Every one of the 18 has the same shape: a literal prefix, then `(?!X)`, then
//! the rest of the pattern. That is exactly "the prefix matches, `X` does not
//! match at that position, and the rest does". Which is a conjunction of two
//! ordinary anchored regexes, and needs no lookaround at all.

use regex::{Regex, RegexBuilder};

/// One compiled pattern.
#[derive(Debug)]
pub enum Pattern {
    /// No lookahead: an ordinary regex.
    Plain(Regex),
    /// `^<prefix>(?!<excluded>)<rest>`.
    ///
    /// `excluded` and `rest` are both anchored at the position just after
    /// `prefix`, which is where the lookahead sat.
    Lookahead {
        prefix: String,
        excluded: Regex,
        rest: Regex,
    },
}

impl Pattern {
    /// Compile a JavaScript-flavoured pattern.
    ///
    /// Returns the reason on failure rather than swallowing it: a rule whose
    /// pattern does not compile is a rule that silently stops working, and the
    /// test suite asserts that none of them do.
    pub fn compile(source: &str, flags: Option<&str>) -> Result<Self, String> {
        let inline = inline_flags(flags);

        if let Some((prefix, excluded, rest)) = split_lookahead(source) {
            let excluded = build(&format!("^(?:{excluded})"), &inline)
                .map_err(|e| format!("lookahead body {excluded:?}: {e}"))?;
            let rest = build(&format!("^(?:{rest})"), &inline)
                .map_err(|e| format!("lookahead tail {rest:?}: {e}"))?;
            return Ok(Pattern::Lookahead {
                prefix,
                excluded,
                rest,
            });
        }

        build(source, &inline)
            .map(Pattern::Plain)
            .map_err(|e| format!("{source:?}: {e}"))
    }

    pub fn is_match(&self, text: &str) -> bool {
        match self {
            Pattern::Plain(re) => re.is_match(text),
            Pattern::Lookahead {
                prefix,
                excluded,
                rest,
            } => match text.strip_prefix(prefix.as_str()) {
                Some(tail) => !excluded.is_match(tail) && rest.is_match(tail),
                None => false,
            },
        }
    }
}

/// Translate JavaScript flags into the inline form Rust's engine takes.
///
/// `u` and `g` are dropped: Rust is Unicode-aware by default and has no
/// stateful global mode, so passing either through would be a parse error.
fn inline_flags(flags: Option<&str>) -> String {
    let mut out = String::new();
    for flag in flags.unwrap_or("").chars() {
        match flag {
            'i' | 'm' | 's' | 'x' => out.push(flag),
            _ => {}
        }
    }
    out
}

fn build(source: &str, inline: &str) -> Result<Regex, String> {
    let source = if inline.is_empty() {
        source.to_string()
    } else {
        format!("(?{inline}){source}")
    };
    RegexBuilder::new(&source)
        .size_limit(1 << 20)
        .build()
        .map_err(|e| e.to_string())
}

/// Split `^<prefix>(?!<excluded>)<rest>` into its three parts.
///
/// Returns `None` when the pattern has no lookahead, or when the lookahead is
/// somewhere this translation cannot express — in which case the caller
/// compiles the pattern as-is and the failure surfaces as an error rather than
/// as a pattern that quietly never matches.
fn split_lookahead(source: &str) -> Option<(String, String, String)> {
    let start = source.find("(?!")?;
    let body_start = start + 3;

    // Walk to the matching close paren, honouring escapes and nesting.
    let bytes: Vec<char> = source.chars().collect();
    let mut depth = 1usize;
    let mut index = source[..body_start].chars().count();
    let mut escaped = false;
    let mut close = None;
    while index < bytes.len() {
        let ch = bytes[index];
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '(' {
            depth += 1;
        } else if ch == ')' {
            depth -= 1;
            if depth == 0 {
                close = Some(index);
                break;
            }
        }
        index += 1;
    }
    let close = close?;

    let excluded: String = bytes[body_start..close].iter().collect();
    let rest: String = bytes[close + 1..].iter().collect();

    // The prefix must be a literal, because it is matched with `strip_prefix`.
    // In the embedded set it is either empty or a single escaped character.
    let raw_prefix: String = bytes[..start].iter().collect();
    let prefix = raw_prefix.strip_prefix('^').unwrap_or(&raw_prefix);
    let prefix = unescape_literal(prefix)?;

    Some((prefix, excluded, rest))
}

/// Turn a literal-only regex fragment into the string it matches.
fn unescape_literal(fragment: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = fragment.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => match chars.next()? {
                escaped @ ('+' | '-' | '.' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|'
                | '^' | '$' | '\\' | '/') => out.push(escaped),
                _ => return None,
            },
            // Any other metacharacter means the prefix is not a literal.
            '.' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '^' | '$' => return None,
            other => out.push(other),
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_pattern_matches_like_a_regex() {
        let pattern = Pattern::compile(r"^ERROR", Some("i")).unwrap();
        assert!(pattern.is_match("error: bad"));
        assert!(!pattern.is_match("an error"));
    }

    /// The shape that appears 14 times in the rule set: a non-blank line that
    /// does not start with one of a set of column headers.
    #[test]
    fn a_leading_negative_lookahead_excludes_the_headers() {
        let pattern = Pattern::compile(r"^(?!USER\s|PID\s).+\S.*$", None).unwrap();
        assert!(!pattern.is_match("USER  PID  COMMAND"));
        assert!(!pattern.is_match("PID   COMMAND"));
        assert!(pattern.is_match("root  1234  bash"));
        // A blank line has no `\S`, so the tail fails too.
        assert!(!pattern.is_match("   "));
    }

    /// `^\+(?!\+\+\+).+` — an added diff line.
    ///
    /// Note what this actually does, because it is not what the pattern looks
    /// like it does: `^\+` consumes one `+`, so the lookahead sees `++ b/file`
    /// and `\+\+\+` does not match there. `+++ b/file` therefore *does* match.
    /// Verified against Node's engine rather than assumed.
    #[test]
    fn a_lookahead_after_a_literal_prefix_works() {
        let pattern = Pattern::compile(r"^\+(?!\+\+\+).+", None).unwrap();
        assert!(pattern.is_match("+console.log('added');"));
        assert!(pattern.is_match("+++ b/src/index.ts"));
        assert!(!pattern.is_match("-removed"));
    }

    #[test]
    fn a_regex_inside_the_lookahead_is_honoured() {
        // `^(?!total\s+\d+).+\S.*$` — skip the `total 48` line `ls -l` prints.
        let pattern = Pattern::compile(r"^(?!total\s+\d+).+\S.*$", None).unwrap();
        assert!(!pattern.is_match("total 48"));
        assert!(pattern.is_match("-rw-r--r--  1 user  staff  100 file.txt"));
    }

    #[test]
    fn an_escaped_prefix_character_is_unescaped() {
        let pattern = Pattern::compile(r"^\-(?!---).+", None).unwrap();
        assert!(pattern.is_match("-removed line"));
        // Same asymmetry as the `+` case: `^\-` eats one dash, so `-- a/file`
        // is what the lookahead sees, and `---` does not match there.
        assert!(pattern.is_match("--- a/file"));
        assert!(!pattern.is_match("+added line"));
    }

    #[test]
    fn a_pattern_without_lookahead_is_plain() {
        assert!(matches!(
            Pattern::compile(r"\bfailed\b", Some("i")).unwrap(),
            Pattern::Plain(_)
        ));
    }

    #[test]
    fn a_broken_pattern_reports_why() {
        let error = Pattern::compile("(unclosed", None).unwrap_err();
        assert!(!error.is_empty());
    }

    /// Every pattern in the embedded rule set must compile. A rule whose
    /// pattern fails is a rule that silently stops filtering, which is the
    /// failure mode this whole module exists to avoid.
    #[test]
    fn every_embedded_pattern_compiles() {
        let raw: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("rules.json")).unwrap();
        let mut failures: Vec<String> = Vec::new();
        let mut checked = 0usize;

        for rule in &raw {
            let id = rule["id"].as_str().unwrap_or("?");
            let mut check = |pattern: &str, flags: Option<&str>, what: &str| {
                checked += 1;
                if let Err(error) = Pattern::compile(pattern, flags) {
                    failures.push(format!("{id} {what}: {error}"));
                }
            };

            if let Some(counters) = rule["counters"].as_array() {
                for counter in counters {
                    check(
                        counter["pattern"].as_str().unwrap_or(""),
                        counter["flags"].as_str(),
                        "counter",
                    );
                }
            }
            for key in ["skipPatterns", "keepPatterns"] {
                if let Some(patterns) = rule["filters"][key].as_array() {
                    for pattern in patterns {
                        check(pattern.as_str().unwrap_or(""), None, key);
                    }
                }
            }
            if let Some(matches) = rule["matchOutput"].as_array() {
                for entry in matches {
                    check(
                        entry["pattern"].as_str().unwrap_or(""),
                        entry["flags"].as_str(),
                        "matchOutput",
                    );
                }
            }
        }

        assert!(checked > 400, "only {checked} patterns were checked");
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}
