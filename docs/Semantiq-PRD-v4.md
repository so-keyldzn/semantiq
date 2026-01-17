# PRODUCT REQUIREMENTS DOCUMENT

# Semantiq
**One MCP Server. Every AI Coding Tool.**

| | |
|---------|-------------|
| Version | 4.0 |
| Date | January 2026 |
| Author | Nicolas |
| Status | Draft |

---

## 1. Executive Summary

### 1.1 The Opportunity
MCP (Model Context Protocol) is now the universal standard for AI tool integrations. Every major AI coding assistant supports it: Claude Code, Cursor, Windsurf, GitHub Copilot, JetBrains IDEs, VS Code, and Codex CLI. This creates a unique opportunity: build ONE MCP server that works everywhere.

### 1.2 The Product
Semantiq is a single MCP server that gives every AI coding tool semantic understanding of your codebase. Install once, configure in 30 seconds, works with all your tools.

### 1.3 Why This Wins
- **Zero fragmentation**: One product, universal compatibility
- **No lock-in**: Users can switch AI tools without losing Semantiq
- **Minimal scope**: No IDE extensions to maintain, no plugins to build
- **Future-proof**: Any new MCP-compatible tool automatically works

### 1.4 Universal Compatibility

| Tool | MCP Support | Semantiq Works |
|------|-------------|-----------------|
| Claude Code | Native (Anthropic) | ✓ |
| Cursor | Full support | ✓ |
| Windsurf | MCP-native | ✓ |
| GitHub Copilot | VS Code, JetBrains, Xcode | ✓ |
| JetBrains IDEs | Built-in (v2025.2+) | ✓ |
| VS Code | Native support | ✓ |
| Codex CLI / Aider | MCP compatible | ✓ |

---

## 2. Problem Statement

### 2.1 The Context Problem
Every AI coding assistant struggles with the same issue: finding the right context. Current tools use basic text search (grep) or naive RAG that chunks code arbitrarily. The result:
- AI asks "Can you show me the relevant files?" repeatedly
- Developers waste 30-50% of interaction time providing context manually
- AI hallucinates because it doesn't understand code relationships
- Search for "user" returns hundreds of irrelevant results

### 2.2 Why MCP Changes Everything
Before MCP, solving this required building separate integrations for each tool. Now, MCP provides a universal protocol that all major AI coding tools support. One well-built MCP server can serve the entire market.

### 2.3 Target Users
Professional developers using AI coding tools daily on medium-to-large codebases (10K+ LOC). They're frustrated by context-switching and want AI that "just understands" their code.

---

## 3. Solution: One MCP Server

### 3.1 Architecture
Semantiq is intentionally simple: a local daemon that indexes your code and exposes MCP tools. No cloud required for core functionality. No extensions to install. No plugins to maintain.

```
┌────────────────────────────────────────────────────────────┐
│                     AI CODING TOOLS                        │
│  Claude Code │ Cursor │ Windsurf │ Copilot │ JetBrains    │
└──────────────────────────┬─────────────────────────────────┘
                           │
                     MCP Protocol
                           │
┌──────────────────────────▼─────────────────────────────────┐
│                                                            │
│                   SEMANTIQ MCP SERVER                     │
│                                                            │
│  ┌──────────────────────────────────────────────────────┐ │
│  │                    MCP TOOLS                          │ │
│  │  • semantiq_search      (semantic + lexical)        │ │
│  │  • semantiq_find_refs   (symbol references)         │ │
│  │  • semantiq_deps        (dependency graph)          │ │
│  │  • semantiq_explain     (codebase overview)         │ │
│  └──────────────────────────────────────────────────────┘ │
│                           │                                │
│  ┌──────────────────────────────────────────────────────┐ │
│  │                 RETRIEVAL ENGINE                      │ │
│  │  ripgrep + tree-sitter + embeddings + dep graph      │ │
│  └──────────────────────────────────────────────────────┘ │
│                           │                                │
│  ┌──────────────────────────────────────────────────────┐ │
│  │                   LOCAL INDEX                         │ │
│  │            SQLite + Vectors (single file)            │ │
│  └──────────────────────────────────────────────────────┘ │
│                                                            │
└────────────────────────────────────────────────────────────┘
                           │
                     Your Codebase
```

### 3.2 Setup Experience (30 seconds)

```bash
# Install
brew install semantiq   # or: npm i -g semantiq / cargo install semantiq

# Add to any MCP config (Claude Code, Cursor, Windsurf, etc.)
{ "mcpServers": { "semantiq": { "command": "semantiq", "args": ["serve"] } } }
```

That's it. Semantiq auto-detects the project root and starts indexing. All MCP-compatible tools can now use it.

### 3.3 MCP Tools Specification

| Tool | Description |
|------|-------------|
| `semantiq_search` | Semantic + lexical search. Input: natural language query. Returns: ranked files with snippets, relevance scores. |
| `semantiq_find_refs` | Find all references to a symbol (function, class, variable). Input: symbol name. Returns: locations with context. |
| `semantiq_deps` | Get dependency graph. Input: file path, direction (imports/exports/both). Returns: related files with relationship type. |
| `semantiq_explain` | Codebase overview. Input: optional focus area. Returns: structure summary, tech stack, key files, architecture patterns. |

---

## 4. Technical Specification

### 4.1 Technology Stack

| Component | Technology | Rationale |
|-----------|------------|-----------|
| Core + MCP | Rust | Single binary, fast, mcp-rs crate available |
| Text Search | Ripgrep | Industry standard, Rust-native |
| AST Parsing | Tree-sitter | 40+ languages, incremental, Rust bindings |
| Embeddings | ONNX + MiniLM | 80MB model, cross-platform, no Python |
| Storage | SQLite + sqlite-vss | Single file, portable, vector search built-in |

### 4.2 Performance Targets
- MCP tool response: <200ms p95
- Initial indexing: <2 minutes for 50K files
- Incremental update: <500ms on file save
- Memory: <300MB idle
- Binary size: <50MB (including model)

### 4.3 Tiered Capabilities
Semantiq adapts to device capabilities automatically:

**Tier 1 (All devices):** Ripgrep + Tree-sitter + dependency graph. ~10MB, runs anywhere.

**Tier 2 (8GB+ RAM):** Adds semantic embeddings. ~100MB total.

**Tier 3 (Cloud, optional):** Advanced re-ranking, multi-repo search. Paid feature.

---

## 5. Business Model

### 5.1 Pricing Tiers

| Feature | Free | Pro $12/mo | Team $25/user |
|---------|------|------------|---------------|
| All MCP tools | ✓ | ✓ | ✓ |
| Projects & files | **Unlimited** | Unlimited | Unlimited |
| Semantic search (local) | ✓ | ✓ | ✓ |
| Cloud re-ranking | — | 500 req/mo | Unlimited |
| Multi-repo search | — | — | ✓ |
| Shared team index | — | — | ✓ |

**Free tier philosophy:** Full local functionality with no limits. Users upgrade for cloud-powered features (better ranking quality, team collaboration).

### 5.2 Unit Economics
- Cloud cost per Pro user: ~$0.50/month
- Gross margin: ~95%
- Free tier cost: $0 (100% local, no server costs)

---

## 6. Roadmap

### 6.1 Phase 1: MVP (Weeks 1-8)
**Goal:** Ship working MCP server, validate with early adopters

- Rust core with MCP server (stdio transport)
- Ripgrep + Tree-sitter integration
- Basic semantic search (MiniLM embeddings)
- Install via brew/npm/cargo

**Success:** 100 active users, works with Claude Code + Cursor + Windsurf

### 6.2 Phase 2: Growth (Weeks 9-16)
**Goal:** Launch paid tiers, add cloud features

- Dependency graph visualization
- Cloud re-ranking API (Cloudflare Workers)
- Stripe billing integration
- Usage analytics + limits enforcement

**Success:** 1,000 DAU, 50 paying customers, $600 MRR

### 6.3 Phase 3: Scale (Months 5-12)
**Goal:** Team features, enterprise readiness

- Multi-repository search
- Shared team index
- SSO integration
- Remote MCP server option

**Success:** 5,000 DAU, 500 paying customers, $10K MRR

---

## 7. Success Metrics

### 7.1 North Star Metric
**"MCP tool calls per day"** — measures actual value delivered to AI tools

### 7.2 Key Milestones

| Metric | Week 8 | Week 16 | Month 12 |
|--------|--------|---------|----------|
| Daily Active Users | 100 | 1,000 | 5,000 |
| MCP calls/day | 5K | 50K | 500K |
| Paying customers | — | 50 | 500 |
| MRR | — | $600 | $10,000 |

---

## 8. Risks & Competitive Landscape

### 8.1 Key Risks

| Risk | Level | Mitigation |
|------|-------|------------|
| AI tools build native search | High | Cross-tool compatibility as moat; they can't all build the same quality |
| MCP standard changes | Low | Anthropic invested heavily; standard is stabilizing |
| Low conversion to paid | Medium | Cloud features provide clear value upgrade for power users |

### 8.2 Competitive Moat
- **Universal compatibility:** Works with ANY MCP tool, current and future
- **No vendor lock-in:** Users can switch AI tools without losing Semantiq
- **Local-first privacy:** Code never leaves machine by default
- **Single focus:** One thing done exceptionally well vs. bloated features

---

**One MCP Server. Every AI Coding Tool. Zero Friction.**

---

*Document Status: This PRD is a living document. Version 4.0 reflects the unlimited free tier with local-only functionality.*
