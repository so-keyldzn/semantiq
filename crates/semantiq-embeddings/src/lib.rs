pub mod model;

pub use model::{EmbeddingModel, EmbeddingConfig, create_embedding_model, StubEmbeddingModel};

#[cfg(feature = "onnx")]
pub use model::ensure_models_downloaded;

/// Dimension of MiniLM embeddings
pub const EMBEDDING_DIM: usize = 384;
