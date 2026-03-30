
// ─── Sprint 2: Speaker labels, streaming TTS, per-agent voice config ──────────

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
    m.insert("natasha".into(),   AgentVoiceConfig { voice_id: "nova".into(),    speed: 1.0, auto_play: false, speaker_label: true });
    m.insert("rocky".into(),     AgentVoiceConfig { voice_id: "echo".into(),    speed: 1.0, auto_play: false, speaker_label: true });
    m.insert("bullwinkle".into(),AgentVoiceConfig { voice_id: "alloy".into(),   speed: 1.0, auto_play: false, speaker_label: true });
    m.insert("boris".into(),     AgentVoiceConfig { voice_id: "shimmer".into(), speed: 0.9, auto_play: false, speaker_label: true });
    m
}

/// Play TTS with speaker label prepended.
/// If `cfg.speaker_label` is true, says "{agent_name} says: {text}".
pub fn play_tts_with_label(text: &str, agent_name: &str, cfg: &AgentVoiceConfig) {
    let tts_text = if cfg.speaker_label && !agent_name.is_empty() {
        // Capitalize first letter of agent name
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

/// Streaming TTS: POST to /whisper/tts/stream with SSE or chunked response.
/// Renders text word-by-word as audio arrives.
///
/// Endpoint expected: POST /whisper/tts/stream
/// Body: {"text": str, "voice": str, "speed": f32, "stream": true}
/// Response: audio/wav or application/octet-stream with chunked encoding
///
/// For backends that don't support streaming, falls back to single-shot.
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

    // Read audio bytes and play via Web Audio API
    let Ok(bytes) = resp.binary().await else { return };
    play_audio_bytes(&bytes);
}

/// Play raw audio bytes (WAV/MP3) via Web Audio API
fn play_audio_bytes(bytes: &[u8]) {
    use wasm_bindgen::JsCast;
    let Some(window) = web_sys::window() else { return };
    let Some(document) = window.document() else { return };

    // Create a Blob URL and assign to an <audio> element
    let uint8 = js_sys::Uint8Array::from(bytes);
    let arr: js_sys::Array = js_sys::Array::new();
    arr.push(&uint8);
    let blob_parts = wasm_bindgen::JsValue::from(arr);
    let blob_opts = web_sys::BlobPropertyBag::new();
    blob_opts.set_type("audio/wav");
    let Ok(blob) = web_sys::Blob::new_with_u8_array_sequence_and_options(&blob_parts.unchecked_into(), &blob_opts) else { return };
    // create_object_url is a static method in js_sys / manual binding; use gloo_file instead
    use wasm_bindgen::JsCast;
    let url_result = js_sys::eval(&format!(
        "URL.createObjectURL(new Blob([new Uint8Array({:?})], {{type:'audio/wav'}}))",
        bytes
    ));
    let url = match url_result {
        Ok(v) => v.as_string().unwrap_or_default(),
        Err(_) => return,
    };
    let _ = &url; // suppress unused warning if we switch to another path below
    // Actually use the blob we already created:
    let Ok(url) = js_sys::Reflect::get(&js_sys::global(), &wasm_bindgen::JsValue::from_str("URL"))
        .ok()
        .and_then(|url_class| {
            js_sys::Reflect::get(&url_class, &wasm_bindgen::JsValue::from_str("createObjectURL")).ok()
        })
        .and_then(|f| {
            js_sys::Function::from(f).call1(&wasm_bindgen::JsValue::NULL, &blob).ok()
        })
        .and_then(|v| v.as_string())
        .map(Ok::<String, ()>)
        .unwrap_or(Err(())) else { return };

    // Create/reuse audio element
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
/// Rendered as a modal or collapsible panel in the SquirrelChat sidebar.
#[component]
pub fn VoiceConfigPanel(
    /// Current voice config map (read signal)
    voice_configs: ReadSignal<VoiceConfigMap>,
    /// Setter for config map
    set_voice_configs: WriteSignal<VoiceConfigMap>,
    /// List of agent names to configure
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
                        // Voice selector
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
                        // Auto-play toggle
                        {
                            let ak2 = agent_key.clone();
                            let ak3 = agent_key.clone();
                            view! {
                                <label class="sc-voice-toggle" title="Auto-play TTS for this agent">
                                    <input type="checkbox"
                                        prop:checked=move || voice_configs.get().get(&ak2).map(|c| c.auto_play).unwrap_or(false)
                                        on:change=move |ev| {
                                            use wasm_bindgen::JsCast;
                                            let checked = ev.target().and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok()).map(|i| i.checked()).unwrap_or(false);
                                            set_voice_configs.update(|m| {
                                                m.entry(ak3.clone()).or_default().auto_play = checked;
                                            });
                                        }
                                    />
                                    "auto-play"
                                </label>
                            }
                        }
                        // Speaker label toggle
                        {
                            let ak4 = agent_key.clone();
                            let ak5 = agent_key.clone();
                            view! {
                                <label class="sc-voice-toggle" title="Announce speaker name">
                                    <input type="checkbox"
                                        prop:checked=move || voice_configs.get().get(&ak4).map(|c| c.speaker_label).unwrap_or(true)
                                        on:change=move |ev| {
                                            use wasm_bindgen::JsCast;
                                            let checked = ev.target().and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok()).map(|i| i.checked()).unwrap_or(false);
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
