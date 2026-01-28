pub mod engine;
pub mod query;
pub mod results;
pub mod text_searcher;
pub mod threshold;

pub use engine::{DependencyInfo, RetrievalEngine, SymbolDefinition, SymbolExplanation};
pub use query::{Query, QueryExpander, SearchOptions};
pub use results::{SearchResult, SearchResultKind};
pub use text_searcher::TextSearcher;
pub use threshold::{
    CalibrationConfig, CalibrationResult, CollectorConfig, Confidence, DistanceCollector,
    DistanceObservation, DistanceStats, LanguageThresholds, ThresholdCalibrator, ThresholdConfig,
    format_calibration_summary,
};
