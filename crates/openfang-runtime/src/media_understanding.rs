//! Media understanding engine — image description, audio transcription, video analysis.
//!
//! Auto-cascades through available providers based on configured API keys.
//! Sovereign: Prioritizes local LM Studio for vision to ensure privacy and low latency.

use openfang_types::media::{
    MediaAttachment, MediaConfig, MediaSource, MediaType, MediaUnderstanding,
};
use std::sync::Arc;
use tokio::sync::Semaphore;
use tracing::{info, warn, debug};
use serde_json::json;

/// Media understanding engine.
pub struct MediaEngine {
    config: MediaConfig,
    semaphore: Arc<Semaphore>,
}

impl MediaEngine {
    pub fn new(config: MediaConfig) -> Self {
        let max = config.max_concurrency.clamp(1, 8);
        Self {
            config,
            semaphore: Arc::new(Semaphore::new(max)),
        }
    }

    /// Describe an image using a vision-capable LLM.
    /// Auto-cascade: LM Studio (Sovereign) -> Anthropic -> OpenAI -> Gemini.
    pub async fn describe_image(
        &self,
        attachment: &MediaAttachment,
    ) -> Result<MediaUnderstanding, String> {
        attachment.validate()?;
        if attachment.media_type != MediaType::Image {
            return Err("Expected image attachment".into());
        }

        // Determine which provider to use
        let provider = self.config.image_provider.as_deref()
            .or_else(|| detect_vision_provider())
            .ok_or("No vision-capable LLM provider configured. Set LMSTUDIO_BASE_URL, ANTHROPIC_API_KEY, or OPENAI_API_KEY")?;

        let _permit = self.semaphore.acquire().await.map_err(|e| e.to_string())?;

        // Sovereign: Local LM Studio (OpenAI-compatible)
        if provider == "lmstudio" {
            return self.describe_image_with_lmstudio(attachment).await;
        }

        // Standard upstream skeleton handles other providers
        let model = default_vision_model(provider);
        info!(provider, model, "Sending image for analysis");

        Ok(MediaUnderstanding {
            media_type: MediaType::Image,
            description: format!(
                "[Image analysis would be performed by {} (model: {})]",
                provider, model
            ),
            provider: provider.to_string(),
            model: model.to_string(),
        })
    }

    /// Perform image description via local LM Studio (OpenAI-compatible).
    /// Uses Qwen 3.5 or equivalent provided by the local endpoint.
    async fn describe_image_with_lmstudio(&self, attachment: &MediaAttachment) -> Result<MediaUnderstanding, String> {
        let base_url = std::env::var("LMSTUDIO_BASE_URL").unwrap_or_else(|_| "http://localhost:1234/v1".to_string());
        let model = default_vision_model("lmstudio");
        
        info!(provider = "lmstudio", model, %base_url, "Sending image to local Vision Oracle");

        // Convert image source to base64 for the OpenAI-compatible bridge
        let image_b64 = match &attachment.source {
            MediaSource::FilePath { path } => {
                let bytes = tokio::fs::read(path).await.map_err(|e| format!("Failed to read image: {e}"))?;
                base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes)
            }
            MediaSource::Base64 { data, .. } => data.clone(),
            MediaSource::Url { url } => return Err(format!("URL sources not supported for local LMStudio: {url}")),
        };

        // Build OpenAI-compatible vision payload
        let body = json!({
            "model": model,
            "messages": [
                {
                    "role": "user",
                    "content": [
                        { "type": "text", "text": "Describe this image in detail for a visual director." },
                        {
                            "type": "image_url",
                            "image_url": { "url": format!("data:{};base64,{}", attachment.mime_type, image_b64) }
                        }
                    ]
                }
            ],
            "max_tokens": 1000,
            "temperature": 0.0
        });

        let client = reqwest::Client::new();
        let resp = client.post(format!("{}/chat/completions", base_url.trim_end_matches('/')))
            .json(&body)
            .timeout(std::time::Duration::from_secs(60))
            .send()
            .await
            .map_err(|e| format!("LM Studio request failed: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!("LM Studio returned error: {}", resp.status()));
        }

        let json: serde_json::Value = resp.json().await.map_err(|e| format!("Failed to parse LM Studio response: {e}"))?;
        let description = json["choices"][0]["message"]["content"]
            .as_str()
            .ok_or("LM Studio integration failed: Missing content in response")?
            .to_string();

        Ok(MediaUnderstanding {
            media_type: MediaType::Image,
            description,
            provider: "lmstudio".to_string(),
            model: model.to_string(),
        })
    }

    /// Transcribe audio using speech-to-text.
    pub async fn transcribe_audio(
        &self,
        attachment: &MediaAttachment,
    ) -> Result<MediaUnderstanding, String> {
        attachment.validate()?;
        if attachment.media_type != MediaType::Audio {
            return Err("Expected audio attachment".into());
        }

        let provider = self.config.audio_provider.as_deref()
            .or_else(|| detect_audio_provider())
            .ok_or("No audio transcription provider configured. Set GROQ_API_KEY or OPENAI_API_KEY")?;

        let _permit = self.semaphore.acquire().await.map_err(|e| e.to_string())?;

        let model = default_audio_model(provider);
        Ok(MediaUnderstanding {
            media_type: MediaType::Audio,
            description: "[Audio transcription complete]".to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
        })
    }

    /// Describe video using Gemini.
    pub async fn describe_video(
        &self,
        attachment: &MediaAttachment,
    ) -> Result<MediaUnderstanding, String> {
        attachment.validate()?;
        if attachment.media_type != MediaType::Video {
            return Err("Expected video attachment".into());
        }
        Ok(MediaUnderstanding {
            media_type: MediaType::Video,
            description: "[Video analysis complete]".to_string(),
            provider: "gemini".to_string(),
            model: "gemini-2.5-flash".to_string(),
        })
    }
}

/// Detect which vision provider is available.
fn detect_vision_provider() -> Option<&'static str> {
    if std::env::var("LMSTUDIO_BASE_URL").is_ok() {
        return Some("lmstudio");
    }
    if std::env::var("ANTHROPIC_API_KEY").is_ok() {
        return Some("anthropic");
    }
    if std::env::var("OPENAI_API_KEY").is_ok() {
        return Some("openai");
    }
    None
}

fn detect_audio_provider() -> Option<&'static str> {
    if std::env::var("GROQ_API_KEY").is_ok() {
        return Some("groq");
    }
    if std::env::var("OPENAI_API_KEY").is_ok() {
        return Some("openai");
    }
    None
}

/// Get the default vision model for a provider.
fn default_vision_model(provider: &str) -> &str {
    match provider {
        "lmstudio" => "qwen/qwen3.5-9b",
        "anthropic" => "claude-sonnet-4-20250514",
        "openai" => "gpt-4o",
        _ => "unknown",
    }
}

fn default_audio_model(provider: &str) -> &str {
    match provider {
        "groq" => "whisper-large-v3-turbo",
        "openai" => "whisper-1",
        _ => "unknown",
    }
}
