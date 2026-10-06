//! Dead-code detection: functions, methods and types that nothing references.
//!
//! A candidate is a symbol whose name has no AST reference outside its own
//! definition span (recursion does not count as a use). Matching is by name,
//! so a name used anywhere keeps every homonym alive: the analysis errs on
//! the side of missing dead code rather than flagging live code.
//!
//! Candidates that are alive by convention are excluded: entry points
//! (`main`, dunder methods), tests, trait / interface method declarations,
//! methods of `impl Trait for Type` blocks and overrides of a project
//! supertype, and — unless `include_public` — public / exported symbols,
//! which other packages may use. The rest are reported with a confidence:
//! lowered for dynamic languages (reflection), public symbols, symbols
//! carrying an attribute or decorator (framework / macro registration) and
//! methods of types extending a library supertype (possible overrides).

use super::RetrievalEngine;
use super::impact::is_test_location;
use super::resolution::Resolver;
use anyhow::Result;
use semantiq_index::UnreferencedSymbol;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fs;
use std::path::Path;
use tracing::{debug, info};

pub const DEFAULT_DEAD_CODE_LIMIT: usize = 100;
pub const MAX_DEAD_CODE_LIMIT: usize = 1000;

/// Upper bound on candidates examined (before filtering) per call.
const MAX_CANDIDATES: usize = 10000;

/// Lines scanned above a definition for attributes / decorators.
const MAX_ATTRIBUTE_LINES: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeadCodeConfidence {
    Low,
    Medium,
    High,
}

impl DeadCodeConfidence {
    pub fn as_str(&self) -> &'static str {
        match self {
            DeadCodeConfidence::Low => "low",
            DeadCodeConfidence::Medium => "medium",
            DeadCodeConfidence::High => "high",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct DeadCodeOptions {
    /// Keep files whose relative path starts with this prefix.
    pub path_prefix: Option<String>,
    /// Keep files of this language (`rust`, `typescript`, …).
    pub language: Option<String>,
    /// Also report public / exported symbols (low confidence).
    pub include_public: bool,
    pub limit: usize,
}

#[derive(Debug, Clone)]
pub struct DeadSymbol {
    pub name: String,
    pub kind: String,
    pub file_path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub signature: Option<String>,
    pub confidence: DeadCodeConfidence,
    pub reasons: Vec<String>,
}

/// Number of unreferenced candidates left out, by reason.
#[derive(Debug, Clone, Default)]
pub struct DeadCodeExclusions {
    pub entry_points: usize,
    pub tests: usize,
    pub public: usize,
    pub trait_members: usize,
}

#[derive(Debug, Clone, Default)]
pub struct DeadCodeReport {
    pub symbols: Vec<DeadSymbol>,
    /// Unreferenced candidates examined before filtering.
    pub candidates: usize,
    pub excluded: DeadCodeExclusions,
    pub truncated: bool,
}

enum Verdict {
    Excluded(fn(&mut DeadCodeExclusions)),
    Dead(DeadCodeConfidence, Vec<String>),
}

impl RetrievalEngine {
    /// List functions, methods and types that nothing references.
    pub fn find_dead_code(&self, options: &DeadCodeOptions) -> Result<DeadCodeReport> {
        info!(?options, "Finding dead code");
        let limit = options.limit.clamp(1, MAX_DEAD_CODE_LIMIT);
        let candidates = self.store.find_unreferenced_symbols(
            options.path_prefix.as_deref(),
            options.language.as_deref(),
            MAX_CANDIDATES,
        )?;

        let mut report = DeadCodeReport {
            candidates: candidates.len(),
            truncated: candidates.len() >= MAX_CANDIDATES,
            ..Default::default()
        };
        let mut resolver = Resolver::new(self);
        let mut files: HashMap<String, Option<Vec<String>>> = HashMap::new();
        let mut relations = HashMap::new();

        for candidate in candidates {
            match self.judge(
                &candidate,
                options.include_public,
                &mut resolver,
                &mut files,
                &mut relations,
            )? {
                Verdict::Excluded(count) => count(&mut report.excluded),
                Verdict::Dead(confidence, reasons) => {
                    let s = candidate.symbol;
                    report.symbols.push(DeadSymbol {
                        name: s.name,
                        kind: s.kind,
                        file_path: candidate.file_path,
                        start_line: s.start_line as usize,
                        end_line: s.end_line as usize,
                        signature: s.signature,
                        confidence,
                        reasons,
                    });
                }
            }
        }

        // Most certain first; stable path/line order within a level.
        report
            .symbols
            .sort_by_key(|s| std::cmp::Reverse(s.confidence));
        if report.symbols.len() > limit {
            report.symbols.truncate(limit);
            report.truncated = true;
        }
        Ok(report)
    }

    fn judge(
        &self,
        candidate: &UnreferencedSymbol,
        include_public: bool,
        resolver: &mut Resolver,
        files: &mut HashMap<String, Option<Vec<String>>>,
        relations: &mut HashMap<i64, Vec<semantiq_index::TypeRelationRecord>>,
    ) -> Result<Verdict> {
        let s = &candidate.symbol;
        let language = candidate.language.as_deref().unwrap_or("");
        let path = candidate.file_path.as_str();

        // References shorter than 2 chars are not indexed: never "unused".
        if s.name.chars().count() < 2 || is_entry_point(&s.name, language) {
            return Ok(Verdict::Excluded(|e| e.entry_points += 1));
        }
        if is_test_location(path, Some(&s.name)) || in_test_module(s.parent.as_deref()) {
            return Ok(Verdict::Excluded(|e| e.tests += 1));
        }

        // Declared by a trait / interface: implemented elsewhere, called
        // through the abstraction.
        let parent = s.parent.as_deref().map(last_path_segment);
        if let Some(parent) = parent
            && resolver
                .file_symbols(s.file_id)?
                .iter()
                .any(|p| p.name == parent && matches!(p.kind.as_str(), "trait" | "interface"))
        {
            return Ok(Verdict::Excluded(|e| e.trait_members += 1));
        }

        // Type relations declared around the symbol: `impl Trait for X`
        // blocks, class headers with extends / implements.
        let file_relations = match relations.entry(s.file_id) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(self.store.get_type_relations_by_file(s.file_id)?),
        };
        let enclosing: Vec<_> = file_relations
            .iter()
            .filter(|r| r.line <= s.start_line && s.end_line <= r.end_line)
            .filter(|r| parent.is_some_and(|p| p == r.type_name))
            .collect();
        let mut external_supertypes = Vec::new();
        for relation in &enclosing {
            if language == "rust" {
                // Every method of `impl Trait for Type` is required by the trait.
                return Ok(Verdict::Excluded(|e| e.trait_members += 1));
            }
            let supertype_defs = resolver.definitions(&relation.super_name)?;
            if supertype_defs.is_empty() {
                external_supertypes.push(relation.super_name.clone());
                continue;
            }
            let overrides = resolver.definitions(&s.name)?.iter().any(|d| {
                d.parent
                    .as_deref()
                    .is_some_and(|p| last_path_segment(p) == relation.super_name)
            });
            if overrides {
                return Ok(Verdict::Excluded(|e| e.trait_members += 1));
            }
        }

        let attributes = self.attributes_above(files, path, s.start_line as usize, language);
        let signature = s.signature.as_deref().unwrap_or("");
        if attributes
            .iter()
            .chain(signature.starts_with('@').then_some(&signature.to_string()))
            .any(|a| is_test_attribute(a))
        {
            return Ok(Verdict::Excluded(|e| e.tests += 1));
        }
        if attributes.iter().any(|a| a.contains("Override")) {
            return Ok(Verdict::Excluded(|e| e.trait_members += 1));
        }

        let public = is_public(signature, language, &s.kind, &s.name);
        if public && !include_public {
            return Ok(Verdict::Excluded(|e| e.public += 1));
        }

        let mut confidence = DeadCodeConfidence::High;
        let mut reasons = vec!["no reference outside its own definition".to_string()];
        if is_dynamic(language) {
            confidence = confidence.min(DeadCodeConfidence::Medium);
            reasons.push(format!(
                "{language} allows dynamic calls (reflection, getattr, strings)"
            ));
        }
        if public {
            confidence = DeadCodeConfidence::Low;
            reasons.push("public / exported: other packages may use it".to_string());
        }
        if let Some(attribute) = attributes.first() {
            confidence = DeadCodeConfidence::Low;
            reasons.push(format!(
                "has attribute `{attribute}`: a framework or macro may call it"
            ));
        }
        if !external_supertypes.is_empty() {
            confidence = DeadCodeConfidence::Low;
            reasons.push(format!(
                "may override a method of library supertype {}",
                external_supertypes.join(", ")
            ));
        }
        Ok(Verdict::Dead(confidence, reasons))
    }

    /// Attribute / decorator lines directly above `line` (`#[...]`, `@...`,
    /// C# `[...]`), skipping doc comments in between.
    fn attributes_above(
        &self,
        files: &mut HashMap<String, Option<Vec<String>>>,
        path: &str,
        line: usize,
        language: &str,
    ) -> Vec<String> {
        let lines = files
            .entry(path.to_string())
            .or_insert_with(|| self.read_project_file(path))
            .as_deref()
            .unwrap_or(&[]);
        let mut attributes = Vec::new();
        let end = line.saturating_sub(1).min(lines.len());
        for text in lines[..end].iter().rev().take(MAX_ATTRIBUTE_LINES) {
            let text = text.trim();
            let is_attribute = text.starts_with("#[")
                || text.starts_with('@')
                || (language == "csharp" && text.starts_with('['));
            let is_comment = ["///", "//", "/*", "*", "#!"]
                .iter()
                .any(|p| text.starts_with(p))
                || (language == "python" && text.starts_with('#') && !text.starts_with("#["));
            if is_attribute {
                attributes.push(text.to_string());
            } else if !is_comment {
                break;
            }
        }
        attributes.reverse();
        attributes
    }

    /// Lines of a project file, or `None` when it cannot be read or lies
    /// outside the project root.
    fn read_project_file(&self, path: &str) -> Option<Vec<String>> {
        let root = Path::new(&self.root_path);
        let canonical_root = root.canonicalize().ok()?;
        let canonical = root.join(path).canonicalize().ok()?;
        if !canonical.starts_with(&canonical_root) {
            return None;
        }
        match fs::read_to_string(&canonical) {
            Ok(content) => Some(content.lines().map(str::to_string).collect()),
            Err(e) => {
                debug!("Cannot read {} for attributes: {}", path, e);
                None
            }
        }
    }
}

/// `a::Foo<T>` → `Foo`.
fn last_path_segment(path: &str) -> &str {
    let head = path.split('<').next().unwrap_or(path).trim();
    head.rsplit(['.', ':']).next().unwrap_or(head)
}

fn is_entry_point(name: &str, language: &str) -> bool {
    name == "main"
        || name == "constructor"
        || (name.starts_with("__") && name.ends_with("__"))
        || (language == "go" && name == "init")
}

/// Rust `mod tests` / `mod test` nesting.
fn in_test_module(parent: Option<&str>) -> bool {
    parent.is_some_and(|p| {
        p.split([':', '.'])
            .any(|seg| seg == "tests" || seg == "test")
    })
}

fn is_test_attribute(attribute: &str) -> bool {
    let lower = attribute.to_ascii_lowercase();
    lower.contains("test") || lower.contains("bench") || lower.contains("fixture")
}

fn is_dynamic(language: &str) -> bool {
    matches!(
        language,
        "python" | "javascript" | "ruby" | "php" | "elixir" | "bash"
    )
}

/// Words of the signature before the parameter list (modifiers, keywords).
fn modifiers(signature: &str) -> Vec<&str> {
    let head = signature.split('(').next().unwrap_or(signature);
    head.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '#'))
        .filter(|w| !w.is_empty())
        .collect()
}

/// Whether other packages / modules can see the symbol.
fn is_public(signature: &str, language: &str, kind: &str, name: &str) -> bool {
    let words = modifiers(signature);
    let has = |w: &str| words.contains(&w);
    match language {
        // `pub(crate)` / `pub(super)` are crate-internal: dead if unused.
        "rust" => {
            let sig = signature.trim_start();
            sig.starts_with("pub ") || sig.starts_with("pub\t")
        }
        "typescript" | "javascript" => {
            if kind == "method" {
                !(has("private") || has("protected") || name.starts_with('#'))
            } else {
                has("export")
            }
        }
        "python" => !name.starts_with('_'),
        "go" => name.chars().next().is_some_and(char::is_uppercase),
        "java" | "csharp" => has("public") || has("protected"),
        "kotlin" | "scala" => !(has("private") || has("internal") || has("protected")),
        "php" => !(has("private") || has("protected")),
        "elixir" => !(signature.starts_with("defp") || signature.starts_with("defmacrop")),
        "c" | "cpp" => !has("static"),
        "bash" => false,
        // Ruby and unknown languages: public by default.
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_public() {
        assert!(is_public("pub fn run() {", "rust", "function", "run"));
        assert!(!is_public(
            "pub(crate) fn run() {",
            "rust",
            "function",
            "run"
        ));
        assert!(!is_public("fn run() {", "rust", "function", "run"));
        assert!(is_public(
            "export function run() {",
            "typescript",
            "function",
            "run"
        ));
        assert!(!is_public(
            "function run() {",
            "typescript",
            "function",
            "run"
        ));
        assert!(!is_public("private run() {", "typescript", "method", "run"));
        assert!(!is_public("def _run(self):", "python", "method", "_run"));
        assert!(is_public("public void run() {", "java", "method", "run"));
        assert!(!is_public("private void run() {", "java", "method", "run"));
        assert!(is_public("func Run() {", "go", "function", "Run"));
        assert!(!is_public("static int run(void) {", "c", "function", "run"));
    }

    #[test]
    fn test_helpers() {
        assert!(is_entry_point("main", "rust"));
        assert!(is_entry_point("__init__", "python"));
        assert!(!is_entry_point("init", "rust"));
        assert!(in_test_module(Some("tests")));
        assert!(in_test_module(Some("server::tests")));
        assert!(!in_test_module(Some("Server")));
        assert!(is_test_attribute("#[tokio::test]"));
        assert!(is_test_attribute("@pytest.fixture"));
        assert!(!is_test_attribute("#[inline]"));
        assert_eq!(last_path_segment("a::B"), "B");
        assert_eq!(last_path_segment("Foo<T>"), "Foo");
    }
}
