use anyhow::Result;
use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{Searcher, Sink, SinkMatch};
use std::io;

/// Longest line content returned by a match. Longer lines (minified or
/// generated code) are cut to a window of this many characters around the
/// first match, marked with `…` on the cut sides.
pub const MAX_LINE_CHARS: usize = 500;

/// A file whose lines average more characters than this is minified or
/// generated (bundles, source maps, SVG sprites, one-line HTML): text search
/// skips it rather than return its whole content as a hit.
pub const MAX_AVERAGE_LINE_CHARS: usize = 500;

/// Bytes inspected for a NUL byte, the mark of a binary file.
const BINARY_PROBE_BYTES: usize = 1024;

/// Whether `content` is binary, minified or generated text that text search
/// should skip: a NUL byte in the first KB, or lines averaging more than
/// [`MAX_AVERAGE_LINE_CHARS`] characters.
pub fn looks_generated(content: &str) -> bool {
    let probe = &content.as_bytes()[..content.len().min(BINARY_PROBE_BYTES)];
    if probe.contains(&0) {
        return true;
    }
    let lines = content.lines().count().max(1);
    content.chars().count() / lines > MAX_AVERAGE_LINE_CHARS
}

/// A text match result from ripgrep-based search
#[derive(Debug, Clone)]
pub struct TextMatch {
    pub line_number: usize,
    pub line_content: String,
    pub match_start: usize,
    pub match_end: usize,
    pub score: f32,
}

/// Text searcher using ripgrep's grep-* crates
pub struct TextSearcher {
    case_insensitive: bool,
}

/// A pre-compiled literal matcher, reusable across many `search_compiled`
/// calls so the regex is only compiled once per query term.
pub struct CompiledMatcher {
    matcher: RegexMatcher,
    pattern: String,
}

impl TextSearcher {
    pub fn new(case_insensitive: bool) -> Self {
        Self { case_insensitive }
    }

    /// Search for a pattern in the given content
    /// Returns matches with line numbers and scores
    pub fn search(&self, content: &str, pattern: &str) -> Result<Vec<TextMatch>> {
        let compiled = self.compile(pattern)?;
        self.search_compiled(content, &compiled)
    }

    /// Pre-compile a literal pattern into a reusable matcher.
    ///
    /// Compiling a regex is non-trivial; when the same set of query terms is
    /// run across many files (as `search_text` does), compiling each term's
    /// matcher once and reusing it via [`search_compiled`] avoids recompiling
    /// per file.
    pub fn compile(&self, pattern: &str) -> Result<CompiledMatcher> {
        // Escape user input to prevent ReDoS attacks
        let escaped = regex::escape(pattern);
        let matcher = RegexMatcherBuilder::new()
            .case_insensitive(self.case_insensitive)
            .word(false)
            .build(&escaped)?;
        Ok(CompiledMatcher {
            matcher,
            pattern: pattern.to_string(),
        })
    }

    /// Pre-compile a literal pattern matched as a whole word: `id` matches
    /// `id` but not `uuid`, `width` or `user_id`. A word boundary is only
    /// required on the sides where the pattern itself is a word character,
    /// so `$state` or `@tool` still match.
    pub fn compile_word(&self, pattern: &str) -> Result<CompiledMatcher> {
        let is_word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        let mut word_pattern = String::new();
        if is_word(pattern.chars().next()) {
            word_pattern.push_str(r"\b");
        }
        word_pattern.push_str(&regex::escape(pattern));
        if is_word(pattern.chars().last()) {
            word_pattern.push_str(r"\b");
        }
        let matcher = RegexMatcherBuilder::new()
            .case_insensitive(self.case_insensitive)
            .word(false)
            .build(&word_pattern)?;
        Ok(CompiledMatcher {
            matcher,
            pattern: pattern.to_string(),
        })
    }

    /// Search `content` using an already-compiled matcher.
    pub fn search_compiled(
        &self,
        content: &str,
        compiled: &CompiledMatcher,
    ) -> Result<Vec<TextMatch>> {
        let mut matches = Vec::new();
        let mut sink = MatchSink::new(
            &mut matches,
            &compiled.matcher,
            &compiled.pattern,
            self.case_insensitive,
        );
        Searcher::new().search_slice(&compiled.matcher, content.as_bytes(), &mut sink)?;
        Ok(matches)
    }

    /// Search with word boundary matching for better precision
    pub fn search_word(&self, content: &str, pattern: &str) -> Result<Vec<TextMatch>> {
        let compiled = self.compile_word(pattern)?;
        self.search_compiled(content, &compiled)
    }

    /// Search with a raw regex pattern.
    ///
    /// # Safety
    ///
    /// The underlying `grep-regex` crate uses the Rust `regex` crate which
    /// guarantees O(n) matching time, providing inherent ReDoS protection.
    /// A pattern length limit is enforced to prevent excessive compilation time.
    pub fn search_regex(&self, content: &str, pattern: &str) -> Result<Vec<TextMatch>> {
        // Limit pattern length to prevent excessive regex compilation time
        if pattern.len() > 1000 {
            anyhow::bail!("Regex pattern exceeds maximum length of 1000 characters");
        }

        let matcher = RegexMatcherBuilder::new()
            .case_insensitive(self.case_insensitive)
            .build(pattern)?;

        let mut matches = Vec::new();
        let mut sink = MatchSink::new(&mut matches, &matcher, pattern, self.case_insensitive);

        Searcher::new().search_slice(&matcher, content.as_bytes(), &mut sink)?;

        Ok(matches)
    }
}

impl Default for TextSearcher {
    fn default() -> Self {
        Self::new(true) // Case insensitive by default
    }
}

/// Sink implementation to collect matches
struct MatchSink<'a> {
    matches: &'a mut Vec<TextMatch>,
    /// The matcher that selected the line, re-run on the trimmed line to
    /// locate the match itself (a substring search for the pattern would
    /// find `id` inside `uuid` before the whole-word match).
    matcher: &'a RegexMatcher,
    pattern: &'a str,
    /// Whether the matcher ignored case: the exact-line check follows the
    /// same rule.
    case_insensitive: bool,
}

impl<'a> MatchSink<'a> {
    fn new(
        matches: &'a mut Vec<TextMatch>,
        matcher: &'a RegexMatcher,
        pattern: &'a str,
        case_insensitive: bool,
    ) -> Self {
        Self {
            matches,
            matcher,
            pattern,
            case_insensitive,
        }
    }

    /// Byte range of the first match in `line`.
    fn match_range(&self, line: &str) -> Option<(usize, usize)> {
        self.matcher
            .find(line.as_bytes())
            .ok()
            .flatten()
            .map(|m| (m.start(), m.end()))
    }

    fn calculate_score(&self, line: &str, match_start: usize) -> f32 {
        let line_trimmed = line.trim();

        // Use eq_ignore_ascii_case for the common ASCII path, avoiding allocation.
        // Fall back to full Unicode lowercase only when ASCII comparison fails.
        let is_exact_line = if !self.case_insensitive {
            line_trimmed == self.pattern
        } else if line_trimmed.is_ascii() && self.pattern.is_ascii() {
            line_trimmed.eq_ignore_ascii_case(self.pattern)
        } else {
            line_trimmed.to_lowercase() == self.pattern.to_lowercase()
        };

        // Base score
        let mut score = if is_exact_line {
            0.9 // Exact line match
        } else if match_start == 0
            || line[..match_start]
                .chars()
                .next_back()
                .map(|c| !c.is_alphanumeric())
                .unwrap_or(true)
        {
            0.7 // Word boundary match
        } else {
            0.5 // Substring match
        };

        // Position bonus (earlier matches are better)
        let position_factor = 1.0 - (match_start as f32 / (line.len() as f32 + 10.0)) * 0.2;
        score *= position_factor;

        score.min(1.0)
    }
}

impl Sink for MatchSink<'_> {
    type Error = io::Error;

    fn matched(&mut self, _searcher: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, Self::Error> {
        let line_number = mat.line_number().unwrap_or(0) as usize;
        let line_bytes = mat.bytes();

        // Convert bytes to string, handling potential UTF-8 issues
        let line_content = String::from_utf8_lossy(line_bytes).trim().to_string();

        // Skip empty lines and comments
        if line_content.is_empty()
            || line_content.starts_with("//")
            || line_content.starts_with('#')
        {
            return Ok(true);
        }

        // Locate the match within the line for scoring and the snippet.
        let (match_start, match_end) = self.match_range(&line_content).unwrap_or((0, 0));

        // Scored on the whole line, then cut around the match if too long.
        let score = self.calculate_score(&line_content, match_start);
        let (line_content, match_start, match_end) =
            snippet_around(line_content, match_start, match_end);

        self.matches.push(TextMatch {
            line_number,
            line_content,
            match_start,
            match_end,
            score,
        });

        Ok(true)
    }
}

/// Cut `line` to [`MAX_LINE_CHARS`] characters around the match at byte
/// range `match_start..match_end` (on char boundaries), with a quarter of
/// the window before the match. Returns the snippet and the match's byte
/// range in it.
fn snippet_around(line: String, match_start: usize, match_end: usize) -> (String, usize, usize) {
    let total = line.chars().count();
    if total <= MAX_LINE_CHARS {
        return (line, match_start, match_end);
    }
    let match_char = line[..match_start].chars().count();
    let start = match_char
        .saturating_sub(MAX_LINE_CHARS / 4)
        .min(total - MAX_LINE_CHARS);
    let end = start + MAX_LINE_CHARS;

    let mut snippet = String::with_capacity(MAX_LINE_CHARS + 6);
    if start > 0 {
        snippet.push('…');
    }
    let prefix_len = snippet.len();
    // Byte offset of the match start, then of the match end, in the window.
    let window_start_byte = line
        .char_indices()
        .nth(start)
        .map(|(i, _)| i)
        .unwrap_or(line.len());
    let new_start = prefix_len + match_start.saturating_sub(window_start_byte);
    let window: String = line.chars().skip(start).take(MAX_LINE_CHARS).collect();
    let window_end_byte = window_start_byte + window.len();
    let new_end =
        prefix_len + match_end.clamp(window_start_byte, window_end_byte) - window_start_byte;
    snippet.push_str(&window);
    if end < total {
        snippet.push('…');
    }
    (snippet, new_start, new_end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_search() {
        let searcher = TextSearcher::new(true);
        let content = "fn main() {\n    println!(\"Hello\");\n}";

        let matches = searcher.search(content, "main").unwrap();

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line_number, 1);
        assert!(matches[0].line_content.contains("main"));
    }

    #[test]
    fn test_case_insensitive() {
        let searcher = TextSearcher::new(true);
        let content = "fn Main() {}\nfn main() {}";

        let matches = searcher.search(content, "main").unwrap();

        assert_eq!(matches.len(), 2);
    }

    #[test]
    fn test_case_sensitive() {
        let searcher = TextSearcher::new(false);
        let content = "fn Main() {}\nfn main() {}";

        let matches = searcher.search(content, "main").unwrap();

        assert_eq!(matches.len(), 1);
    }

    #[test]
    fn test_word_search() {
        let searcher = TextSearcher::new(true);
        let content = "let mainValue = 1;\nfn main() {}";

        let matches = searcher.search_word(content, "main").unwrap();

        // Should only match "main" as a word, not "mainValue"
        assert_eq!(matches.len(), 1);
        assert!(matches[0].line_content.contains("fn main"));
    }

    #[test]
    fn test_word_search_case_sensitive_identifier() {
        // Regression for #43: the fallback matched `id` inside `uuid`,
        // `width`, `provider` and `IDENTITY`.
        let searcher = TextSearcher::new(false);
        let content =
            "let id = 1;\nlet uuid = 2;\nlet width = 3;\nlet ID = 4;\nlet user_id = 5;\nx.id\n";

        let compiled = searcher.compile_word("id").unwrap();
        let lines: Vec<usize> = searcher
            .search_compiled(content, &compiled)
            .unwrap()
            .iter()
            .map(|m| m.line_number)
            .collect();
        assert_eq!(lines, vec![1, 6]);
    }

    #[test]
    fn test_match_offset_is_the_matcher_match_not_the_first_substring() {
        // `find("id")` located `id` inside `uuid` (col 6) and scored the
        // whole-word match at col 11 as a substring.
        let searcher = TextSearcher::new(false);
        let compiled = searcher.compile_word("id").unwrap();
        let m = &searcher
            .search_compiled("let uuid = id;", &compiled)
            .unwrap()[0];
        assert_eq!((m.match_start, m.match_end), (11, 13));
        assert_eq!(&m.line_content[m.match_start..m.match_end], "id");
        assert!(m.score > 0.6, "word-boundary score, got {}", m.score);
    }

    #[test]
    fn test_long_line_with_multibyte_case_folding_before_the_match() {
        // `İ` lowercases to two chars (3 bytes instead of 2): offsets taken
        // from `to_lowercase()` did not fit the line and the snippet slice
        // could panic on a non-char boundary.
        let searcher = TextSearcher::new(true);
        let line = format!(
            "{}{}é_needle{}",
            "İ".repeat(300),
            "x".repeat(300),
            "y".repeat(300)
        );
        let m = &searcher.search(&line, "É_NEEDLE").unwrap()[0];
        assert_eq!(&m.line_content[m.match_start..m.match_end], "é_needle");
        assert!(m.line_content.chars().count() <= MAX_LINE_CHARS + 2);
    }

    #[test]
    fn test_word_search_keeps_non_word_edges() {
        let searcher = TextSearcher::new(false);
        let content = "$state = 1;\n$states = 2;\nstate\n";
        let compiled = searcher.compile_word("$state").unwrap();
        let lines: Vec<usize> = searcher
            .search_compiled(content, &compiled)
            .unwrap()
            .iter()
            .map(|m| m.line_number)
            .collect();
        assert_eq!(lines, vec![1]);
    }

    #[test]
    fn test_long_line_is_cut_around_the_match() {
        // Regression for #44: the whole line of a minified file came back.
        let searcher = TextSearcher::new(true);
        let line = format!("{}needle(){}", "a".repeat(3000), "b".repeat(3000));
        let content = format!("short needle\n{line}\n");

        let matches = searcher.search(&content, "needle").unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].line_content, "short needle");

        let long = &matches[1];
        assert!(long.line_content.chars().count() <= MAX_LINE_CHARS + 2);
        assert!(long.line_content.starts_with('…') && long.line_content.ends_with('…'));
        assert_eq!(
            &long.line_content[long.match_start..long.match_end],
            "needle",
            "{}",
            long.line_content
        );
        // A quarter of the window of context before the match.
        assert_eq!(
            long.line_content[..long.match_start].chars().count(),
            MAX_LINE_CHARS / 4 + 1
        );
    }

    #[test]
    fn test_long_line_near_its_end_keeps_a_full_window() {
        let searcher = TextSearcher::new(true);
        let content = format!("{}needle", "x".repeat(2000));
        let m = &searcher.search(&content, "needle").unwrap()[0];
        assert_eq!(m.line_content.chars().count(), MAX_LINE_CHARS + 1);
        assert!(m.line_content.starts_with('…') && m.line_content.ends_with("needle"));
        assert_eq!(&m.line_content[m.match_start..m.match_end], "needle");
    }

    #[test]
    fn test_looks_generated() {
        assert!(!looks_generated("fn main() {\n    println!(\"hi\");\n}\n"));
        assert!(!looks_generated(""));
        // One minified line.
        assert!(looks_generated(&format!(
            "!function(){{{}}}();",
            "a=1;".repeat(2000)
        )));
        // A short file with a huge embedded line still averages too long.
        assert!(looks_generated(&format!("x\n{}\n", "y".repeat(5000))));
        // Binary marker.
        assert!(looks_generated("ab\0cd\n"));
        // Long but normal lines are fine.
        assert!(!looks_generated(
            &"let value = compute(argument_one, argument_two, argument_three);\n".repeat(50)
        ));
    }

    #[test]
    fn test_regex_search() {
        let searcher = TextSearcher::new(true);
        let content = "fn test_one() {}\nfn test_two() {}\nfn other() {}";

        let matches = searcher.search_regex(content, r"test_\w+").unwrap();

        assert_eq!(matches.len(), 2);
    }

    #[test]
    fn test_skip_comments() {
        let searcher = TextSearcher::new(true);
        let content = "// fn main() {}\nfn main() {}";

        let matches = searcher.search(content, "main").unwrap();

        // Should skip the comment line
        assert_eq!(matches.len(), 1);
        assert!(!matches[0].line_content.starts_with("//"));
    }

    #[test]
    fn test_score_calculation() {
        let searcher = TextSearcher::new(true);
        let content = "main\nmain = 1\nlet main = 1";

        let matches = searcher.search(content, "main").unwrap();

        // Exact match should have highest score
        assert!(matches[0].score > matches[2].score);
    }

    #[test]
    fn test_regex_search_rejects_oversized_pattern() {
        let searcher = TextSearcher::new(true);
        let content = "fn main() {}";
        let long_pattern = "a".repeat(1001);

        let result = searcher.search_regex(content, &long_pattern);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("maximum length"));
    }

    #[test]
    fn test_regex_search_accepts_max_length_pattern() {
        let searcher = TextSearcher::new(true);
        let content = "fn main() {}";
        let pattern = "a".repeat(1000);

        // Should not error on length validation (may not match, but shouldn't panic)
        let result = searcher.search_regex(content, &pattern);
        assert!(result.is_ok());
    }
}
