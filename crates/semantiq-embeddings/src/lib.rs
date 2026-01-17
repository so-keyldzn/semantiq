pub mod model;

pub use model::{EmbeddingModel, EmbeddingConfig};

/// Dimension of MiniLM embeddings
pub const EMBEDDING_DIM: usize = 384;
