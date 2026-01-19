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
            "ts" | "tsx" => Some(Language::TypeScript),
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
            Language::TypeScript => &["ts", "tsx"],
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

pub struct LanguageSupport {
    parsers: std::collections::HashMap<Language, tree_sitter::Parser>,
}

impl LanguageSupport {
    pub fn new() -> Result<Self> {
        let mut parsers = std::collections::HashMap::new();

        // Initialize parsers for each language
        Self::add_parser(
            &mut parsers,
            Language::Rust,
            tree_sitter_rust::LANGUAGE.into(),
        )?;
        Self::add_parser(
            &mut parsers,
            Language::TypeScript,
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        )?;
        Self::add_parser(
            &mut parsers,
            Language::JavaScript,
            tree_sitter_javascript::LANGUAGE.into(),
        )?;
        Self::add_parser(
            &mut parsers,
            Language::Python,
            tree_sitter_python::LANGUAGE.into(),
        )?;
        Self::add_parser(&mut parsers, Language::Go, tree_sitter_go::LANGUAGE.into())?;
        Self::add_parser(
            &mut parsers,
            Language::Java,
            tree_sitter_java::LANGUAGE.into(),
        )?;
        Self::add_parser(&mut parsers, Language::C, tree_sitter_c::LANGUAGE.into())?;
        Self::add_parser(
            &mut parsers,
            Language::Cpp,
            tree_sitter_cpp::LANGUAGE.into(),
        )?;
        Self::add_parser(
            &mut parsers,
            Language::Php,
            tree_sitter_php::LANGUAGE_PHP.into(),
        )?;
        Self::add_parser(
            &mut parsers,
            Language::Ruby,
            tree_sitter_ruby::LANGUAGE.into(),
        )?;
        Self::add_parser(
            &mut parsers,
            Language::CSharp,
            tree_sitter_c_sharp::LANGUAGE.into(),
        )?;
        Self::add_parser(
            &mut parsers,
            Language::Kotlin,
            tree_sitter_kotlin_ng::LANGUAGE.into(),
        )?;
        Self::add_parser(
            &mut parsers,
            Language::Scala,
            tree_sitter_scala::LANGUAGE.into(),
        )?;
        Self::add_parser(
            &mut parsers,
            Language::Html,
            tree_sitter_html::LANGUAGE.into(),
        )?;
        Self::add_parser(
            &mut parsers,
            Language::Json,
            tree_sitter_json::LANGUAGE.into(),
        )?;
        Self::add_parser(
            &mut parsers,
            Language::Yaml,
            tree_sitter_yaml::LANGUAGE.into(),
        )?;
        Self::add_parser(
            &mut parsers,
            Language::Toml,
            tree_sitter_toml_ng::LANGUAGE.into(),
        )?;
        Self::add_parser(
            &mut parsers,
            Language::Bash,
            tree_sitter_bash::LANGUAGE.into(),
        )?;
        Self::add_parser(
            &mut parsers,
            Language::Elixir,
            tree_sitter_elixir::LANGUAGE.into(),
        )?;

        Ok(Self { parsers })
    }

    fn add_parser(
        parsers: &mut std::collections::HashMap<Language, tree_sitter::Parser>,
        lang: Language,
        grammar: tree_sitter::Language,
    ) -> Result<()> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&grammar)
            .map_err(|e| anyhow!("Failed to set {} language: {}", lang.name(), e))?;
        parsers.insert(lang, parser);
        Ok(())
    }

    pub fn parse(&mut self, lang: Language, source: &str) -> Result<tree_sitter::Tree> {
        let parser = self
            .parsers
            .get_mut(&lang)
            .ok_or_else(|| anyhow!("No parser for language: {:?}", lang))?;

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
        assert_eq!(Language::TypeScript.file_extensions(), &["ts", "tsx"]);
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
        assert!(LanguageSupport::supported_languages().len() > 0);
        drop(support);
    }
}
