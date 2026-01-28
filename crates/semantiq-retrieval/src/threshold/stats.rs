//! Statistical utilities for distance analysis.

use serde::{Deserialize, Serialize};

/// Statistics computed from distance observations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistanceStats {
    /// Number of observations.
    pub count: usize,
    /// Mean (average) distance.
    pub mean: f32,
    /// Standard deviation.
    pub std_dev: f32,
    /// Minimum distance observed.
    pub min: f32,
    /// Maximum distance observed.
    pub max: f32,
    /// 10th percentile (low distance = very similar).
    pub p10: f32,
    /// 25th percentile (first quartile).
    pub p25: f32,
    /// 50th percentile (median).
    pub p50: f32,
    /// 75th percentile (third quartile).
    pub p75: f32,
    /// 90th percentile (high distance threshold candidate).
    pub p90: f32,
    /// 95th percentile (very high distance threshold candidate).
    pub p95: f32,
}

impl DistanceStats {
    /// Compute statistics from a slice of distance values.
    ///
    /// Returns `None` if the input is empty.
    pub fn compute(distances: &[f32]) -> Option<Self> {
        if distances.is_empty() {
            return None;
        }

        let count = distances.len();

        // Sort for percentile computation
        let mut sorted: Vec<f32> = distances.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        // Basic statistics
        let sum: f32 = sorted.iter().sum();
        let mean = sum / count as f32;

        let variance: f32 = sorted.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / count as f32;
        let std_dev = variance.sqrt();

        let min = sorted[0];
        let max = sorted[count - 1];

        // Percentiles using linear interpolation
        let p10 = percentile(&sorted, 10.0);
        let p25 = percentile(&sorted, 25.0);
        let p50 = percentile(&sorted, 50.0);
        let p75 = percentile(&sorted, 75.0);
        let p90 = percentile(&sorted, 90.0);
        let p95 = percentile(&sorted, 95.0);

        Some(Self {
            count,
            mean,
            std_dev,
            min,
            max,
            p10,
            p25,
            p50,
            p75,
            p90,
            p95,
        })
    }

    /// Get a specific percentile value.
    ///
    /// `p` should be between 0 and 100.
    pub fn percentile(&self, p: f32) -> f32 {
        match p as i32 {
            0..=10 => self.p10,
            11..=25 => self.p25,
            26..=50 => self.p50,
            51..=75 => self.p75,
            76..=90 => self.p90,
            _ => self.p95,
        }
    }

    /// Convert distance to similarity score.
    ///
    /// Uses the formula: similarity = 1.0 / (1.0 + distance)
    pub fn distance_to_similarity(distance: f32) -> f32 {
        1.0 / (1.0 + distance)
    }

    /// Convert similarity score to distance.
    ///
    /// Inverse of `distance_to_similarity`: distance = (1.0 / similarity) - 1.0
    pub fn similarity_to_distance(similarity: f32) -> f32 {
        if similarity <= 0.0 {
            f32::MAX
        } else {
            (1.0 / similarity) - 1.0
        }
    }
}

/// Compute the percentile value from a sorted slice.
///
/// Uses linear interpolation between adjacent values.
/// `p` should be between 0 and 100.
fn percentile(sorted: &[f32], p: f32) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }

    if sorted.len() == 1 {
        return sorted[0];
    }

    // Clamp percentile to valid range
    let p = p.clamp(0.0, 100.0);

    // Calculate the index (0-based)
    let n = sorted.len() as f32;
    let index = (p / 100.0) * (n - 1.0);

    let lower = index.floor() as usize;
    let upper = index.ceil() as usize;

    if lower == upper {
        sorted[lower]
    } else {
        // Linear interpolation
        let fraction = index - lower as f32;
        sorted[lower] * (1.0 - fraction) + sorted[upper] * fraction
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_stats_empty() {
        assert!(DistanceStats::compute(&[]).is_none());
    }

    #[test]
    fn test_compute_stats_single() {
        let stats = DistanceStats::compute(&[0.5]).unwrap();
        assert_eq!(stats.count, 1);
        assert!((stats.mean - 0.5).abs() < 0.001);
        assert!((stats.min - 0.5).abs() < 0.001);
        assert!((stats.max - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_compute_stats_basic() {
        let distances = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let stats = DistanceStats::compute(&distances).unwrap();

        assert_eq!(stats.count, 5);
        assert!((stats.mean - 3.0).abs() < 0.001);
        assert!((stats.min - 1.0).abs() < 0.001);
        assert!((stats.max - 5.0).abs() < 0.001);
        assert!((stats.p50 - 3.0).abs() < 0.001); // Median
    }

    #[test]
    fn test_compute_stats_unsorted_input() {
        let distances = vec![5.0, 1.0, 3.0, 2.0, 4.0];
        let stats = DistanceStats::compute(&distances).unwrap();

        assert_eq!(stats.count, 5);
        assert!((stats.min - 1.0).abs() < 0.001);
        assert!((stats.max - 5.0).abs() < 0.001);
    }

    #[test]
    fn test_percentile_computation() {
        // 0-99 gives us nice round percentiles
        let distances: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let stats = DistanceStats::compute(&distances).unwrap();

        // With 100 values 0-99:
        // p10 should be around 10
        // p50 should be around 50
        // p90 should be around 89
        assert!((stats.p10 - 9.9).abs() < 0.5);
        assert!((stats.p50 - 49.5).abs() < 0.5);
        assert!((stats.p90 - 89.1).abs() < 0.5);
    }

    #[test]
    fn test_distance_to_similarity() {
        // distance 0 -> similarity 1
        assert!((DistanceStats::distance_to_similarity(0.0) - 1.0).abs() < 0.001);

        // distance 1 -> similarity 0.5
        assert!((DistanceStats::distance_to_similarity(1.0) - 0.5).abs() < 0.001);

        // distance 2.333... -> similarity ~0.3
        assert!((DistanceStats::distance_to_similarity(2.333) - 0.3).abs() < 0.01);
    }

    #[test]
    fn test_similarity_to_distance() {
        // similarity 1 -> distance 0
        assert!((DistanceStats::similarity_to_distance(1.0) - 0.0).abs() < 0.001);

        // similarity 0.5 -> distance 1
        assert!((DistanceStats::similarity_to_distance(0.5) - 1.0).abs() < 0.001);

        // similarity 0.3 -> distance ~2.333
        assert!((DistanceStats::similarity_to_distance(0.3) - 2.333).abs() < 0.01);

        // similarity 0 -> distance MAX
        assert!(DistanceStats::similarity_to_distance(0.0) > 1e10);
    }

    #[test]
    fn test_roundtrip_conversion() {
        for dist in [0.0, 0.5, 1.0, 1.5, 2.0, 3.0] {
            let sim = DistanceStats::distance_to_similarity(dist);
            let back = DistanceStats::similarity_to_distance(sim);
            assert!((dist - back).abs() < 0.001, "Roundtrip failed for {}", dist);
        }
    }

    #[test]
    fn test_percentile_method() {
        let distances: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let stats = DistanceStats::compute(&distances).unwrap();

        // Test the percentile lookup method
        assert!((stats.percentile(10.0) - stats.p10).abs() < 0.001);
        assert!((stats.percentile(50.0) - stats.p50).abs() < 0.001);
        assert!((stats.percentile(90.0) - stats.p90).abs() < 0.001);
    }

    #[test]
    fn test_std_dev() {
        // All same values -> std dev 0
        let same = vec![5.0, 5.0, 5.0, 5.0, 5.0];
        let stats = DistanceStats::compute(&same).unwrap();
        assert!(stats.std_dev < 0.001);

        // Varied values -> non-zero std dev
        let varied = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let stats = DistanceStats::compute(&varied).unwrap();
        assert!(stats.std_dev > 1.0);
    }
}
