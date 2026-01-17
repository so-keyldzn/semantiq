use anyhow::{anyhow, Result};
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
        Self::add_parser(&mut parsers, Language::Rust, tree_sitter_rust::LANGUAGE.into())?;
        Self::add_parser(&mut parsers, Language::TypeScript, tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())?;
        Self::add_parser(&mut parsers, Language::JavaScript, tree_sitter_javascript::LANGUAGE.into())?;
        Self::add_parser(&mut parsers, Language::Python, tree_sitter_python::LANGUAGE.into())?;
        Self::add_parser(&mut parsers, Language::Go, tree_sitter_go::LANGUAGE.into())?;
        Self::add_parser(&mut parsers, Language::Java, tree_sitter_java::LANGUAGE.into())?;
        Self::add_parser(&mut parsers, Language::C, tree_sitter_c::LANGUAGE.into())?;
        Self::add_parser(&mut parsers, Language::Cpp, tree_sitter_cpp::LANGUAGE.into())?;
        Self::add_parser(&mut parsers, Language::Php, tree_sitter_php::LANGUAGE_PHP.into())?;

        Ok(Self { parsers })
    }

    fn add_parser(
        parsers: &mut std::collections::HashMap<Language, tree_sitter::Parser>,
        lang: Language,
        grammar: tree_sitter::Language,
    ) -> Result<()> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&grammar)
            .map_err(|e| anyhow!("Failed to set {} language: {}", lang.name(), e))?;
        parsers.insert(lang, parser);
        Ok(())
    }

    pub fn parse(&mut self, lang: Language, source: &str) -> Result<tree_sitter::Tree> {
        let parser = self.parsers.get_mut(&lang)
            .ok_or_else(|| anyhow!("No parser for language: {:?}", lang))?;

        parser.parse(source, None)
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
        ]
    }
}

impl Default for LanguageSupport {
    fn default() -> Self {
        Self::new().expect("Failed to initialize language support")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_from_extension() {
        assert_eq!(Language::from_extension("rs"), Some(Language::Rust));
        assert_eq!(Language::from_extension("ts"), Some(Language::TypeScript));
        assert_eq!(Language::from_extension("py"), Some(Language::Python));
        assert_eq!(Language::from_extension("unknown"), None);
    }

    #[test]
    fn test_parse_rust() {
        let mut support = LanguageSupport::new().unwrap();
        let tree = support.parse(Language::Rust, "fn main() {}").unwrap();
        assert!(!tree.root_node().has_error());
    }
}
