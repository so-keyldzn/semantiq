use anyhow::Result;
use serde::{Deserialize, Serialize};
#[cfg(feature = "onnx")]
use std::path::Path;
#[cfg(feature = "onnx")]
use tracing::info;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    pub model_path: String,
    pub tokenizer_path: String,
    pub max_length: usize,
    pub batch_size: usize,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            model_path: "models/minilm.onnx".to_string(),
            tokenizer_path: "models/tokenizer.json".to_string(),
            max_length: 512,
            batch_size: 32,
        }
    }
}

/// Trait for embedding models
pub trait EmbeddingModel: Send + Sync {
    fn embed(&self, text: &str) -> Result<Vec<f32>>;
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    fn dimension(&self) -> usize;
}

/// Stub embedding model for when ONNX is not available
pub struct StubEmbeddingModel {
    dimension: usize,
}

impl StubEmbeddingModel {
    pub fn new() -> Self {
        Self { dimension: 384 }
    }
}

impl Default for StubEmbeddingModel {
    fn default() -> Self {
        Self::new()
    }
}

impl EmbeddingModel for StubEmbeddingModel {
    fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        // Return zero vector as placeholder
        Ok(vec![0.0; self.dimension])
    }

    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![0.0; self.dimension]).collect())
    }

    fn dimension(&self) -> usize {
        self.dimension
    }
}

#[cfg(feature = "onnx")]
pub mod onnx {
    use super::*;
    use ndarray::{Array1, Array2, Axis};
    use ort::{GraphOptimizationLevel, Session};
    use tokenizers::Tokenizer;

    pub struct OnnxEmbeddingModel {
        session: Session,
        tokenizer: Tokenizer,
        config: EmbeddingConfig,
    }

    impl OnnxEmbeddingModel {
        pub fn load(config: EmbeddingConfig) -> Result<Self> {
            info!("Loading ONNX model from {}", config.model_path);

            let session = Session::builder()?
                .with_optimization_level(GraphOptimizationLevel::Level3)?
                .with_intra_threads(4)?
                .commit_from_file(&config.model_path)?;

            let tokenizer = Tokenizer::from_file(&config.tokenizer_path)
                .map_err(|e| anyhow::anyhow!("Failed to load tokenizer: {}", e))?;

            Ok(Self {
                session,
                tokenizer,
                config,
            })
        }

        fn tokenize(&self, text: &str) -> Result<(Vec<i64>, Vec<i64>)> {
            let encoding = self.tokenizer
                .encode(text, true)
                .map_err(|e| anyhow::anyhow!("Tokenization failed: {}", e))?;

            let input_ids: Vec<i64> = encoding.get_ids().iter().map(|&x| x as i64).collect();
            let attention_mask: Vec<i64> = encoding.get_attention_mask().iter().map(|&x| x as i64).collect();

            // Truncate if needed
            let max_len = self.config.max_length;
            let input_ids = if input_ids.len() > max_len {
                input_ids[..max_len].to_vec()
            } else {
                input_ids
            };
            let attention_mask = if attention_mask.len() > max_len {
                attention_mask[..max_len].to_vec()
            } else {
                attention_mask
            };

            Ok((input_ids, attention_mask))
        }

        fn mean_pooling(&self, token_embeddings: &Array2<f32>, attention_mask: &[i64]) -> Vec<f32> {
            let seq_len = token_embeddings.shape()[0];
            let hidden_size = token_embeddings.shape()[1];

            let mut sum = vec![0.0f32; hidden_size];
            let mut count = 0.0f32;

            for i in 0..seq_len {
                if i < attention_mask.len() && attention_mask[i] == 1 {
                    for j in 0..hidden_size {
                        sum[j] += token_embeddings[[i, j]];
                    }
                    count += 1.0;
                }
            }

            if count > 0.0 {
                for v in &mut sum {
                    *v /= count;
                }
            }

            // L2 normalize
            let norm: f32 = sum.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 0.0 {
                for v in &mut sum {
                    *v /= norm;
                }
            }

            sum
        }
    }

    impl EmbeddingModel for OnnxEmbeddingModel {
        fn embed(&self, text: &str) -> Result<Vec<f32>> {
            let (input_ids, attention_mask) = self.tokenize(text)?;
            let seq_len = input_ids.len();

            let input_ids_array = Array2::from_shape_vec((1, seq_len), input_ids.clone())?;
            let attention_mask_array = Array2::from_shape_vec((1, seq_len), attention_mask.clone())?;

            let outputs = self.session.run(ort::inputs![
                "input_ids" => input_ids_array,
                "attention_mask" => attention_mask_array,
            ]?)?;

            let embeddings = outputs[0].try_extract_tensor::<f32>()?;
            let embeddings = embeddings.view();

            // Get first batch item
            let token_embeddings = embeddings.index_axis(Axis(0), 0);
            let token_embeddings = token_embeddings.to_owned();

            Ok(self.mean_pooling(&token_embeddings, &attention_mask))
        }

        fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            // For simplicity, process one at a time
            // A production implementation would batch properly
            texts.iter().map(|text| self.embed(text)).collect()
        }

        fn dimension(&self) -> usize {
            384 // MiniLM dimension
        }
    }
}

/// Create an embedding model based on available features
pub fn create_embedding_model(#[allow(unused_variables)] config: Option<EmbeddingConfig>) -> Result<Box<dyn EmbeddingModel>> {
    #[cfg(feature = "onnx")]
    {
        let config = config.unwrap_or_default();
        if Path::new(&config.model_path).exists() {
            info!("Using ONNX embedding model");
            return Ok(Box::new(onnx::OnnxEmbeddingModel::load(config)?));
        }
    }

    Ok(Box::new(StubEmbeddingModel::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stub_model() {
        let model = StubEmbeddingModel::new();
        let embedding = model.embed("test").unwrap();
        assert_eq!(embedding.len(), 384);
    }
}
