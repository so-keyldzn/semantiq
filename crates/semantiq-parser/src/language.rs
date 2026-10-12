use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Rust,
    TypeScript,
    JavaScript,
    Python,
    Go,
    Java,
    C,
    Cpp,
    Php,
    Ruby,
    CSharp,
    Kotlin,
    Scala,
    Html,
    Json,
    Yaml,
    Toml,
    Bash,
    Elixir,
}

impl Language {
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_lowercase().as_str() {
            "rs" => Some(Language::Rust),
            "ts" | "tsx" | "mts" | "cts" => Some(Language::TypeScript),
            "js" | "jsx" | "mjs" | "cjs" => Some(Language::JavaScript),
            "py" | "pyi" => Some(Language::Python),
            "go" => Some(Language::Go),
            "java" => Some(Language::Java),
            "c" | "h" => Some(Language::C),
            "cpp" | "cc" | "cxx" | "hpp" | "hxx" | "hh" => Some(Language::Cpp),
            "php" | "phtml" | "php3" | "php4" | "php5" | "php7" | "phps" => Some(Language::Php),
            "rb" | "rake" | "gemspec" => Some(Language::Ruby),
            "cs" => Some(Language::CSharp),
            "kt" | "kts" => Some(Language::Kotlin),
            "scala" | "sc" => Some(Language::Scala),
            "html" | "htm" => Some(Language::Html),
            "json" => Some(Language::Json),
            "yaml" | "yml" => Some(Language::Yaml),
            "toml" => Some(Language::Toml),
            "sh" | "bash" | "zsh" => Some(Language::Bash),
            "ex" | "exs" => Some(Language::Elixir),
            _ => None,
        }
    }

    pub fn from_path(path: &Path) -> Option<Self> {
        path.extension()
            .and_then(|ext| ext.to_str())
            .and_then(Self::from_extension)
    }

    /// The tree-sitter grammar for a file of this language: one per language,
    /// except `.tsx`, whose JSX needs the TSX grammar, and `.h` headers, parsed
    /// with the C++ grammar (a superset of C) because a C++ project declares
    /// its classes, namespaces and templates in `.h` files, which the C
    /// grammar turns into errors and keyword "references". The file keeps
    /// `Language::C` for its statistics and search filters.
    pub fn grammar_for_path(self, path: &Path) -> Grammar {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if self == Language::TypeScript && ext.eq_ignore_ascii_case("tsx") {
            Grammar::Tsx
        } else if self == Language::C && ext.eq_ignore_ascii_case("h") {
            Grammar::Language(Language::Cpp)
        } else {
            Grammar::Language(self)
        }
    }

    /// The grammar that produced `tree` for a file of this language: the
    /// alternatives of [`Language::grammar_for_path`] are told apart from the
    /// tree itself, so the extractors need no path.
    pub fn grammar_of(self, tree: &tree_sitter::Tree) -> Grammar {
        let alternative = match self {
            Language::TypeScript => Some(Grammar::Tsx),
            Language::C => Some(Grammar::Language(Language::Cpp)),
            _ => None,
        };
        match alternative {
            Some(grammar) if grammar.parsed(tree) => grammar,
            _ => Grammar::Language(self),
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::TypeScript => "typescript",
            Language::JavaScript => "javascript",
            Language::Python => "python",
            Language::Go => "go",
            Language::Java => "java",
            Language::C => "c",
            Language::Cpp => "cpp",
            Language::Php => "php",
            Language::Ruby => "ruby",
            Language::CSharp => "csharp",
            Language::Kotlin => "kotlin",
            Language::Scala => "scala",
            Language::Html => "html",
            Language::Json => "json",
            Language::Yaml => "yaml",
            Language::Toml => "toml",
            Language::Bash => "bash",
            Language::Elixir => "elixir",
        }
    }

    pub fn file_extensions(&self) -> &'static [&'static str] {
        match self {
            Language::Rust => &["rs"],
            Language::TypeScript => &["ts", "tsx", "mts", "cts"],
            Language::JavaScript => &["js", "jsx", "mjs", "cjs"],
            Language::Python => &["py", "pyi"],
            Language::Go => &["go"],
            Language::Java => &["java"],
            Language::C => &["c", "h"],
            Language::Cpp => &["cpp", "cc", "cxx", "hpp", "hxx", "hh"],
            Language::Php => &["php", "phtml", "php3", "php4", "php5", "php7", "phps"],
            Language::Ruby => &["rb", "rake", "gemspec"],
            Language::CSharp => &["cs"],
            Language::Kotlin => &["kt", "kts"],
            Language::Scala => &["scala", "sc"],
            Language::Html => &["html", "htm"],
            Language::Json => &["json"],
            Language::Yaml => &["yaml", "yml"],
            Language::Toml => &["toml"],
            Language::Bash => &["sh", "bash", "zsh"],
            Language::Elixir => &["ex", "exs"],
        }
    }
}

/// A tree-sitter grammar. `Language::TypeScript` has two: plain TypeScript
/// and TSX (`.tsx` files), whose JSX the plain grammar parses as type
/// assertions and `ERROR` nodes. The queries and extractors are shared: TSX
/// is a superset with the same node kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Grammar {
    Language(Language),
    Tsx,
}

impl Grammar {
    pub fn tree_sitter_language(self) -> tree_sitter::Language {
        match self {
            Grammar::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Grammar::Language(lang) => lang.tree_sitter_language(),
        }
    }

    /// Whether `tree` was produced by this grammar.
    pub fn parsed(self, tree: &tree_sitter::Tree) -> bool {
        *tree.language() == self.tree_sitter_language()
    }

    /// The language whose node kinds this grammar produces: the extractors
    /// that branch on the language (type relations) use it for a `.h` header
    /// parsed as C++ instead of the file's `Language::C` label.
    pub fn language(self) -> Language {
        match self {
            Grammar::Tsx => Language::TypeScript,
            Grammar::Language(lang) => lang,
        }
    }
}

impl Language {
    pub fn tree_sitter_language(self) -> tree_sitter::Language {
        match self {
            Language::Rust => tree_sitter_rust::LANGUAGE.into(),
            Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Language::Python => tree_sitter_python::LANGUAGE.into(),
            Language::Go => tree_sitter_go::LANGUAGE.into(),
            Language::Java => tree_sitter_java::LANGUAGE.into(),
            Language::C => tree_sitter_c::LANGUAGE.into(),
            Language::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            Language::Php => tree_sitter_php::LANGUAGE_PHP.into(),
            Language::Ruby => tree_sitter_ruby::LANGUAGE.into(),
            Language::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
            Language::Kotlin => tree_sitter_kotlin_ng::LANGUAGE.into(),
            Language::Scala => tree_sitter_scala::LANGUAGE.into(),
            Language::Html => tree_sitter_html::LANGUAGE.into(),
            Language::Json => tree_sitter_json::LANGUAGE.into(),
            Language::Yaml => tree_sitter_yaml::LANGUAGE.into(),
            Language::Toml => tree_sitter_toml_ng::LANGUAGE.into(),
            Language::Bash => tree_sitter_bash::LANGUAGE.into(),
            Language::Elixir => tree_sitter_elixir::LANGUAGE.into(),
        }
    }
}

pub struct LanguageSupport {
    parsers: std::collections::HashMap<Grammar, tree_sitter::Parser>,
}

impl LanguageSupport {
    pub fn new() -> Result<Self> {
        let mut parsers = std::collections::HashMap::new();

        for &lang in Self::supported_languages() {
            Self::add_parser(&mut parsers, Grammar::Language(lang))?;
        }
        Self::add_parser(&mut parsers, Grammar::Tsx)?;

        Ok(Self { parsers })
    }

    fn add_parser(
        parsers: &mut std::collections::HashMap<Grammar, tree_sitter::Parser>,
        grammar: Grammar,
    ) -> Result<()> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&grammar.tree_sitter_language())
            .map_err(|e| anyhow!("Failed to set {:?} language: {}", grammar, e))?;
        parsers.insert(grammar, parser);
        Ok(())
    }

    /// Parse with the language's default grammar. `.tsx` files need
    /// [`LanguageSupport::parse_path`] (or `parse_with`) for the TSX grammar.
    pub fn parse(&mut self, lang: Language, source: &str) -> Result<tree_sitter::Tree> {
        self.parse_with(Grammar::Language(lang), source)
    }

    /// Parse a file: its grammar is chosen from `path` (`.tsx` → TSX).
    pub fn parse_path(
        &mut self,
        lang: Language,
        path: &Path,
        source: &str,
    ) -> Result<tree_sitter::Tree> {
        self.parse_with(lang.grammar_for_path(path), source)
    }

    pub fn parse_with(&mut self, grammar: Grammar, source: &str) -> Result<tree_sitter::Tree> {
        let parser = self
            .parsers
            .get_mut(&grammar)
            .ok_or_else(|| anyhow!("No parser for grammar: {:?}", grammar))?;

        parser
            .parse(source, None)
            .ok_or_else(|| anyhow!("Failed to parse source"))
    }

    pub fn supported_languages() -> &'static [Language] {
        &[
            Language::Rust,
            Language::TypeScript,
            Language::JavaScript,
            Language::Python,
            Language::Go,
            Language::Java,
            Language::C,
            Language::Cpp,
            Language::Php,
            Language::Ruby,
            Language::CSharp,
            Language::Kotlin,
            Language::Scala,
            Language::Html,
            Language::Json,
            Language::Yaml,
            Language::Toml,
            Language::Bash,
            Language::Elixir,
        ]
    }
}

// Note: We intentionally do NOT implement Default for LanguageSupport
// because parser initialization can fail (e.g., tree-sitter errors).
// Use LanguageSupport::new() which returns Result<Self> for proper error handling.

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_language_from_extension() {
        assert_eq!(Language::from_extension("rs"), Some(Language::Rust));
        assert_eq!(Language::from_extension("ts"), Some(Language::TypeScript));
        assert_eq!(Language::from_extension("py"), Some(Language::Python));
        assert_eq!(Language::from_extension("unknown"), None);
    }

    #[test]
    fn test_language_from_extension_case_insensitive() {
        assert_eq!(Language::from_extension("RS"), Some(Language::Rust));
        assert_eq!(Language::from_extension("Ts"), Some(Language::TypeScript));
        assert_eq!(Language::from_extension("PY"), Some(Language::Python));
    }

    #[test]
    fn test_language_from_extension_all_languages() {
        // Rust
        assert_eq!(Language::from_extension("rs"), Some(Language::Rust));

        // TypeScript
        assert_eq!(Language::from_extension("ts"), Some(Language::TypeScript));
        assert_eq!(Language::from_extension("tsx"), Some(Language::TypeScript));
        assert_eq!(Language::from_extension("mts"), Some(Language::TypeScript));
        assert_eq!(Language::from_extension("cts"), Some(Language::TypeScript));

        // JavaScript
        assert_eq!(Language::from_extension("js"), Some(Language::JavaScript));
        assert_eq!(Language::from_extension("jsx"), Some(Language::JavaScript));
        assert_eq!(Language::from_extension("mjs"), Some(Language::JavaScript));
        assert_eq!(Language::from_extension("cjs"), Some(Language::JavaScript));

        // Python
        assert_eq!(Language::from_extension("py"), Some(Language::Python));
        assert_eq!(Language::from_extension("pyi"), Some(Language::Python));

        // Go
        assert_eq!(Language::from_extension("go"), Some(Language::Go));

        // Java
        assert_eq!(Language::from_extension("java"), Some(Language::Java));

        // C
        assert_eq!(Language::from_extension("c"), Some(Language::C));
        assert_eq!(Language::from_extension("h"), Some(Language::C));

        // C++
        assert_eq!(Language::from_extension("cpp"), Some(Language::Cpp));
        assert_eq!(Language::from_extension("cc"), Some(Language::Cpp));
        assert_eq!(Language::from_extension("cxx"), Some(Language::Cpp));
        assert_eq!(Language::from_extension("hpp"), Some(Language::Cpp));
        assert_eq!(Language::from_extension("hxx"), Some(Language::Cpp));
        assert_eq!(Language::from_extension("hh"), Some(Language::Cpp));

        // PHP
        assert_eq!(Language::from_extension("php"), Some(Language::Php));
        assert_eq!(Language::from_extension("phtml"), Some(Language::Php));
    }

    #[test]
    fn test_language_from_path() {
        assert_eq!(
            Language::from_path(Path::new("src/main.rs")),
            Some(Language::Rust)
        );
        assert_eq!(
            Language::from_path(Path::new("app/index.tsx")),
            Some(Language::TypeScript)
        );
        assert_eq!(
            Language::from_path(Path::new("script.py")),
            Some(Language::Python)
        );
        assert_eq!(Language::from_path(Path::new("README.md")), None);
        assert_eq!(Language::from_path(Path::new("noextension")), None);
    }

    #[test]
    fn test_language_name() {
        assert_eq!(Language::Rust.name(), "rust");
        assert_eq!(Language::TypeScript.name(), "typescript");
        assert_eq!(Language::JavaScript.name(), "javascript");
        assert_eq!(Language::Python.name(), "python");
        assert_eq!(Language::Go.name(), "go");
        assert_eq!(Language::Java.name(), "java");
        assert_eq!(Language::C.name(), "c");
        assert_eq!(Language::Cpp.name(), "cpp");
        assert_eq!(Language::Php.name(), "php");
    }

    #[test]
    fn test_language_file_extensions() {
        assert_eq!(Language::Rust.file_extensions(), &["rs"]);
        assert_eq!(
            Language::TypeScript.file_extensions(),
            &["ts", "tsx", "mts", "cts"]
        );
        assert_eq!(
            Language::JavaScript.file_extensions(),
            &["js", "jsx", "mjs", "cjs"]
        );
        assert_eq!(Language::Python.file_extensions(), &["py", "pyi"]);
        assert_eq!(Language::Go.file_extensions(), &["go"]);
        assert_eq!(Language::Java.file_extensions(), &["java"]);
    }

    #[test]
    fn test_parse_rust() {
        let mut support = LanguageSupport::new().unwrap();
        let tree = support.parse(Language::Rust, "fn main() {}").unwrap();
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn test_parse_typescript() {
        let mut support = LanguageSupport::new().unwrap();
        let tree = support
            .parse(
                Language::TypeScript,
                "function hello(): string { return 'hi'; }",
            )
            .unwrap();
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn test_parse_javascript() {
        let mut support = LanguageSupport::new().unwrap();
        let tree = support
            .parse(
                Language::JavaScript,
                "const x = () => console.log('hello');",
            )
            .unwrap();
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn test_parse_python() {
        let mut support = LanguageSupport::new().unwrap();
        let tree = support
            .parse(Language::Python, "def hello():\n    print('hello')")
            .unwrap();
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn test_parse_go() {
        let mut support = LanguageSupport::new().unwrap();
        let tree = support
            .parse(Language::Go, "package main\n\nfunc main() {}")
            .unwrap();
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn test_parse_java() {
        let mut support = LanguageSupport::new().unwrap();
        let tree = support
            .parse(
                Language::Java,
                "public class Main { public static void main(String[] args) {} }",
            )
            .unwrap();
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn test_parse_c() {
        let mut support = LanguageSupport::new().unwrap();
        let tree = support
            .parse(Language::C, "int main() { return 0; }")
            .unwrap();
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn test_parse_cpp() {
        let mut support = LanguageSupport::new().unwrap();
        let tree = support
            .parse(Language::Cpp, "int main() { return 0; }")
            .unwrap();
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn test_parse_php() {
        let mut support = LanguageSupport::new().unwrap();
        let tree = support
            .parse(Language::Php, "<?php function hello() { echo 'hello'; }")
            .unwrap();
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn test_grammar_for_path() {
        assert_eq!(
            Language::TypeScript.grammar_for_path(Path::new("app/App.tsx")),
            Grammar::Tsx
        );
        assert_eq!(
            Language::TypeScript.grammar_for_path(Path::new("app/App.TSX")),
            Grammar::Tsx
        );
        assert_eq!(
            Language::TypeScript.grammar_for_path(Path::new("app/index.ts")),
            Grammar::Language(Language::TypeScript)
        );
        assert_eq!(
            Language::JavaScript.grammar_for_path(Path::new("app/App.jsx")),
            Grammar::Language(Language::JavaScript)
        );
        // `.h` headers hold C++ classes as often as C prototypes: the C++
        // grammar parses both, the C grammar only the latter.
        assert_eq!(
            Language::C.grammar_for_path(Path::new("inc/shape.h")),
            Grammar::Language(Language::Cpp)
        );
        assert_eq!(
            Language::C.grammar_for_path(Path::new("src/shape.c")),
            Grammar::Language(Language::C)
        );
    }

    #[test]
    fn test_grammar_of_tree() {
        let mut support = LanguageSupport::new().unwrap();
        let header = support
            .parse_path(Language::C, Path::new("inc/a.h"), "class A {};")
            .unwrap();
        assert_eq!(
            Language::C.grammar_of(&header),
            Grammar::Language(Language::Cpp)
        );
        let unit = support
            .parse_path(Language::C, Path::new("src/a.c"), "int a;")
            .unwrap();
        assert_eq!(
            Language::C.grammar_of(&unit),
            Grammar::Language(Language::C)
        );
        let tsx = support
            .parse_path(Language::TypeScript, Path::new("App.tsx"), "const a = 1;")
            .unwrap();
        assert_eq!(Language::TypeScript.grammar_of(&tsx), Grammar::Tsx);
        let ts = support
            .parse_path(Language::TypeScript, Path::new("a.mts"), "const a = 1;")
            .unwrap();
        assert_eq!(
            Language::TypeScript.grammar_of(&ts),
            Grammar::Language(Language::TypeScript)
        );
    }

    #[test]
    fn test_parse_tsx_uses_the_tsx_grammar() {
        let mut support = LanguageSupport::new().unwrap();
        let source = "export function App() {\n  return <div className=\"app\"><Button onClick={go} /></div>;\n}\n";
        let path = Path::new("src/App.tsx");
        let tree = support
            .parse_path(Language::TypeScript, path, source)
            .unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        assert!(Grammar::Tsx.parsed(&tree));
        assert!(!Grammar::Language(Language::TypeScript).parsed(&tree));

        // The plain grammar reads the JSX as type assertions and fails.
        let plain = support.parse(Language::TypeScript, source).unwrap();
        assert!(plain.root_node().has_error());
        assert!(Grammar::Language(Language::TypeScript).parsed(&plain));
    }

    #[test]
    fn test_supported_languages() {
        let languages = LanguageSupport::supported_languages();
        assert_eq!(languages.len(), 19);
        assert!(languages.contains(&Language::Rust));
        assert!(languages.contains(&Language::TypeScript));
        assert!(languages.contains(&Language::JavaScript));
        assert!(languages.contains(&Language::Python));
        assert!(languages.contains(&Language::Go));
        assert!(languages.contains(&Language::Java));
        assert!(languages.contains(&Language::C));
        assert!(languages.contains(&Language::Cpp));
        assert!(languages.contains(&Language::Php));
        assert!(languages.contains(&Language::Ruby));
        assert!(languages.contains(&Language::CSharp));
        assert!(languages.contains(&Language::Kotlin));
        assert!(languages.contains(&Language::Scala));
        assert!(languages.contains(&Language::Html));
        assert!(languages.contains(&Language::Json));
        assert!(languages.contains(&Language::Yaml));
        assert!(languages.contains(&Language::Toml));
        assert!(languages.contains(&Language::Bash));
        assert!(languages.contains(&Language::Elixir));
    }

    #[test]
    fn test_language_support_new() {
        // Test that LanguageSupport::new() succeeds and initializes properly
        let support = LanguageSupport::new().expect("Failed to create LanguageSupport");
        assert!(!LanguageSupport::supported_languages().is_empty());
        drop(support);
    }
}
