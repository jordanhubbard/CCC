//! Sprint 2: Speaker labels, streaming TTS, per-agent voice config

use leptos::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;

/// Per-agent voice configuration stored client-side.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentVoiceConfig {
    /// Voice ID (e.g. "alloy", "echo", "nova", "shimmer") — passed to TTS backend
    pub voice_id: String,
    /// Speed multiplier (0.5–2.0, default 1.0)
    pub speed: f32,
    /// Whether to auto-play TTS when this agent sends a message
    pub auto_play: bool,
    /// Whether to announce the speaker name before reading ("Natasha says: ...")
    pub speaker_label: bool,
}

impl Default for AgentVoiceConfig {
    fn default() -> Self {
        Self {
            voice_id: "nova".into(),
            speed: 1.0,
            auto_play: false,
            speaker_label: true,
        }
    }
}

/// Global registry of per-agent voice configs.
/// Key: agent name (e.g. "natasha", "rocky", "bullwinkle")
pub type VoiceConfigMap = std::collections::HashMap<String, AgentVoiceConfig>;

/// Default voice assignments per known agent
pub fn default_voice_config() -> VoiceConfigMap {
    let mut m = VoiceConfigMap::new();
    m.insert("natasha".into(),    AgentVoiceConfig { voice_id: "nova".into(),    speed: 1.0, auto_play: false, speaker_label: true });
    m.insert("rocky".into(),      AgentVoiceConfig { voice_id: "echo".into(),    speed: 1.0, auto_play: false, speaker_label: true });
    m.insert("bullwinkle".into(), AgentVoiceConfig { voice_id: "alloy".into(),   speed: 1.0, auto_play: false, speaker_label: true });
    m.insert("boris".into(),      AgentVoiceConfig { voice_id: "shimmer".into(), speed: 0.9, auto_play: false, speaker_label: true });
    m
}

/// Play TTS with speaker label prepended.
pub fn play_tts_with_label(text: &str, agent_name: &str, cfg: &AgentVoiceConfig) {
    let tts_text = if cfg.speaker_label && !agent_name.is_empty() {
        let mut name = agent_name.to_string();
        let first = name.remove(0);
        format!("{}{} says: {}", first.to_uppercase(), name, text)
    } else {
        text.to_string()
    };
    let voice = cfg.voice_id.clone();
    let speed = cfg.speed;
    spawn_local(async move {
        stream_tts_words(&tts_text, &voice, speed).await;
    });
}

/// Streaming TTS: POST to /whisper/tts
pub async fn stream_tts_words(text: &str, voice: &str, speed: f32) {
    let whisper_url = "/whisper/tts";
    let body = gloo_net::http::RequestBuilder::new(whisper_url)
        .method(gloo_net::http::Method::POST)
        .json(&serde_json::json!({
            "text": text,
            "voice": voice,
            "speed": speed,
        }));

    let Ok(request) = body else { return };
    let Ok(resp) = request.send().await else { return };
    if !resp.ok() { return; }

    let Ok(bytes) = resp.binary().await else { return };
    play_audio_bytes(&bytes);
}

/// Play raw audio bytes (WAV/MP3) via Web Audio API
fn play_audio_bytes(bytes: &[u8]) {
    let Some(window) = web_sys::window() else { return };
    let Some(document) = window.document() else { return };

    // Build a blob URL via JS eval (avoids BlobPropertyBag feature flag issues)
    let url_result = js_sys::eval(&format!(
        "URL.createObjectURL(new Blob([new Uint8Array({:?})], {{type:'audio/wav'}}))",
        bytes
    ));
    let url = match url_result {
        Ok(v) => v.as_string().unwrap_or_default(),
        Err(_) => return,
    };
    if url.is_empty() { return; }

    let audio: web_sys::HtmlAudioElement = if let Some(el) = document.get_element_by_id("sc-tts-player") {
        el.unchecked_into()
    } else {
        let Ok(el) = document.create_element("audio") else { return };
        el.set_id("sc-tts-player");
        let _ = document.body().map(|b| b.append_child(&el));
        el.unchecked_into()
    };
    audio.set_src(&url);
    let _ = audio.play();
}

/// Leptos component: per-agent voice config panel.
#[component]
pub fn VoiceConfigPanel(
    voice_configs: ReadSignal<VoiceConfigMap>,
    set_voice_configs: WriteSignal<VoiceConfigMap>,
    agents: Vec<String>,
) -> impl IntoView {
    let available_voices = vec!["alloy", "echo", "fable", "nova", "onyx", "shimmer"];

    view! {
        <div class="sc-voice-config-panel">
            <div class="sc-voice-config-header">"🎙️ Voice Settings"</div>
            {agents.into_iter().map(|agent_name| {
                let agent_name_clone = agent_name.clone();
                let agent_key = agent_name.clone();

                view! {
                    <div class="sc-voice-agent-row">
                        <span class="sc-voice-agent-name">{agent_name.clone()}</span>
                        <select class="sc-voice-select"
                            on:change={
                                let ak = agent_key.clone();
                                move |ev| {
                                    let val = event_target_value(&ev);
                                    set_voice_configs.update(|m| {
                                        m.entry(ak.clone()).or_default().voice_id = val;
                                    });
                                }
                            }
                            prop:value=move || {
                                voice_configs.get()
                                    .get(&agent_name_clone)
                                    .map(|c| c.voice_id.clone())
                                    .unwrap_or_else(|| "nova".into())
                            }
                        >
                            {available_voices.iter().map(|v| {
                                let v = *v;
                                view! { <option value=v>{v}</option> }
                            }).collect::<Vec<_>>()}
                        </select>
                        {
                            let ak2 = agent_key.clone();
                            let ak3 = agent_key.clone();
                            view! {
                                <label class="sc-voice-toggle" title="Auto-play TTS for this agent">
                                    <input type="checkbox"
                                        prop:checked=move || voice_configs.get().get(&ak2).map(|c| c.auto_play).unwrap_or(false)
                                        on:change=move |ev| {
                                            let checked = ev.target()
                                                .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
                                                .map(|i| i.checked())
                                                .unwrap_or(false);
                                            set_voice_configs.update(|m| {
                                                m.entry(ak3.clone()).or_default().auto_play = checked;
                                            });
                                        }
                                    />
                                    "auto-play"
                                </label>
                            }
                        }
                        {
                            let ak4 = agent_key.clone();
                            let ak5 = agent_key;
                            view! {
                                <label class="sc-voice-toggle" title="Announce speaker name">
                                    <input type="checkbox"
                                        prop:checked=move || voice_configs.get().get(&ak4).map(|c| c.speaker_label).unwrap_or(true)
                                        on:change=move |ev| {
                                            let checked = ev.target()
                                                .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
                                                .map(|i| i.checked())
                                                .unwrap_or(false);
                                            set_voice_configs.update(|m| {
                                                m.entry(ak5.clone()).or_default().speaker_label = checked;
                                            });
                                        }
                                    />
                                    "say name"
                                </label>
                            }
                        }
                    </div>
                }
            }).collect::<Vec<_>>()}
        </div>
    }
}
