pub mod model;

pub use model::{EmbeddingConfig, EmbeddingModel, StubEmbeddingModel, create_embedding_model};

#[cfg(feature = "onnx")]
pub use model::ensure_models_downloaded;

/// Dimension of MiniLM embeddings (all-MiniLM-L6-v2 produces 384-dim vectors).
pub const EMBEDDING_DIMENSION: usize = 384;
