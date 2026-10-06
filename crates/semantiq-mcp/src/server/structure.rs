//! Structural tools: call graph, type hierarchy and dead code.

use super::{
    CallEdgeOut, CallsOutput, CallsParams, DeadCodeExcludedOut, DeadCodeOutput, DeadCodeParams,
    DeadSymbolOut, HierarchyOutput, HierarchyParams, ImpactDefinitionOut, SemantiqServer,
    TypeRelationOut, validate_input,
};
use semantiq_retrieval::{
    CallDirection, CallSite, DEFAULT_CALL_DEPTH, DEFAULT_CALL_EDGES, DEFAULT_DEAD_CODE_LIMIT,
    DEFAULT_HIERARCHY_DEPTH, DEFAULT_HIERARCHY_EDGES, DeadCodeOptions, ImpactDefinition, TypeEdge,
};
use tracing::{debug, error};

/// Validate an optional relative path argument (no traversal).
fn validate_path(value: Option<&str>, label: &str) -> Result<Option<String>, String> {
    match value {
        Some(path) => {
            let path = validate_input(path, label)?;
            if path.contains("..") {
                return Err(format!("{} must not contain '..'", label));
            }
            Ok(Some(path))
        }
        None => Ok(None),
    }
}

fn definitions_out(definitions: Vec<ImpactDefinition>) -> Vec<ImpactDefinitionOut> {
    definitions
        .into_iter()
        .map(|d| ImpactDefinitionOut {
            file_path: d.file_path,
            line: d.line,
            kind: d.kind,
        })
        .collect()
}

fn call_edge_out(site: CallSite) -> CallEdgeOut {
    CallEdgeOut {
        depth: site.depth,
        caller: site.caller,
        callee: site.callee,
        file_path: site.file_path,
        line: site.line,
        confidence: site.confidence.as_str().to_string(),
    }
}

fn type_relation_out(edge: TypeEdge) -> TypeRelationOut {
    TypeRelationOut {
        depth: edge.depth,
        sub_type: edge.sub_type,
        super_type: edge.super_type,
        kind: edge.kind,
        file_path: edge.file_path,
        line: edge.line,
        resolved: edge.resolved,
    }
}

impl SemantiqServer {
    pub async fn calls(&self, params: CallsParams) -> Result<CallsOutput, String> {
        debug!(symbol = %params.symbol, direction = ?params.direction, "semantiq_calls called");

        let symbol = validate_input(&params.symbol, "Symbol name")?;
        let file_path = validate_path(params.file_path.as_deref(), "File path")?;
        let direction = match params.direction.as_deref() {
            None => CallDirection::Both,
            Some(value) => CallDirection::parse(value)
                .ok_or_else(|| "direction must be callers, callees or both".to_string())?,
        };
        let max_depth = params.max_depth.unwrap_or(DEFAULT_CALL_DEPTH);
        let limit = params.limit.unwrap_or(DEFAULT_CALL_EDGES);

        let owned_symbol = symbol.clone();
        let graph = self
            .run_blocking(move |engine| {
                engine.call_graph(
                    &owned_symbol,
                    file_path.as_deref(),
                    direction,
                    max_depth,
                    limit,
                )
            })
            .await
            .map_err(|e| {
                error!("Call graph failed: {}", e);
                "Call graph failed: an internal error occurred".to_string()
            })?;

        Ok(CallsOutput {
            symbol: graph.symbol,
            direction: match direction {
                CallDirection::Callers => "callers",
                CallDirection::Callees => "callees",
                CallDirection::Both => "both",
            }
            .to_string(),
            definitions: definitions_out(graph.definitions),
            callers: graph.callers.into_iter().map(call_edge_out).collect(),
            callees: graph.callees.into_iter().map(call_edge_out).collect(),
            external_callees: graph.external_callees,
            truncated: graph.truncated,
        })
    }

    pub async fn hierarchy(&self, params: HierarchyParams) -> Result<HierarchyOutput, String> {
        debug!(symbol = %params.symbol, "semantiq_hierarchy called");

        let symbol = validate_input(&params.symbol, "Symbol name")?;
        let max_depth = params.max_depth.unwrap_or(DEFAULT_HIERARCHY_DEPTH);
        let limit = params.limit.unwrap_or(DEFAULT_HIERARCHY_EDGES);

        let owned_symbol = symbol.clone();
        let hierarchy = self
            .run_blocking(move |engine| engine.type_hierarchy(&owned_symbol, max_depth, limit))
            .await
            .map_err(|e| {
                error!("Type hierarchy failed: {}", e);
                "Type hierarchy failed: an internal error occurred".to_string()
            })?;

        Ok(HierarchyOutput {
            symbol: hierarchy.symbol,
            definitions: definitions_out(hierarchy.definitions),
            supertypes: hierarchy
                .supertypes
                .into_iter()
                .map(type_relation_out)
                .collect(),
            subtypes: hierarchy
                .subtypes
                .into_iter()
                .map(type_relation_out)
                .collect(),
            truncated: hierarchy.truncated,
        })
    }

    pub async fn dead_code(&self, params: DeadCodeParams) -> Result<DeadCodeOutput, String> {
        debug!(prefix = ?params.path_prefix, language = ?params.language, "semantiq_dead_code called");

        let path_prefix = validate_path(params.path_prefix.as_deref(), "Path prefix")?;
        let language = match params.language.as_deref() {
            Some(language) => Some(validate_input(language, "Language")?.to_ascii_lowercase()),
            None => None,
        };
        let options = DeadCodeOptions {
            path_prefix,
            language,
            include_public: params.include_public.unwrap_or(false),
            limit: params.limit.unwrap_or(DEFAULT_DEAD_CODE_LIMIT),
        };

        let report = self
            .run_blocking(move |engine| engine.find_dead_code(&options))
            .await
            .map_err(|e| {
                error!("Dead code analysis failed: {}", e);
                "Dead code analysis failed: an internal error occurred".to_string()
            })?;

        Ok(DeadCodeOutput {
            symbols: report
                .symbols
                .into_iter()
                .map(|s| DeadSymbolOut {
                    name: s.name,
                    kind: s.kind,
                    file_path: s.file_path,
                    start_line: s.start_line,
                    end_line: s.end_line,
                    signature: s.signature,
                    confidence: s.confidence.as_str().to_string(),
                    reasons: s.reasons,
                })
                .collect(),
            candidates: report.candidates,
            excluded: DeadCodeExcludedOut {
                entry_points: report.excluded.entry_points,
                tests: report.excluded.tests,
                public: report.excluded.public,
                trait_members: report.excluded.trait_members,
            },
            truncated: report.truncated,
        })
    }
}
