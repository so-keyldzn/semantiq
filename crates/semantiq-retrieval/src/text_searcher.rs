use anyhow::Result;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{Searcher, Sink, SinkMatch};
use std::io;

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

impl TextSearcher {
    pub fn new(case_insensitive: bool) -> Self {
        Self { case_insensitive }
    }

    /// Search for a pattern in the given content
    /// Returns matches with line numbers and scores
    pub fn search(&self, content: &str, pattern: &str) -> Result<Vec<TextMatch>> {
        // Build regex matcher
        let matcher = RegexMatcherBuilder::new()
            .case_insensitive(self.case_insensitive)
            .word(false)
            .build(pattern)?;

        let mut matches = Vec::new();
        let mut sink = MatchSink::new(&mut matches, pattern);

        // Create searcher and run search
        Searcher::new().search_slice(&matcher, content.as_bytes(), &mut sink)?;

        Ok(matches)
    }

    /// Search with word boundary matching for better precision
    pub fn search_word(&self, content: &str, pattern: &str) -> Result<Vec<TextMatch>> {
        // Build regex matcher with word boundaries
        let word_pattern = format!(r"\b{}\b", regex::escape(pattern));
        let matcher = RegexMatcherBuilder::new()
            .case_insensitive(self.case_insensitive)
            .build(&word_pattern)?;

        let mut matches = Vec::new();
        let mut sink = MatchSink::new(&mut matches, pattern);

        Searcher::new().search_slice(&matcher, content.as_bytes(), &mut sink)?;

        Ok(matches)
    }

    /// Search with a raw regex pattern
    pub fn search_regex(&self, content: &str, pattern: &str) -> Result<Vec<TextMatch>> {
        let matcher = RegexMatcherBuilder::new()
            .case_insensitive(self.case_insensitive)
            .build(pattern)?;

        let mut matches = Vec::new();
        let mut sink = MatchSink::new(&mut matches, pattern);

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
    pattern: &'a str,
}

impl<'a> MatchSink<'a> {
    fn new(matches: &'a mut Vec<TextMatch>, pattern: &'a str) -> Self {
        Self { matches, pattern }
    }

    fn calculate_score(&self, line: &str, match_start: usize) -> f32 {
        let line_trimmed = line.trim();
        let pattern_lower = self.pattern.to_lowercase();
        let line_lower = line_trimmed.to_lowercase();

        // Base score
        let mut score = if line_lower == pattern_lower {
            0.9 // Exact line match
        } else if match_start == 0 || line.chars().nth(match_start.saturating_sub(1))
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

        // Find match position within line for scoring
        let match_start = line_content
            .to_lowercase()
            .find(&self.pattern.to_lowercase())
            .unwrap_or(0);

        let match_end = match_start + self.pattern.len();

        let score = self.calculate_score(&line_content, match_start);

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
}
