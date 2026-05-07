use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::Cursor;
use std::path::PathBuf;
use std::time::Instant;
use tauri::{AppHandle, State};
use tokio::sync::Mutex;
use whisper_rs::{
    get_lang_id, get_lang_str, FullParams, SamplingStrategy, WhisperContext,
    WhisperContextParameters,
};

const MODEL_FILE_NAME: &str = "ggml-tiny.bin";
const MODEL_DISPLAY_NAME: &str = "Whisper tiny";
const MODEL_DOWNLOAD_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.bin";
const MODEL_SHA256: &str = "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21";
const MAX_AUDIO_BYTES: usize = 12 * 1024 * 1024;
const EXPECTED_SAMPLE_RATE: u32 = 16_000;

pub struct SpeechState {
    download_lock: Mutex<()>,
}

impl Default for SpeechState {
    fn default() -> Self {
        Self {
            download_lock: Mutex::new(()),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeechModelStatus {
    pub available: bool,
    pub model_name: String,
    pub model_path: String,
    pub model_size_bytes: Option<u64>,
    pub download_url: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeechTranscriptionResult {
    pub text: String,
    pub language: Option<String>,
    pub duration_ms: u64,
    pub model_name: String,
}

fn speech_model_path(app: &AppHandle) -> Result<PathBuf, String> {
    let base_dir = crate::portable::app_data_dir(app)?;
    Ok(base_dir.join("speech").join(MODEL_FILE_NAME))
}

fn build_status(app: &AppHandle) -> Result<SpeechModelStatus, String> {
    let path = speech_model_path(app)?;
    let metadata = std::fs::metadata(&path).ok();
    Ok(SpeechModelStatus {
        available: metadata.is_some(),
        model_name: MODEL_DISPLAY_NAME.to_string(),
        model_path: path.to_string_lossy().to_string(),
        model_size_bytes: metadata.map(|m| m.len()),
        download_url: MODEL_DOWNLOAD_URL.to_string(),
    })
}

fn normalize_language(language: Option<String>) -> Option<String> {
    let raw = language?;
    let normalized = raw
        .split(['-', '_'])
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if normalized.is_empty() || normalized == "auto" {
        return None;
    }
    get_lang_id(&normalized)?;
    Some(normalized)
}

fn read_wav_samples(bytes: &[u8]) -> Result<Vec<f32>, String> {
    if bytes.len() > MAX_AUDIO_BYTES {
        return Err(format!(
            "Audio payload exceeds {} MB",
            MAX_AUDIO_BYTES / 1024 / 1024
        ));
    }

    let mut reader = hound::WavReader::new(Cursor::new(bytes))
        .map_err(|e| format!("Invalid WAV payload: {e}"))?;
    let spec = reader.spec();

    if spec.channels != 1 {
        return Err("Voice input must be mono audio".to_string());
    }
    if spec.sample_rate != EXPECTED_SAMPLE_RATE {
        return Err(format!(
            "Voice input must be {} Hz mono WAV",
            EXPECTED_SAMPLE_RATE
        ));
    }

    match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 16) => reader
            .samples::<i16>()
            .map(|sample| {
                sample
                    .map(|value| value as f32 / i16::MAX as f32)
                    .map_err(|e| format!("Invalid PCM sample: {e}"))
            })
            .collect(),
        (hound::SampleFormat::Int, 32) => reader
            .samples::<i32>()
            .map(|sample| {
                sample
                    .map(|value| value as f32 / i32::MAX as f32)
                    .map_err(|e| format!("Invalid PCM sample: {e}"))
            })
            .collect(),
        (hound::SampleFormat::Float, 32) => reader
            .samples::<f32>()
            .map(|sample| sample.map_err(|e| format!("Invalid float PCM sample: {e}")))
            .collect(),
        _ => Err(format!(
            "Unsupported WAV format: {:?} {}-bit",
            spec.sample_format, spec.bits_per_sample
        )),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn transcribe_pcm_with_model(
    model_path: &std::path::Path,
    pcm: &[f32],
    normalized_language: Option<String>,
) -> Result<(String, Option<String>), String> {
    let model_path_str = model_path
        .to_str()
        .ok_or_else(|| "Speech model path contains invalid UTF-8".to_string())?;
    let context =
        WhisperContext::new_with_params(model_path_str, WhisperContextParameters::default())
            .map_err(|e| format!("Failed to load speech model: {e}"))?;
    let mut whisper_state = context
        .create_state()
        .map_err(|e| format!("Failed to create speech state: {e}"))?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    let thread_count = std::thread::available_parallelism()
        .map(|value| value.get().min(6) as i32)
        .unwrap_or(4);
    params.set_n_threads(thread_count);
    params.set_translate(false);
    params.set_no_context(true);
    params.set_no_timestamps(true);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_single_segment(false);
    if let Some(language) = normalized_language.as_deref() {
        params.set_language(Some(language));
    } else {
        params.set_language(Some("auto"));
        params.set_detect_language(true);
    }

    whisper_state
        .full(params, pcm)
        .map_err(|e| format!("Speech transcription failed: {e}"))?;

    let text = whisper_state
        .as_iter()
        .filter_map(|segment| segment.to_str_lossy().ok())
        .map(|segment| segment.trim().to_string())
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    let language = if let Some(language) = normalized_language {
        Some(language)
    } else {
        let lang_id = whisper_state.full_lang_id_from_state();
        if lang_id >= 0 {
            get_lang_str(lang_id).map(|value| value.to_string())
        } else {
            None
        }
    };

    Ok((text, language))
}

#[tauri::command]
pub async fn speech_model_status(app: AppHandle) -> Result<SpeechModelStatus, String> {
    build_status(&app)
}

#[tauri::command]
pub async fn download_speech_model(
    app: AppHandle,
    state: State<'_, SpeechState>,
) -> Result<SpeechModelStatus, String> {
    let _guard = state.download_lock.lock().await;
    let model_path = speech_model_path(&app)?;

    if model_path.exists() {
        return build_status(&app);
    }

    let parent = model_path
        .parent()
        .ok_or_else(|| "Invalid speech model path".to_string())?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|e| format!("Failed to create speech model directory: {e}"))?;

    let temp_path = model_path.with_extension("download");
    let _ = tokio::fs::remove_file(&temp_path).await;
    let response = reqwest::Client::new()
        .get(MODEL_DOWNLOAD_URL)
        .send()
        .await
        .map_err(|e| format!("Failed to download speech model: {e}"))?;

    if !response.status().is_success() {
        return Err(format!(
            "Speech model download failed with HTTP {}",
            response.status()
        ));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("Failed to read speech model download: {e}"))?;

    if bytes.len() < 1_000_000 {
        return Err("Downloaded speech model looks incomplete".to_string());
    }

    let digest = sha256_hex(&bytes);
    if digest != MODEL_SHA256 {
        return Err(format!(
            "Speech model integrity check failed (expected {}, got {})",
            MODEL_SHA256, digest,
        ));
    }

    tokio::fs::write(&temp_path, &bytes)
        .await
        .map_err(|e| format!("Failed to persist speech model: {e}"))?;
    tokio::fs::rename(&temp_path, &model_path)
        .await
        .map_err(|e| {
            let _ = std::fs::remove_file(&temp_path);
            format!("Failed to finalize speech model: {e}")
        })?;

    build_status(&app)
}

#[tauri::command]
pub async fn speech_to_text(
    app: AppHandle,
    state: State<'_, SpeechState>,
    audio_base64: String,
    language: Option<String>,
) -> Result<SpeechTranscriptionResult, String> {
    let _guard = state.download_lock.lock().await;
    let status = build_status(&app)?;
    if !status.available {
        return Err("Speech model not installed. Download it before transcribing.".to_string());
    }

    let audio_bytes = STANDARD
        .decode(audio_base64.as_bytes())
        .map_err(|e| format!("Invalid base64 audio payload: {e}"))?;
    let pcm = read_wav_samples(&audio_bytes)?;
    let model_path = PathBuf::from(&status.model_path);
    let normalized_language = normalize_language(language);
    let duration_ms = ((pcm.len() as u64) * 1000) / EXPECTED_SAMPLE_RATE as u64;

    let started_at = Instant::now();
    let (text, detected_language) =
        tokio::task::spawn_blocking(move || -> Result<(String, Option<String>), String> {
            transcribe_pcm_with_model(&model_path, &pcm, normalized_language)
        })
        .await
        .map_err(|e| format!("Speech transcription task failed: {e}"))??;

    let elapsed_ms = started_at.elapsed().as_millis() as u64;
    Ok(SpeechTranscriptionResult {
        text,
        language: detected_language,
        duration_ms: duration_ms.max(elapsed_ms),
        model_name: MODEL_DISPLAY_NAME.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_wav_bytes(sample_rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("sample.wav");
        let spec = hound::WavSpec {
            channels,
            sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).expect("create wav");
        for sample in samples {
            writer.write_sample(*sample).expect("write sample");
        }
        writer.finalize().expect("finalize wav");
        std::fs::read(path).expect("read wav")
    }

    #[test]
    fn normalize_language_accepts_valid_prefix() {
        assert_eq!(
            normalize_language(Some("it-IT".to_string())),
            Some("it".to_string())
        );
        assert_eq!(
            normalize_language(Some("EN_us".to_string())),
            Some("en".to_string())
        );
    }

    #[test]
    fn normalize_language_rejects_invalid_values() {
        assert_eq!(normalize_language(Some("auto".to_string())), None);
        assert_eq!(normalize_language(Some("".to_string())), None);
        assert_eq!(normalize_language(Some("zz-invalid".to_string())), None);
    }

    #[test]
    fn read_wav_samples_accepts_valid_mono_16khz() {
        let bytes = make_wav_bytes(EXPECTED_SAMPLE_RATE, 1, &[0, 1000, -1000, 2000]);
        let samples = read_wav_samples(&bytes).expect("valid wav should parse");
        assert_eq!(samples.len(), 4);
    }

    #[test]
    fn read_wav_samples_rejects_wrong_sample_rate() {
        let bytes = make_wav_bytes(44_100, 1, &[0, 1000, -1000, 2000]);
        let error = read_wav_samples(&bytes).expect_err("wrong sample rate should fail");
        assert!(error.contains("16000 Hz mono WAV"));
    }

    #[test]
    fn read_wav_samples_rejects_stereo() {
        let bytes = make_wav_bytes(EXPECTED_SAMPLE_RATE, 2, &[0, 1000, -1000, 2000]);
        let error = read_wav_samples(&bytes).expect_err("stereo wav should fail");
        assert!(error.contains("mono audio"));
    }

    #[test]
    fn transcribe_pcm_with_model_handles_silence_when_model_available() {
        let model_path = std::env::var("AEROFTP_SPEECH_MODEL").unwrap_or_default();
        if model_path.is_empty() || !std::path::Path::new(&model_path).exists() {
            return;
        }

        let pcm = vec![0.0_f32; EXPECTED_SAMPLE_RATE as usize];
        let result = transcribe_pcm_with_model(
            std::path::Path::new(&model_path),
            &pcm,
            Some("en".to_string()),
        );
        assert!(
            result.is_ok(),
            "silence smoke test should not fail: {result:?}"
        );
    }
}
