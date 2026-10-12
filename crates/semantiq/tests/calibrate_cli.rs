//! `semantiq calibrate` through the binary: the report, the dry run, the
//! saved calibration and the errors (#122). Asserts on the observable shape
//! (exit code, report headings), not on the computed thresholds.

use semantiq_index::IndexStore;
use semantiq_index::store::CalibrationRecord;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

fn semantiq(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_semantiq"))
        .args(args)
        .current_dir(dir)
        .env("SEMANTIQ_EMBEDDINGS", "stub")
        .env("SEMANTIQ_UPDATE_CHECK", "false")
        .output()
        .expect("failed to run semantiq")
}

/// The calibration saved for `language`, if any.
fn calibration(dir: &Path, language: &str) -> Option<CalibrationRecord> {
    let store = IndexStore::open(&dir.join(".semantiq.db")).unwrap();
    store
        .load_all_calibrations()
        .unwrap()
        .into_iter()
        .find(|c| c.language == language)
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// An indexed project whose database holds `per_language` synthetic distance
/// observations for each of `languages` (what semantic searches would have
/// collected).
fn project_with_observations(languages: &[&str], per_language: usize) -> TempDir {
    let dir = TempDir::new().unwrap();
    fs::create_dir(dir.path().join("src")).unwrap();
    fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn compute(a: u32) -> u32 {\n    a + 1\n}\n",
    )
    .unwrap();
    let output = semantiq(dir.path(), &["index", ".", "--no-embeddings"]);
    assert!(output.status.success(), "index failed: {}", stderr(&output));

    let store = IndexStore::open(&dir.path().join(".semantiq.db")).unwrap();
    let mut observations = Vec::new();
    for (l, language) in languages.iter().enumerate() {
        for i in 0..per_language {
            // Distances spread over [0.4, 1.4): distinct query hashes, since
            // (query_hash, language) is unique.
            let distance = 0.4 + (i % 50) as f32 / 50.0;
            observations.push((
                language.to_string(),
                distance,
                (l * 100_000 + i) as u64,
                1_700_000_000 + i as i64,
            ));
        }
    }
    let inserted = store
        .insert_distance_observations_batch(&observations)
        .unwrap();
    assert_eq!(inserted, observations.len());
    dir
}

#[test]
fn calibrate_without_observations_explains_and_succeeds() {
    let project = project_with_observations(&[], 0);
    let output = semantiq(project.path(), &["calibrate"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(out.contains("Semantiq Threshold Calibration"), "{out}");
    assert!(out.contains("No distance observations found."), "{out}");
}

#[test]
fn calibrate_dry_run_reports_without_saving() {
    let project = project_with_observations(&["rust", "python"], 120);
    let output = semantiq(
        project.path(),
        &["calibrate", "--dry-run", "--min-samples", "100"],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(out.contains("Semantiq Threshold Calibration"), "{out}");
    assert!(out.contains("Dry run: true"), "{out}");
    assert!(out.contains("rust: 120 (ready)"), "{out}");
    assert!(out.contains("python: 120 (ready)"), "{out}");
    assert!(out.contains("Dry run - not saving to database."), "{out}");
    assert!(!out.contains("Calibration saved"), "{out}");

    assert!(
        calibration(project.path(), "rust").is_none(),
        "a dry run saves nothing"
    );
}

#[test]
fn calibrate_saves_thresholds_for_the_languages_with_enough_samples() {
    let project = project_with_observations(&["rust", "python"], 120);
    let output = semantiq(project.path(), &["calibrate", "--min-samples", "100"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(out.contains("Calibration saved to database."), "{out}");

    for language in ["rust", "python", "_global"] {
        let saved = calibration(project.path(), language)
            .unwrap_or_else(|| panic!("no calibration saved for {language}: {out}"));
        assert!(saved.sample_count >= 100, "{language}: {saved:?}");
        assert!(saved.max_distance > 0.0, "{language}: {saved:?}");
    }
}

#[test]
fn calibrate_one_language_needs_min_samples() {
    let project = project_with_observations(&["rust"], 30);
    let output = semantiq(
        project.path(),
        &["calibrate", "--language", "rust", "--min-samples", "100"],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(out.contains("rust: 30 (insufficient)"), "{out}");
    assert!(
        out.contains("Insufficient samples for rust: 30 < 100 required"),
        "{out}"
    );
    assert!(calibration(project.path(), "rust").is_none());
}

#[test]
fn calibrate_one_language_saves_its_thresholds() {
    let project = project_with_observations(&["rust", "python"], 120);
    let output = semantiq(project.path(), &["calibrate", "--language", "rust"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(out.contains("Calibration Result for rust:"), "{out}");
    assert!(out.contains("Calibration saved to database."), "{out}");
    assert!(calibration(project.path(), "rust").is_some());
    assert!(
        calibration(project.path(), "python").is_none(),
        "--language calibrates that language only"
    );
}

#[test]
fn calibrate_without_a_database_is_an_error() {
    let dir = TempDir::new().unwrap();
    let output = semantiq(dir.path(), &["calibrate"]);
    assert_eq!(output.status.code(), Some(1));
    let err = stderr(&output);
    assert!(err.contains("Database not found"), "{err}");
    assert!(err.contains("semantiq index"), "{err}");
}
