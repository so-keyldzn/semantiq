pub mod model;

pub use model::{EmbeddingConfig, EmbeddingModel, StubEmbeddingModel, create_embedding_model};

#[cfg(feature = "onnx")]
pub use model::ensure_models_downloaded;

/// Dimension of MiniLM embeddings
pub const EMBEDDING_DIM: usize = 384;
