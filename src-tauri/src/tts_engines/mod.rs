//! Runtime-selectable TTS backends.
//!
//! The engines have nothing in common at the type level — Silero is a PyTorch
//! package driven through a Python sidecar, OpenAI is an HTTPS call — so this
//! module reduces them to the single narrow interface the playback pipeline
//! actually needs, and `TTSManager` holds them as trait objects.
//!
//! Adding an engine means implementing `TtsBackend` and extending
//! `load_backend`; nothing in `TTSManager` needs to change.

pub mod openai;
pub mod openai_usage;
pub mod silero;

use std::path::PathBuf;

use log::info;
use tts_rs::{
    engines::kokoro::{KokoroEngine, KokoroInferenceParams, KokoroModelParams},
    SynthesisEngine, SynthesisResult,
};

use crate::managers::model::EngineType;
use openai::{OpenAiEngine, OpenAiInferenceParams, OpenAiModelParams};
use silero::{SileroEngine, SileroInferenceParams, SileroModelParams};

/// Engine-neutral synthesis request. Fields not meaningful to a given backend
/// are ignored by it rather than being modelled as an enum, so callers do not
/// have to branch per engine.
#[derive(Debug, Clone)]
pub struct SynthParams {
    pub voice: String,
    pub speed: f32,
    /// Kokoro only: overrides the style-vector index. Ignored elsewhere.
    pub style_index: Option<usize>,
}

impl Default for SynthParams {
    fn default() -> Self {
        Self {
            voice: String::new(),
            speed: 1.0,
            style_index: None,
        }
    }
}

/// Everything a backend might need to load, gathered by the caller once.
#[derive(Debug, Clone, Default)]
pub struct BackendContext {
    /// Kokoro: bundled espeak-ng binary and data directory.
    pub espeak_ng_path: Option<PathBuf>,
    pub espeak_ng_data_path: Option<PathBuf>,
    /// Kokoro: writable location for the pre-optimized ORT graph.
    pub optimized_model_cache_path: Option<PathBuf>,
    /// Silero: interpreter with torch installed, and the sidecar script.
    pub python_path: Option<PathBuf>,
    pub sidecar_script_path: Option<PathBuf>,
    /// Silero: inference threads for this worker.
    pub num_threads: Option<usize>,
    /// OpenAI: credentials and request shaping.
    pub openai_api_key: Option<String>,
    pub openai_model: Option<String>,
    pub openai_proxy: Option<String>,
    pub openai_instructions: Option<String>,
    /// Where the local OpenAI spend ledger lives.
    pub openai_usage_ledger_path: Option<PathBuf>,
    /// Monthly OpenAI spend cap in US dollars; `None` means no cap.
    pub openai_monthly_budget_usd: Option<f64>,
}

pub trait TtsBackend: Send {
    /// Which engine this is. Voice-naming conventions differ per engine, so the
    /// caller needs this to interpret the voice list.
    fn kind(&self) -> EngineType;

    /// Voice names this backend can synthesize with, in model order.
    fn list_voices(&self) -> Vec<String>;

    fn synthesize(&mut self, text: &str, params: &SynthParams) -> Result<SynthesisResult, String>;

    /// Short utterance run once after loading so the first real request does not
    /// pay lazy-initialization cost.
    fn warmup(&mut self);
}

struct KokoroBackend(KokoroEngine);

impl TtsBackend for KokoroBackend {
    fn kind(&self) -> EngineType {
        EngineType::Kokoro
    }

    fn list_voices(&self) -> Vec<String> {
        self.0.list_voices().iter().map(|v| v.to_string()).collect()
    }

    fn synthesize(&mut self, text: &str, params: &SynthParams) -> Result<SynthesisResult, String> {
        let mut kokoro_params = KokoroInferenceParams {
            speed: params.speed,
            style_index: params.style_index,
            ..Default::default()
        };
        if !params.voice.is_empty() {
            kokoro_params.voice = params.voice.clone();
        }
        self.0
            .synthesize(text, Some(kokoro_params))
            .map_err(|e| e.to_string())
    }

    fn warmup(&mut self) {
        let _ = self.0.synthesize("Hello.", None);
    }
}

struct SileroBackend(SileroEngine);

impl TtsBackend for SileroBackend {
    fn kind(&self) -> EngineType {
        EngineType::Silero
    }

    fn list_voices(&self) -> Vec<String> {
        self.0.list_voices().to_vec()
    }

    fn synthesize(&mut self, text: &str, params: &SynthParams) -> Result<SynthesisResult, String> {
        self.0
            .synthesize(
                text,
                Some(SileroInferenceParams {
                    voice: params.voice.clone(),
                    speed: params.speed,
                    accent: true,
                }),
            )
            .map_err(|e| e.to_string())
    }

    fn warmup(&mut self) {
        // Russian on purpose: the Cyrillic path exercises the accentor, which is
        // where the lazy initialization actually happens.
        let _ = self
            .0
            .synthesize("Проверка.", Some(SileroInferenceParams::default()));
    }
}

struct OpenAiBackend(OpenAiEngine);

impl TtsBackend for OpenAiBackend {
    fn kind(&self) -> EngineType {
        EngineType::OpenAi
    }

    fn list_voices(&self) -> Vec<String> {
        self.0.list_voices()
    }

    fn synthesize(&mut self, text: &str, params: &SynthParams) -> Result<SynthesisResult, String> {
        self.0
            .synthesize(
                text,
                Some(OpenAiInferenceParams {
                    voice: params.voice.clone(),
                    speed: params.speed,
                }),
            )
            .map_err(|e| e.to_string())
    }

    fn warmup(&mut self) {
        // Deliberately does nothing: warming up a network engine would spend a
        // paid request to save nothing, since there is no local state to prime.
    }
}

/// Loads the backend matching `engine_type`.
///
/// `model_dir` is only meaningful for engines with local files; network engines
/// ignore it.
pub fn load_backend(
    engine_type: &EngineType,
    model_dir: &std::path::Path,
    context: &BackendContext,
) -> Result<Box<dyn TtsBackend>, String> {
    match engine_type {
        EngineType::Kokoro => {
            let mut engine = KokoroEngine::with_espeak(
                context.espeak_ng_path.clone(),
                context.espeak_ng_data_path.clone(),
            );
            engine
                .load_model_with_params(
                    model_dir,
                    KokoroModelParams {
                        num_threads: context.num_threads,
                        optimized_model_cache_path: context.optimized_model_cache_path.clone(),
                    },
                )
                .map_err(|e| e.to_string())?;
            info!("Loaded Kokoro backend from {}", model_dir.display());
            Ok(Box::new(KokoroBackend(engine)))
        }
        EngineType::Silero => {
            let mut engine = SileroEngine::new();
            engine
                .load_model_with_params(
                    model_dir,
                    SileroModelParams {
                        python_path: context.python_path.clone(),
                        script_path: context.sidecar_script_path.clone(),
                        num_threads: context.num_threads,
                    },
                )
                .map_err(|e| e.to_string())?;
            info!("Loaded Silero backend from {}", model_dir.display());
            Ok(Box::new(SileroBackend(engine)))
        }
        EngineType::OpenAi => {
            let mut engine = OpenAiEngine::new();
            engine
                .load_model_with_params(
                    model_dir,
                    OpenAiModelParams {
                        api_key: context.openai_api_key.clone(),
                        model: context.openai_model.clone(),
                        proxy: context.openai_proxy.clone(),
                        instructions: context.openai_instructions.clone(),
                        usage_ledger_path: context.openai_usage_ledger_path.clone(),
                        monthly_budget_usd: context.openai_monthly_budget_usd,
                    },
                )
                .map_err(|e| e.to_string())?;
            Ok(Box::new(OpenAiBackend(engine)))
        }
    }
}
