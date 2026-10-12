//! Repository map: a compact, ranked overview of the codebase under a token
//! budget, in the spirit of Aider's repo map.
//!
//! Files are the nodes of a graph whose edges come from the index:
//! - a reference (`refs`, non-definition) to a name defined in another file
//!   links the referencing file to the defining file, weighted by
//!   `sqrt(occurrences)`, damped for short/private/widely-defined names and
//!   split between homonym definitions (names defined in more than
//!   `MAX_NAME_DEFINERS` files are left out by the store: every referencing
//!   file x every defining file is too many edges for a damped weight);
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
//! The work is split in three stages so that a large index (a million
//! symbols) is loaded once, not on every call:
//! 1. [`RepoGraph`]: the graph itself, independent of the focus: nodes,
//!    the files defining each name and the weight each reference
//!    contributes to an edge. Names and kinds are interned, and of each
//!    symbol's signature and doc only the rendered size is kept. The engine
//!    caches it until the index changes (see [`IndexStore::graph_fingerprint`]).
//! 2. [`RankedRepo`]: PageRank with the focus applied, and the symbols
//!    scored: milliseconds on the cached graph.
//! 3. The render picks how many symbols fit the budget from their sizes,
//!    then fetches the signatures and docs of those few by id.
//!
//! Everything is computed from the index, without any LLM call, and every
//! iteration order is fixed so the same index always yields the same map.

use crate::engine::{RetrievalEngine, is_test_location};
use anyhow::Result;
use semantiq_index::{IndexStore, RepoGraphData};
use std::collections::{HashMap, HashSet};
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
/// The size estimate counts one `⋮ parent` line per (file, parent) pair,
/// which the rendering may need more or fewer of: the render then moves the
/// cut symbol by symbol until the budget is filled, fetching the texts of
/// the symbols it tries this many at a time.
const TEXT_FETCH_BATCH: usize = 32;
/// Smallest entry a symbol can take (indent, one-character signature,
/// newline): bounds how many symbols a map can hold.
const MIN_SYMBOL_LINE_CHARS: usize = 6;

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
    chars_to_tokens(text.chars().count())
}

fn chars_to_tokens(chars: usize) -> usize {
    chars.div_ceil(4)
}

/// Build a repo map straight from the store (no engine, no embedding model).
pub fn build_repo_map(store: &IndexStore, options: &RepoMapOptions) -> Result<RepoMap> {
    let start = Instant::now();
    let graph = Arc::new(load_graph(store)?);
    let ranked = rank_repo(&graph, &options.focus);
    render_repo_map(store, &ranked, options, start)
}

/// The engine's cache: the graph of the index at `fingerprint` and, once
/// built, its unfocused ranking.
pub(crate) struct RepoMapCache {
    fingerprint: String,
    graph: Arc<RepoGraph>,
    unfocused: Option<Arc<RankedRepo>>,
}

impl RetrievalEngine {
    /// Build a repo map. The loaded graph, and the ranking without focus,
    /// are cached until the index changes (see [`IndexStore::graph_fingerprint`]).
    pub fn repo_map(&self, options: &RepoMapOptions) -> Result<RepoMap> {
        let start = Instant::now();
        let (graph, unfocused) = self.cached_graph()?;
        let ranked = if options.focus.is_empty() {
            match unfocused {
                Some(ranked) => ranked,
                None => {
                    let ranked = Arc::new(rank_repo(&graph, &[]));
                    if let Ok(mut cache) = self.repo_map_cache.lock()
                        && let Some(cache) = cache.as_mut()
                        && Arc::ptr_eq(&cache.graph, &graph)
                    {
                        cache.unfocused = Some(Arc::clone(&ranked));
                    }
                    ranked
                }
            }
        } else {
            let ranking = Instant::now();
            let ranked = Arc::new(rank_repo(&graph, &options.focus));
            debug!(
                rank_ms = ranking.elapsed().as_millis(),
                "repo map ranked with focus"
            );
            ranked
        };
        render_repo_map(&self.store, &ranked, options, start)
    }

    /// The graph of the current index, from the cache when it matches, and
    /// the cached unfocused ranking if there is one.
    fn cached_graph(&self) -> Result<(Arc<RepoGraph>, Option<Arc<RankedRepo>>)> {
        let fingerprint = self.store.graph_fingerprint()?;
        if let Ok(cache) = self.repo_map_cache.lock()
            && let Some(cache) = cache.as_ref()
            && cache.fingerprint == fingerprint
        {
            debug!("repo map graph served from cache");
            return Ok((Arc::clone(&cache.graph), cache.unfocused.clone()));
        }

        let graph = Arc::new(load_graph(&self.store)?);
        // Only cache a snapshot no write interleaved with.
        if self.store.graph_fingerprint()? == fingerprint
            && let Ok(mut cache) = self.repo_map_cache.lock()
        {
            *cache = Some(RepoMapCache {
                fingerprint,
                graph: Arc::clone(&graph),
                unfocused: None,
            });
        }
        Ok((graph, None))
    }

    #[cfg(test)]
    pub(crate) fn cached_repo_graph(&self) -> Option<(Arc<RepoGraph>, bool)> {
        let cache = self.repo_map_cache.lock().ok()?;
        cache
            .as_ref()
            .map(|c| (Arc::clone(&c.graph), c.unfocused.is_some()))
    }
}

/// Rendered size of a symbol's entry: characters of its cleaned signature
/// and doc line (0: no doc).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct TextSize {
    signature: u16,
    doc: u16,
}

fn text_size(name: &str, kind: &str, signature: Option<&str>, doc: Option<&str>) -> TextSize {
    let signature = clean_signature(signature, kind, name).chars().count();
    let doc = clean_doc(doc).map_or(0, |d| d.chars().count());
    TextSize {
        signature: signature.min(u16::MAX as usize) as u16,
        doc: doc.min(u16::MAX as usize) as u16,
    }
}

fn load_graph(store: &IndexStore) -> Result<RepoGraph> {
    let start = Instant::now();
    let data = store.load_repo_graph(text_size)?;
    let loaded = start.elapsed();
    let graph = build_graph(data);
    debug!(
        load_ms = loaded.as_millis(),
        build_ms = (start.elapsed() - loaded).as_millis(),
        files = graph.files.len(),
        symbols = graph.symbols.len(),
        edges = graph.edges.len(),
        contributions = graph.contributions.len(),
        "repo map graph loaded"
    );
    Ok(graph)
}

/// The code graph, independent of the focus (see the module docs).
#[derive(Debug)]
pub(crate) struct RepoGraph {
    names: Vec<String>,
    kinds: Vec<String>,
    /// Code files, in path order: the nodes.
    files: Vec<GraphNode>,
    /// Symbols eligible for the map, one per (file, parent, name, kind), in
    /// file then line order.
    symbols: Vec<MapSymbol>,
    /// By name: the nodes defining it, each with the mask of kinds defined.
    definers: Vec<Vec<(u32, u64)>>,
    /// `(source, target, weight)` without any focus boost, sorted: the
    /// references (`contributions` summed per edge) and the imports.
    edges: Vec<(usize, usize, f64)>,
    /// What the references to a name in `src` contribute to the edge towards
    /// a node defining it, before any focus boost; grouped by `groups`.
    contributions: Vec<Contribution>,
    /// One group per (name, defining node), sorted by name: the range of
    /// `contributions` that hand file rank to that definition.
    groups: Vec<Group>,
}

#[derive(Debug)]
struct GraphNode {
    path: String,
    language: Option<String>,
    is_test: bool,
}

#[derive(Debug)]
struct MapSymbol {
    id: i64,
    /// Index into `RepoGraph::files`.
    file: u32,
    name: u32,
    kind: u32,
    line: u32,
    parent: Option<u32>,
    size: TextSize,
    /// The group of contributions ranking this (file, name), if any.
    group: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
struct Contribution {
    src: u32,
    weight: f64,
}

#[derive(Debug, Clone, Copy)]
struct Group {
    name: u32,
    dst: u32,
    start: u32,
    end: u32,
}

/// Ranked files and symbols, independent of the budget and path prefix.
#[derive(Debug)]
pub(crate) struct RankedRepo {
    graph: Arc<RepoGraph>,
    /// PageRank of each node.
    file_rank: Vec<f64>,
    /// Score of each symbol of the graph (the render sorts what it needs).
    scores: Vec<f64>,
    focus_files: Vec<String>,
    focus_symbols: Vec<String>,
    unmatched_focus: Vec<String>,
}

fn normalize_path(value: &str) -> String {
    crate::engine::normalize_input_path(value)
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

/// Bit of a kind in a definer's mask (kinds are few; a 65th shares a bit).
fn kind_bit(kind: u32) -> u64 {
    1u64 << kind.min(63)
}

/// Merge two edge lists sorted by (source, target), summing shared edges
/// (`a`'s weight first).
fn merge_edges(
    a: Vec<(usize, usize, f64)>,
    b: Vec<(usize, usize, f64)>,
) -> Vec<(usize, usize, f64)> {
    let mut merged = Vec::with_capacity(a.len() + b.len());
    let (mut a, mut b) = (a.into_iter().peekable(), b.into_iter().peekable());
    loop {
        match (a.peek(), b.peek()) {
            (Some(x), Some(y)) => match (x.0, x.1).cmp(&(y.0, y.1)) {
                std::cmp::Ordering::Less => merged.push(a.next().unwrap()),
                std::cmp::Ordering::Greater => merged.push(b.next().unwrap()),
                std::cmp::Ordering::Equal => {
                    let (x, y) = (a.next().unwrap(), b.next().unwrap());
                    merged.push((x.0, x.1, x.2 + y.2));
                }
            },
            (Some(_), None) => merged.push(a.next().unwrap()),
            (None, Some(_)) => merged.push(b.next().unwrap()),
            (None, None) => break,
        }
    }
    merged
}

/// Reference kinds, as the store counts them (alphabetical: the order the
/// occurrences of a name are summed in).
const REFERENCE_KINDS: [&str; 4] = ["call", "import", "reference", "type"];

fn reference_counts(rc: &semantiq_index::GraphRefCount) -> [u32; 4] {
    [rc.calls, rc.imports, rc.references, rc.types]
}

/// Stage 1: the focus-independent graph.
fn build_graph(data: RepoGraphData<TextSize>) -> RepoGraph {
    let RepoGraphData {
        names,
        kinds,
        files,
        symbols,
        file_edges,
        ref_counts,
    } = data;

    // Nodes: code files, in path order (the store sorts them).
    let mut node_of: HashMap<i64, u32> = HashMap::new();
    let mut nodes = Vec::new();
    for file in files {
        if !is_code_file(file.language.as_deref()) {
            continue;
        }
        node_of.insert(file.id, nodes.len() as u32);
        nodes.push(GraphNode {
            is_test: is_test_location(&file.path, None),
            path: file.path,
            language: file.language,
        });
    }

    let skipped_kind: Vec<bool> = kinds
        .iter()
        .map(|k| SKIPPED_KINDS.contains(&k.as_str()))
        .collect();
    let type_kind = kinds.iter().position(|k| k == "type").map(|k| k as u32);

    // Associated types (`type Error = …` in an impl) are trait plumbing.
    // One entry per name and scope (cfg variants, overloads): the first.
    let mut definers: Vec<Vec<(u32, u64)>> = vec![Vec::new(); names.len()];
    let mut seen: HashSet<(u32, Option<u32>, u32, u32)> = HashSet::new();
    let mut map_symbols = Vec::new();
    for s in symbols {
        let Some(&file) = node_of.get(&s.file_id) else {
            continue;
        };
        if skipped_kind[s.kind as usize] || (Some(s.kind) == type_kind && s.parent.is_some()) {
            continue;
        }
        let list = &mut definers[s.name as usize];
        match list.iter_mut().find(|(node, _)| *node == file) {
            Some((_, mask)) => *mask |= kind_bit(s.kind),
            None => list.push((file, kind_bit(s.kind))),
        }
        if seen.insert((file, s.parent, s.name, s.kind)) {
            map_symbols.push(MapSymbol {
                id: s.id,
                file,
                name: s.name,
                kind: s.kind,
                line: s.start_line,
                parent: s.parent,
                size: s.details,
                group: None,
            });
        }
    }

    // Reference-kind fit for each (definition kind, reference kind).
    let fit: Vec<[f64; 4]> = kinds
        .iter()
        .map(|def_kind| REFERENCE_KINDS.map(|ref_kind| reference_fit(def_kind, ref_kind)))
        .collect();

    // One term per (source, name, target): the occurrences of `name` in
    // `source`, discounted when the reference kind does not fit what the
    // target defines, turned into an edge weight. Summed per edge in
    // (source, target, name) order and grouped per (name, target) in source
    // order, so the same index always yields the same sums.
    let mut terms: Vec<(u32, u32, u32, f64)> = Vec::new();
    for rc in &ref_counts {
        let Some(&src) = node_of.get(&rc.file_id) else {
            continue;
        };
        let defs = &definers[rc.name as usize];
        // A name the file defines itself most likely refers to that definition.
        if defs.is_empty() || defs.iter().any(|(node, _)| *node == src) {
            continue;
        }
        let counts = reference_counts(rc);
        let definitions = defs.len();
        let name_weight = name_weight(&names[rc.name as usize], definitions);
        for &(dst, mask) in defs {
            // Production code never uses test helpers: such a match is a homonym.
            if nodes[dst as usize].is_test && !nodes[src as usize].is_test {
                continue;
            }
            let mut count = 0.0;
            for (ref_kind, &n) in counts.iter().enumerate() {
                if n == 0 {
                    continue;
                }
                let best_fit = (0..kinds.len() as u32)
                    .filter(|k| mask & kind_bit(*k) != 0)
                    .map(|k| fit[k as usize][ref_kind])
                    .fold(0.0, f64::max);
                count += n as f64 * best_fit;
            }
            let weight = name_weight * count.sqrt() / definitions as f64;
            if weight > 0.0 {
                terms.push((src, dst, rc.name, weight));
            }
        }
    }
    drop(ref_counts);

    // Edges: the terms summed per (source, target), plus the imports.
    terms.sort_unstable_by_key(|t| (t.0, t.1, t.2));
    let mut referenced: Vec<(usize, usize, f64)> = Vec::new();
    for &(src, dst, _, weight) in &terms {
        match referenced.last_mut() {
            Some(edge) if edge.0 == src as usize && edge.1 == dst as usize => edge.2 += weight,
            _ => referenced.push((src as usize, dst as usize, weight)),
        }
    }
    let mut imported: Vec<(usize, usize, f64)> = file_edges
        .iter()
        .filter_map(|(source, target)| {
            let (src, dst) = (*node_of.get(source)?, *node_of.get(target)?);
            (src != dst).then_some((src as usize, dst as usize, IMPORT_EDGE_WEIGHT))
        })
        .collect();
    imported.sort_unstable_by_key(|e| (e.0, e.1));
    imported.dedup_by(|a, b| (a.0, a.1) == (b.0, b.1));
    let edges = merge_edges(referenced, imported);

    // Contributions grouped by (name, target), sources in order.
    terms.sort_unstable_by_key(|t| (t.2, t.1, t.0));
    let mut contributions = Vec::with_capacity(terms.len());
    let mut groups: Vec<Group> = Vec::new();
    for &(src, dst, name, weight) in &terms {
        match groups.last_mut() {
            Some(group) if group.name == name && group.dst == dst => group.end += 1,
            _ => groups.push(Group {
                name,
                dst,
                start: contributions.len() as u32,
                end: contributions.len() as u32 + 1,
            }),
        }
        contributions.push(Contribution { src, weight });
    }
    drop(terms);
    for symbol in &mut map_symbols {
        symbol.group = groups
            .binary_search_by(|g| (g.name, g.dst).cmp(&(symbol.name, symbol.file)))
            .ok()
            .map(|g| g as u32);
    }

    RepoGraph {
        names,
        kinds,
        files: nodes,
        symbols: map_symbols,
        definers,
        edges,
        contributions,
        groups,
    }
}

/// Stage 2: PageRank with the focus applied, and the symbols scored.
pub(crate) fn rank_repo(graph: &Arc<RepoGraph>, focus: &[String]) -> RankedRepo {
    let files = &graph.files;
    let n = files.len();

    // Resolve focus entries: path, directory, path suffix, then symbol name.
    let mut focus_nodes: Vec<u32> = Vec::new();
    let mut focus_files = Vec::new();
    let mut focus_symbols = Vec::new();
    let mut focus_names: Vec<u32> = Vec::new();
    let mut unmatched_focus = Vec::new();
    for entry in focus {
        let entry = normalize_path(entry);
        if entry.is_empty() {
            continue;
        }
        let dir = format!("{}/", entry.trim_end_matches('/'));
        let suffix = format!("/{}", entry);
        let matching = |pred: &dyn Fn(&str) -> bool| -> Vec<u32> {
            (0..n as u32)
                .filter(|&i| pred(&files[i as usize].path))
                .collect()
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
                    focus_files.push(files[node as usize].path.clone());
                }
            }
            continue;
        }
        let name = graph
            .names
            .iter()
            .position(|name| *name == entry)
            .filter(|&name| !graph.definers[name].is_empty());
        if let Some(name) = name {
            focus_symbols.push(entry.clone());
            if !focus_names.contains(&(name as u32)) {
                focus_names.push(name as u32);
            }
            for &(node, _) in &graph.definers[name] {
                if !focus_nodes.contains(&node) {
                    focus_nodes.push(node);
                }
            }
        } else {
            unmatched_focus.push(entry);
        }
    }
    let is_focus: HashSet<u32> = focus_nodes.iter().copied().collect();

    // Groups of the focused names (sorted by name), whose weight is boosted.
    let mut boosted_groups: Vec<u32> = Vec::new();
    for &name in &focus_names {
        let first = graph.groups.partition_point(|g| g.name < name);
        let mut i = first;
        while i < graph.groups.len() && graph.groups[i].name == name {
            boosted_groups.push(i as u32);
            i += 1;
        }
    }
    boosted_groups.sort_unstable();
    let boost_of = |group: u32| -> f64 {
        if boosted_groups.binary_search(&group).is_ok() {
            FOCUS_NAME_BOOST
        } else {
            1.0
        }
    };

    // Edges: the precomputed ones, plus the extra weight of boosted groups.
    let mut edges = graph.edges.clone();
    for &g in &boosted_groups {
        let group = graph.groups[g as usize];
        for c in &graph.contributions[group.start as usize..group.end as usize] {
            let key = (c.src as usize, group.dst as usize);
            if let Ok(i) = edges.binary_search_by(|e| (e.0, e.1).cmp(&key)) {
                edges[i].2 += c.weight * (FOCUS_NAME_BOOST - 1.0);
            }
        }
    }

    // Teleport vector: uniform, or focused files plus their neighbours.
    let teleport = if is_focus.is_empty() {
        normalized(vec![1.0; n])
    } else {
        let mut neighbours = vec![0.0; n];
        for &(src, dst, w) in &edges {
            let (src_focus, dst_focus) = (
                is_focus.contains(&(src as u32)),
                is_focus.contains(&(dst as u32)),
            );
            if src_focus && !dst_focus {
                neighbours[dst] += w;
            } else if dst_focus && !src_focus {
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
                if is_focus.contains(&(i as u32)) {
                    1.0
                } else {
                    neighbours[i] * neighbour_scale
                }
            })
            .collect();
        normalized(weights)
    };

    let rank = pagerank(n, &edges, &teleport);

    // Each file hands its rank down to the names it references: the rank
    // of a (name, defining file) group.
    let out_weight = out_weights(n, &edges);
    let group_rank: Vec<f64> = graph
        .groups
        .iter()
        .enumerate()
        .map(|(g, group)| {
            let boost = boost_of(g as u32);
            graph.contributions[group.start as usize..group.end as usize]
                .iter()
                .map(|c| rank[c.src as usize] * c.weight * boost / out_weight[c.src as usize])
                .sum()
        })
        .collect();

    let scores = graph
        .symbols
        .iter()
        .map(|s| {
            let focused = is_focus.contains(&s.file);
            let share = if focused {
                FOCUS_FILE_RANK_SHARE
            } else {
                FILE_RANK_SHARE
            };
            let file = &files[s.file as usize];
            let mut score = s.group.map_or(0.0, |g| group_rank[g as usize])
                + share * rank[s.file as usize] * kind_weight(&graph.kinds[s.kind as usize]);
            if !focused
                && (file.is_test
                    || is_test_location(&file.path, Some(&graph.names[s.name as usize])))
            {
                score *= TEST_PENALTY;
            }
            score
        })
        .collect();

    RankedRepo {
        graph: Arc::clone(graph),
        file_rank: rank,
        scores,
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

fn header(
    listed_files: usize,
    total_files: usize,
    shown: usize,
    eligible: usize,
    extra: &str,
) -> String {
    format!(
        "Repo map: {} of {} files, {} of {} symbols, the most used ones{}\n",
        listed_files, total_files, shown, eligible, extra
    )
}

/// Characters of a symbol's own line: indent, signature, doc, newline.
fn symbol_line_chars(symbol: &MapSymbol) -> usize {
    let indent = if symbol.parent.is_some() { 6 } else { 4 };
    let doc = if symbol.size.doc > 0 {
        4 + symbol.size.doc as usize
    } else {
        0
    };
    indent + symbol.size.signature as usize + doc + 1
}

/// Size in characters of the rendering of the first `k` eligible symbols,
/// for every `k`, without rendering: each symbol adds its line, plus the
/// file and directory headers the first time its file / directory is
/// listed, plus a `⋮ parent` line the first time its parent is met in the
/// file (the render may need more or fewer of those: see the adjustment).
fn estimated_chars(
    ranked: &RankedRepo,
    eligible: &[(u32, f64)],
    total_files: usize,
    total_symbols: usize,
    header_extra: &str,
) -> Vec<usize> {
    let graph = &ranked.graph;
    let mut dirs: HashSet<&str> = HashSet::new();
    let mut files: HashSet<u32> = HashSet::new();
    let mut parents: HashSet<(u32, u32)> = HashSet::new();
    let mut body = 0;
    let mut sizes = Vec::with_capacity(eligible.len() + 1);
    sizes.push(
        header(0, total_files, 0, total_symbols, header_extra)
            .chars()
            .count(),
    );
    for (k, &(index, _)) in eligible.iter().enumerate() {
        let symbol = &graph.symbols[index as usize];
        if files.insert(symbol.file) {
            let (dir, name) = split_path(&graph.files[symbol.file as usize].path);
            body += name.chars().count() + 3;
            if dirs.insert(dir) {
                body += dir.chars().count() + 2;
            }
        }
        if let Some(parent) = symbol.parent
            && parents.insert((symbol.file, parent))
        {
            body += graph.names[parent as usize].chars().count() + 7;
        }
        body += symbol_line_chars(symbol);
        let head = header(files.len(), total_files, k + 1, total_symbols, header_extra);
        sizes.push(head.chars().count() + body);
    }
    sizes
}

/// Render the `k` best candidates. Returns the text and the listed files.
#[allow(clippy::too_many_arguments)]
fn render_selection(
    ranked: &RankedRepo,
    eligible: &[(u32, f64)],
    k: usize,
    total_files: usize,
    total_symbols: usize,
    header_extra: &str,
    texts: &HashMap<i64, (Option<String>, Option<String>)>,
) -> (String, Vec<RepoMapFile>) {
    let graph = &ranked.graph;
    let mut selected: Vec<(&MapSymbol, f64)> = eligible[..k]
        .iter()
        .map(|&(index, score)| (&graph.symbols[index as usize], score))
        .collect();
    selected.sort_by(|(a, _), (b, _)| {
        a.file
            .cmp(&b.file)
            .then_with(|| a.line.cmp(&b.line))
            .then_with(|| graph.names[a.name as usize].cmp(&graph.names[b.name as usize]))
    });

    let mut files: Vec<RepoMapFile> = Vec::new();
    for (symbol, score) in &selected {
        let node = &graph.files[symbol.file as usize];
        if files.last().map(|f| &f.path) != Some(&node.path) {
            files.push(RepoMapFile {
                path: node.path.clone(),
                language: node.language.clone(),
                rank: round_rank(ranked.file_rank[symbol.file as usize]),
                symbols: Vec::new(),
            });
        }
        let name = &graph.names[symbol.name as usize];
        let kind = &graph.kinds[symbol.kind as usize];
        let (signature, doc) = texts
            .get(&symbol.id)
            .map(|(s, d)| (s.as_deref(), d.as_deref()))
            .unwrap_or((None, None));
        if let Some(file) = files.last_mut() {
            file.symbols.push(RepoMapSymbol {
                name: name.clone(),
                kind: kind.clone(),
                line: symbol.line as usize,
                parent: symbol.parent.map(|p| graph.names[p as usize].clone()),
                signature: clean_signature(signature, kind, name),
                doc: clean_doc(doc),
                rank: round_rank(*score),
            });
        }
    }
    // Directory order, so that each directory is listed once.
    files.sort_by(|a, b| split_path(&a.path).cmp(&split_path(&b.path)));

    let mut text = header(files.len(), total_files, k, total_symbols, header_extra);
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

/// Stage 3: pick what fits the budget and render it.
fn render_repo_map(
    store: &IndexStore,
    ranked: &RankedRepo,
    options: &RepoMapOptions,
    start: Instant,
) -> Result<RepoMap> {
    let graph = &ranked.graph;
    let max_tokens = options
        .max_tokens
        .clamp(MIN_REPO_MAP_TOKENS, MAX_REPO_MAP_TOKENS);
    let prefix = options
        .path_prefix
        .as_deref()
        .map(normalize_path)
        .filter(|p| !p.is_empty());
    let in_scope = |path: &str| prefix.as_deref().is_none_or(|p| path.starts_with(p));

    let total_files = graph.files.iter().filter(|f| in_scope(&f.path)).count();
    let mut eligible: Vec<(u32, f64)> = ranked
        .scores
        .iter()
        .enumerate()
        .filter(|&(index, _)| in_scope(&graph.files[graph.symbols[index].file as usize].path))
        .map(|(index, &score)| (index as u32, score))
        .collect();
    // Best first; only as many as could possibly fit need sorting.
    let by_rank = |&(a, sa): &(u32, f64), &(b, sb): &(u32, f64)| {
        let (a, b) = (&graph.symbols[a as usize], &graph.symbols[b as usize]);
        sb.total_cmp(&sa)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line.cmp(&b.line))
            .then_with(|| graph.names[a.name as usize].cmp(&graph.names[b.name as usize]))
    };
    let total_symbols = eligible.len();
    let most = MAX_REPO_MAP_TOKENS * 4 / MIN_SYMBOL_LINE_CHARS + 1;
    if eligible.len() > most {
        eligible.select_nth_unstable_by(most, by_rank);
        eligible.truncate(most);
    }
    eligible.sort_by(by_rank);

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
        let text = if graph.files.is_empty() {
            "Repo map: the index has no source files yet.\n".to_string()
        } else {
            format!("Repo map: no indexed source file{}.\n", header_extra)
        };
        (text, Vec::new(), 0)
    } else {
        // Largest k whose estimated size fits: the estimate grows with k.
        let sizes = estimated_chars(ranked, &eligible, total_files, total_symbols, &header_extra);
        let (mut lo, mut hi) = (0, eligible.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if chars_to_tokens(sizes[mid]) <= max_tokens {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        // The estimate is approximate around the `⋮ parent` lines (it
        // charges one per pair, which the rendering skips when the parent
        // was just listed): check the real rendering and move the cut down
        // until it fits, then up as long as the next symbol fits.
        let mut texts: HashMap<i64, (Option<String>, Option<String>)> = HashMap::new();
        let mut fetched = 0;
        let mut fetch_texts = |upto: usize, texts: &mut HashMap<_, _>| -> Result<()> {
            let upto = upto.min(eligible.len());
            if upto > fetched {
                let ids: Vec<i64> = eligible[fetched..upto]
                    .iter()
                    .map(|&(index, _)| graph.symbols[index as usize].id)
                    .collect();
                texts.extend(store.symbol_texts(&ids)?);
                fetched = upto;
            }
            Ok(())
        };
        fetch_texts(lo + TEXT_FETCH_BATCH, &mut texts)?;
        let render = |k: usize, texts: &HashMap<_, _>| {
            render_selection(
                ranked,
                &eligible,
                k,
                total_files,
                total_symbols,
                &header_extra,
                texts,
            )
        };
        let mut k = lo;
        let mut rendered = render(k, &texts);
        while k > 0 && estimate_tokens(&rendered.0) > max_tokens {
            k -= 1;
            rendered = render(k, &texts);
        }
        while k < eligible.len() {
            fetch_texts(k + 1 + TEXT_FETCH_BATCH, &mut texts)?;
            let candidate = render(k + 1, &texts);
            if estimate_tokens(&candidate.0) > max_tokens {
                break;
            }
            k += 1;
            rendered = candidate;
        }
        let (text, files) = rendered;
        (text, files, k)
    };

    Ok(RepoMap {
        estimated_tokens: estimate_tokens(&text),
        files,
        total_files,
        total_symbols,
        shown_symbols: shown,
        max_tokens,
        focus_files: ranked.focus_files.clone(),
        focus_symbols: ranked.focus_symbols.clone(),
        unmatched_focus: ranked.unmatched_focus.clone(),
        build_time_ms: start.elapsed().as_millis() as u64,
        text,
    })
}

#[cfg(test)]
mod tests;
