//! Silero TTS engine, driven through a Python sidecar process.
//!
//! Silero publishes its TTS models only as PyTorch `torch.package` archives —
//! there is no ONNX export — so the model cannot run natively in Rust. Instead
//! a long-lived Python process holds it warm and this engine talks to it over
//! stdin/stdout using the framing described in `resources/silero_sidecar.py`.
//!
//! One engine owns exactly one sidecar process. `TTSManager` keeps a pool of
//! engines and hands each one to a single worker at a time, so no locking is
//! needed here beyond what the pool already provides.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use log::{debug, error, info, warn};
use serde_json::{json, Value};
use tts_rs::{SynthesisEngine, SynthesisResult};

/// Sample rate requested from the sidecar. Silero can emit 8/24/48 kHz; 24 kHz
/// matches what the OpenAI engine returns, so both engines feed the playback
/// pipeline (chunk crossfading, mixing) at one rate.
pub const SAMPLE_RATE: u32 = 24_000;

/// Filename the model archive is stored under inside the model directory.
pub const MODEL_FILENAME: &str = "model.pt";

/// Upper bound on a single synthesis payload (~10 min of 24 kHz mono f32).
/// Guards against a desynchronized stream being interpreted as a huge length.
const MAX_PAYLOAD_SAMPLES: usize = 24_000 * 60 * 10;

#[derive(Debug, Clone, Default)]
pub struct SileroModelParams {
    /// Interpreter that has `torch` installed. Required.
    pub python_path: Option<PathBuf>,
    /// Path to `silero_sidecar.py`. Required.
    pub script_path: Option<PathBuf>,
    /// Torch intra-op threads for this sidecar. `None` leaves the torch default.
    pub num_threads: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct SileroInferenceParams {
    /// Speaker name as exposed by the model, e.g. `"ru_eduard"`.
    pub voice: String,
    /// Speech speed multiplier. Snapped to an SSML prosody bucket by the sidecar.
    pub speed: f32,
    /// Run Silero's neural stress/yo placement. Off produces noticeably worse
    /// Russian prosody but is slightly faster.
    pub accent: bool,
}

impl Default for SileroInferenceParams {
    fn default() -> Self {
        Self {
            voice: String::new(),
            speed: 1.0,
            accent: true,
        }
    }
}

struct Sidecar {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Sidecar {
    /// Sends one request and reads exactly one response frame.
    ///
    /// The protocol is strictly request/response over a pipe, so a failure here
    /// leaves the stream at an unknown offset; callers treat any error as fatal
    /// to the sidecar and tear it down rather than trying to resynchronize.
    fn request(&mut self, mut body: Value) -> Result<(Value, Vec<u8>), String> {
        self.next_id += 1;
        let id = self.next_id;
        body["id"] = json!(id);

        let mut line = serde_json::to_string(&body).map_err(|e| e.to_string())?;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|_| self.stdin.flush())
            .map_err(|e| format!("failed to write to sidecar: {}", e))?;

        let (header, payload) = read_frame(&mut self.stdout)?;
        if header.get("id").and_then(Value::as_u64) != Some(id) {
            return Err(format!(
                "sidecar response id mismatch (expected {}, got {:?})",
                id,
                header.get("id")
            ));
        }
        if !header.get("ok").and_then(Value::as_bool).unwrap_or(false) {
            let message = header
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown sidecar error");
            return Err(message.to_string());
        }
        Ok((header, payload))
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        // Best effort: ask the sidecar to exit, then make sure it is gone. A
        // stuck interpreter must not outlive the app and hold the model in RAM.
        let _ = self.stdin.write_all(b"{\"id\":0,\"cmd\":\"shutdown\"}\n");
        let _ = self.stdin.flush();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_frame(stdout: &mut BufReader<ChildStdout>) -> Result<(Value, Vec<u8>), String> {
    let mut length_bytes = [0u8; 4];
    stdout
        .read_exact(&mut length_bytes)
        .map_err(|e| format!("sidecar closed while reading frame header: {}", e))?;
    let header_len = u32::from_le_bytes(length_bytes) as usize;

    let mut header_bytes = vec![0u8; header_len];
    stdout
        .read_exact(&mut header_bytes)
        .map_err(|e| format!("sidecar closed while reading header body: {}", e))?;
    let header: Value = serde_json::from_slice(&header_bytes)
        .map_err(|e| format!("malformed sidecar header: {}", e))?;

    let sample_count = header.get("samples").and_then(Value::as_u64).unwrap_or(0) as usize;
    if sample_count > MAX_PAYLOAD_SAMPLES {
        return Err(format!(
            "sidecar announced {} samples, exceeding the {} sample limit",
            sample_count, MAX_PAYLOAD_SAMPLES
        ));
    }

    let mut payload = vec![0u8; sample_count * 4];
    if !payload.is_empty() {
        stdout
            .read_exact(&mut payload)
            .map_err(|e| format!("sidecar closed while reading payload: {}", e))?;
    }
    Ok((header, payload))
}

/// Forwards sidecar stderr into the app log so Python-side failures are visible
/// without attaching a console.
fn pump_stderr(child_stderr: std::process::ChildStderr) {
    std::thread::spawn(move || {
        for line in BufReader::new(child_stderr).lines() {
            match line {
                Ok(line) if !line.trim().is_empty() => debug!("[silero] {}", line),
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });
}

pub struct SileroEngine {
    sidecar: Option<Sidecar>,
    voices: Vec<String>,
}

impl SileroEngine {
    pub fn new() -> Self {
        Self {
            sidecar: None,
            voices: Vec::new(),
        }
    }

    /// Speaker names reported by the loaded model. Empty when no model is loaded.
    pub fn list_voices(&self) -> &[String] {
        &self.voices
    }
}

impl Default for SileroEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl SynthesisEngine for SileroEngine {
    type SynthesisParams = SileroInferenceParams;
    type ModelParams = SileroModelParams;

    fn load_model_with_params(
        &mut self,
        model_path: &Path,
        params: Self::ModelParams,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.unload_model();

        let python = params
            .python_path
            .ok_or("Silero requires a Python interpreter with torch installed")?;
        let script = params
            .script_path
            .ok_or("Silero requires the path to silero_sidecar.py")?;

        // Callers pass the model directory; a direct file path also works.
        let model_file = if model_path.is_dir() {
            model_path.join(MODEL_FILENAME)
        } else {
            model_path.to_path_buf()
        };
        if !model_file.exists() {
            return Err(format!("Silero model not found at {}", model_file.display()).into());
        }
        if !python.exists() {
            return Err(format!("Python interpreter not found at {}", python.display()).into());
        }
        if !script.exists() {
            return Err(format!("Sidecar script not found at {}", script.display()).into());
        }

        let mut command = Command::new(&python);
        command
            .arg(&script)
            .arg(&model_file)
            .arg(params.num_threads.unwrap_or(0).to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Unbuffered stdio: the sidecar flushes explicitly, but Python may
            // still block-buffer when stdout is a pipe rather than a console.
            .env("PYTHONUNBUFFERED", "1")
            .env("PYTHONIOENCODING", "utf-8");

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = command
            .spawn()
            .map_err(|e| format!("failed to start Silero sidecar: {}", e))?;

        if let Some(stderr) = child.stderr.take() {
            pump_stderr(stderr);
        }
        let stdin = child.stdin.take().ok_or("sidecar stdin unavailable")?;
        let stdout = child.stdout.take().ok_or("sidecar stdout unavailable")?;
        let mut sidecar = Sidecar {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 0,
        };

        // The sidecar emits a ready frame (id 0) once the model is in memory.
        let (ready, _) = read_frame(&mut sidecar.stdout)?;
        if !ready.get("ok").and_then(Value::as_bool).unwrap_or(false) {
            let message = ready
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("sidecar failed to start")
                .to_string();
            return Err(message.into());
        }

        self.voices = ready
            .get("voices")
            .and_then(Value::as_array)
            .map(|voices| {
                voices
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        info!(
            "Silero sidecar ready: {} voices from {}",
            self.voices.len(),
            model_file.display()
        );
        self.sidecar = Some(sidecar);
        Ok(())
    }

    fn unload_model(&mut self) {
        if self.sidecar.take().is_some() {
            debug!("Silero sidecar stopped");
        }
        self.voices.clear();
    }

    fn synthesize(
        &mut self,
        text: &str,
        params: Option<Self::SynthesisParams>,
    ) -> Result<SynthesisResult, Box<dyn std::error::Error>> {
        let params = params.unwrap_or_default();
        let sidecar = self
            .sidecar
            .as_mut()
            .ok_or("Silero model is not loaded")?;

        let request = json!({
            "cmd": "synthesize",
            "text": text,
            "voice": params.voice,
            "speed": params.speed,
            "accent": params.accent,
            "sample_rate": SAMPLE_RATE,
        });

        let (header, payload) = match sidecar.request(request) {
            Ok(response) => response,
            Err(err) => {
                // A protocol-level failure leaves the pipe unusable. Drop the
                // process so the next load starts from a clean state instead of
                // reading garbage off a desynchronized stream.
                if is_fatal_transport_error(&err) {
                    warn!("Silero sidecar transport failed, restarting on next load: {}", err);
                    self.unload_model();
                }
                return Err(err.into());
            }
        };

        let sample_rate = header
            .get("sample_rate")
            .and_then(Value::as_u64)
            .unwrap_or(SAMPLE_RATE as u64) as u32;

        let samples = payload
            .chunks_exact(4)
            .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            .collect::<Vec<f32>>();

        if samples.is_empty() {
            error!("Silero returned no audio for {} chars of text", text.len());
            return Err("Silero produced no audio".into());
        }

        Ok(SynthesisResult {
            samples,
            sample_rate,
        })
    }
}

/// Distinguishes "this request was rejected" from "the pipe is broken".
/// Only the latter requires tearing the process down.
fn is_fatal_transport_error(error: &str) -> bool {
    error.contains("sidecar closed")
        || error.contains("failed to write to sidecar")
        || error.contains("id mismatch")
        || error.contains("malformed sidecar header")
        || error.contains("exceeding the")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_errors_are_fatal_but_request_errors_are_not() {
        assert!(is_fatal_transport_error("sidecar closed while reading payload: eof"));
        assert!(!is_fatal_transport_error("empty text"));
        assert!(!is_fatal_transport_error("sample_rate 16000 unsupported"));
    }

    #[test]
    fn inference_params_default_to_natural_speed_with_accents() {
        let params = SileroInferenceParams::default();
        assert_eq!(params.speed, 1.0);
        assert!(params.accent);
    }
}
