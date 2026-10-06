//! Repository map: a compact, ranked overview of the codebase under a token
//! budget, in the spirit of Aider's repo map.
//!
//! Files are the nodes of a graph whose edges come from the index:
//! - a reference (`refs`, non-definition) to a name defined in another file
//!   links the referencing file to the defining file, weighted by
//!   `sqrt(occurrences)`, damped for short/private/widely-defined names and
//!   split between homonym definitions;
//! - an import resolved to an indexed file (`dependencies.resolved_path`).
//!
//! PageRank over that graph ranks files. Each file then hands its rank to the
//! definitions it references, in proportion to the edge weights, which ranks
//! symbols. The map lists the best symbols (signature + first doc line),
//! grouped by directory and file, as many as fit in `max_tokens` (≈ chars/4).
//!
//! `focus` personalizes the PageRank teleport vector (like Aider's chat
//! files): focused files, files defining focused symbols and, with less
//! weight, their direct neighbours. References to focused symbols weigh 10x.
//!
//! Everything is computed from the index, without any LLM call, and every
//! iteration order is fixed so the same index always yields the same map.

use crate::engine::{RetrievalEngine, is_test_location};
use anyhow::Result;
use semantiq_index::{IndexStore, RepoGraphData};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;
use tracing::debug;

pub const DEFAULT_REPO_MAP_TOKENS: usize = 1500;
pub const MIN_REPO_MAP_TOKENS: usize = 256;
pub const MAX_REPO_MAP_TOKENS: usize = 8000;

const DAMPING: f64 = 0.85;
const MAX_ITERATIONS: usize = 100;
const TOLERANCE: f64 = 1e-12;
/// Implicit outgoing weight of every file towards the teleport vector.
const OUT_LEAK: f64 = 1.0;
/// Weight of an import resolved to an indexed file.
const IMPORT_EDGE_WEIGHT: f64 = 1.0;
/// Share of its file's rank a symbol gets on top of its own reference rank,
/// so that unreferenced symbols of important files come before those of
/// minor files. Higher for focused files, whose own content matters.
const FILE_RANK_SHARE: f64 = 0.02;
const FOCUS_FILE_RANK_SHARE: f64 = 0.5;
/// Tests and fixtures matter less in an overview, unless focused.
const TEST_PENALTY: f64 = 0.1;
/// Teleport mass of the neighbours of focused files, relative to the focus.
const NEIGHBOUR_MASS: f64 = 0.5;
const FOCUS_NAME_BOOST: f64 = 10.0;
const MAX_SIGNATURE_CHARS: usize = 120;
const MAX_DOC_CHARS: usize = 80;

/// Method and macro names so common in standard libraries that most calls to
/// them do not target the project's definition (matching is by name only).
const COMMON_NAME_WEIGHT: f64 = 0.01;
#[rustfmt::skip]
const COMMON_NAMES: &[&str] = &[
    // Methods of standard containers, strings and traits
    "new", "default", "from", "into", "get", "set", "len", "is_empty", "as_str", "as_ref",
    "to_string", "clone", "drop", "fmt", "eq", "cmp", "hash", "next", "iter", "push", "pop",
    "insert", "remove", "contains", "map", "join", "split", "add", "append", "length",
    "__init__", "toString", "equals", "hashCode", "constructor",
    // Generic verbs and accessors
    "run", "name", "build", "parse", "read", "write", "open", "close", "init", "start", "stop",
    "update", "delete", "call", "apply", "execute", "main", "value", "key", "id", "kind",
    "path", "size", "count", "config", "store", "load", "metadata",
    // Macros and prelude names
    "format", "print", "println", "eprintln", "vec", "assert", "assert_eq", "matches", "debug",
    "info", "warn", "error", "trace", "panic", "Error", "Result", "Ok", "Some", "None", "Item",
    "Output", "Target",
];
/// Data languages: their keys are indexed as symbols but make poor map entries.
const DATA_LANGUAGES: &[&str] = &["json", "yaml", "toml", "html"];
/// Symbol kinds left out of the map (locals, imports, `mod x;` lines).
const SKIPPED_KINDS: &[&str] = &["import", "module", "variable"];

#[derive(Debug, Clone, Default)]
pub struct RepoMapOptions {
    /// Token budget (≈ chars/4), clamped to `MIN..=MAX_REPO_MAP_TOKENS`.
    pub max_tokens: usize,
    /// File paths (exact, path suffix or directory) or symbol names.
    pub focus: Vec<String>,
    /// Only list files whose path starts with this prefix.
    pub path_prefix: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RepoMapSymbol {
    pub name: String,
    pub kind: String,
    pub line: usize,
    pub parent: Option<String>,
    pub signature: String,
    pub doc: Option<String>,
    pub rank: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RepoMapFile {
    pub path: String,
    pub language: Option<String>,
    pub rank: f64,
    pub symbols: Vec<RepoMapSymbol>,
}

#[derive(Debug, Clone)]
pub struct RepoMap {
    /// Listed files, grouped by directory.
    pub files: Vec<RepoMapFile>,
    /// Code files eligible for the map (after `path_prefix`).
    pub total_files: usize,
    /// Symbols eligible for the map (after `path_prefix`).
    pub total_symbols: usize,
    pub shown_symbols: usize,
    pub max_tokens: usize,
    /// Estimated size of `text` (chars/4, rounded up).
    pub estimated_tokens: usize,
    pub focus_files: Vec<String>,
    pub focus_symbols: Vec<String>,
    /// Focus entries matching no indexed code file nor symbol.
    pub unmatched_focus: Vec<String>,
    pub build_time_ms: u64,
    /// Rendered map.
    pub text: String,
}

/// Rough token count used for the budget.
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

/// Build a repo map straight from the store (no engine, no embedding model).
pub fn build_repo_map(store: &IndexStore, options: &RepoMapOptions) -> Result<RepoMap> {
    let start = Instant::now();
    let ranked = rank_repo(store.load_repo_graph()?, &options.focus);
    Ok(render_repo_map(&ranked, options, start))
}

impl RetrievalEngine {
    /// Build a repo map. Without focus, the ranking is cached until the index
    /// changes (see [`IndexStore::graph_fingerprint`]).
    pub fn repo_map(&self, options: &RepoMapOptions) -> Result<RepoMap> {
        let start = Instant::now();
        let ranked = if options.focus.is_empty() {
            self.cached_ranking()?
        } else {
            Arc::new(rank_repo(self.store.load_repo_graph()?, &options.focus))
        };
        Ok(render_repo_map(&ranked, options, start))
    }

    fn cached_ranking(&self) -> Result<Arc<RankedRepo>> {
        let fingerprint = self.store.graph_fingerprint()?;
        if let Ok(cache) = self.repo_map_cache.lock()
            && let Some((cached_fingerprint, ranked)) = cache.as_ref()
            && *cached_fingerprint == fingerprint
        {
            debug!("repo map ranking served from cache");
            return Ok(Arc::clone(ranked));
        }

        let ranked = Arc::new(rank_repo(self.store.load_repo_graph()?, &[]));
        // Only cache a snapshot no write interleaved with.
        if self.store.graph_fingerprint()? == fingerprint
            && let Ok(mut cache) = self.repo_map_cache.lock()
        {
            *cache = Some((fingerprint, Arc::clone(&ranked)));
        }
        Ok(ranked)
    }
}

/// Ranked files and symbols, independent of the budget and path prefix.
#[derive(Debug)]
pub(crate) struct RankedRepo {
    /// Code files, in path order.
    files: Vec<RankedFile>,
    /// Symbols, best first.
    candidates: Vec<Candidate>,
    focus_files: Vec<String>,
    focus_symbols: Vec<String>,
    unmatched_focus: Vec<String>,
}

#[derive(Debug)]
struct RankedFile {
    path: String,
    language: Option<String>,
    rank: f64,
}

#[derive(Debug)]
struct Candidate {
    /// Index into `RankedRepo::files`.
    file: usize,
    name: String,
    kind: String,
    line: usize,
    parent: Option<String>,
    signature: String,
    doc: Option<String>,
    score: f64,
}

fn normalize_path(value: &str) -> String {
    let value = value.trim().replace('\\', "/");
    value.trim_start_matches("./").to_string()
}

fn is_code_file(language: Option<&str>) -> bool {
    language.is_some_and(|lang| !DATA_LANGUAGES.contains(&lang))
}

/// Aider's identifier weighting: descriptive names matter more, private and
/// widely defined ones less.
fn name_weight(name: &str, definitions: usize) -> f64 {
    if COMMON_NAMES.contains(&name) {
        return COMMON_NAME_WEIGHT;
    }
    let mut weight = 1.0;
    let descriptive = name.contains('_')
        || (name.chars().any(|c| c.is_lowercase()) && name.chars().any(|c| c.is_uppercase()));
    if descriptive && name.chars().count() >= 8 {
        weight *= 10.0;
    }
    if name.starts_with('_') {
        weight *= 0.1;
    }
    if definitions > 5 {
        weight *= 0.1;
    }
    weight
}

/// How likely an occurrence of a reference kind points at a definition of
/// `def_kind`. A plain reference to a function's name is usually a local
/// variable or field that shares it; a call to a constant cannot be one.
fn reference_fit(def_kind: &str, ref_kind: &str) -> f64 {
    match (def_kind, ref_kind) {
        (_, "import") => 1.0,
        ("function" | "method", "call") => 1.0,
        ("function" | "method", _) => 0.1,
        ("class" | "struct" | "enum" | "interface" | "trait" | "type", "type" | "call") => 1.0,
        ("class" | "struct" | "enum" | "interface" | "trait" | "type", _) => 0.5,
        (_, "reference") => 1.0,
        _ => 0.5,
    }
}

fn kind_weight(kind: &str) -> f64 {
    match kind {
        "class" | "struct" | "enum" | "interface" | "trait" | "type" => 1.0,
        "function" => 0.6,
        _ => 0.3,
    }
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut out: String = text.chars().take(max - 1).collect();
        out.push('…');
        out
    }
}

/// First line of the declaration, without its body (one-liners included).
fn clean_signature(signature: Option<&str>, kind: &str, name: &str) -> String {
    let mut first = signature
        .and_then(|s| s.lines().map(str::trim).find(|l| !l.is_empty()))
        .unwrap_or("");
    if first.ends_with('}')
        && let Some(body) = first.find(" {")
    {
        first = &first[..body];
    }
    let first = first.trim_end_matches(['{', ';']).trim_end();
    if first.is_empty() {
        format!("{} {}", kind, name)
    } else {
        truncate_chars(first, MAX_SIGNATURE_CHARS)
    }
}

/// First meaningful line of a doc comment, without comment markers.
fn clean_doc(doc: Option<&str>) -> Option<String> {
    const PREFIXES: &[&str] = &[
        "///", "//!", "/**", "/*!", "/*", "//", "*", "#", "\"\"\"", "'''", "--",
    ];
    doc?.lines().find_map(|line| {
        let mut line = line.trim();
        if let Some(prefix) = PREFIXES.iter().find(|p| line.starts_with(**p)) {
            line = line[prefix.len()..].trim();
        }
        let line = line
            .trim_end_matches("*/")
            .trim_end_matches("\"\"\"")
            .trim_end_matches("'''")
            .trim();
        (!line.is_empty()).then(|| truncate_chars(line, MAX_DOC_CHARS))
    })
}

/// Total outgoing weight of each node, plus the teleport leak.
fn out_weights(n: usize, edges: &[(usize, usize, f64)]) -> Vec<f64> {
    let mut out = vec![OUT_LEAK; n];
    for &(src, _, w) in edges {
        out[src] += w;
    }
    out
}

/// Weighted PageRank with a personalized teleport vector. `edges` are
/// `(source, target, weight)`. Each node also leaks `OUT_LEAK` of weight to
/// the teleport vector, so a file with a single faint edge (one call to a
/// common name) does not hand its whole rank to it.
fn pagerank(n: usize, edges: &[(usize, usize, f64)], teleport: &[f64]) -> Vec<f64> {
    let out_weight = out_weights(n, edges);
    let mut rank = teleport.to_vec();
    for _ in 0..MAX_ITERATIONS {
        let mut next = vec![0.0; n];
        let mut followed = 0.0;
        for &(src, dst, w) in edges {
            let flow = rank[src] * w / out_weight[src];
            next[dst] += DAMPING * flow;
            followed += flow;
        }
        // Mass not sent along an edge (leak, dangling nodes) teleports.
        let base = DAMPING * (rank.iter().sum::<f64>() - followed) + (1.0 - DAMPING);
        for (value, t) in next.iter_mut().zip(teleport) {
            *value += base * t;
        }
        let delta: f64 = next.iter().zip(&rank).map(|(a, b)| (a - b).abs()).sum();
        rank = next;
        if delta < TOLERANCE {
            break;
        }
    }
    rank
}

/// Normalize to a probability vector, or uniform if `weights` is all zero.
fn normalized(weights: Vec<f64>) -> Vec<f64> {
    let total: f64 = weights.iter().sum();
    if total > 0.0 {
        weights.into_iter().map(|w| w / total).collect()
    } else {
        let n = weights.len().max(1) as f64;
        vec![1.0 / n; weights.len()]
    }
}

pub(crate) fn rank_repo(graph: RepoGraphData, focus: &[String]) -> RankedRepo {
    // Nodes: code files, in path order (the store sorts them).
    let files: Vec<_> = graph
        .files
        .into_iter()
        .filter(|f| is_code_file(f.language.as_deref()))
        .collect();
    let node_of: HashMap<i64, usize> = files.iter().enumerate().map(|(i, f)| (f.id, i)).collect();
    let n = files.len();

    let is_test_file: Vec<bool> = files
        .iter()
        .map(|f| is_test_location(&f.path, None))
        .collect();

    // Associated types (`type Error = …` in an impl) are trait plumbing.
    let symbols: Vec<_> = graph
        .symbols
        .into_iter()
        .filter(|s| {
            node_of.contains_key(&s.file_id)
                && !SKIPPED_KINDS.contains(&s.kind.as_str())
                && !(s.kind == "type" && s.parent.is_some())
        })
        .collect();
    // name -> files defining it, with the kinds defined there.
    let mut definers: HashMap<&str, Vec<(usize, Vec<&str>)>> = HashMap::new();
    for symbol in &symbols {
        let list = definers.entry(symbol.name.as_str()).or_default();
        let node = node_of[&symbol.file_id];
        match list.iter_mut().find(|(n, _)| *n == node) {
            Some((_, kinds)) => {
                if !kinds.contains(&symbol.kind.as_str()) {
                    kinds.push(symbol.kind.as_str());
                }
            }
            None => list.push((node, vec![symbol.kind.as_str()])),
        }
    }

    // Resolve focus entries: path, directory, path suffix, then symbol name.
    let mut focus_nodes: Vec<usize> = Vec::new();
    let mut focus_files = Vec::new();
    let mut focus_symbols = Vec::new();
    let mut unmatched_focus = Vec::new();
    for entry in focus {
        let entry = normalize_path(entry);
        if entry.is_empty() {
            continue;
        }
        let dir = format!("{}/", entry.trim_end_matches('/'));
        let suffix = format!("/{}", entry);
        let matching = |pred: &dyn Fn(&str) -> bool| -> Vec<usize> {
            (0..n).filter(|&i| pred(&files[i].path)).collect()
        };
        let mut nodes = matching(&|p| p == entry);
        if nodes.is_empty() {
            nodes = matching(&|p| p.starts_with(&dir));
        }
        if nodes.is_empty() {
            nodes = matching(&|p| p.ends_with(&suffix));
        }
        if !nodes.is_empty() {
            for node in nodes {
                if !focus_nodes.contains(&node) {
                    focus_nodes.push(node);
                    focus_files.push(files[node].path.clone());
                }
            }
        } else if let Some(defs) = definers.get(entry.as_str()) {
            focus_symbols.push(entry.clone());
            for &(node, _) in defs {
                if !focus_nodes.contains(&node) {
                    focus_nodes.push(node);
                }
            }
        } else {
            unmatched_focus.push(entry);
        }
    }
    let focus_names: HashSet<&str> = focus_symbols.iter().map(String::as_str).collect();
    let is_focus: HashSet<usize> = focus_nodes.iter().copied().collect();

    // Edges, accumulated in a BTreeMap so summation order is fixed.
    let mut edge_weights: BTreeMap<(usize, usize), f64> = BTreeMap::new();
    // (source, target, name, weight) for handing file rank down to symbols.
    let mut contributions: Vec<(usize, usize, &str, f64)> = Vec::new();
    // (source, name, target) -> occurrences, discounted when the reference
    // kind does not fit what the target defines.
    let mut occurrences: BTreeMap<(usize, &str, usize), f64> = BTreeMap::new();
    for rc in &graph.ref_counts {
        let (Some(&src), Some(defs)) = (node_of.get(&rc.file_id), definers.get(rc.name.as_str()))
        else {
            continue;
        };
        // A name the file defines itself most likely refers to that definition.
        if defs.iter().any(|(node, _)| *node == src) {
            continue;
        }
        for (dst, kinds) in defs {
            // Production code never uses test helpers: such a match is a homonym.
            if is_test_file[*dst] && !is_test_file[src] {
                continue;
            }
            let fit = kinds
                .iter()
                .map(|kind| reference_fit(kind, &rc.kind))
                .fold(0.0, f64::max);
            *occurrences
                .entry((src, rc.name.as_str(), *dst))
                .or_default() += rc.count as f64 * fit;
        }
    }
    for (&(src, name, dst), &count) in &occurrences {
        let definitions = definers[name].len();
        let mut weight = name_weight(name, definitions) * count.sqrt() / definitions as f64;
        if focus_names.contains(name) {
            weight *= FOCUS_NAME_BOOST;
        }
        if weight > 0.0 {
            *edge_weights.entry((src, dst)).or_default() += weight;
            contributions.push((src, dst, name, weight));
        }
    }
    for (source, target) in &graph.file_edges {
        if let (Some(&src), Some(&dst)) = (node_of.get(source), node_of.get(target))
            && src != dst
        {
            *edge_weights.entry((src, dst)).or_default() += IMPORT_EDGE_WEIGHT;
        }
    }
    let edges: Vec<(usize, usize, f64)> = edge_weights
        .iter()
        .map(|(&(src, dst), &w)| (src, dst, w))
        .collect();

    // Teleport vector: uniform, or focused files plus their neighbours.
    let teleport = if is_focus.is_empty() {
        normalized(vec![1.0; n])
    } else {
        let mut neighbours = vec![0.0; n];
        for &(src, dst, w) in &edges {
            if is_focus.contains(&src) && !is_focus.contains(&dst) {
                neighbours[dst] += w;
            } else if is_focus.contains(&dst) && !is_focus.contains(&src) {
                neighbours[src] += w;
            }
        }
        let neighbour_total: f64 = neighbours.iter().sum();
        let neighbour_scale = if neighbour_total > 0.0 {
            NEIGHBOUR_MASS * is_focus.len() as f64 / neighbour_total
        } else {
            0.0
        };
        let weights = (0..n)
            .map(|i| {
                if is_focus.contains(&i) {
                    1.0
                } else {
                    neighbours[i] * neighbour_scale
                }
            })
            .collect();
        normalized(weights)
    };

    let rank = pagerank(n, &edges, &teleport);

    let out_weight = out_weights(n, &edges);
    let mut symbol_rank: HashMap<(usize, &str), f64> = HashMap::new();
    for &(src, dst, name, w) in &contributions {
        *symbol_rank.entry((dst, name)).or_default() += rank[src] * w / out_weight[src];
    }

    let mut candidates: Vec<Candidate> = symbols
        .iter()
        .map(|s| {
            let file = node_of[&s.file_id];
            let focused = is_focus.contains(&file);
            let share = if focused {
                FOCUS_FILE_RANK_SHARE
            } else {
                FILE_RANK_SHARE
            };
            let mut score = symbol_rank
                .get(&(file, s.name.as_str()))
                .copied()
                .unwrap_or(0.0)
                + share * rank[file] * kind_weight(&s.kind);
            if !focused
                && (is_test_file[file] || is_test_location(&files[file].path, Some(&s.name)))
            {
                score *= TEST_PENALTY;
            }
            Candidate {
                file,
                name: s.name.clone(),
                kind: s.kind.clone(),
                line: s.start_line.max(0) as usize,
                parent: s.parent.clone(),
                signature: clean_signature(s.signature.as_deref(), &s.kind, &s.name),
                doc: clean_doc(s.doc_comment.as_deref()),
                score,
            }
        })
        .collect();
    // Keep one entry per name and scope (cfg variants, overloads).
    let mut seen: HashSet<(usize, Option<String>, String, String)> = HashSet::new();
    candidates.retain(|c| seen.insert((c.file, c.parent.clone(), c.name.clone(), c.kind.clone())));
    candidates.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line.cmp(&b.line))
            .then_with(|| a.name.cmp(&b.name))
    });

    RankedRepo {
        files: files
            .into_iter()
            .zip(rank)
            .map(|(f, rank)| RankedFile {
                path: f.path,
                language: f.language,
                rank,
            })
            .collect(),
        candidates,
        focus_files,
        focus_symbols,
        unmatched_focus,
    }
}

/// `(directory, file name)`, with `.` for files at the root.
fn split_path(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or((".", path))
}

fn round_rank(value: f64) -> f64 {
    (value * 1e6).round() / 1e6
}

/// Render the `k` best candidates. Returns the text and the listed files.
fn render_selection(
    ranked: &RankedRepo,
    eligible: &[&Candidate],
    k: usize,
    total_files: usize,
    header_extra: &str,
) -> (String, Vec<RepoMapFile>) {
    let mut selected: Vec<&Candidate> = eligible[..k].to_vec();
    selected.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then_with(|| a.line.cmp(&b.line))
            .then_with(|| a.name.cmp(&b.name))
    });

    let mut files: Vec<RepoMapFile> = Vec::new();
    for c in &selected {
        if files.last().map(|f| &f.path) != Some(&ranked.files[c.file].path) {
            let file = &ranked.files[c.file];
            files.push(RepoMapFile {
                path: file.path.clone(),
                language: file.language.clone(),
                rank: round_rank(file.rank),
                symbols: Vec::new(),
            });
        }
        if let Some(file) = files.last_mut() {
            file.symbols.push(RepoMapSymbol {
                name: c.name.clone(),
                kind: c.kind.clone(),
                line: c.line,
                parent: c.parent.clone(),
                signature: c.signature.clone(),
                doc: c.doc.clone(),
                rank: round_rank(c.score),
            });
        }
    }
    // Directory order, so that each directory is listed once.
    files.sort_by(|a, b| split_path(&a.path).cmp(&split_path(&b.path)));

    let mut text = format!(
        "Repo map: {} of {} files, {} of {} symbols, the most used ones{}\n",
        files.len(),
        total_files,
        k,
        eligible.len(),
        header_extra
    );
    let mut current_dir: Option<&str> = None;
    for file in &files {
        let (dir, name) = split_path(&file.path);
        if current_dir != Some(dir) {
            text.push_str(&format!("{}/\n", dir));
            current_dir = Some(dir);
        }
        text.push_str(&format!("  {}\n", name));
        // Name of the last top-level symbol listed, to nest its members.
        let mut context: Option<&str> = None;
        for symbol in &file.symbols {
            let indent = match symbol.parent.as_deref() {
                Some(parent) => {
                    if context != Some(parent) {
                        text.push_str(&format!("    ⋮ {}\n", parent));
                        context = Some(parent);
                    }
                    "      "
                }
                None => {
                    context = Some(symbol.name.as_str());
                    "    "
                }
            };
            text.push_str(indent);
            text.push_str(&symbol.signature);
            if let Some(doc) = &symbol.doc {
                text.push_str("  — ");
                text.push_str(doc);
            }
            text.push('\n');
        }
    }
    (text, files)
}

fn render_repo_map(ranked: &RankedRepo, options: &RepoMapOptions, start: Instant) -> RepoMap {
    let max_tokens = options
        .max_tokens
        .clamp(MIN_REPO_MAP_TOKENS, MAX_REPO_MAP_TOKENS);
    let prefix = options
        .path_prefix
        .as_deref()
        .map(normalize_path)
        .filter(|p| !p.is_empty());
    let in_scope = |path: &str| prefix.as_deref().is_none_or(|p| path.starts_with(p));

    let total_files = ranked.files.iter().filter(|f| in_scope(&f.path)).count();
    let eligible: Vec<&Candidate> = ranked
        .candidates
        .iter()
        .filter(|c| in_scope(&ranked.files[c.file].path))
        .collect();

    let mut header_extra = String::new();
    if !ranked.focus_files.is_empty() || !ranked.focus_symbols.is_empty() {
        let focus: Vec<&str> = ranked
            .focus_files
            .iter()
            .chain(&ranked.focus_symbols)
            .map(String::as_str)
            .collect();
        header_extra.push_str(&format!(" (focus: {})", focus.join(", ")));
    }
    if let Some(prefix) = &prefix {
        header_extra.push_str(&format!(" (under {})", prefix));
    }

    let (text, files, shown) = if total_files == 0 {
        let text = if ranked.files.is_empty() {
            "Repo map: the index has no source files yet.\n".to_string()
        } else {
            format!("Repo map: no indexed source file{}.\n", header_extra)
        };
        (text, Vec::new(), 0)
    } else {
        // Largest k whose rendering fits: the size grows with k.
        let fits = |k: usize| render_selection(ranked, &eligible, k, total_files, &header_extra);
        let (mut lo, mut hi) = (0, eligible.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if estimate_tokens(&fits(mid).0) <= max_tokens {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        let (text, files) = fits(lo);
        (text, files, lo)
    };

    RepoMap {
        estimated_tokens: estimate_tokens(&text),
        files,
        total_files,
        total_symbols: eligible.len(),
        shown_symbols: shown,
        max_tokens,
        focus_files: ranked.focus_files.clone(),
        focus_symbols: ranked.focus_symbols.clone(),
        unmatched_focus: ranked.unmatched_focus.clone(),
        build_time_ms: start.elapsed().as_millis() as u64,
        text,
    }
}

#[cfg(test)]
mod tests;
