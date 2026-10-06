pub mod model;

pub use model::{
    EmbeddingConfig, EmbeddingModel, Pooling, QUERY_PREFIX, StubEmbeddingModel,
    create_embedding_model, pool_and_normalize,
};

#[cfg(feature = "onnx")]
pub use model::ensure_models_downloaded;

/// Dimension of the embedding vectors (CodeRankEmbed produces 768-dim vectors).
/// Single source of truth: `semantiq-index` sizes `chunks_vec` from it.
pub const EMBEDDING_DIMENSION: usize = 768;

/// Identifier of the embedding model whose vectors are stored in the index.
/// Persisted in the index metadata; changing it forces `chunks_vec` to be
/// rebuilt and the project to be fully re-indexed.
pub const EMBEDDING_MODEL_ID: &str = "nomic-ai/CodeRankEmbed@3c4b608+onnx-int8@e74f446";
