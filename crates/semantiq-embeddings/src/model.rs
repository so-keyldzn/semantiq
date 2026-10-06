use anyhow::Result;
use serde::{Deserialize, Serialize};
#[cfg(feature = "onnx")]
use sha2::{Digest, Sha256};
#[cfg(feature = "onnx")]
use std::fs;
#[cfg(feature = "onnx")]
use std::io::Write;
#[cfg(feature = "onnx")]
use std::path::{Path, PathBuf};
#[cfg(feature = "onnx")]
use tracing::{info, warn};

/// Prefix CodeRankEmbed expects in front of search queries. Documents (code
/// chunks) are embedded without any prefix.
pub const QUERY_PREFIX: &str = "Represent this query for searching relevant code: ";

/// How per-token hidden states are reduced to a single sentence vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Pooling {
    /// Take the first token (`[CLS]`). Used by CodeRankEmbed.
    #[default]
    Cls,
    /// Average all non-padding tokens (attention mask = 1).
    Mean,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    pub model_path: String,
    pub tokenizer_path: String,
    pub max_length: usize,
    pub batch_size: usize,
    /// Number of threads for ONNX intra-op parallelism.
    /// Defaults to number of CPU cores, capped at 8.
    pub num_threads: usize,
    /// Pooling strategy applied to the model's token embeddings.
    pub pooling: Pooling,
    /// Prefix prepended to queries by `embed_query` (empty = none).
    pub query_prefix: String,
}

#[cfg(feature = "onnx")]
const MODEL_FILENAME: &str = "coderankembed-int8.onnx";
#[cfg(feature = "onnx")]
const TOKENIZER_FILENAME: &str = "coderankembed-tokenizer.json";

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
        let (model_path, tokenizer_path) = {
            let models_dir = get_models_dir();
            (
                models_dir
                    .join(MODEL_FILENAME)
                    .to_string_lossy()
                    .to_string(),
                models_dir
                    .join(TOKENIZER_FILENAME)
                    .to_string_lossy()
                    .to_string(),
            )
        };
        #[cfg(not(feature = "onnx"))]
        let (model_path, tokenizer_path) = (
            "models/coderankembed-int8.onnx".to_string(),
            "models/coderankembed-tokenizer.json".to_string(),
        );

        Self {
            model_path,
            tokenizer_path,
            max_length: 512,
            batch_size: 32,
            num_threads,
            pooling: Pooling::Cls,
            query_prefix: QUERY_PREFIX.to_string(),
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

// nomic-ai/CodeRankEmbed, community INT8 ONNX export. Both URLs are pinned to
// an immutable commit and verified against hard-coded SHA-256 digests, so a
// compromised or silently updated upstream file is rejected instead of trusted.
#[cfg(feature = "onnx")]
const MODEL_URL: &str = "https://huggingface.co/mrsladoje/CodeRankEmbed-onnx-int8/resolve/e74f446dc6e67e29fcee77213472c142f73a6bbb/onnx/model.onnx";
#[cfg(feature = "onnx")]
const MODEL_SHA256: &str = "4eae31d09b1843103a1ebd5e2b2e24b5a5cad441a33906b35b12b1e2ed91d1db";
#[cfg(feature = "onnx")]
const TOKENIZER_URL: &str = "https://huggingface.co/nomic-ai/CodeRankEmbed/resolve/3c4b60807d71f79b43f3c4363786d9493691f8b1/tokenizer.json";
#[cfg(feature = "onnx")]
const TOKENIZER_SHA256: &str = "91f1def9b9391fdabe028cd3f3fcc4efd34e5d1f08c3bf2de513ebb5911a1854";

/// Upper bound on a single download. The INT8 model is ~139 MB.
#[cfg(feature = "onnx")]
const MAX_DOWNLOAD_BYTES: u64 = 200 * 1024 * 1024;

/// Compute SHA-256 hash of a byte slice
#[cfg(feature = "onnx")]
fn compute_sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

/// Download a file, verify it against `expected_sha256`, then atomically move
/// it into place. Nothing is written at `path` if the digest does not match.
#[cfg(feature = "onnx")]
fn download_file(url: &str, path: &Path, expected_sha256: &str) -> Result<()> {
    info!("Downloading {} to {:?}", url, path);

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let agent = ureq::Agent::new_with_config(
        ureq::config::Config::builder()
            .http_status_as_error(true)
            .build(),
    );

    let response = agent.get(url).call()?;
    // The default body limit (10MB) is too small for the model.
    let bytes = response
        .into_body()
        .with_config()
        .limit(MAX_DOWNLOAD_BYTES)
        .read_to_vec()?;

    let checksum = compute_sha256(&bytes);
    if checksum != expected_sha256 {
        anyhow::bail!(
            "SHA-256 mismatch for {}: expected {}, got {}",
            url,
            expected_sha256,
            checksum
        );
    }

    let tmp_path = path.with_extension("part");
    {
        let mut file = fs::File::create(&tmp_path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp_path, path)?;

    info!(
        "Downloaded {:?} ({} bytes, sha256 verified: {}...)",
        path,
        bytes.len(),
        &checksum[..16]
    );
    Ok(())
}

/// Ensure a file exists and matches its pinned SHA-256, downloading if necessary
#[cfg(feature = "onnx")]
fn ensure_file_downloaded(url: &str, path: &Path, expected_sha256: &str, name: &str) -> Result<()> {
    if path.exists() {
        let actual = compute_sha256(&fs::read(path)?);
        if actual == expected_sha256 {
            info!("{} checksum verified", name);
            return Ok(());
        }
        warn!(
            "{} checksum mismatch (expected {}..., got {}...)! File may have been corrupted or tampered with. Re-downloading...",
            name,
            &expected_sha256[..16],
            &actual[..16]
        );
        fs::remove_file(path)?;
    } else {
        info!("{} not found, downloading...", name);
    }
    download_file(url, path, expected_sha256)
}

/// Best-effort removal of the files left by the previous all-MiniLM-L6-v2
/// model (~90MB), which nothing reads anymore.
#[cfg(feature = "onnx")]
fn remove_legacy_model_files(models_dir: &Path) {
    for name in [
        "minilm.onnx",
        "minilm.onnx.sha256",
        "tokenizer.json",
        "tokenizer.json.sha256",
    ] {
        let path = models_dir.join(name);
        if path.exists() {
            match fs::remove_file(&path) {
                Ok(()) => info!("Removed legacy model file {:?}", path),
                Err(e) => warn!("Could not remove legacy model file {:?}: {}", path, e),
            }
        }
    }
}

#[cfg(feature = "onnx")]
pub fn ensure_models_downloaded() -> Result<EmbeddingConfig> {
    remove_legacy_model_files(&get_models_dir());
    let config = EmbeddingConfig::default();
    let model_path = Path::new(&config.model_path);
    let tokenizer_path = Path::new(&config.tokenizer_path);

    ensure_file_downloaded(MODEL_URL, model_path, MODEL_SHA256, "Model")?;
    ensure_file_downloaded(TOKENIZER_URL, tokenizer_path, TOKENIZER_SHA256, "Tokenizer")?;

    Ok(config)
}

/// Reduce one sequence's token embeddings (`[seq_len, hidden]`, row-major) to a
/// single L2-normalized vector.
///
/// `attention_mask` is only consulted for [`Pooling::Mean`]; padding positions
/// (mask 0) are ignored. [`Pooling::Cls`] takes row 0, which is the `[CLS]`
/// token as long as the tokenizer adds special tokens.
pub fn pool_and_normalize(
    token_embeddings: &[f32],
    hidden_size: usize,
    attention_mask: &[i64],
    pooling: Pooling,
) -> Vec<f32> {
    let mut pooled = vec![0.0f32; hidden_size];
    if hidden_size == 0 || token_embeddings.len() < hidden_size {
        return pooled;
    }

    match pooling {
        Pooling::Cls => pooled.copy_from_slice(&token_embeddings[..hidden_size]),
        Pooling::Mean => {
            let mut count = 0.0f32;
            for (row, &m) in token_embeddings
                .chunks_exact(hidden_size)
                .zip(attention_mask)
            {
                if m == 1 {
                    for (acc, v) in pooled.iter_mut().zip(row) {
                        *acc += v;
                    }
                    count += 1.0;
                }
            }
            if count > 0.0 {
                for v in &mut pooled {
                    *v /= count;
                }
            }
        }
    }

    let norm: f32 = pooled.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for v in &mut pooled {
            *v /= norm;
        }
    }
    pooled
}

/// Trait for embedding models
pub trait EmbeddingModel: Send + Sync {
    /// Embed a document (code chunk). No prefix is added.
    fn embed(&self, text: &str) -> Result<Vec<f32>>;
    /// Embed a batch of documents. No prefix is added.
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    /// Embed a search query. Models trained with asymmetric query/document
    /// encoding (CodeRankEmbed) prepend their query instruction here.
    fn embed_query(&self, query: &str) -> Result<Vec<f32>>;
    fn dimension(&self) -> usize;

    /// Whether this model is a no-op stub that returns zero vectors.
    /// Real models (e.g. ONNX) keep the default `false`; the stub overrides it.
    fn is_stub(&self) -> bool {
        false
    }
}

/// Stub embedding model for when ONNX is not available
pub struct StubEmbeddingModel {
    dimension: usize,
}

impl StubEmbeddingModel {
    pub fn new() -> Self {
        Self {
            dimension: crate::EMBEDDING_DIMENSION,
        }
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

    fn embed_query(&self, _query: &str) -> Result<Vec<f32>> {
        Ok(vec![0.0; self.dimension])
    }

    fn dimension(&self) -> usize {
        self.dimension
    }

    fn is_stub(&self) -> bool {
        true
    }
}

#[cfg(feature = "onnx")]
pub mod onnx {
    use super::*;
    use ndarray::Array2;
    use ort::inputs;
    use ort::session::{Session, builder::GraphOptimizationLevel};
    use ort::value::TensorRef;
    use std::sync::Mutex;
    use tokenizers::{Tokenizer, TruncationParams};

    pub struct OnnxEmbeddingModel {
        session: Mutex<Session>,
        tokenizer: Tokenizer,
        config: EmbeddingConfig,
        /// Whether the graph declares a `token_type_ids` input (BERT exports
        /// do, the CodeRankEmbed export does not).
        wants_token_type_ids: bool,
    }

    impl OnnxEmbeddingModel {
        pub fn load(config: EmbeddingConfig) -> Result<Self> {
            info!(
                "Loading ONNX model from {} (threads: {}, pooling: {:?})",
                config.model_path, config.num_threads, config.pooling
            );

            let session = Session::builder()?
                .with_optimization_level(GraphOptimizationLevel::Level3)?
                .with_intra_threads(config.num_threads)?
                .commit_from_file(&config.model_path)?;

            let wants_token_type_ids = session
                .inputs()
                .iter()
                .any(|i| i.name() == "token_type_ids");

            let mut tokenizer = Tokenizer::from_file(&config.tokenizer_path)
                .map_err(|e| anyhow::anyhow!("Failed to load tokenizer: {}", e))?;
            // Let the tokenizer truncate so the trailing [SEP] is preserved;
            // padding is done per batch below.
            tokenizer
                .with_truncation(Some(TruncationParams {
                    max_length: config.max_length,
                    ..Default::default()
                }))
                .map_err(|e| anyhow::anyhow!("Failed to configure truncation: {}", e))?;
            tokenizer.with_padding(None);

            Ok(Self {
                session: Mutex::new(session),
                tokenizer,
                config,
                wants_token_type_ids,
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

            Ok((input_ids, attention_mask))
        }
    }

    impl EmbeddingModel for OnnxEmbeddingModel {
        fn embed(&self, text: &str) -> Result<Vec<f32>> {
            self.embed_batch(&[text.to_string()])?
                .pop()
                .ok_or_else(|| anyhow::anyhow!("Embedding batch returned no vector"))
        }

        fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
            self.embed(&format!("{}{}", self.config.query_prefix, query))
        }

        fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            // Bound the tensor size: callers pass every chunk of a file at once.
            // Note: the INT8 export quantizes activations dynamically with a
            // per-tensor scale, so a text's vector shifts slightly with its
            // batch neighbours (cos ~0.97 vs. embedding it alone). Batching is
            // still kept: ~1.7x faster indexing than one forward pass per chunk.
            let batch_size = self.config.batch_size.max(1);
            let mut results = Vec::with_capacity(texts.len());
            for batch in texts.chunks(batch_size) {
                results.extend(self.run_batch(batch)?);
            }
            Ok(results)
        }

        fn dimension(&self) -> usize {
            crate::EMBEDDING_DIMENSION
        }
    }

    impl OnnxEmbeddingModel {
        fn run_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            if texts.is_empty() {
                return Ok(Vec::new());
            }

            // True tensor batching: tokenize every text, pad them all to a common
            // sequence length, run a single forward pass over the [N, max_len] batch,
            // then pool each row independently. Padding tokens are masked out by
            // the per-row attention mask.

            // 1. Tokenize every text (already truncated by the tokenizer).
            let tokenized: Vec<(Vec<i64>, Vec<i64>)> = texts
                .iter()
                .map(|t| self.tokenize(t))
                .collect::<Result<Vec<_>>>()?;

            // 2. Common padded length = longest sequence in the batch. Guard
            //    against an all-empty batch so the tensor shape stays valid.
            let max_len = tokenized
                .iter()
                .map(|(ids, _)| ids.len())
                .max()
                .unwrap_or(0)
                .max(1);
            let batch_size = tokenized.len();

            // 3. Build flat row-major [N, max_len] buffers, right-padding with 0.
            let mut input_ids_flat = vec![0i64; batch_size * max_len];
            let mut attention_mask_flat = vec![0i64; batch_size * max_len];

            for (row, (ids, mask)) in tokenized.iter().enumerate() {
                let offset = row * max_len;
                input_ids_flat[offset..offset + ids.len()].copy_from_slice(ids);
                attention_mask_flat[offset..offset + mask.len()].copy_from_slice(mask);
            }

            let input_ids_array = Array2::from_shape_vec((batch_size, max_len), input_ids_flat)?;
            let attention_mask_array =
                Array2::from_shape_vec((batch_size, max_len), attention_mask_flat)?;
            // Single-sequence task => all zeros; only sent if the graph asks for it.
            let token_type_ids_array = Array2::<i64>::zeros((batch_size, max_len));

            // 4. Single forward pass over the whole batch.
            let mut session = self
                .session
                .lock()
                .map_err(|e| anyhow::anyhow!("ONNX session lock poisoned: {}", e))?;
            let mut model_inputs = inputs![
                "input_ids" => TensorRef::from_array_view(input_ids_array.view())?,
                "attention_mask" => TensorRef::from_array_view(attention_mask_array.view())?,
            ];
            if self.wants_token_type_ids {
                model_inputs.push((
                    "token_type_ids".into(),
                    TensorRef::from_array_view(token_type_ids_array.view())?.into(),
                ));
            }
            let outputs = session.run(model_inputs)?;

            // Output shape: [batch_size, max_len, hidden_size]. Prefer the
            // named per-token output; fall back to the first output for
            // exports that don't name it.
            let token_output = match outputs.get("token_embeddings") {
                Some(v) => v,
                None => &outputs[0],
            };
            let (shape, data) = token_output.try_extract_tensor::<f32>()?;
            if shape.len() != 3 || shape[0] as usize != batch_size {
                anyhow::bail!("Unexpected token embedding shape: {:?}", shape);
            }
            let seq_len = shape[1] as usize;
            let hidden_size = shape[2] as usize;
            if hidden_size != crate::EMBEDDING_DIMENSION {
                anyhow::bail!(
                    "Model hidden size {} does not match EMBEDDING_DIMENSION {}",
                    hidden_size,
                    crate::EMBEDDING_DIMENSION
                );
            }

            // 5. Pool each row, preserving order.
            let row_len = seq_len * hidden_size;
            let results = tokenized
                .iter()
                .enumerate()
                .map(|(row, (_, mask))| {
                    pool_and_normalize(
                        &data[row * row_len..(row + 1) * row_len],
                        hidden_size,
                        mask,
                        self.config.pooling,
                    )
                })
                .collect();

            Ok(results)
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
        assert_eq!(embedding.len(), crate::EMBEDDING_DIMENSION);
    }

    #[test]
    fn test_stub_is_stub() {
        let model = StubEmbeddingModel::new();
        assert!(model.is_stub());
    }

    #[test]
    fn test_stub_embed_query() {
        let model = StubEmbeddingModel::new();
        let embedding = model.embed_query("parse a toml config file").unwrap();
        assert_eq!(embedding.len(), crate::EMBEDDING_DIMENSION);
        assert!(embedding.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn test_stub_embed_batch_dimensions() {
        let model = StubEmbeddingModel::new();
        let texts = vec!["one".to_string(), "two".to_string(), "three".to_string()];
        let embeddings = model.embed_batch(&texts).unwrap();
        assert_eq!(embeddings.len(), texts.len());
        for e in &embeddings {
            assert_eq!(e.len(), crate::EMBEDDING_DIMENSION);
        }
    }

    #[test]
    fn test_default_config_uses_cls_and_query_prefix() {
        let config = EmbeddingConfig::default();
        assert_eq!(config.pooling, Pooling::Cls);
        assert_eq!(config.query_prefix, QUERY_PREFIX);
        assert!(config.model_path.ends_with("coderankembed-int8.onnx"));
    }

    #[test]
    fn test_cls_pooling_takes_first_token_and_normalizes() {
        // 3 tokens x 4 dims; the last token is padding.
        let tokens = [
            3.0, 0.0, 4.0, 0.0, // [CLS]
            1.0, 1.0, 1.0, 1.0, //
            9.0, 9.0, 9.0, 9.0, // [PAD]
        ];
        let pooled = pool_and_normalize(&tokens, 4, &[1, 1, 0], Pooling::Cls);
        assert_eq!(pooled, vec![0.6, 0.0, 0.8, 0.0]);
    }

    #[test]
    fn test_mean_pooling_ignores_padding() {
        let tokens = [
            2.0, 0.0, //
            0.0, 2.0, //
            100.0, 100.0, // [PAD]
        ];
        let pooled = pool_and_normalize(&tokens, 2, &[1, 1, 0], Pooling::Mean);
        let expected = 1.0 / 2.0f32.sqrt();
        assert!((pooled[0] - expected).abs() < 1e-6);
        assert!((pooled[1] - expected).abs() < 1e-6);
    }

    #[test]
    fn test_pooling_zero_vector_stays_zero() {
        let pooled = pool_and_normalize(&[0.0; 8], 4, &[1, 1], Pooling::Cls);
        assert_eq!(pooled, vec![0.0; 4]);
    }

    #[cfg(feature = "onnx")]
    mod onnx_model {
        use super::super::*;

        fn cosine(a: &[f32], b: &[f32]) -> f32 {
            a.iter().zip(b).map(|(x, y)| x * y).sum()
        }

        /// Runs against the locally cached CodeRankEmbed model; skipped when
        /// the model has not been downloaded yet (no network in tests).
        #[test]
        fn test_coderankembed_query_ranks_related_code_higher() {
            let config = EmbeddingConfig::default();
            if !Path::new(&config.model_path).exists()
                || !Path::new(&config.tokenizer_path).exists()
            {
                eprintln!("skipping: model not found at {}", config.model_path);
                return;
            }
            let model = onnx::OnnxEmbeddingModel::load(config).unwrap();

            let query = model.embed_query("parse a toml config file").unwrap();
            let related = model
                .embed(
                    "fn load_config(path: &Path) -> Result<Config> {\n    \
                     let text = std::fs::read_to_string(path)?;\n    \
                     let config: Config = toml::from_str(&text)?;\n    \
                     Ok(config)\n}",
                )
                .unwrap();
            let unrelated = model
                .embed(
                    "fn draw_circle(canvas: &mut Canvas, x: f32, y: f32, r: f32) {\n    \
                     canvas.set_color(Color::RED);\n    \
                     canvas.arc(x, y, r, 0.0, std::f32::consts::TAU);\n}",
                )
                .unwrap();

            for v in [&query, &related, &unrelated] {
                assert_eq!(v.len(), crate::EMBEDDING_DIMENSION);
                let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                assert!((norm - 1.0).abs() < 1e-3, "norm = {norm}");
            }

            let related_sim = cosine(&query, &related);
            let unrelated_sim = cosine(&query, &unrelated);
            eprintln!("cos(related) = {related_sim}, cos(unrelated) = {unrelated_sim}");
            assert!(related_sim > unrelated_sim);

            // Batch and single-item paths must agree up to the noise of the
            // dynamic INT8 activation quantization (scale shared per batch).
            let batch = model
                .embed_batch(&[
                    "short".to_string(),
                    "a much longer piece of text".to_string(),
                ])
                .unwrap();
            let single = model.embed("short").unwrap();
            let agreement = cosine(&batch[0], &single);
            assert!(agreement > 0.95, "batch/single mismatch: cos = {agreement}");
        }
    }
}
