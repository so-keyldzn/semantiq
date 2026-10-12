pub mod chunks;
pub mod imports;
pub mod language;
mod python_stdlib;
pub mod query_extractor;
pub mod references;
pub mod resolve;
pub mod structure;
pub mod symbols;

/// Version du parser. Incrémenter force une réindexation complète.
/// Incrémenter quand : ajout/modif de types de noeuds, changement logique d'extraction
pub const PARSER_VERSION: u32 = 21; // v21: Annotated Java/C#/Kotlin/Scala/PHP members and decorated TS classes get the name line as signature (not the annotation), Go interface methods / type aliases / per-spec imports / receiver parents, JS/TS function-valued class fields and `#private` methods as methods, C `#define` macros and function-pointer typedefs as symbols with macro names and enumerators as definitions, Python docstrings and Elixir `@doc` as doc comments, HTML `id` elements and nested `<script>` / `<style>` as symbols, C++ out-of-class methods under their bare name with the qualifier as parent and in-class declarations as methods, `.h` headers parsed with the C++ grammar, `.mts` / `.cts` as TypeScript. v20: Imports of the project's own modules and packages are `local` and resolved (Rust `mod x;` / `self::` / `super::`, Python absolute imports, Java/Kotlin/Scala/C#/PHP declared packages, Go without `go.mod`, C `#include "x.h"`, Ruby `require_relative`), `..` above the root never resolves, Ruby `require` by method, Elixir brace aliases expanded. v18: PHP variables (`$x`) no longer referenced as `x`, declaration-site names (enum variants / constants / entries / cases, TS enum members and interface signatures) are definitions, PHP `implements` names are types, calls with type arguments (`f::<T>()`, `make_unique<T>()`) are calls, Ruby mixins (`include` / `extend` / `prepend`) and Kotlin interfaces with modifiers in type relations. v17: Hidden directories indexed (`.github`, `.storybook`, `.vscode`, `.config`...), only VCS, credential, dependency, build, cache and IDE directories excluded by name (`EXCLUDED_DIRS`), credential files (`.env*`, `.secret*`, `.credential*`) excluded by prefix. v16: `.tsx` parsed with the TSX grammar (JSX tags as calls, attribute names ignored), TS/JS re-exports and `require()` as imports and `export_specifier` names no longer definitions, `export` in TS/JS signatures, Kotlin imports (`import` node), Python aliased / multi-module imports, Scala and PHP brace imports expanded, Rust `use` comments dropped, Go `go.mod`-local imports. v15: Stored paths (`files.path`, `resolved_path`) use `/` on every platform (Windows indexes held backslashes). v14: Elixir `def*` heads are definitions (keywords not indexed, directive arguments are imports), Rust calls inside macro arguments are `call` references, identifier strings in attribute arguments (`#[serde(default = "f")]`) are references, doc comments found through `export` / decorator wrappers and above Rust attributes, oversized symbol spans split into windowed chunks. v13: Local imports resolved on the full file name (`./user.service` -> `user.service.ts`), explicit `.js`/`.jsx`/`.mjs` -> `.ts`/`.tsx` mapping, bundler query suffixes (`?raw`) ignored. v12: Occurrence count on references and call edges, import lines on dependencies. v11: Call edges (caller of each call) and type relations (extends/implements). v10: AST references (identifier occurrences) extracted for find_refs. v9: Rust brace imports expanded into one import per leaf. v8: Multi-declarator dedup (name range in key), Scala multi-binding val/var, doc-comment blank-line break

pub use chunks::{ChunkExtractor, CodeChunk};
pub use imports::{Import, ImportContext, ImportExtractor, ImportKind, go_module_path};
pub use language::{Grammar, Language, LanguageSupport};
pub use query_extractor::QuerySymbolExtractor;
pub use references::{Reference, ReferenceExtractor, ReferenceKind};
pub use resolve::{resolve_import, resolve_local_import, slash_path};
pub use structure::{CallEdge, FileStructure, RelationKind, StructureExtractor, TypeRelation};
pub use symbols::{Symbol, SymbolExtractor, SymbolKind};
