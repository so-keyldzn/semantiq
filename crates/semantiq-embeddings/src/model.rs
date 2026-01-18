use anyhow::Result;
use serde::{Deserialize, Serialize};
#[cfg(feature = "onnx")]
use std::fs;
#[cfg(feature = "onnx")]
use std::io::Write;
#[cfg(feature = "onnx")]
use std::path::{Path, PathBuf};
#[cfg(feature = "onnx")]
use tracing::info;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    pub model_path: String,
    pub tokenizer_path: String,
    pub max_length: usize,
    pub batch_size: usize,
    /// Number of threads for ONNX intra-op parallelism.
    /// Defaults to number of CPU cores, capped at 8.
    pub num_threads: usize,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        // Get number of threads from environment or use sensible default
        let num_threads = std::env::var("SEMANTIQ_ONNX_THREADS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| {
                // Default to number of CPU cores, capped at 8
                std::thread::available_parallelism()
                    .map(|n| n.get().min(8))
                    .unwrap_or(4)
            });

        #[cfg(feature = "onnx")]
        {
            let models_dir = get_models_dir();
            Self {
                model_path: models_dir.join("minilm.onnx").to_string_lossy().to_string(),
                tokenizer_path: models_dir
                    .join("tokenizer.json")
                    .to_string_lossy()
                    .to_string(),
                max_length: 512,
                batch_size: 32,
                num_threads,
            }
        }
        #[cfg(not(feature = "onnx"))]
        {
            Self {
                model_path: "models/minilm.onnx".to_string(),
                tokenizer_path: "models/tokenizer.json".to_string(),
                max_length: 512,
                batch_size: 32,
                num_threads,
            }
        }
    }
}

#[cfg(feature = "onnx")]
fn get_models_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("semantiq")
        .join("models")
}

#[cfg(feature = "onnx")]
const MODEL_URL: &str =
    "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/onnx/model.onnx";
#[cfg(feature = "onnx")]
const TOKENIZER_URL: &str =
    "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/tokenizer.json";

#[cfg(feature = "onnx")]
fn download_file(url: &str, path: &Path) -> Result<()> {
    info!("Downloading {} to {:?}", url, path);

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Use an agent with no body size limit (model is ~90MB)
    let agent = ureq::Agent::new_with_config(
        ureq::config::Config::builder()
            .http_status_as_error(true)
            .build(),
    );

    let response = agent.get(url).call()?;
    // Read with no limit (default is 10MB which is too small for the model)
    let bytes = response
        .into_body()
        .with_config()
        .limit(200 * 1024 * 1024)
        .read_to_vec()?;

    let mut file = fs::File::create(path)?;
    file.write_all(&bytes)?;

    info!("Downloaded {:?} ({} bytes)", path, bytes.len());
    Ok(())
}

#[cfg(feature = "onnx")]
pub fn ensure_models_downloaded() -> Result<EmbeddingConfig> {
    let config = EmbeddingConfig::default();
    let model_path = Path::new(&config.model_path);
    let tokenizer_path = Path::new(&config.tokenizer_path);

    if !model_path.exists() {
        info!("Model not found, downloading...");
        download_file(MODEL_URL, model_path)?;
    }

    if !tokenizer_path.exists() {
        info!("Tokenizer not found, downloading...");
        download_file(TOKENIZER_URL, tokenizer_path)?;
    }

    Ok(config)
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
    use ndarray::{Array2, Axis};
    use ort::inputs;
    use ort::session::{Session, builder::GraphOptimizationLevel};
    use ort::value::TensorRef;
    use std::sync::Mutex;
    use tokenizers::Tokenizer;

    pub struct OnnxEmbeddingModel {
        session: Mutex<Session>,
        tokenizer: Tokenizer,
        config: EmbeddingConfig,
    }

    impl OnnxEmbeddingModel {
        pub fn load(config: EmbeddingConfig) -> Result<Self> {
            info!(
                "Loading ONNX model from {} (threads: {})",
                config.model_path, config.num_threads
            );

            let session = Session::builder()?
                .with_optimization_level(GraphOptimizationLevel::Level3)?
                .with_intra_threads(config.num_threads)?
                .commit_from_file(&config.model_path)?;

            let tokenizer = Tokenizer::from_file(&config.tokenizer_path)
                .map_err(|e| anyhow::anyhow!("Failed to load tokenizer: {}", e))?;

            Ok(Self {
                session: Mutex::new(session),
                tokenizer,
                config,
            })
        }

        fn tokenize(&self, text: &str) -> Result<(Vec<i64>, Vec<i64>)> {
            let encoding = self
                .tokenizer
                .encode(text, true)
                .map_err(|e| anyhow::anyhow!("Tokenization failed: {}", e))?;

            let input_ids: Vec<i64> = encoding.get_ids().iter().map(|&x| x as i64).collect();
            let attention_mask: Vec<i64> = encoding
                .get_attention_mask()
                .iter()
                .map(|&x| x as i64)
                .collect();

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
            let attention_mask_array =
                Array2::from_shape_vec((1, seq_len), attention_mask.clone())?;
            // token_type_ids: all zeros for single-sequence tasks
            let token_type_ids: Vec<i64> = vec![0; seq_len];
            let token_type_ids_array = Array2::from_shape_vec((1, seq_len), token_type_ids)?;

            let mut session = self
                .session
                .lock()
                .map_err(|e| anyhow::anyhow!("ONNX session lock poisoned: {}", e))?;
            let outputs = session.run(inputs![
                "input_ids" => TensorRef::from_array_view(input_ids_array.view())?,
                "attention_mask" => TensorRef::from_array_view(attention_mask_array.view())?,
                "token_type_ids" => TensorRef::from_array_view(token_type_ids_array.view())?,
            ])?;

            let embeddings = outputs[0].try_extract_array::<f32>()?;

            // Get first batch item (shape: [1, seq_len, hidden_size])
            let token_embeddings = embeddings.index_axis(Axis(0), 0);
            // Convert from dynamic dimension to Array2
            let shape = token_embeddings.shape();
            let token_embeddings = token_embeddings
                .to_owned()
                .into_shape_with_order((shape[0], shape[1]))?;

            Ok(self.mean_pooling(&token_embeddings, &attention_mask))
        }

        fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            // Process texts sequentially but with reduced lock contention
            // True batching would require padding and handling variable sequence lengths
            // which adds complexity for marginal gains in single-threaded scenarios
            let mut results = Vec::with_capacity(texts.len());
            for text in texts {
                results.push(self.embed(text)?);
            }
            Ok(results)
        }

        fn dimension(&self) -> usize {
            384 // MiniLM dimension
        }
    }
}

/// Create an embedding model based on available features
pub fn create_embedding_model(
    #[allow(unused_variables)] config: Option<EmbeddingConfig>,
) -> Result<Box<dyn EmbeddingModel>> {
    #[cfg(feature = "onnx")]
    {
        // Download models if needed
        let config = match config {
            Some(c) => c,
            None => ensure_models_downloaded()?,
        };

        if Path::new(&config.model_path).exists() {
            info!("Using ONNX embedding model from {:?}", config.model_path);
            return Ok(Box::new(onnx::OnnxEmbeddingModel::load(config)?));
        } else {
            info!(
                "ONNX model not found at {:?}, using stub",
                config.model_path
            );
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
