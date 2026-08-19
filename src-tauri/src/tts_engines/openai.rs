//! OpenAI text-to-speech, called over HTTPS.
//!
//! Unlike the local engines there is no model to load: "loading" means checking
//! that a usable API key exists and building an HTTP client. Synthesis asks for
//! `pcm`, which OpenAI returns as raw 24 kHz mono signed 16-bit little-endian —
//! the same rate the rest of the pipeline uses, so no resampling is needed.

use std::path::{Path, PathBuf};
use std::time::Duration;

use log::{debug, info, warn};
use serde_json::{json, Map, Value};
use tts_rs::{SynthesisEngine, SynthesisResult};

use super::openai_usage;

/// OpenAI returns 24 kHz mono PCM for `response_format: "pcm"`.
pub const SAMPLE_RATE: u32 = 24_000;

const SPEECH_ENDPOINT: &str = "https://api.openai.com/v1/audio/speech";

/// The API rejects longer input outright, so oversized chunks are caught here
/// with a comprehensible message instead of a 400 from the server.
const MAX_INPUT_CHARS: usize = 4096;

/// A single request has to finish before playback of the previous chunk runs
/// out, but a hung connection must not wedge the worker forever.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

pub const DEFAULT_MODEL: &str = "gpt-4o-mini-tts";
pub const DEFAULT_VOICE: &str = "coral";

/// Voices accepted by `/v1/audio/speech`.
pub const VOICES: &[&str] = &[
    "alloy", "ash", "ballad", "coral", "echo", "fable", "nova", "onyx", "sage", "shimmer", "verse",
];

#[derive(Debug, Clone, Default)]
pub struct OpenAiModelParams {
    /// API key. Required — synthesis cannot start without one.
    pub api_key: Option<String>,
    /// TTS model id. Defaults to [`DEFAULT_MODEL`].
    pub model: Option<String>,
    /// Optional HTTP/SOCKS proxy, e.g. `http://127.0.0.1:10801`.
    pub proxy: Option<String>,
    /// Free-form delivery instructions, honoured by the `gpt-4o-*` models.
    pub instructions: Option<String>,
    /// Where the local spend ledger lives. Without it usage is not tracked and
    /// no budget can be enforced.
    pub usage_ledger_path: Option<PathBuf>,
    /// Monthly spend cap in US dollars. `None` means no cap.
    pub monthly_budget_usd: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct OpenAiInferenceParams {
    pub voice: String,
    pub speed: f32,
}

impl Default for OpenAiInferenceParams {
    fn default() -> Self {
        Self {
            voice: DEFAULT_VOICE.to_string(),
            speed: 1.0,
        }
    }
}

/// `speed` is only honoured by the `tts-1` family; the `gpt-4o-*` models reject
/// or ignore it and take pacing through `instructions` instead.
fn model_supports_speed(model: &str) -> bool {
    model.starts_with("tts-1")
}

/// Turns a speed multiplier into wording the `gpt-4o-*` models understand.
/// Returns `None` when the speed is close enough to normal to say nothing.
fn pace_instruction(speed: f32) -> Option<&'static str> {
    if speed >= 1.15 {
        Some("Speak at a noticeably faster pace than normal.")
    } else if speed <= 0.85 {
        Some("Speak at a noticeably slower pace than normal.")
    } else {
        None
    }
}

/// Distinguishes "no money left" from other failures.
///
/// OpenAI signals an exhausted balance with `insufficient_quota` (HTTP 429) and
/// `billing_hard_limit_reached`; a plain 429 without those codes is ordinary
/// rate limiting and will succeed on retry, so it must not be reported as an
/// empty account. HTTP 402 is included for forward compatibility.
fn is_quota_exhausted(status: u16, code: &str) -> bool {
    status == 402
        || matches!(
            code,
            "insufficient_quota" | "billing_hard_limit_reached" | "billing_not_active"
        )
}

pub struct OpenAiEngine {
    client: Option<reqwest::blocking::Client>,
    api_key: String,
    model: String,
    instructions: Option<String>,
    usage_ledger_path: Option<PathBuf>,
    monthly_budget_usd: Option<f64>,
}

impl OpenAiEngine {
    pub fn new() -> Self {
        Self {
            client: None,
            api_key: String::new(),
            model: DEFAULT_MODEL.to_string(),
            instructions: None,
            usage_ledger_path: None,
            monthly_budget_usd: None,
        }
    }

    pub fn list_voices(&self) -> Vec<String> {
        VOICES.iter().map(|voice| voice.to_string()).collect()
    }

    fn build_body(&self, text: &str, params: &OpenAiInferenceParams) -> Value {
        let voice = if params.voice.trim().is_empty() {
            DEFAULT_VOICE.to_string()
        } else {
            params.voice.clone()
        };

        let mut body = Map::new();
        body.insert("model".into(), json!(self.model));
        body.insert("input".into(), json!(text));
        body.insert("voice".into(), json!(voice));
        body.insert("response_format".into(), json!("pcm"));

        if model_supports_speed(&self.model) {
            if (params.speed - 1.0).abs() > f32::EPSILON {
                body.insert("speed".into(), json!(params.speed.clamp(0.25, 4.0)));
            }
        } else {
            // Merge the user's own instructions with the pacing hint so both survive.
            let mut parts: Vec<String> = Vec::new();
            if let Some(configured) = self
                .instructions
                .as_ref()
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
            {
                parts.push(configured.to_string());
            }
            if let Some(pace) = pace_instruction(params.speed) {
                parts.push(pace.to_string());
            }
            if !parts.is_empty() {
                body.insert("instructions".into(), json!(parts.join(" ")));
            }
        }

        Value::Object(body)
    }
}

impl Default for OpenAiEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl SynthesisEngine for OpenAiEngine {
    type SynthesisParams = OpenAiInferenceParams;
    type ModelParams = OpenAiModelParams;

    /// `_model_path` is unused: this engine has no local files. The argument
    /// stays to satisfy the shared `SynthesisEngine` interface.
    fn load_model_with_params(
        &mut self,
        _model_path: &Path,
        params: Self::ModelParams,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let api_key = params
            .api_key
            .map(|key| key.trim().to_string())
            .filter(|key| !key.is_empty())
            .ok_or(
                "No OpenAI API key configured. Set one in Settings, or export OPENAI_API_KEY.",
            )?;

        let mut builder = reqwest::blocking::Client::builder().timeout(REQUEST_TIMEOUT);
        if let Some(proxy) = params
            .proxy
            .as_ref()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
        {
            builder = builder.proxy(
                reqwest::Proxy::all(proxy)
                    .map_err(|e| format!("Invalid proxy '{}': {}", proxy, e))?,
            );
            info!("OpenAI TTS routed through proxy {}", proxy);
        }

        self.client = Some(builder.build()?);
        self.api_key = api_key;
        self.model = params
            .model
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());
        self.instructions = params.instructions;
        self.usage_ledger_path = params.usage_ledger_path;
        self.monthly_budget_usd = params.monthly_budget_usd.filter(|value| *value > 0.0);

        info!(
            "OpenAI TTS ready (model {}{})",
            self.model,
            match self.monthly_budget_usd {
                Some(budget) => format!(", monthly budget ${:.2}", budget),
                None => String::new(),
            }
        );
        Ok(())
    }

    fn unload_model(&mut self) {
        self.client = None;
        // The key lives only as long as the engine does.
        self.api_key.clear();
    }

    fn synthesize(
        &mut self,
        text: &str,
        params: Option<Self::SynthesisParams>,
    ) -> Result<SynthesisResult, Box<dyn std::error::Error>> {
        let params = params.unwrap_or_default();
        let client = self.client.as_ref().ok_or("OpenAI engine is not loaded")?;

        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err("empty text".into());
        }
        if trimmed.chars().count() > MAX_INPUT_CHARS {
            return Err(format!(
                "chunk of {} characters exceeds the API limit of {}",
                trimmed.chars().count(),
                MAX_INPUT_CHARS
            )
            .into());
        }

        // Checked before the request so an exhausted budget costs nothing.
        if let (Some(path), Some(budget)) =
            (self.usage_ledger_path.as_ref(), self.monthly_budget_usd)
        {
            let spent = openai_usage::read(path).estimated_usd;
            if spent >= budget {
                return Err(format!(
                    "Monthly OpenAI budget reached: ${:.2} of ${:.2} spent.                      Raise the limit in Settings or wait for the next month.",
                    spent, budget
                )
                .into());
            }
        }

        let response = client
            .post(SPEECH_ENDPOINT)
            .bearer_auth(&self.api_key)
            .json(&self.build_body(trimmed, &params))
            .send()
            .map_err(|e| format!("OpenAI request failed: {}", e))?;

        let status = response.status();
        if !status.is_success() {
            // The body is consumed once and reused for both the quota check and
            // the message, since a response can only be read a single time.
            let body = response.text().unwrap_or_default();
            let parsed = serde_json::from_str::<Value>(&body).ok();
            let error = parsed.as_ref().and_then(|value| value.get("error"));
            let code = error
                .and_then(|error| error.get("code"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let message = error
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .unwrap_or(body.trim());

            if is_quota_exhausted(status.as_u16(), code) {
                return Err(format!(
                    "OpenAI is out of credit for this key, so nothing can be synthesized.                      Top up the balance or switch to the local Silero engine. ({})",
                    message
                )
                .into());
            }
            if status.as_u16() == 401 {
                return Err(format!(
                    "OpenAI rejected the API key. Check it in Settings. ({})",
                    message
                )
                .into());
            }
            return Err(format!("OpenAI returned {}: {}", status, message).into());
        }

        let bytes = response
            .bytes()
            .map_err(|e| format!("failed to read OpenAI audio: {}", e))?;
        if bytes.len() < 2 {
            return Err("OpenAI returned no audio".into());
        }

        let samples = bytes
            .chunks_exact(2)
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]]) as f32 / 32768.0)
            .collect::<Vec<f32>>();

        let audio_seconds = samples.len() as f64 / SAMPLE_RATE as f64;
        let characters = trimmed.chars().count() as u64;
        debug!(
            "OpenAI synthesized {:.2}s of audio for {} chars",
            audio_seconds, characters
        );

        if let Some(path) = self.usage_ledger_path.as_ref() {
            let ledger = openai_usage::record(path, &self.model, characters, audio_seconds);
            if let Some(budget) = self.monthly_budget_usd {
                if ledger.estimated_usd >= budget {
                    warn!(
                        "OpenAI monthly budget exhausted: ${:.2} of ${:.2}",
                        ledger.estimated_usd, budget
                    );
                }
            }
        }

        Ok(SynthesisResult {
            samples,
            sample_rate: SAMPLE_RATE,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine_with(model: &str, instructions: Option<&str>) -> OpenAiEngine {
        let mut engine = OpenAiEngine::new();
        engine.model = model.to_string();
        engine.instructions = instructions.map(str::to_string);
        engine
    }

    #[test]
    fn tts1_carries_speed_as_a_parameter() {
        let engine = engine_with("tts-1", None);
        let body = engine.build_body(
            "hello",
            &OpenAiInferenceParams {
                voice: "nova".into(),
                speed: 1.5,
            },
        );
        assert_eq!(body["speed"], json!(1.5));
        assert!(body.get("instructions").is_none());
    }

    #[test]
    fn gpt4o_expresses_speed_as_an_instruction() {
        let engine = engine_with(DEFAULT_MODEL, None);
        let body = engine.build_body(
            "hello",
            &OpenAiInferenceParams {
                voice: "coral".into(),
                speed: 1.5,
            },
        );
        assert!(body.get("speed").is_none());
        assert!(body["instructions"].as_str().unwrap().contains("faster"));
    }

    #[test]
    fn configured_instructions_survive_alongside_the_pace_hint() {
        let engine = engine_with(DEFAULT_MODEL, Some("Sound calm."));
        let body = engine.build_body(
            "hello",
            &OpenAiInferenceParams {
                voice: "coral".into(),
                speed: 0.5,
            },
        );
        let instructions = body["instructions"].as_str().unwrap();
        assert!(instructions.contains("Sound calm."));
        assert!(instructions.contains("slower"));
    }

    #[test]
    fn normal_speed_adds_no_pacing_text() {
        let engine = engine_with(DEFAULT_MODEL, None);
        let body = engine.build_body("hello", &OpenAiInferenceParams::default());
        assert!(body.get("instructions").is_none());
        assert!(body.get("speed").is_none());
    }

    #[test]
    fn quota_errors_are_told_apart_from_ordinary_rate_limiting() {
        assert!(is_quota_exhausted(429, "insufficient_quota"));
        assert!(is_quota_exhausted(429, "billing_hard_limit_reached"));
        assert!(is_quota_exhausted(402, ""));
        // A bare 429 is throttling, not an empty account.
        assert!(!is_quota_exhausted(429, "rate_limit_exceeded"));
        assert!(!is_quota_exhausted(401, "invalid_api_key"));
    }

    #[test]
    fn empty_voice_falls_back_to_the_default() {
        let engine = engine_with(DEFAULT_MODEL, None);
        let body = engine.build_body(
            "hello",
            &OpenAiInferenceParams {
                voice: "  ".into(),
                speed: 1.0,
            },
        );
        assert_eq!(body["voice"], json!(DEFAULT_VOICE));
    }
}
