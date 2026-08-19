//! Local accounting of OpenAI speech spend.
//!
//! OpenAI exposes no endpoint that returns a remaining balance to an ordinary
//! project key — the billing endpoint was withdrawn, and the organization costs
//! API needs an admin key and reports spend rather than what is left. So the
//! budget shown in the app is computed here: every successful synthesis is
//! recorded, priced from OpenAI's published rates, and compared against a cap
//! the user sets. "Remaining" therefore means "remaining against your own cap",
//! which is a number this app can actually stand behind.
//!
//! The ledger is a small JSON file in the app data directory. Writes are
//! serialized through a process-wide lock and land via a temporary file, so
//! parallel synthesis workers cannot interleave and a crash cannot leave a
//! half-written file that would read as zero spend.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{Datelike, Local};
use log::warn;
use serde::{Deserialize, Serialize};
use specta::Type;

/// Serializes read-modify-write cycles on the ledger within this process.
static LEDGER_LOCK: Mutex<()> = Mutex::new(());

/// How a model is charged. OpenAI prices the `tts-1` family per input
/// character and the `gpt-4o-*` speech models per audio token, which in
/// practice tracks the duration of the audio produced.
#[derive(Debug, Clone, Copy)]
enum Rate {
    /// US dollars per million characters of input.
    PerMillionChars(f64),
    /// US dollars per minute of generated audio.
    PerAudioMinute(f64),
}

/// Published OpenAI rates. Kept in one place because prices change and this is
/// the only thing that needs editing when they do.
const RATES: &[(&str, Rate)] = &[
    ("tts-1-hd", Rate::PerMillionChars(30.0)),
    ("tts-1", Rate::PerMillionChars(15.0)),
    ("gpt-4o-mini-tts", Rate::PerAudioMinute(0.015)),
    ("gpt-4o-audio", Rate::PerAudioMinute(0.06)),
];

/// Rate applied to an unrecognised model id, so a new model still produces a
/// plausible figure instead of silently costing nothing.
const FALLBACK_RATE: Rate = Rate::PerAudioMinute(0.015);

fn rate_for(model: &str) -> Rate {
    // Longest prefix wins, so "tts-1-hd" is not matched by "tts-1".
    RATES
        .iter()
        .filter(|(prefix, _)| model.starts_with(prefix))
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(_, rate)| *rate)
        .unwrap_or(FALLBACK_RATE)
}

/// Estimated cost in US dollars of one synthesis.
pub fn estimate_usd(model: &str, characters: u64, audio_seconds: f64) -> f64 {
    match rate_for(model) {
        Rate::PerMillionChars(per_million) => characters as f64 / 1_000_000.0 * per_million,
        Rate::PerAudioMinute(per_minute) => audio_seconds / 60.0 * per_minute,
    }
}

/// Spend so far in the current calendar month.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Type)]
pub struct UsageLedger {
    /// `YYYY-MM` of the figures below. A different month resets them.
    #[serde(default)]
    pub month: String,
    #[serde(default)]
    pub requests: u64,
    #[serde(default)]
    pub characters: u64,
    #[serde(default)]
    pub audio_seconds: f64,
    #[serde(default)]
    pub estimated_usd: f64,
}

/// What the UI shows: spend plus the user's cap, if one is set.
#[derive(Debug, Clone, Serialize, Type)]
pub struct UsageReport {
    pub month: String,
    pub requests: u64,
    pub characters: u64,
    pub audio_seconds: f64,
    pub estimated_usd: f64,
    /// The user's monthly cap, when configured.
    pub budget_usd: Option<f64>,
    /// `budget_usd - estimated_usd`, never negative. `None` without a cap.
    pub remaining_usd: Option<f64>,
    /// True once spend has reached the cap; synthesis is refused in that state.
    pub over_budget: bool,
}

fn current_month() -> String {
    let now = Local::now();
    format!("{:04}-{:02}", now.year(), now.month())
}

pub fn ledger_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("openai_usage.json")
}

/// Reads the ledger, resetting it in memory when the stored month has passed.
///
/// A missing or corrupt file reads as empty rather than failing: losing the
/// spend estimate must never stop the app from speaking.
pub fn read(path: &Path) -> UsageLedger {
    let month = current_month();
    let mut ledger = match fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str::<UsageLedger>(&raw).unwrap_or_else(|e| {
            warn!("Ignoring unreadable OpenAI usage ledger: {}", e);
            UsageLedger::default()
        }),
        Err(_) => UsageLedger::default(),
    };
    if ledger.month != month {
        ledger = UsageLedger {
            month,
            ..Default::default()
        };
    }
    ledger
}

fn write_atomic(path: &Path, ledger: &UsageLedger) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(ledger)?)?;
    fs::rename(&temporary, path)
}

/// Adds one synthesis to the ledger and returns the updated totals.
pub fn record(
    path: &Path,
    model: &str,
    characters: u64,
    audio_seconds: f64,
) -> UsageLedger {
    let _guard = LEDGER_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let mut ledger = read(path);
    ledger.requests += 1;
    ledger.characters += characters;
    ledger.audio_seconds += audio_seconds;
    ledger.estimated_usd += estimate_usd(model, characters, audio_seconds);

    if let Err(e) = write_atomic(path, &ledger) {
        warn!("Failed to persist OpenAI usage ledger: {}", e);
    }
    ledger
}

/// Builds the figures the UI displays.
pub fn report(path: &Path, budget_usd: Option<f64>) -> UsageReport {
    let ledger = read(path);
    let budget = budget_usd.filter(|value| *value > 0.0);
    let remaining = budget.map(|cap| (cap - ledger.estimated_usd).max(0.0));
    UsageReport {
        month: ledger.month,
        requests: ledger.requests,
        characters: ledger.characters,
        audio_seconds: ledger.audio_seconds,
        estimated_usd: ledger.estimated_usd,
        budget_usd: budget,
        remaining_usd: remaining,
        over_budget: budget.is_some_and(|cap| ledger.estimated_usd >= cap),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tts1_hd_is_not_matched_by_the_shorter_tts1_prefix() {
        let hd = estimate_usd("tts-1-hd", 1_000_000, 0.0);
        let plain = estimate_usd("tts-1", 1_000_000, 0.0);
        assert_eq!(hd, 30.0);
        assert_eq!(plain, 15.0);
    }

    #[test]
    fn gpt4o_is_priced_by_audio_duration() {
        // One minute of audio at the published per-minute rate.
        assert!((estimate_usd("gpt-4o-mini-tts", 500, 60.0) - 0.015).abs() < 1e-9);
    }

    #[test]
    fn unknown_models_still_produce_a_nonzero_estimate() {
        assert!(estimate_usd("some-future-model", 1000, 60.0) > 0.0);
    }

    #[test]
    fn report_without_a_budget_has_no_remaining_figure() {
        let report = report(Path::new("does-not-exist.json"), None);
        assert!(report.remaining_usd.is_none());
        assert!(!report.over_budget);
    }

    #[test]
    fn a_zero_budget_is_treated_as_unset_rather_than_immediately_exhausted() {
        let report = report(Path::new("does-not-exist.json"), Some(0.0));
        assert!(report.budget_usd.is_none());
        assert!(!report.over_budget);
    }

    #[test]
    fn ledger_round_trips_and_accumulates() {
        let dir = std::env::temp_dir().join("parrot-usage-test");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("openai_usage.json");
        let _ = fs::remove_file(&path);

        record(&path, "tts-1", 1_000_000, 0.0);
        let ledger = record(&path, "tts-1", 1_000_000, 0.0);

        assert_eq!(ledger.requests, 2);
        assert_eq!(ledger.characters, 2_000_000);
        assert!((ledger.estimated_usd - 30.0).abs() < 1e-9);
        assert_eq!(ledger.month, current_month());

        let report = report(&path, Some(50.0));
        assert!((report.remaining_usd.unwrap() - 20.0).abs() < 1e-9);
        assert!(!report.over_budget);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn remaining_never_goes_negative() {
        let dir = std::env::temp_dir().join("parrot-usage-test-overrun");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("openai_usage.json");
        let _ = fs::remove_file(&path);

        record(&path, "tts-1", 1_000_000, 0.0);
        let report = report(&path, Some(5.0));
        assert_eq!(report.remaining_usd, Some(0.0));
        assert!(report.over_budget);

        let _ = fs::remove_file(&path);
    }
}
