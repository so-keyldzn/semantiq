# Changelog

All notable changes to Semantiq will be documented in this file.

## [Unreleased]

The text index (`chunks_fts`, below) is built from the existing chunks the
first time this version opens an index (a few seconds per 100 MB of source);
it grows the database by about 2.5× the size of the indexed source text.

Existing indexes are rebuilt on the first run of this version
(`PARSER_VERSION` 21, schema v11): the parser fixes below change what is
extracted, the import fixes change what `deps`, the repo map and the
`imports` confidence of `impact` / `calls` see, and the `refs` table changes
shape. Phase 1 only: embeddings are kept.

### Changed
- The text strategy of `search` (and the `refs` fallback for names unknown
  to the syntax index) answers from the index: a trigram FTS5 table over the
  chunks selects the chunks holding each term and the term's regex locates
  the lines, instead of reading every file of the project on each search
  that did not fill `limit` (3.6-4.4 s for a query without a hit on a
  24k-file corpus, now a few milliseconds). Only indexed files are searched,
  as they were last indexed; files of other extensions (`.sql`, `.css`...)
  are no longer text-searched. Two processes opening an older index at the
  same time (`serve` and a CLI command) build the text index once: the
  second waits for the first instead of failing on the existing table
  (#160).
- `search --symbol-kind` applies to every strategy before its limit:
  semantic hits whose chunk defines no symbol of the wanted kinds and text
  hits (a line has no kind) are left out, where it used to filter the
  symbol strategy only and let the other hits through (#93).
- Reference extraction is about 14x faster (808 ms -> 57 ms on this
  repository's sources, release build) with byte-for-byte identical output:
  `ReferenceExtractor` reads the context of each identifier from the ancestor
  stack of its own walk instead of `Node::parent()` (which tree-sitter
  resolves by descending from the root), and matches node kinds and fields
  against per-grammar tables of precomputed ids instead of ~32 string
  comparisons per identifier. Part of #158.
- Phase 1 indexing (`semantiq index`, `serve` startup, the query refresh)
  parses files on `SEMANTIQ_INDEX_THREADS` threads (default: the CPU count,
  at most 8; the parsers of a thread are built on its first file, so a walk
  with nothing to parse builds none) and writes each file's row and tables
  in one transaction with cached statements, one commit per file instead of
  six; `semantiq index` logs the files/s rate and the thread count (#158).
- The indexer's walk no longer reads and hashes every file: an indexed file
  whose mtime did not move since its content was hashed is trusted unchanged
  (a file modified within 2 s before the hash, the mtime granularity of
  FAT and SMB mounts, or after it is hashed again, like git's racy index
  entries), the `files` table is loaded once instead of one
  query per file, and the query commands run that single walk instead of a
  hash walk followed by a second one; an up-to-date index is neither written
  to nor reported on (#157).
- `homebrew/Formula/semantiq.rb` is removed: no tap ever
  published it (placeholder checksums, version 0.1.0). The npm installer's
  fallback message, the README and `semantiq update` no longer point at
  `brew install so-keyldzn/tap/semantiq` or `cargo install semantiq` (neither
  the tap nor the crates.io crate exists) but at the GitHub Releases archive
  and `cargo install --git`; a binary under a Homebrew `Cellar` is still
  refused by `semantiq update`, with a hint that names no tap (#117).
- Dockerfile: `.dockerignore` keeps `target/`, `.git`, index DBs, `bench/`
  and `npm/` out of the build context (7 GB before); a cargo-chef stage caches
  the dependency build so a source edit only recompiles the workspace crates;
  `cargo build --release --locked`; the demo project is a copy of the sources
  being built instead of a `git clone` of `main` at build time
  (reproducible); `libssl-dev`, `pkg-config` and `git` are dropped (ureq uses
  rustls); the image ships the embedding model (`/app/data`, `XDG_DATA_HOME`)
  and the index of the demo project, both built with the image: `serve`
  loads the model before it binds the listener, so a container that had to
  download it first could outlast the health check's start period before
  `/health` answered (#118).
- Schema v11: the `refs` table stores an interned name id and a 1-byte kind
  code instead of repeating the name and kind words on every row and in
  `idx_refs_file_id` (table + index a third smaller on this repository), and the
  `idx_files_path` index, a duplicate of the `UNIQUE` index on `files.path`,
  is dropped (#162).
- `impact` and `calls --direction callers` rank the occurrences of a common
  name by file set (near files, their importers, the rest) computed once per
  query instead of a correlated `EXISTS` over `dependencies` evaluated for
  every occurrence before the `LIMIT` (0.7 s per BFS node for `new` on a
  24k-file index) (#161).
- The repo map cache key is O(1): a `graph_generation` counter maintained by
  triggers on `files` plus the last symbol / dependency ids, instead of
  aggregating `files` and counting `refs` on every unfocused `repo_map`
  call; the store's read queries use cached prepared statements (#165).
- The chunks / chunks_vec integrity canary runs once at `serve` startup
  instead of on every CLI command, each of which joined both tables in full
  when building its engine (#164).
- CI runs the test suite on Ubuntu, macOS and Windows (#121), plus a
  `--no-default-features` job that lints and tests the stub-embeddings
  configuration shipped for macOS Intel (#108); every `cargo` invocation in
  CI, Build, Release and the Dockerfile passes `--locked` (#119).
- Workflows cancel the previous run of the same branch, cache cargo in Build
  and Release, install prebuilt `cargo-audit` / `cargo-deny`, and
  `cargo audit --deny warnings` turns the three unmaintained ignores into a
  real gate (#124).
- The Release page shows the `## [<version>]` section of this file above the
  generated PR list; tagging a version without a section fails `check-version`
  (#120).
- Cargo workspace: `rust-version = "1.88"` (let-chains), `strip = true` for
  release builds, `publish = false` on the library crates (#125).
- `impact`, `calls`, `hierarchy`: `definitions` lists at most 20 definitions
  and a `definition_count` field gives their number; `impact new` on a large
  index listed thousands (820 KB of JSON). An impacted file's `is_test` is
  present only when true (#163).
- `dead-code` judges the candidates in path order only until `limit`
  high-confidence symbols are known (the report is the same: that order is
  the final one), instead of reading every candidate's file for its
  attributes first; the `excluded` counts cover the candidates judged (#163).
- Repo map: the loaded graph is cached by the index fingerprint and reused by
  focused maps, which only re-run PageRank; names and kinds are interned,
  signatures and docs are loaded for the listed symbols only, the reference
  counts are summed by SQLite without a sorter, and the budget is chosen from
  the symbols' sizes rather than by rendering. The map itself is unchanged.
  On a 23k-file index (1.7M symbols, 16M references, debug build) one
  `semantiq map` goes from 82 s of CPU and 3.3 GB to 35 s and 1.3 GB, and in
  a running server a focused map after the first call takes the ranking
  alone (8 s instead of a 37 s rebuild, in that build) (#156).

### Security
- Symlinks inside the project are no longer followed by the indexer, the
  query refresh, the watcher and the text search: a link to a file or
  directory outside the project (`config.yaml -> ~/.aws/credentials`) was
  read through, indexed and returned by search, and a link inside the project
  indexed the same file twice (#109).
- Credential stores are excluded by name, dot-prefixed or not: data files
  whose stem says `secret`, `credential`, `service-account` or `adminsdk`
  (`secrets.yaml`, `credentials.json`, `x-firebase-adminsdk-y.json`),
  `kubeconfig*`, SSH keys (`id_rsa*`), dotenv files (`*.env`, which the text
  search read under an explicit `file_type: "env"`) and key or certificate
  files (`*.pem`, `*.key`, `*.p12`, `*.jks`...). `.gitignore`, `.ignore` and
  the global git excludes are honoured outside a git repository too (an
  exported archive, a build directory, a volume without `.git`) (#113).
- `serve --http-port` refuses `--cors-origin '*'` and `null`: with a wildcard,
  any website the developer visits could read the indexed source through the
  loopback API. The flag now takes one or more comma-separated absolute
  `http(s)://host[:port]` origins (no path, no wildcard; default ports are
  dropped as browsers do), validated before the server starts (#112).
- The npm installer extracts the verified archive with `execFileSync('tar',
  [...])` (an argv) instead of a shell command line built by string
  interpolation: a quote or `$(...)` in the install path is no longer
  interpreted (#114).
- Release archives carry a GitHub build provenance attestation
  (`gh attestation verify <archive> --repo so-keyldzn/semantiq`) and the npm
  package is published with `--provenance`; the `.sha256` sidecar is
  documented as a corruption check, not an authenticity one (#111).
- Workflows run with a read-only token; only the release job can write to
  the repository and only the release and npm jobs get an OIDC token. `cross`
  and `rust-toolchain` are pinned to commits like every other action (#116).

### Fixed
- Two timing-dependent tests of `semantiq-index` are deterministic: the
  unchanged-mtime test stamps the file with one value (two `hour_ago()` calls
  could straddle a second boundary, so the macOS/Windows jobs failed about one
  run in three) and the background-embedder poll test waits for the pass to be
  cleared, not only for the pending count to reach zero.
- A directory rename, move or removal is applied by `serve`'s watcher: the
  rows under the old name are removed and the files under the new one
  indexed, instead of ghost rows until a restart (#85).
- Editing or removing `.gitignore`, `.ignore` or `.git/info/exclude` while
  `serve` runs re-applies the rules: files that became ignored lose their
  rows, files that became un-ignored are indexed, and a root rule that
  ignores every file empties the index instead of being refused (#138).
- A file whose parse fails no longer keeps the symbols, references, chunks
  and imports of its previous version: they are cleared, the failure is
  counted as an error, and the file is parsed again when it changes (#90).
- The query commands' refresh check sees a source file that became
  non-UTF-8 or vanished between the listing and the read, as `semantiq
  index` does: its rows are pruned instead of being served until another
  file changes (#107).
- `semantiq index <file>`, `serve --project <file>` and `init <file>` fail
  with `<file> is not a directory` instead of a SQLite "unable to open"
  error or an index whose only row has an empty path (#137).
- `search --symbol-kind` / `--file-type` apply before the per-strategy
  `LIMIT`: in SQL for the symbol and text strategies, by growing the KNN
  candidate pool (up to 2048) for the semantic one. `--symbol-kind struct
  --limit 5` found no struct when more than 5 functions of the same name
  outranked it (#134).
- A query token without a letter or digit (`auth - login`, `_`) no longer
  expands to an empty term whose regex matched every line of the first
  file walked: the first 10 lines of an arbitrary file came back at
  score 0.525 (#92).
- Text search keeps the lines starting with `#` in the languages where `#`
  is not a comment: `#[derive(...)]`, `#[tool]`, `#define`, `#include`,
  `#pragma` and `#region` are found; `#` lines are still skipped in
  Python, Ruby, Bash, Elixir, YAML, TOML and PHP (except `#[` attributes),
  where a `#!` shebang is kept as code (#94).
- `refs` no longer lists import statements as definitions and `search` no
  longer returns them as symbol hits (they scored like definitions and ate
  `--limit`): an import is a usage, listed once under `usages` with `kind:
  import`. `--symbol-kind import` opts them back in as symbol hits (#77).
- `refs <common name>` reads at most `--limit` definitions, in one SQL
  query with their file path, and each file once for the snippets, where
  it read every definition's file before the limit (`refs new` on a
  24k-file corpus: 7 780 definition spans read for 50 results) (#159).
- `semantiq search` says on stderr when semantic search is unavailable (a
  build without the `onnx` feature, e.g. macOS Intel, or
  `SEMANTIQ_EMBEDDINGS=stub`), as the MCP server already did in its
  instructions: only symbol and text matches are returned (#106).
- `serve` loads one embedding model instead of two, in the background after
  the structural index rather than before answering `initialize`, and a load
  that fails at startup (first run offline, interrupted download) is retried
  with a backoff by the embedder thread and by the next search: semantic
  search comes back without a restart. The "restart the server" notice is
  gone (#76, #97).
- A panic on the embedder thread (ONNX Runtime or the tokenizer on an
  unexpected input) no longer kills phase 2 silently with the "Semantic index
  N% ready" notice stuck forever: it is reported like a failed model load and
  retried after the backoff (#87).
- The model download has network timeouts (connect, response, body) instead
  of blocking forever on a stalled connection (#97), streams into a uniquely
  named temporary file so two processes downloading at once no longer
  truncate each other's `.part` (#98), and the reason shown by the
  `semantiq_search` notice and `GET /stats` when the model cannot be loaded is
  a short classification (download failed, digest mismatch, files could not
  be loaded…) with no local path, URL or digest; the detail is logged (#100).
- The model directory no longer falls back to the current directory, i.e. the
  indexed project, when no data directory is available: `SEMANTIQ_MODELS_DIR`
  names it explicitly, and without it or `HOME` the load fails with a hint
  (#115).
- `semantiq index --embeddings-only` exits 1 when the embedding model cannot
  be loaded, instead of reporting success with every chunk left pending; a
  full `index` still warns and keeps its structure (#103).
- Threshold calibration can become effective: the collector records one
  observation per search query and language (the median candidate distance),
  which is what `distance_observations` stores, and reconciles its count with
  the rows actually inserted, so the bootstrap no longer ends after ~25
  searches with 25 rows; auto-calibration runs again every 100 stored
  observations and only saves calibrations search applies (500 observations
  per language, confidence medium); `semantiq calibrate` and `semantiq stats`
  say when a saved calibration is below that floor and not applied; `serve`
  flushes the collected observations on shutdown (#135).
- `semantiq calibrate` locates the index like the other commands (`.semantiq.db`
  in the current directory or a parent, or `--database`) instead of asking for
  a second, nested index from a subdirectory (#105).
- PHP variables (`$helper`, `$this`) are no longer references to the bare
  name: `refs`, `explain`, `impact`, `dead-code` and the repo map no longer
  confuse `$helper` with `helper()`; `$this->helper` still counts (#141).
- Declaration-site names are `definition` references, not usages: Rust enum
  variants, Java enum constants, Kotlin `enum_entry`, PHP `enum_case`,
  TypeScript enum members and interface / abstract method signatures.
  `find_refs` no longer counts them as usages and `dead-code` no longer keeps
  a homonym alive (#82, #149).
- PHP `implements` names, including qualified ones (`\JsonSerializable`),
  are `type` references like `extends` (#149).
- Calls with type arguments (`f::<T>()`, `s.parse::<u32>()`,
  `.collect::<Vec<_>>()`, `std::make_unique<T>()`, `obj.get<T>()`) are `call`
  references with a call edge: `calls`, `impact` (beyond depth 1) and the repo
  map see them (#80).
- Ruby `include` / `extend` / `prepend` in a class or module body are
  `implements` type relations, so `hierarchy` shows the mixins (#152).
- A Kotlin `interface` preceded by modifiers (`public`, `sealed`, an
  annotation) extends its supertypes instead of implementing them (#153).
- The Kotlin parser probes `b5_kotlin_kinds` and `b7_kotlin_parent` assert
  the extracted kinds and parents instead of printing them (#123).
- A deeply nested file (10 000 nested JSON arrays, a 10 000-term expression)
  overflowed the stack of the indexing thread in the import and chunk
  extractors and aborted `serve` at every restart; both walks are iterative
  now (#78).
- Rust `mod x;` (and `#[path = "..."] mod x;`) is a `local` import of the
  module, resolved to `x.rs` / `x/mod.rs`; `self::` and `super::` paths
  resolve from the module tree the file layout defines, and a `super::` path
  written inside an inline module (`mod tests { use super::helper; }`) is
  stored rebased on the file's module (`self::helper`), so it points at the
  right file; a `mod child;` written inside `mod outer { }` is stored
  `self::outer::child` (`outer/x.rs` with a `#[path]` attribute), which is
  the file Rust loads; `crate::` paths are anchored at the file's crate root
  (`src/bin/<name>/main.rs`, `tests/<name>.rs`), not at the nearest `src/`
  (#143).
- C/C++ `#include "x.h"` and Ruby `require_relative 'x'` resolve without a
  leading `./`: an include is looked up next to the including file, then from
  the project root and `include/`; a `require_relative` path is stored with
  the `./` prefix (`deps` shows `./x`) and only resolves next to the
  requiring file, while `require 'x'` goes through `lib/`, the root, `test/`
  and `spec/` before the file's own directory (#142).
- Imports of the project's own packages are `local` and resolved for Java,
  Kotlin, Scala, C#, PHP and Go without a `go.mod`: the indexer records the
  `package` / `namespace` declarations of the project's files (and the
  directories of its Go files) when it starts, and maps an import to the
  file named after the imported member, else to the single file of the
  package; `using X = A.B;` (C#) imports `A.B`. A C# namespace and a JVM
  package sharing a name are kept apart (a `using` only sees `.cs` files),
  and an import of the `go.mod` module path itself resolves to the root
  package (#147).
- Absolute Python imports of the project's own modules (`from app.models
  import User`) are `local` and resolved, from the importing file's
  directory, the project root and `src/`; in general an import that names a
  file of the project is stored `local` whatever its syntax (#81).
- An import whose `..` climb above the project root (`../../secret` from
  `src/`) no longer resolves to a root-level homonym; absolute paths never
  resolve (#83).
- Ruby: a call whose text merely starts with `require`
  (`requirements.push("json")`) or has a receiver is no import. Elixir:
  `alias A.{B, C}` gives one import per leaf instead of the fragment `A.{B`,
  and `alias A.B, as: C` is named `C` (#84).
- Java, C#, Kotlin, Scala, PHP (and TS classes with decorators): an annotated
  member's `signature` is the line that carries its name and modifiers
  (`public double area() {`) instead of the annotation (`@Override`), so
  `dead-code` reads its visibility; `dead-code` also reads the attributes at
  the top of the symbol's span (`@Test`, `@Override`, `[Fact]`), which excludes
  tests and overrides and lowers the confidence of framework-annotated members
  (#140).
- Go: interface methods (`method_elem`) and `type X = Y` aliases are symbols,
  methods get their receiver type as `parent` (`Circle::Area`), and a grouped
  `import ( … )` yields one import symbol per spec named after its alias or
  last path segment instead of one symbol holding the whole block (#145).
- JS/TS: class fields holding a function (`onClick = (e) => {…}`, `static
  handler = function () {}`) and `#private()` methods are method symbols, so
  their calls are attributed to them and not to the class (#146).
- C/C++: `#define` macros (`constant`, function-like ones `function`) and
  function-pointer typedefs (`typedef int (*cb)(int)`) are symbols; the names
  declared by `#define` and by enumerators are definitions, not usages (#148).
- Python docstrings (first statement of a `def` / `class` body, PEP 257
  dedented, taken whole between the delimiters) and Elixir `@doc` /
  `@moduledoc` strings are attached as the symbol's doc comment (#150).
- HTML: `<script>` / `<style>` at any depth and every element with an `id`
  (named after the id; `ID=`, bare values and self-closing tags included) are
  symbols, not only the root `<html>` (#154).
- C++: a method defined out of its class (`int C::add(int)`) is indexed under
  its bare name `add` with `parent = C` (`ns::C` with the enclosing namespace),
  reachable by `explain` / `calls` / `refs`; its definition line is no longer a
  usage; in-class method and constructor declarations (`int add(int);`, pure
  virtual methods, `C();`, `C() = default;`) are method symbols (#79).
- `.h` headers are parsed with the C++ grammar (they keep the `c` language
  label): classes, namespaces, templates, member declarations and inheritance
  (`hierarchy`) of C++ headers are indexed and their keywords no longer
  pollute `refs` (#144).
- `.mts` / `.cts` files are indexed as TypeScript and `./x.mjs` / `./x.cjs`
  imports resolve to `x.mts` / `x.cts` (#151).
- The update channel works again: the GitHub `releases/latest` answer (~20 KB
  with its assets and notes) was cut at 10 KB, so the version check and
  `semantiq update` failed on every run since February and blamed the network.
  The body cap is now 1 MiB and a truncated or unparsable answer is reported as
  such (#110).
- `RUST_LOG` is honoured: the filter was built from a fixed string, so the
  documented variable was never read. `--verbose` still wins; the defaults are
  `info,ort=warn` (`warn,ort=error` for query commands) (#128).
- HTTP API: the query endpoints no longer queue behind a saturated server. A
  global 50-request concurrency cap now rejects the next request at once with
  503 (`OVERLOADED`), a request still running after 30 s is answered 504
  (`TIMEOUT`) while its handler keeps the slot until its SQLite work ends, and
  `/health` runs outside both limits so a busy server still reports as alive
  to a Docker `HEALTHCHECK` (#99).
- README: documents HTTP mode (`--http-port`, `--http-host`, `--cors-origin`,
  the ten endpoints, REST defaults and caps, which endpoints share the MCP
  shapes) (#129); Known Limitations no longer claims `export const` is not
  indexed and states the real size caps (1 MB for code, 256 KB for JSON /
  YAML / TOML) (#130); the data formats are described as indexing keys and
  sections as symbols, the tool and strategy counts, the `stats` sample
  (`Embeddings`, `ML Calibration`), the `semantiq_explain` fields, the
  reindex triggers (embedding model change) and the dead setup-guide link are
  fixed, and the npm README lists the 19 languages (#131). Limit caps are
  stated once: every `limit` is clamped to 1-1000 (hierarchy: per direction)
  and `max_tokens` to 256-8000, in REFERENCE.md, SKILL.md, the README, the
  CLI `--help` and the MCP parameter descriptions (#133).
- `calls <name>` (and `semantiq_calls`) failed with `Expression tree is too
  large (maximum depth 1000)` on any name with about 1000 definitions or more
  (`new`, `fmt`, `from`, `default` in a large Rust or Java repository): the
  definition sites were filtered with an `OR` chain. They now go into a
  row-value `IN (VALUES …)` list, 4000 sites per statement (#155).
- An embedding-model mismatch no longer wipes the structural index. Opening a
  CodeRankEmbed index from a stub process (`SEMANTIQ_EMBEDDINGS=stub semantiq
  stats`, a build without `onnx`) left zero files; it now changes nothing, and
  the stub's phase 2 writes nothing into a real model's vectors. A real model
  finding another model's vectors drops only those (every chunk is pending
  again) and keeps files, symbols and references. The reconciliation runs
  when phase 2 starts, not at open, and before any "nothing pending" shortcut:
  a complete index of another model's vectors is pending from the first
  chunk for `semantiq index`, `--embeddings-only` and `search` alike (#86).
- A parser version bump no longer throws the embeddings away: the structural
  tables are cleared and every file is read and re-extracted (its mtime stamp
  is reset too), and the embedding of a chunk whose content did not change is
  carried over; `semantiq index
  --embeddings-only` rebuilds the structure first in that case (#86).
- Two processes opening an old index together (`serve` and a CLI command right
  after `semantiq update`) migrated it twice: the second one failed with
  `duplicate column name`. The schema version is read again under the write
  lock (#88).
- A failing statement in `semantiq index --force`'s clear left the shared
  connection inside an open transaction, where every later write failed until
  restart; the clear is rolled back instead (#139).

- `insert_file` upserts the row and returns its id in one statement
  (`RETURNING`), and a file's symbols, references, call edges, chunks and
  dependencies are written in the same transaction as its hash: a concurrent
  prune (`--force`, a newer binary's rebuild) can no longer make `index_file`
  fail with "Query returned no rows" or leave new rows under the old hash
  (#89).
- The Docker image serves the HTTP API on `0.0.0.0` (`HTTP_HOST`, like
  `HTTP_PORT`): with the 0.12.0 loopback default, the published port never
  answered while the in-container health check stayed green. The health check
  follows `HTTP_PORT` (#101).
- `semantiq init` keeps a customised `semantiq` entry in `.mcp.json` (absolute
  `command`, extra `args`, `env`) instead of replacing it with the default on
  every run; `--force` replaces it, like an edited skill file (#102).
- The npm installer downloads through the proxy npm is configured with
  (`npm_config_https_proxy` / `npm_config_proxy`, then `HTTPS_PROXY`,
  `https_proxy`, `ALL_PROXY`; `NO_PROXY` / `npm_config_noproxy` exempt hosts),
  tunnelling the GitHub download with HTTP CONNECT (Basic proxy credentials
  from the URL); the exemptions are checked again at every redirect, since
  GitHub sends release assets to another host (#104).
- The npm installer refuses Linux musl (Alpine) with a clear message instead
  of installing a glibc binary that fails at launch; `getPlatform`,
  `install()` and the extraction are covered by `npm test` against a local
  release, and the served platforms are documented in `npm/README.md`
  (`win32-arm64` passes npm's separate `os` × `cpu` filters and is refused by
  the installer) (#126).
- bench/agent: task suites are validated when loaded (file and task id in the
  error) instead of a `ZeroDivisionError` in `score()` on an empty
  `expected`; `scoring.score` raises on it too; the README documents every
  `run.py` option, and `test_bench.py` covers the validation and the scoring
  (#127).
- `impact`: a name the project does not define (a library method such as
  `to_string`) was labelled `unique_name` at every site and followed to
  `max_depth`; 0 definitions no longer counts as one, such sites stay
  `name_only` and nothing propagates (#95).
- `impact`: the enclosing symbol of a site is the innermost function, method
  or type; a local binding indexed as a one-line `variable` (TS/JS `const`,
  Go `var`, Scala `val`, Bash) no longer stands in for the function around
  it, so the local homonyms of other functions are not reported as depth-2
  sites. A `const`/`static` is the enclosing symbol only at top level (#91).
- `impact` / `calls` with `--file <path>`: a file that defines no symbol of
  that name yields an error naming the files that do, instead of a silent
  `name_only` analysis of every homonym (#136).
- `explain`: the usage count of a name without AST references (a key in a
  data file) only discounts the text hits that land on a definition line;
  it subtracted every definition although the text search skips data files,
  under-estimating the count and clamping it at 0 (#96).


### Added
- Tests for the surfaces that had none: the `semantiq update` flow against a
  local release server (`SEMANTIQ_UPDATE_BASE_URL`, a test hook that redirects
  the release lookup and downloads), `calibrate`, `init-cursor`,
  `index --embeddings-only`, `serve` on stdio (stdout holds JSON-RPC only),
  the Streamable HTTP `/mcp` endpoint, and the v3 → current schema migration
  chain through `IndexStore::open` (#122).

## [0.12.0] - 2026-10-11

Existing indexes are rebuilt on the first run of this version
(`PARSER_VERSION` 17, schema v10): the parser fixes below change what is
extracted, stored paths use `/` on every platform, and the symbol score scale
moves down (see #34 if you pass `--min-score`).

### Security
- The HTTP API (`serve --http-port`) rejects requests whose `Host` header is
  not `localhost`, `127.0.0.1`, `[::1]` or the bound address with 403 when
  bound to loopback, on every endpoint and not only `/mcp`: a page whose DNS name re-resolves to
  127.0.0.1 can no longer read the indexed code same-origin (DNS rebinding).
  `--http-host` with a non-loopback address disables the check (#38).
- Dependencies: `anyhow` 1.0.104 (RUSTSEC-2026-0190), `crossbeam-epoch` 0.9.21
  (RUSTSEC-2026-0204) and `memmap2` 0.9.11 (RUSTSEC-2026-0186). `rmcp` 3.5
  already carries the Streamable HTTP `Host` check (RUSTSEC-2026-0189). The
  `Security` workflow is green again; its remaining ignores are the three
  unmaintained-only advisories without a fixed release (#39).

### Added
- `semantiq stats` reports the structure tables: `References`, `Call edges`
  and `Type relations` (the data behind `refs` / `impact` / `dead-code`,
  `calls` and `hierarchy`); `GET /stats` gains `indexed_refs`,
  `indexed_call_edges` and `indexed_type_relations` (#50).

### Changed
- The release workflow fails before building anything when the `v*` tag does
  not match the version in `Cargo.toml`, `npm/package.json`,
  `.claude-plugin/plugin.json` and `.claude-plugin/marketplace.json`: a
  release whose binary reports another version made every MCP session log
  `Update available` and `semantiq update` reinstall it forever (#51).
- `semantiq update` checks before downloading that the binary can be
  replaced: a Homebrew (`Cellar`) or npm (`node_modules`) install is refused
  with the matching `brew upgrade semantiq` / `npm update -g semantiq-mcp`
  command, and an unwritable install directory fails right away. Without a
  platform data directory (sandboxed launch without HOME / XDG), the version
  check no longer writes its cache into the current project (#51).
- `semantiq_search` resolves its semantic candidates in 3 queries instead
  of ~500 for `limit 100`: the candidate chunks come back with their file's
  path and language in one query and the symbol kinds of every file
  touched in another, instead of one lookup per chunk, per file and per
  file's symbols on the shared connection. The repo map leaves out names
  defined in more than 100 files (`new` in a monorepo): their references
  linked every referencing file to every defining file (tens of millions
  of pairs) for a weight already damped to nothing (#45).
- **Hidden directories are indexed** (#65): `.github/workflows`, `.storybook`,
  `.vscode`, `.config`, `.husky`, `.changeset`, `.devcontainer`, `.claude`...
  follow `.gitignore` like any other directory instead of being skipped for
  their leading dot. What stays excluded by name is what never holds project
  source: VCS state (`.git`, `.hg`, `.svn`, `.jj`), dependencies
  (`node_modules`, `vendor`, `.venv`, `.yarn`...), build output (`target`,
  `dist`, `.next`, `.nuxt`...), caches (`.cache`, `.turbo`, `.mypy_cache`...),
  IDE state (`.idea`, `.vs`), credential stores (`.aws`, `.azure`,
  `.docker`, `.gnupg`, `.kube`, `.ssh`) and Semantiq's own files
  (`.semantiq*`); the full list is `EXCLUDED_DIRS`. Hidden files are indexed
  too (`.eslintrc.json`, `.gitlab-ci.yml`), except credential files, whose
  name starts with `.env`, `.secret` or `.credential` (`.env.json`,
  `.env.yaml`, `.secrets.yml`, `.credentials.json`): never parsed, chunked,
  embedded or returned by search. The initial walk, the query refresh check,
  the text search and the watcher share one walk configuration
  (`project_walker`). Text search still leaves dot-files out, since it reads
  files of any extension.

### Fixed
- Elixir `def` / `defp` / `defmodule` heads are `definition` references instead
  of calls: `refs foo` shows the definition line, `calls foo` no longer lists
  `foo` as its own caller nor `def` as a callee, and `alias` / `import` / `use`
  / `require` arguments are `import` references (#59).
- Rust calls inside macro arguments (`assert_eq!(compute(1), 1)`,
  `println!("{}", compute(2))`, `vec![make()]`, `tracing::info!(n = total())`)
  are `call` references: `calls compute --direction callers` finds functions
  exercised only through macros, and `impact` follows them beyond depth 1 (#60).
- Functions named only in attribute strings (`#[serde(default = "f")]`,
  `deserialize_with`, `with`, `skip_serializing_if`, `validate(custom = ...)`,
  Python decorator and Java / Kotlin / C# annotation arguments) get a
  `reference` occurrence, so `dead-code` no longer reports serde helpers with
  high confidence. Only identifier-shaped values of function-path keys count in
  Rust (`#[cfg(feature = "x")]`, `#[serde(rename = "x")]` reference nothing),
  attributes naming JSON properties, columns, routes or warnings are skipped
  elsewhere, and strings outside attributes still never match (#64).
- Doc comments are attached to exported TypeScript / JavaScript symbols
  (`/** Doc */ export function f()`, `export default class`, `declare`),
  to decorated Python functions (comment above the decorator) and to Rust items
  behind attributes (`/// Doc` above `#[derive(...)]`); `explain` shows the
  JSDoc of exported APIs again. Rust doc lines no longer carry their trailing
  newline (#61).
- A function larger than twice the chunk size (generated code, big `match`
  dispatchers, long tests) is split into overlapping windows of about one chunk
  size, each listing the symbols it overlaps, instead of one chunk whose
  embedding only covered its first 512 tokens: semantic search can now hit the
  whole body, and a hit returns a window rather than a 3000-line chunk. The
  embedding carry-over on edit is per window (#62).
- A query command given `--database` without `--project` no longer walks the
  database's directory and prunes every indexed file as vanished: `index` and
  `serve` record the project root in the database, the query commands read it
  back (the current directory for an index built by an older version), and a
  refresh that finds no indexable file under its root while the index holds
  files refuses to prune, with a "wrong project root?" hint (#32).
- Stored paths (`files.path`, `resolved_path`, every `path` / `file_path` in
  the outputs) use `/` on every platform: a Windows index held backslashes,
  so `deps src/lib/foo.ts`, `--focus` and `path_prefix` did not match and the
  output differed from the documented contract. Path arguments of the CLI, the
  MCP tools and the HTTP API accept either separator (#68).
- An older Semantiq binary opening an index written by a newer one (after a
  `semantiq update` rollback, or a plugin binary next to a `cargo run` on a
  branch that bumped the schema) refuses it with an error naming both schema
  versions, instead of writing its lower version back and making the newer
  binary fail on every start (#35).
- `delete_file` purges the `chunks_vec` rows and deletes the file in one
  transaction: a crash or `SQLITE_BUSY` between the two statements can no
  longer leave a chunk with an embedding but no vector, invisible to both
  phase 2 and semantic search. `IndexStore::count_chunks_missing_vectors`
  pins that direction of the invariant (#40).
- `serve` embeds the chunks another process left pending. The background
  embedder now checks every 30 s whether another connection committed
  (`PRAGMA data_version`, no table scan on an idle server) and embeds what is
  pending, and the watcher wakes it on any file event, not only on a reindex: a CLI refresh
  (`semantiq refs x` after editing files) or `semantiq index --no-embeddings`
  no longer leaves chunks without an embedding until an unrelated file changes
  (#37).
- `serve` still runs the initial index and the embedder when the file watcher
  cannot be created (inotify watch limit on a large tree): it falls back to an
  indexer without watcher, logs a WARN that live reindexing is off, says so in
  the MCP instructions, and `GET /stats` reports `"watching": false` (#47).
- A failed embedding model load (download, disk, digest) is retried with a
  backoff (30 s, doubling, up to 15 min) instead of once per server lifetime;
  until it succeeds `semantiq_search` starts with `⚠️ Semantic index
  unavailable: the embedding model could not be loaded (...)` and `GET /stats`
  reports `"semantic_search": false` with the reason. When the search
  engine's own model copy failed to load at startup (loaded once, not
  retried), the reason says to restart the server. The progress counters
  read by the notice and `GET /stats` are stored as one atomic so `pending`
  can no longer underflow between two passes (#52). `ModelLoader` is now a
  `Fn`, called again on retry.
- `calls` and `impact` on a common name (`new`, `get`, `run`) are no longer
  silently partial: the store lookups put the occurrences in the defining
  files and in files importing them first instead of plain path order, so
  the cap (5 × the limit) only cuts name-only homonyms; `callees` filters
  the caller's location in SQL before the cap instead of after it; and
  `truncated` is set when a lookup hit its cap and a cut occurrence could
  have been reported (any at depth 1, a trusted one deeper). `hierarchy` flags a type
  with more relations than `--limit` as truncated instead of cutting
  silently (#42).
- Text search no longer returns the whole line of a minified or generated
  file (up to 1 MB) as a hit's `content`: files whose lines average more
  than 500 characters, or that hold a NUL byte, are skipped, and a hit on a
  longer line is cut to a 500-character window around the match (#44).
- The text fallback of `find_refs` / `explain` (names unknown to the syntax
  index, such as keys of data files) matches the whole identifier,
  case-sensitive: `id` no longer reports `uuid`, `width`, `provider` or
  `IDENTITY` lines as usages, nor counts them in `usage_count` (#43).
- `impact`: the analysed symbol counts as visited from the start, so mutual
  recursion (`a` calls `b`, `b` calls `a`) no longer reports the depth-1
  sites again at depth 3, eating the site budget. `find_refs` no longer
  fails when a definition's file cannot be read (renamed before the watcher
  caught up, `--no-refresh`): the definition is listed with an empty
  snippet (#46).
- Search ranking: each strategy's scores are only weighted (symbol 1.0,
  semantic 0.95, text 0.75), no longer min-max normalized per strategy. The
  weakest symbol hit of a query (`parse_configuration` for `parse_config`)
  no longer scores 0 under every grep line, and a lone weak hit is no longer
  inflated to full strength above the best semantic chunk. Symbol boosts are
  rescaled into `[0, 1]` instead of clamped, so an exact function match
  outranks a boosted prefix match instead of tying with it. The symbol
  scale moves down: an exact function match scores ~0.87-0.93 for typical
  names (1.0 only for a one-character function), an exact variable ~0.70,
  a substring match on a variable ~0.49, so `--min-score 0.5` now drops
  substring hits on variables it kept before; a line that is exactly the
  query (text, 0.68) now outranks realistic semantic hits (<= 0.63) (#34).
- Calibrated semantic thresholds (`semantiq calibrate`, auto-calibration)
  keep ~90 % of the observed candidates instead of ~10 %: `min_similarity`
  is derived from the same 90th-percentile distance as `max_distance`
  instead of the 10th percentile, so the two gates cut at the same point
  (the calibrated `max_distance` can still be stricter than the default
  1.2) (#33).
- The watcher applies the project's ignore rules (#36): saving a file matched
  by `.gitignore`, the global git excludes or `.git/info/exclude` (nested
  `.gitignore` files included) no longer indexes it until the next refresh
  prunes it; a file that became ignored since it was indexed is dropped on
  save. `AutoIndexer::process_file_events` applies a batch of events (one
  restricted walk per batch, duplicate events collapsed) and is what
  `process_events` calls.
- Walk errors no longer prune the files they hid (#53): a directory that
  cannot be listed (permissions, mount hiccup) is logged and counted in
  `errors`, and the files under it keep their rows and embeddings, since the
  walk cannot tell them from files that vanished; pruning goes on everywhere
  else, so a permanently unreadable directory (root-owned volume, restrictive
  mount) does not disable it, and the next walk that lists the directory
  prunes under it for real. A walk error without a path blocks pruning for
  that run. The query refresh check applies the same rule. A previously
  indexed file that is gone or not UTF-8 any more loses its stale row; a
  file that cannot be read for a transient reason (permissions,
  interruption) keeps its row and embeddings and is counted in `errors`.
- Indexing writes less (#41): a file's imports are written in one transaction
  (`IndexStore::replace_dependencies`, one commit per file instead of one per
  import), and the partial index `idx_chunks_pending` (`chunks(id) WHERE
  embedding IS NULL`, schema v10) lets `embedding_counts` and
  `pending_embedding_chunks` skip the chunk rows and their embedding BLOBs:
  on a 99k-chunk index with nothing pending, the counts query goes from
  210 ms to 1 ms and the pending window from 60 ms to 1 ms, on every
  `semantiq search` refresh and every watcher tick.
- Local import resolution keeps dotted module names: `./user.service` resolves to
  `user.service.ts` instead of `user.ts` (#55, by @penielgold-hub). Explicit
  `.js` / `.jsx` / `.mjs` imports map to `.ts` / `.tsx` on purpose rather than by
  accident, and bundler query suffixes (`./registry.ts?raw`, `./styles.css?url`)
  are ignored.
- npm installer (`npm install -g semantiq-mcp`): release downloads follow
  301/302/303/307/308 redirects (at most 5, each redirect response drained),
  abort after 30 s without data instead of hanging `postinstall`, and retry
  transient failures (network errors, timeouts, 429/5xx) three times with
  backoff before printing the fallback instructions; a 404 is final. The
  helpers are covered by `npm test` (`node --test`, Node 18+) against a local
  HTTP server (#48).
- `semantiq stats` finds the index the way the query commands do:
  `.semantiq.db` in the current directory or a parent, or `--database`. From
  a subdirectory of an indexed project it no longer fails with a hint that
  would create a second, nested index (#50).
- `semantiq map --json | head -5` (any query command, `--json` or markdown,
  and `stats` / `calibrate`) ends quietly with exit code 0 when the reader
  closes the pipe early, instead of panicking with `Broken pipe` and exit
  code 101 (#49).
- `.tsx` files are parsed with the TSX grammar (#54): JSX no longer yields
  `ERROR` nodes. In JSX, whether TSX or JavaScript (`.jsx`, `.js`), a rendered
  component (`<Button />`, `<Ui.Card>`) is a call of the component (an edge in
  `calls`, a use for `dead-code`), attribute values are references, attribute
  names, intrinsic tags and closing tags are not.
- Kotlin imports are extracted (#56): the extractor matched the old
  `import_header` node, so `deps` and the repo map had nothing for Kotlin
  files. Aliases (`import a.B as C`) and wildcards are handled.
- Python `import numpy as np` and `import os, sys` yield one import per module,
  with the alias as its name; `from . import a, b` yields one import per name
  with path `.` (#57).
- TS/JS barrel files (#58): `export * from './x'`, `export { a } from './x'`
  and `require('./x')` are imports (`deps`, repo map edges), and a re-exported
  name is an import (or a plain reference for `export { a }`), no longer a
  definition site in `refs`, `explain`, `impact` and `dead-code`.
- TS/JS `export` / `export default` is part of the signature of exported
  declarations, as `pub` is for Rust, so `dead-code` treats them as public:
  excluded unless `--include-public`, then reported with low confidence (#63).
- Parser defects (#66): comments inside a Rust `use` group are dropped from
  the import path; Go imports under the `go.mod` module path are `local`;
  Scala `import a.b.{C, D => E}` and PHP `use Foo\{A, B as C}` yield one import
  per leaf; call edges are attached by a line-sorted sweep instead of a scan
  of every symbol per call (1.2 s → linear on a 32k-line file).

## [0.11.0] - 2026-10-07

### Added
- **Two-phase indexing**: phase 1 stores symbols, references, call edges,
  type relations, imports and chunks and stamps the file hash as its commit
  point; phase 2 embeds the chunks still without an embedding, 32 at a time
  across files, outside the DB lock, one transaction per batch, resumable by
  construction. On a large repository the structural tools answer after 14 s
  instead of ~13 min. `serve` runs phase 2 on a background thread and
  `semantiq_search` prepends `Semantic index N% ready` while it runs;
  `semantiq index` runs both phases with progress and gains `--no-embeddings`
  and `--embeddings-only`; the query commands' refresh is phase 1 only, and
  `search` embeds up to 64 pending chunks itself. `semantiq stats` and
  `GET /stats` report embedded / total chunks. An edit only re-embeds the
  chunks whose content changed.
- **Import lines**: `deps` says where each import statement is, `(L3)` or
  `(L3-5)` for multi-line statements, grouped per line and kind:
  `local: a::{X, Y} (L3); b::Z (L5)`. `line` and `end_line` are in the JSON
  output of `semantiq_deps` and `semantiq deps --json`, `line` in
  `POST /deps`.
- **Occurrence counts**: a line that uses a name several times is counted as
  such. `calls` shows `run ×2` when `run` calls the symbol twice on one line
  (`f(f(x))`), `count` appears in the JSON edge when above 1, `explain`'s
  `usage_count` counts occurrences, and the repo map weights edges by
  occurrences. Only occurrences of the kind kept for the line count: a plain
  reference next to a call (`retry(retry)`) is not a second call.

### Changed
- **Smaller MCP tool definitions**: `tools/list` drops from ~5.1k to ~3.0k
  tokens (chars/4, -42 %) for the 9 tools; the part forwarded to the model
  (names, descriptions, input schemas) from ~1.76k to ~1.04k (-41 %), and the
  server instructions from ~190 to ~110. Descriptions say in one or two
  sentences when to use each tool rather than grep; schemas drop generated
  noise (`format`, `minimum`, `$schema`, titles, `null` unions), output schemas
  their field descriptions, and definitions used once are inlined. All tools
  stay read-only (`readOnlyHint`, `openWorldHint: false`; the
  `destructiveHint` / `idempotentHint` that only apply to writing tools are
  gone).
- **Compact tool output by default** (MCP text content and CLI): one line per
  result, each path written once, no timings. On this repository,
  `semantiq_search` answers shrink by 49-80 % as text and 88-96 % as
  `structuredContent`; `explain` by 81 %, `calls` by 64 %, `deps` by 63 %.
  - `semantiq_search`: each hit is `path:start-end kind name (score)` plus its
    most relevant line (the declaration, or the line holding most query
    words, cut to 120 characters) instead of a 200-character snippet. New
    `snippets` parameter (`--snippets` on the CLI) adds the code back.
    Default `limit` 20 → 10.
  - `find_refs`: usages grouped by file; default `limit` 50 → 30 (CLI too).
  - `impact` / `calls`: sites grouped by file, repeated sites merged into one
    line (`120,134 call index_file in initial_index`), the queried symbol
    implied.
  - `deps`: imports grouped by kind and module (`a::{X, Y}`).
  - `explain`: import statements listed on one line, without the module docs
    the index attached to them.
  - A cut list says so: `(more exist: raise limit)`.
- **Breaking (`--json` / `structuredContent`)**:
  - `semantiq_search`: `total_count` and `search_time_ms` removed (use
    `results.length`); `content` only with `snippets`; new `preview` and
    `truncated`; `score` rounded to 2 decimals.
  - `semantiq_find_refs`: `total_count` and `search_time_ms` replaced by
    `truncated`; `content` is one line (cut to 120 characters), for
    definitions the declaration line instead of the whole source; a
    definition's `kind` is the symbol kind (`function`, `struct`…) instead of
    `definition`.
  - `semantiq_explain`: `import` entries move from `definitions` to
    `imported_in` (`path:line` strings).
  - `semantiq_deps`: `imported_by` lists each file once, sorted;
    `import_name` is omitted when `target_path` ends with it; both lists are
    absent rather than `null` when the lookup fails.
  - `semantiq_dead_code`: `reasons` no longer repeats "no reference outside
    its own definition" (true of every result) and is omitted when empty.
  - `semantiq_repo_map`: `rank` values keep 4 significant digits.
  - Optional fields are never `null`: they are omitted.
- **Index schema v9** (migrated in place on first start, then a full reindex
  fills the new columns): `dependencies.line` / `end_line`, `refs.count` /
  `call_edges.count`; the never-read `symbols` / `chunks` byte offsets and
  `files.size` are dropped.
- **Calibration observations are pruned**: `distance_observations` older than
  30 days are deleted at each flush, always keeping the 500 most recent so a
  calibration sample stays available. Calibrated thresholds are untouched.

### Removed
- Store API with no caller outside tests: `IndexStore::update_chunk_embedding`
  (use `store_chunk_embeddings`), `get_chunks_without_embeddings`,
  `get_file_language`, `insert_distance_observation`, `load_calibration`,
  `clear_calibrations`; `RetrievalEngine::bootstrap_status`,
  `FileWatcher::unwatch`, `DistanceCollector::production`.
  `get_chunks_by_file`, `needs_full_reindex` and `set_parser_version` are now
  behind the `semantiq-index/test-support` feature.

## [0.10.1] - 2026-10-06

### Fixed
- **Large data files no longer slow indexing down**: JSON, YAML and TOML files
  above 256 KB (`MAX_DATA_FILE_SIZE`) are skipped; code files keep the 1 MB
  limit. A single 840 KB JSON file held 52 % of this repository's chunks:
  a full index now takes 56 s instead of 243 s. Oversized files indexed by an
  earlier version (or that grew past the limit) are removed from the index on
  the next run.

## [0.10.0] - 2026-10-06

Pivot release: Semantiq moves from "semantic search over MCP" to structural
code intelligence usable three ways (MCP, CLI + Agent Skill, HTTP), backed by
a code-specific embedding model that is now on by default.

### Upgrade notes
- **One-time full reindex** on first start: schema v5 -> v8 and
  `PARSER_VERSION` 8 -> 11 (new embedding space, AST references, call edges,
  type relations). Nothing to do by hand.
- **New embedding model download** (~139 MB, CodeRankEmbed INT8, SHA-256
  pinned) on first run, cached in the data directory. Set
  `SEMANTIQ_EMBEDDINGS=stub` to skip it (semantic search off).
- **HTTP API listens on `127.0.0.1` by default** (was `0.0.0.0`); pass
  `--http-host 0.0.0.0` to expose it. The API is unauthenticated.
- **MCP**: `rmcp` 0.1 -> 3.5. Tools now return `structuredContent` with an
  `outputSchema`, plus read-only annotations; `semantiq serve --http-port`
  also serves MCP Streamable HTTP at `/mcp`. MCP tools: `semantiq_search`,
  `semantiq_repo_map`, `semantiq_find_refs`, `semantiq_deps`,
  `semantiq_explain`, `semantiq_impact`, `semantiq_calls`,
  `semantiq_hierarchy`, `semantiq_dead_code`.
- `semantiq init` now also installs the `semantiq` Agent Skill and rewrites
  its `CLAUDE.md` section as a marked block (see below); re-run it in existing
  projects to pick up the skill.

### Changed
- **Code-specific embedding model**: all-MiniLM-L6-v2 (384-D) replaced by
  nomic-ai/CodeRankEmbed INT8 (768-D, CLS pooling, query prefix via the new
  `EmbeddingModel::embed_query`). Model and tokenizer URLs are pinned to
  immutable revisions and verified against hard-coded SHA-256 digests. On a
  local benchmark (doc comment -> function), R@1 goes from 0.60 to 0.90.
  Schema v6 rebuilds `chunks_vec` at 768 dimensions and forces a full
  reindex; the active model id is stored in metadata so future model changes
  (including stub -> ONNX builds) trigger the same rebuild.
- HTTP API binds to `127.0.0.1` by default; use `--http-host` to expose it.
- README: indexing before `serve` is optional, `serve` indexes on startup (#17).
- `semantiq init` writes its `CLAUDE.md` instructions as a marked block
  (`<!-- semantiq:start -->`) refreshed in place on re-run and appended to an
  existing `CLAUDE.md` (previously skipped); it now points at the CLI/skill
  as well as the MCP tools. An unmodified CLAUDE.md from an older `init` is
  replaced. Re-running `init` leaves an identical `.mcp.json` untouched.
- `.gitignore` entry is now `.semantiq.db*` (covers the WAL/SHM files).
- **`semantiq` skill**: covers `calls`, `hierarchy`, `dead-code` and `map`
  (command table, examples, how to read the results, JSON fields in
  `REFERENCE.md`). Its description now lists the structural and conceptual
  questions that should trigger it ("who calls", "what implements", unused
  code, "where is X handled", before renaming or changing a symbol): the
  agent benchmark never loaded the previous one.
- The `CLAUDE.md` block written by `semantiq init`, the `AGENTS.md` block and
  the Cursor rules of `init-cursor` now say *when* to prefer Semantiq (real
  references, impact, calls, hierarchy, dead code, concepts with no keyword)
  and to keep grep for exact strings, instead of "use Semantiq first", which
  doubled the cost in the benchmark without better answers. They mention the
  new commands and tools.
- The global `--json` flag prints results as JSON for query commands (it
  still switches logs to JSON for the others); query commands log warnings
  and errors only unless `--verbose`.

- **Real embeddings by default**: the `onnx` feature is now a default feature
  of the `semantiq` binary, so `cargo install` / `cargo build` get
  CodeRankEmbed instead of the zero-vector stub. Opt out with
  `--no-default-features` (used for the `x86_64-apple-darwin` release, which
  `ort` cannot target). CI and release workflows drop the redundant
  `--features onnx`.
- The embedding model id written to the index is resolved at runtime
  (`embedding_model_id()` replaces the `EMBEDDING_MODEL_ID` const), so
  switching between the stub and the real model always rebuilds the vectors.

### Added
- **AST-based references**: `semantiq_find_refs` (and `semantiq refs`) read
  identifier occurrences from the syntax tree (`refs` table) instead of text
  search, so comments, strings and longer names never match; each usage is
  tagged `call`, `type`, `import` or `reference`.
- **Change impact**: `semantiq_impact` / `semantiq impact` lists the places
  that may break when a symbol changes (transitive, up to `max_depth`),
  grouped by file, with a confidence per site and the test files to run.
- **Claude Code plugin**: install the skill and the MCP server from Claude
  Code with `/plugin marketplace add so-keyldzn/semantiq` then
  `/plugin install semantiq@semantiq` (the `semantiq` binary is still
  installed separately).
- **Agent benchmark** (`bench/agent/`): reproducible harness running Claude
  Code headless with and without Semantiq (MCP, guided, CLI + skill), with
  automatic scoring and a first 300-run report.
- **Structural intelligence tools** (MCP, REST and CLI):
  - `semantiq_calls` / `POST /calls` / `semantiq calls`: callers and callees of a function or
    method up to 3 levels, each edge with a resolution confidence
    (`same_file`, `imports`, `unique_name`, `name_only`); library calls are
    summarised separately.
  - `semantiq_hierarchy` / `POST /hierarchy` / `semantiq hierarchy`: supertypes and subtypes /
    implementors of a type, transitively (Rust `impl Trait for Type` and
    supertraits, TS/JS, Python, Java, Kotlin, C#, C++, PHP, Ruby, Scala; Go's
    implicit interfaces are out of scope).
  - `semantiq_dead_code` / `POST /dead-code` / `semantiq dead-code`: functions, methods and types
    with no reference outside their definition, excluding entry points,
    tests, trait members and (by default) public symbols, each with a
    confidence and reasons.
  `PARSER_VERSION` 10 → 11 and schema v8 (`call_edges`, `type_relations`
  tables) trigger a one-time full reindex.
  The CLI commands take the same parameters as the tools, print their
  structured output with `--json` (shared `calls_output` /
  `hierarchy_output` / `dead_code_output` builders) and skip the embedding
  model.
- **CLI for every MCP tool**: `semantiq refs`, `deps`, `explain` and `impact`
  join `search`, all with `--json` printing the exact structured output of the
  matching MCP tool (shared `*_output()` builders in `semantiq-mcp`). They find
  `.semantiq.db` in the current directory or a parent, reindex changed files
  before answering (`--no-refresh` to skip), exit 1 with a `semantiq index`
  hint when the index is missing or empty, and 2 on usage errors. Only
  `search` loads the embedding model (new
  `RetrievalEngine::without_embeddings`): the others answer in ~20 ms.
- **`semantiq` Agent Skill** (`skills/semantiq/SKILL.md` + `REFERENCE.md`):
  when to use each command rather than grep and how to read scores and
  confidences. `semantiq init` installs it in `.claude/skills/semantiq/`
  (kept if edited, unless `--force`); `semantiq init --global` installs it in
  `~/.claude/skills/semantiq/`.
- `semantiq init --no-mcp` (skill only), `--no-skill` (MCP only) and
  `--no-index`. `init-cursor` adds a Semantiq section to `AGENTS.md`.
- `SEMANTIQ_EMBEDDINGS=stub` forces the stub model (no download, semantic
  search off); `SEMANTIQ_EMBEDDINGS=onnx` forces the real one. The test suite
  uses the stub through a dev-only `test-stub` feature and never downloads the
  model.
- When semantic search is unavailable (stub build or override), `serve` and
  `index` log a warning at startup, `semantiq stats` shows an `Embeddings`
  section, `GET /stats` reports `embedding_model`, `semantic_search` and
  `semantic_search_unavailable_reason`, and the MCP instructions say so.
- **Repository map**: new MCP tool `semantiq_repo_map`, CLI `semantiq map` and
  HTTP `POST /map`. Ranks files with PageRank over AST references and resolved
  imports (personalized by an optional `focus` list of files, directories or
  symbols), then renders the signatures and first doc line of the most used
  symbols, grouped by directory, within `max_tokens` (default 1500, 256-8000,
  chars / 4). Deterministic, no schema change; the unfocused ranking is cached
  until the index changes. Server instructions now suggest calling it first on
  an unfamiliar repository.
- MCP tool responses start with a `⏳ Initial indexing in progress` notice
  while the startup index pass runs, and `GET /stats` reports `indexing` (#17).

### Fixed
- File watcher: exclusions are evaluated on the path relative to the project
  root, so projects living under a hidden or excluded directory (e.g.
  `~/.config/app`, a `.tmpXXXX` dir) are auto-indexed again.
- Renamed, moved or deleted files are removed from the index (watcher events
  on vanished paths, and files missing at startup are pruned); unchanged
  files are no longer re-parsed and re-embedded on every watcher event.
- Semantic search keeps the nearest chunks (results were truncated in rowid
  order), and `min_score` is applied to raw per-strategy scores, so the
  weakest hit of each strategy is no longer always dropped.
- MCP and HTTP handlers run engine calls on the blocking pool, so a long
  reindex no longer stalls the async runtime.
- Rust brace imports (`use a::{B, C}`) are expanded into one import per item,
  and `get_dependents` no longer issues one query per importer.
- `semantiq update`: a failed Windows install restores the previous binary;
  `--force` never downgrades a newer build; pre-release versions compare
  correctly (`1.0.1` > `1.0.1-beta`).
- The update notice points at `semantiq update` instead of `npm install -g`,
  and the `semantiq_search` description states the real default `min_score`
  (0.3).
- npm: Windows install ran the `/bin/sh` placeholder instead of `semantiq.exe`.
  The `bin` entry is now a Node launcher, so npm's `.cmd`/`.ps1` shims work in
  PowerShell, cmd and VS Code (#16).

### Security
- Bump `rustls` 0.23.36 -> 0.23.45 and `rustls-webpki` 0.103.13 -> 0.103.15.

## [0.9.0] - 2026-05-29

Reliability, supply-chain, and search-quality release. Re-indexing no longer
leaks orphan `chunks_vec` rows (file ids are now stable), embeddings are
genuinely batched, and a new `semantiq update` self-update command ships
SHA256-verified binaries. Two behavior changes to note: HTTP CORS is now
restrictive by default, and `PARSER_VERSION` 7 → 8 triggers a one-time full
reindex on first start after upgrading.

### Added
- **`semantiq update` self-update command**. Downloads the matching release
  archive, verifies its published SHA256 (fail-closed — aborts if the
  checksum is missing or mismatched), and atomically replaces the running
  binary. Supports `--check` (report availability only) and `--force`.
- **Batched embeddings**. `embed_batch` now runs a single padded forward
  pass instead of one inference per chunk. The CLI `index` command and the
  parser pipeline feed embeddings in batches. `EmbeddingModel` exposes
  `is_stub()` and a unified `EMBEDDING_DIMENSION`.
- **Regression and integration tests**: schema v4 → v5 migration tests, an
  `AutoIndexer` integration suite, the `vec_invariant` test pinning the
  no-orphan contract, and reindex/embedding-dimension checks.

### Changed
- **CORS is restrictive by default (behavior change)**. The HTTP API
  (`--http-port`) no longer allows cross-origin requests out of the box;
  cross-origin access must be opted into explicitly via `--cors-origin`.
  Same-origin clients are unaffected.
- **npm installer verifies the published SHA256 before extraction
  (fail-closed)**. Releases must now ship a `.sha256` file alongside each
  archive or installation aborts. The out-of-lockfile `ort` prebuilt
  runtime is now documented.
- **`PARSER_VERSION` 7 → 8** — triggers a one-time full reindex on the first
  start after upgrading.
- **MCP/HTTP serving hardening**. The auto-indexer's `process_events` now
  runs on `spawn_blocking`, the HTTP server shuts down gracefully, and the
  dead `tools` module was removed.

### Fixed
- **`chunks_vec` orphan rows on reindex (HIGH-1)**. File rows are now kept
  stable across reindexes via `INSERT ... ON CONFLICT(path) DO UPDATE`
  instead of delete-then-reinsert, so reusing the same file id no longer
  strands the previous chunk vectors. `update_chunk_embedding` is now
  transactional with a dimension check, the `foreign_keys` PRAGMA is enabled
  on in-memory connections, and FTS5 results use `ORDER BY rank`.
- **Resilient incremental indexing**. Exclusion is now checked against the
  relative path, the file hash is committed last (so an interrupted index no
  longer marks a file as up-to-date), and lexical paths normalize `..`
  components.
- **Resilient per-file CLI `index`**. A single file error no longer aborts
  the whole run; failures are collected and reported in an error summary.
  Embeddings are generated in batches.
- **Search ranking quality**. Semantic search is skipped on the stub model,
  expanded-variant FTS matches are deduplicated with a penalty, cross-strategy
  scores are min-max normalized, and the min-score floor is aligned. Removed
  N+1 path/language lookups, the file-list cache is now `Arc`'d, text matchers
  are compiled once, and per-file read size is capped.
- **Parser extraction**. Every declarator in a multi-declarator statement is
  now indexed (dedup key includes the name range), Scala multi-binding
  `val`/`var` are handled, doc-comments break across blank lines, text is read
  via `utf8_text()` instead of manual byte slicing, and symbol/import kinds
  gain `Display`/`FromStr`.

### Security
- **Bump `openssl` 0.10.79 → 0.10.80** (GHSA-phqj-4mhp-q6mq, transitive
  dependency) — fixes a potential out-of-bounds write in AES-KW-PAD ciphers.
  Semantiq does not exercise that code path; updated as a precaution.

## [0.8.0] - 2026-05-08

Bugfix release that repairs the semantic search pipeline. Six out of seven
natural-language ("intent") queries returned zero results on real codebases
because of accumulated orphan rows in the sqlite-vec virtual table. After
upgrading, the v4→v5 migration runs automatically on first start and cleans
the existing residue.

### Fixed
- **`chunks_vec` orphan rows**. sqlite-vec virtual tables don't honor
  `FK ON DELETE CASCADE`, so every prior `INSERT INTO chunks` (after the
  delete-then-reinsert pattern in `insert_chunks`) left the previous
  chunk's vector behind. Over many reindex cycles those orphans
  dominated the KNN top-k and silently broke semantic search. All
  delete paths (`insert_chunks`, `delete_file`, `clear_all_data`,
  `check_and_prepare_for_reindex`) now purge `chunks_vec` in the same
  transaction. A new integration test (`vec_invariant`) pins the
  contract.
- **Ghost files with absolute paths**. The legacy
  `path.strip_prefix(root).unwrap_or(path)` pattern silently fell through
  to absolute paths when `strip_prefix` failed, leaving duplicate rows
  in `files`. Replaced everywhere with `paths::to_relative_string`,
  which warns instead of failing silently.
- **`pub use` re-exports not extracted**. `parse_rust_use_path` stripped
  `"use "` first, so any line starting with `pub` (or `pub(crate)`,
  `pub(super)`, `pub(in path)`) returned `None`. This broke
  `semantiq_deps` on every Rust `lib.rs`, since those files are mostly
  re-exports.
- **`symbol_kind` missing in semantic search results**. `search_semantic`
  hard-coded `symbol_kind: None`, so output showed `Symbol: foo (unknown)`
  even though the kind was already in the database. Now batch-fetched
  per file and propagated.

### Added
- **Schema v4 → v5 migration**: purges existing `chunks_vec` orphans and
  ghost rows with absolute paths (POSIX, Windows drive paths, UNC).
  Wrapped in a single transaction for crash safety. Visible at startup
  as e.g. `Migrating schema v4 -> v5 (a): purged N orphan rows`.
- `IndexStore::count_orphan_chunk_vectors()` for diagnostics. The
  `RetrievalEngine` calls it at startup and emits a `WARN` if the
  invariant is broken.
- New `paths::to_relative_string` helper in `semantiq-index`. Tries a
  literal `strip_prefix` first and only canonicalizes on the fallback
  path, keeping the hot indexing loop free of `realpath` syscalls.

### Performance
- ~6× lower latency on natural-language queries via the MCP server now
  that the KNN top-k surfaces real chunks instead of zombie zero-vectors
  (28–93 ms vs 78–123 ms previously, on the reference repo).

### Migration notes
- The v4→v5 migration runs automatically the first time a v0.8.0 binary
  opens an existing database. It is idempotent and crash-safe (single
  `BEGIN IMMEDIATE` transaction). Expect a one-shot log line reporting
  how many rows were swept; on heavily-reindexed databases this can be
  in the thousands.
- No changes to MCP tool signatures or output schema. Existing clients
  keep working; output for `semantiq_search` now includes meaningful
  `symbol_kind` values where it previously said `unknown`.

## [0.7.0] - 2026-05-07

### Changed — BREAKING (extraction de symboles)
- **`PARSER_VERSION` 5 → 6** — déclenche un reindex complet automatique au prochain démarrage.
- Migration de tous les langages (18) vers l'extraction par tree-sitter queries
  (`crates/semantiq-parser/queries/<lang>/tags.scm`). Le parcours AST récursif legacy
  reste accessible en interne (`pub(crate) extract_legacy`) comme oracle pour les tests.

#### Changements de format observables
- **Imports** : `name` extrait = nom court (dernier segment du path) au lieu du
  texte entier. Exemples :
  - Rust : `use std::collections::HashMap;` → `name = "HashMap"` (avant : la déclaration entière)
  - Python : `import os` → `name = "os"`
  - PHP : `use Foo\Bar;` → `name = "Bar"`
- **Kotlin** : `interface Greeter` est maintenant capturé comme `Interface`
  (avant : `Class`). `enum class Status` est maintenant `Enum` (avant : `Class`).
  Les méthodes dans `class_body` sont `Method` (avant : `Function`).
  Les imports Kotlin (`import x.y.z`) sont désormais capturés.
- **C++** : les méthodes inline (`class C { int add(int) {} }`) sont maintenant
  extraites comme `Method` avec le parent classe. Avant : non capturées.
  Destructeurs (`~C`) et opérateurs (`operator+`) sont aussi capturés.
- **Elixir** : `defmodule X` est `Module` (avant : `Function`). `def`, `defp`,
  `defmacro`, `defmacrop` sont tous capturés. Le `parent` des `def` reflète
  le `defmodule` englobant ; les modules imbriqués utilisent `.` comme séparateur
  (`MyApp.Outer.Inner`) au lieu de `::`.
- **Python** : les méthodes décorées (`@staticmethod def m`) ne sont plus
  doublées en `Method` + `Function` ; un seul `Method` est extrait.
- **HTML** : seuls les éléments **top-level** (enfants directs de `document`)
  sont extraits. Évite l'explosion d'index sur du HTML réel (auparavant chaque
  `<div>`/`<p>` imbriqué devenait un symbole).
- **JSON / YAML / TOML** : les clés imbriquées ont désormais un `parent` au
  format dot-separated (`a.b.c`). Avant : `parent = None` pour toutes.
- **Rust** : `impl_item` n'est plus extrait comme `Class` parasite.

#### Hardening
- `QuerySymbolExtractor::new()` **panique** désormais si une query .scm échoue
  à compiler, avec la liste exhaustive des erreurs. Avant : `tracing::warn!`
  silencieux + dégradation cachée vers le legacy.
- Suppression de l'instance dupliquée de `QuerySymbolExtractor` dans
  `LanguageSupport` ; une seule source de vérité via le `OnceLock` global.

## [0.6.2] - 2026-05-04

### Security
- **HIGH**: Bumped `rustls-webpki` 0.103.9 → 0.103.13 to fix four CVEs:
  - RUSTSEC-2026-0049 — CRLs not considered authoritative by Distribution Point due to faulty matching logic
  - RUSTSEC-2026-0098 — Name constraints for URI names were incorrectly accepted
  - RUSTSEC-2026-0099 — Name constraints accepted for certificates asserting a wildcard name
  - RUSTSEC-2026-0104 — Reachable panic in certificate revocation list parsing
- **LOW**: Bumped `rand` 0.9.2 → 0.9.4 (RUSTSEC-2026-0097, unsoundness with custom logger)
- Bumped `openssl` 0.10.75 → 0.10.79

### Changed
- Split `crates/semantiq-mcp/src/server.rs` test module into per-tool files
  (`server/tests/{search,find_refs,deps,explain,server_handler,edge_cases}.rs`).
  `server.rs` shrinks from 931 to 479 lines; no behavior change.

## [0.6.1] - 2026-05-04

### Fixed
- Persist `SCHEMA_VERSION` in `metadata` after migration so future migrations can correctly detect that `v3 → v4` was applied
- `IndexStore::open_in_memory` now runs `migrate_schema`, so test fixtures exercise the migration path
- Added regression tests for path-traversal-escaping imports and Python 3-dot relative imports (`from ...top`)

### Changed
- Extracted `PYTHON_STD_MODULES` from `imports.rs` (1148 → 929 lines) into a dedicated `python_stdlib` module
- Converted 10 `unused_self` methods to associated functions in `ChunkExtractor`, `QueryExpander`, `RetrievalEngine`, and `ThresholdCalibrator`
- Tightened visibility of internal items in `semantiq` and `semantiq-retrieval` from `pub` to `pub(crate)` / `pub(super)`
- Replaced wildcard `use super::types::*` in HTTP routes with explicit imports
- `resolve_python_import` now returns `Vec<PathBuf>` instead of always-`Some` `Option<Vec<PathBuf>>`

## [0.6.0] - 2026-02-18

### Added
- **HTTP API server** — Alternative to MCP stdio with `--http-port`, endpoints: `/health`, `/stats`, `/search`, `/find-refs`, `/deps`, `/explain`. Middleware: 1MB body limit, 50 concurrent requests, CORS configurable (`--cors-origin`)
- **Local import resolution** — Resolution of local import paths to actual files on disk (JS/TS, Python, Rust, Go)
- **`resolved_path` column** — Dependencies now store the resolved path, improving `find_refs` accuracy
- **Schema migration v3→v4** — Automatic incremental migration (adds `resolved_path` column)
- **Python stdlib detection** — Accurate classification of Python standard vs external imports (200+ modules, binary search)
- **Symbol parent tracking** — Symbols now include their parent (e.g., method → struct/class)
- **Dockerfile** — Multi-stage Docker image for deployment (Railway-ready)

### Changed
- Bump schema version 3 → 4 (automatic migration, no reindex required)
- Auto-indexer and CLI `index` command use local import resolution

### Fixed
- Correct git clone URL in Dockerfile
- Resolve clippy `module_inception` warning in HTTP tests
- Bump Rust version in Dockerfile to support edition 2024 and let-chains

## [0.5.2] - 2026-02-10

### Security
- **HIGH**: Fixed ReDoS vulnerability - user input is now escaped with `regex::escape()` in `TextSearcher::search()` before regex compilation
- **HIGH**: Updated `bytes` crate 1.11.0 → 1.11.1 to fix integer overflow in `BytesMut::reserve` (RUSTSEC-2026-0007)
- **HIGH**: Text search walker now uses `hidden(true)` and `should_exclude_entry` filtering, preventing reads from `.env`, `.git/`, and other sensitive directories
- **MEDIUM**: Fixed path traversal in `read_file_lines()` - paths are now canonicalized and verified to stay within the project root
- **MEDIUM**: Added input validation (empty, length ≤ 500, limit ≤ 1000) to `semantiq_find_refs`, `semantiq_explain`, and `semantiq_deps` MCP handlers
- **MEDIUM**: Added path traversal rejection (`..`) in `semantiq_deps` file path parameter
- **MEDIUM**: `resolve_project_root()` now canonicalizes paths to normalize `..` components and symlinks
- **LOW**: FTS5 query escaping now strips null bytes and control characters
- **LOW**: Query expansion limited to 10 terms to prevent amplification attacks
- **LOW**: MCP error messages sanitized to avoid leaking internal file paths
- **LOW**: Version check HTTP response limited to 10KB to prevent memory exhaustion
- **LOW**: Poisoned mutex recovery in `DistanceCollector` now logs warnings instead of silently continuing

### Changed
- Capped `limit` parameter to 1000 on all MCP tool handlers at the server level

## [0.5.0] - 2026-01-31

### Added
- **Adaptive ML Thresholds** - Automatic calibration of semantic search thresholds per programming language
  - Bootstrap mode: Collects 100% of distance observations until 500 samples
  - Production mode: Switches to 10% sampling after bootstrap
  - Auto-calibration: Triggers automatically when bootstrap completes
  - Percentile-based thresholds: Uses p90 for max_distance, p10 for min_similarity
  - Per-language calibration with fallback cascade (language → global → defaults)
- **New `calibrate` CLI command** - Manual threshold calibration with `--dry-run` option
- **ML stats in `stats` command** - Shows bootstrap progress, observations per language, calibrated thresholds
- **New database tables** - `distance_observations` and `threshold_calibration` for ML data
- **CI workflows for `dev` branch** - Tests, Clippy, format checks, and multi-platform builds

### Changed
- **Refactored `store.rs`** (2108 lines → 8 modules) - Better code organization
  - `store/mod.rs` - Core IndexStore struct and helpers
  - `store/files.rs` - File operations and parser version management
  - `store/symbols.rs` - Symbol search and insertion
  - `store/chunks.rs` - Chunk operations and embeddings
  - `store/dependencies.rs` - Dependency graph operations
  - `store/observations.rs` - ML distance observation storage
  - `store/calibrations.rs` - Threshold calibration persistence
  - `store/tests.rs` - All unit tests
- **Refactored `engine.rs`** (1049 lines → 5 modules) - Cleaner architecture
  - `engine/mod.rs` - RetrievalEngine struct and construction
  - `engine/search.rs` - Semantic, symbol, and text search
  - `engine/threshold.rs` - Adaptive threshold management
  - `engine/analysis.rs` - References, dependencies, symbol explanation
  - `engine/tests.rs` - Unit tests
- Schema version bumped to 3 (triggers automatic reindex)

## [0.4.0] - 2026-01-28

### Added
- **JSON logging support** - Structured logging throughout the codebase
- **JSON logging by default** for `serve` command - Better integration with log aggregators
- **MCP tests** - Comprehensive test coverage for MCP server functionality
- **CI and security workflows** - Automated testing and security scanning

### Changed
- **`init-cursor` command is now language-agnostic** - Works with any project type
- Updated `deny.toml` to v2 schema

### Fixed
- Cross-platform FFI compatibility using `c_char`
- Clippy compatibility with `is_multiple_of()`
- Cargo audit integration (replaced rustsec/audit-check action)
- Various clippy warnings resolved throughout codebase
- Added CDLA-Permissive-2.0 license for webpki-roots dependency
- Cross-compilation for aarch64-linux using `cross`

## [0.3.4] - 2026-01-20

### Added
- **macOS Intel (x86_64-apple-darwin) support restored** - Binary now available for Intel Macs
- **CI build workflow** - New `build.yml` for testing builds on push/PR without publishing

### Changed
- **ONNX feature now optional** - `--features onnx` required on supported platforms (Apple Silicon, Linux, Windows)
- Intel Mac builds use `StubEmbeddingModel` (no ONNX) due to missing prebuilt binaries
- Updated CI to use `macos-15` runner for Intel Mac cross-compilation

## [0.3.3] - 2026-01-19

### Added
- **Search filtering options** for `semantiq_search` - more precise and relevant results
  - `min_score` - Minimum relevance score threshold (0.0-1.0, default: 0.35)
  - `file_type` - Filter by file extensions (e.g., "rs,ts,py")
  - `symbol_kind` - Filter by symbol type (e.g., "function,class,struct")
- **CLI flags** for search command: `--min-score`, `--file-type`, `--symbol-kind`
- **Smart default exclusions** - Automatically excludes non-code files (.json, .lock, .yaml, .md, .toml, etc.)
- **`SearchOptions` struct** in `semantiq-retrieval` with builder pattern

### Changed
- `RetrievalEngine::search()` now accepts optional `SearchOptions` parameter
- Improved search relevance by filtering low-score results by default
- Removed obsolete `is_code_file()` function in favor of `SearchOptions::accepts_extension()`

### Added (Tests)
- 12 new unit tests for `SearchOptions` in `query.rs`

## [0.3.2] - 2026-01-19

### Added
- **`.gitignore` support in `init-cursor`** - automatically adds Semantiq database entries
  - Creates `.gitignore` if not present
  - Updates existing `.gitignore` preserving original content
  - Skips if entries already present (no duplication)

### Added (Tests)
- 3 new tests for `.gitignore` handling in `init_cursor.rs`

## [0.3.1] - 2026-01-19

### Added
- **New `init-cursor` command** for Cursor/VS Code configuration setup
  - Creates `.cursor/rules/project.mdc` (general project guidelines)
  - Creates `.cursor/rules/semantiq.mdc` (Semantiq MCP tools usage)
  - Creates `.cursor/mcp.json` (MCP server configuration)
  - Creates `.cursorignore` (indexing exclusions)
  - Creates `.vscode/` config (settings, tasks, launch, extensions)
  - Preserves existing files (skip instead of overwrite)

### Changed
- Centralized `DEFAULT_DB_NAME` and path resolution utilities in `common.rs`
- Refactored all CLI commands to use shared utilities
- CLI description now generic ("for a project" instead of "for a Rust project")

### Added (Tests)
- 7 new unit tests for `common.rs` and `init_cursor.rs`

## [0.3.0] - 2026-01-19

### Added
- **sqlite-vec integration** for vector similarity search (384-dim MiniLM-L6-v2 embeddings)
- **Automatic initial indexing** when MCP server starts (no more manual `semantiq index` required)
- **6 new languages**: HTML, JSON, YAML, TOML, Bash, Elixir (total: 15 languages)
- **ripgrep integration** for fast regex text search via `TextSearcher`
- New `search_similar_chunks()` method for semantic vector search
- New `InitialIndexResult` struct for tracking initial indexing progress

### Fixed
- **"Imported by" always empty** in `semantiq_deps` - rewrote `get_dependents()` to match JS/TS import paths (`@/...`, `./...`, `../...`)
- Import path resolution now handles basename matching with multiple extensions

### Changed
- Schema version bumped to 2 (triggers automatic reindex)
- Added `chunks_vec` virtual table for sqlite-vec embeddings
- `start_auto_indexer()` now runs `initial_index()` before watching for changes
- Improved dependency matching with multiple LIKE patterns and post-filtering

## [0.2.9] - 2026-01-19

### Fixed
- Arrow functions (`const fn = () => {}`) now correctly indexed as `function` instead of `variable`
- Function expressions (`const fn = function() {}`) now correctly indexed as `function`

### Changed
- Added `is_function_variable()` helper to detect functions assigned to variables
- Added `arrow_function` and `lexical_declaration` to chunk boundaries for TypeScript/JavaScript
- Bumped `PARSER_VERSION` to 3 (triggers automatic reindex)

## [0.2.8] - 2026-01-18

### Security
- **CRITICAL**: Added SHA-256 checksum verification for ONNX model downloads (TOFU + hardcoded support)
- **CRITICAL**: Added path traversal protection with canonicalization in `validate_path()`
- **HIGH**: Added `MAX_AST_DEPTH=500` recursion limit in parser to prevent stack overflow attacks
- **HIGH**: Added `safe_slice()` function to prevent panic on invalid byte indices
- **HIGH**: Changed model directory fallback from "." to system temp dir (prevents writes to unexpected locations)
- **HIGH**: Added pagination for `get_chunks_with_embeddings()` to prevent memory exhaustion DoS
- **HIGH**: Reduced download size limit from 500MB to 100MB
- **HIGH**: Added restrictive file permissions (0600 on Unix) for downloaded models and database
- **MEDIUM**: Added explicit symlink handling (`follow_links(false)`) to prevent escape from project root

### Changed
- Refactored `download_file()` with connection timeouts (30s connect, 5min global)
- Improved checksum verification with detailed warning messages

## [0.2.7] - 2026-01-18

### Added
- Automatic version update notification at server startup
- Non-blocking background check using GitHub Releases API
- Local cache (24h) to avoid repeated API calls
- `--no-update-check` CLI flag to disable update notifications
- `SEMANTIQ_UPDATE_CHECK` environment variable for configuration

### Changed
- Updated author info to keyldzn

## [0.2.6] - 2026-01-18

### Added
- Automatic reindexation when parser version changes (no more manual `--force` needed)
- `PARSER_VERSION` constant to track parser logic changes
- Support for `const`/`let` variable extraction in TypeScript/JavaScript
- GitHub Sponsors funding configuration

### Changed
- Version detection uses atomic transactions to prevent race conditions
- Documentation updated with known limitations and setup guides

### Fixed
- Filter out verbose ONNX Runtime logs during indexing

## [0.2.4] - 2026-01-18

### Fixed
- Model download failing in async Tokio context (replaced `reqwest::blocking` with `ureq`)
- Download size limit too small for 90MB ONNX model (increased to 200MB)
- ONNX inference crash due to missing `token_type_ids` input
- Embeddings not generated during `semantiq index` command

### Changed
- `semantiq index` now generates embeddings for all chunks
- Centralized file exclusion logic into `exclusions.rs` module
- Auto-indexer and FileWatcher now use shared exclusion patterns

## [0.2.3] - 2026-01-18

### Added
- ONNX embedding model integration for semantic search
- Automatic model download on first run
- Cosine similarity search for vector matching
- Alternative installation via `cargo install --git`
- CHANGELOG.md for version history

### Changed
- Embeddings now generated automatically during auto-indexing
- Switch from OpenSSL to rustls for better cross-compilation support
- Use ort download-binaries for automatic ONNX Runtime provisioning

### Removed
- macOS Intel (x86_64-apple-darwin) binary - ONNX Runtime does not support this target

## [0.2.2] - 2026-01-17

### Changed
- Improved CLAUDE.md template to prioritize Semantiq tools over grep/Glob

## [0.2.1] - 2026-01-17

### Fixed
- Error handling with proper mutex propagation
- SQL injection vulnerability via LIKE escaping
- UTF-8 safety in tree-sitter text extraction
- N+1 query pattern in get_stats() (4 queries → 1)

### Changed
- Shared single `Arc<IndexStore>` instead of 3 separate DB connections
- Improved scoring algorithm with symbol type boosting
- Results limited to 500 to prevent memory issues
- Added `PRAGMA busy_timeout=5000` for concurrent access

## [0.2.0] - 2026-01-17

### Added
- Automatic npm package version update from git tag

## [0.1.3] - 2026-01-17

### Added
- New `semantiq init` command for easy project setup
- Auto-creates `.claude/settings.json`, `CLAUDE.md`, updates `.gitignore`
- Runs initial indexation automatically

## [0.1.2] - 2026-01-17

### Added
- Auto-indexing for real-time file updates
- FileWatcher integration with create/modify/delete events
- Background task with 2-second polling

## [0.1.1] - 2026-01-17

### Added
- npm README documentation
- Updated main README with correct npm package name

## [0.1.0] - 2026-01-17

### Added
- Initial release
- MCP server with 4 tools: search, find_refs, deps, explain
- Support for 9 languages via tree-sitter
- SQLite storage with FTS5 search
