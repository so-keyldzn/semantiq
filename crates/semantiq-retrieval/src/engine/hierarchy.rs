//! Type hierarchy: what a type extends / implements, and what extends or
//! implements it, followed transitively up to `max_depth`.
//!
//! Built from the `type_relations` table. Names are matched without
//! qualification, so two unrelated types sharing a name share their
//! relations. Go is not covered: its interfaces are satisfied implicitly.

use super::RetrievalEngine;
use super::impact::ImpactDefinition;
use super::resolution::Resolver;
use anyhow::Result;
use std::collections::{HashSet, VecDeque};
use tracing::info;

pub const DEFAULT_HIERARCHY_DEPTH: usize = 3;
pub const MAX_HIERARCHY_DEPTH: usize = 5;
pub const DEFAULT_HIERARCHY_EDGES: usize = 200;
pub const MAX_HIERARCHY_EDGES: usize = 1000;

/// Kinds that can take part in a type hierarchy.
fn is_type_kind(kind: &str) -> bool {
    matches!(
        kind,
        "class" | "struct" | "enum" | "interface" | "trait" | "type"
    )
}

/// `sub_type` extends / implements `super_type`, declared at `file_path:line`.
#[derive(Debug, Clone)]
pub struct TypeEdge {
    /// 1 = direct relation of the analysed type, 2 = one hop further…
    pub depth: usize,
    pub sub_type: String,
    pub super_type: String,
    /// extends or implements
    pub kind: String,
    pub file_path: String,
    pub line: usize,
    /// The other end of the edge (the supertype when walking up, the subtype
    /// when walking down) is defined in the index; false for library types.
    pub resolved: bool,
}

#[derive(Debug, Clone)]
pub struct TypeHierarchy {
    pub symbol: String,
    pub definitions: Vec<ImpactDefinition>,
    pub supertypes: Vec<TypeEdge>,
    pub subtypes: Vec<TypeEdge>,
    pub truncated: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Walk {
    Up,
    Down,
}

impl RetrievalEngine {
    /// Supertypes and subtypes / implementors of `type_name`.
    pub fn type_hierarchy(
        &self,
        type_name: &str,
        max_depth: usize,
        max_edges: usize,
    ) -> Result<TypeHierarchy> {
        info!(symbol = %type_name, max_depth, max_edges, "Building type hierarchy");
        let max_depth = max_depth.clamp(1, MAX_HIERARCHY_DEPTH);
        let max_edges = max_edges.clamp(1, MAX_HIERARCHY_EDGES);
        let mut resolver = Resolver::new(self);

        let definitions = resolver
            .definitions(type_name)?
            .iter()
            .filter(|d| is_type_kind(&d.kind))
            .map(|d| ImpactDefinition {
                file_path: d.file_path.clone(),
                line: d.line as usize,
                kind: d.kind.clone(),
            })
            .collect();

        let mut truncated = false;
        let supertypes = self.walk_hierarchy(
            &mut resolver,
            type_name,
            Walk::Up,
            max_depth,
            max_edges,
            &mut truncated,
        )?;
        let subtypes = self.walk_hierarchy(
            &mut resolver,
            type_name,
            Walk::Down,
            max_depth,
            max_edges,
            &mut truncated,
        )?;

        Ok(TypeHierarchy {
            symbol: type_name.to_string(),
            definitions,
            supertypes,
            subtypes,
            truncated,
        })
    }

    fn walk_hierarchy(
        &self,
        resolver: &mut Resolver,
        type_name: &str,
        walk: Walk,
        max_depth: usize,
        max_edges: usize,
        truncated: &mut bool,
    ) -> Result<Vec<TypeEdge>> {
        let mut edges = Vec::new();
        let mut visited = HashSet::from([type_name.to_string()]);
        let mut queue = VecDeque::from([(type_name.to_string(), 1)]);

        while let Some((name, depth)) = queue.pop_front() {
            let relations = match walk {
                Walk::Up => self.store.find_supertypes(&name, max_edges)?,
                Walk::Down => self.store.find_subtypes(&name, max_edges)?,
            };
            for relation in relations {
                if edges.len() >= max_edges {
                    *truncated = true;
                    return Ok(edges);
                }
                let next = match walk {
                    Walk::Up => relation.super_name.clone(),
                    Walk::Down => relation.type_name.clone(),
                };
                let resolved = resolver
                    .definitions(&next)?
                    .iter()
                    .any(|d| is_type_kind(&d.kind));
                edges.push(TypeEdge {
                    depth,
                    sub_type: relation.type_name,
                    super_type: relation.super_name,
                    kind: relation.kind,
                    file_path: relation.file_path,
                    line: relation.line as usize,
                    resolved,
                });
                if depth < max_depth && visited.insert(next.clone()) {
                    queue.push_back((next, depth + 1));
                }
            }
        }
        Ok(edges)
    }
}
