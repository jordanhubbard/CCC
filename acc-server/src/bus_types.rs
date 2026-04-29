/// Shared MediaType enum — single source of truth for all supported media types
/// on the AgentBus. Every message with content MUST carry a `mime` field matching
/// one of these variants. Unknown variants are routed to the dead-letter queue.

macro_rules! media_types {
    ($(($variant:ident, $mime:literal, $binary:expr)),* $(,)?) => {

        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub enum MediaType {
            $($variant,)*
            Unknown(String),
        }

        impl MediaType {
            pub fn as_str(&self) -> &str {
                match self {
                    $(MediaType::$variant => $mime,)*
                    MediaType::Unknown(s) => s.as_str(),
                }
            }

            /// True for audio, video, image, and binary types that require base64 encoding.
            pub fn is_binary(&self) -> bool {
                match self {
                    $(MediaType::$variant => $binary,)*
                    MediaType::Unknown(_) => true,
                }
            }

            pub fn is_known(&self) -> bool {
                !matches!(self, MediaType::Unknown(_))
            }

            /// All canonical MIME strings (used for JSON schema generation).
            pub fn all_known() -> &'static [&'static str] {
                &[$($mime,)*]
            }
        }

        impl std::str::FromStr for MediaType {
            type Err = std::convert::Infallible;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(match s {
                    $($mime => MediaType::$variant,)*
                    other => MediaType::Unknown(other.to_string()),
                })
            }
        }

        impl std::fmt::Display for MediaType {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl serde::Serialize for MediaType {
            fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
                ser.serialize_str(self.as_str())
            }
        }

        impl<'de> serde::Deserialize<'de> for MediaType {
            fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
                let s = String::deserialize(de)?;
                Ok(s.parse().unwrap_or_else(|_| unreachable!()))
            }
        }
    }
}

media_types! {
    // ── Text ──────────────────────────────────────────────────────────────────
    (TextPlain,        "text/plain",                false),
    (TextMarkdown,     "text/markdown",             false),
    (TextHtml,         "text/html",                 false),
    (ApplicationJson,  "application/json",          false),
    // ── Audio ─────────────────────────────────────────────────────────────────
    (AudioWav,         "audio/wav",                 true),
    (AudioMp3,         "audio/mp3",                 true),
    (AudioOgg,         "audio/ogg",                 true),
    (AudioFlac,        "audio/flac",                true),
    // ── Video ─────────────────────────────────────────────────────────────────
    (VideoMp4,         "video/mp4",                 true),
    (VideoWebm,        "video/webm",                true),
    (VideoOgg,         "video/ogg",                 true),
    // ── 2D Graphics ───────────────────────────────────────────────────────────
    (ImagePng,         "image/png",                 true),
    (ImageJpeg,        "image/jpeg",                true),
    (ImageGif,         "image/gif",                 true),
    (ImageWebp,        "image/webp",                true),
    (ImageSvg,         "image/svg+xml",             false), // SVG is XML text
    // ── 3D Models ─────────────────────────────────────────────────────────────
    (ModelGltfJson,    "model/gltf+json",           false), // glTF JSON
    (ModelGltfBinary,  "model/gltf-binary",         true),  // glTF binary (GLB)
    (ModelObj,         "model/obj",                 false), // Wavefront OBJ (text)
    (ModelUsdz,        "model/vnd.usdz+zip",        true),  // USDZ container
    (ModelStl,         "model/stl",                 true),  // STL (binary or ASCII)
    (ModelPly,         "model/ply",                 true),  // PLY polygon file
    (ModelVrml,        "model/vrml",                false), // VRML (text)
    (ModelFbx,         "model/fbx",                 true),  // FBX binary container
    // ── Binary ────────────────────────────────────────────────────────────────
    (OctetStream,      "application/octet-stream",  true),
}

/// Blob metadata — stored server-side in memory, persisted on-demand.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BlobMeta {
    pub id: String,
    pub mime_type: MediaType,
    pub size_bytes: u64,
    pub uploaded_by: String,
    pub uploaded_at: String,
    pub expires_at: Option<String>,
    pub allowed_agents: Vec<String>,
    pub total_chunks: usize,
    pub chunks_received: usize,
    pub complete: bool,
}

/// Dead-letter queue entry — messages that failed dispatch or had unknown types.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DlqEntry {
    pub id: String,
    pub ts: String,
    pub error: String,
    pub message: serde_json::Value,
    pub retry_count: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_known_types_round_trip() {
        for mime in MediaType::all_known() {
            let parsed: MediaType = mime.parse().unwrap();
            assert!(parsed.is_known(), "{mime} should be known");
            assert_eq!(parsed.as_str(), *mime);
        }
    }

    #[test]
    fn test_unknown_type_recognized() {
        let mt: MediaType = "application/x-custom".parse().unwrap();
        assert!(!mt.is_known());
        assert!(mt.is_binary()); // unknown treated as binary for safety
        assert_eq!(mt.as_str(), "application/x-custom");
    }

    #[test]
    fn test_binary_classification() {
        assert!(!MediaType::TextPlain.is_binary());
        assert!(!MediaType::TextMarkdown.is_binary());
        assert!(!MediaType::TextHtml.is_binary());
        assert!(!MediaType::ApplicationJson.is_binary());
        assert!(!MediaType::ImageSvg.is_binary()); // SVG is text
        assert!(MediaType::AudioWav.is_binary());
        assert!(MediaType::AudioMp3.is_binary());
        assert!(MediaType::AudioOgg.is_binary());
        assert!(MediaType::AudioFlac.is_binary());
        assert!(MediaType::VideoMp4.is_binary());
        assert!(MediaType::VideoWebm.is_binary());
        assert!(MediaType::VideoOgg.is_binary());
        assert!(MediaType::ImagePng.is_binary());
        assert!(MediaType::ImageJpeg.is_binary());
        assert!(MediaType::ImageGif.is_binary());
        assert!(MediaType::ImageWebp.is_binary());
        assert!(MediaType::OctetStream.is_binary());
        // 3-D model types
        assert!(!MediaType::ModelGltfJson.is_binary()); // JSON text
        assert!(MediaType::ModelGltfBinary.is_binary());
        assert!(!MediaType::ModelObj.is_binary()); // OBJ is text
        assert!(MediaType::ModelUsdz.is_binary());
        assert!(MediaType::ModelStl.is_binary());
        assert!(MediaType::ModelPly.is_binary());
        assert!(!MediaType::ModelVrml.is_binary()); // VRML is text
        assert!(MediaType::ModelFbx.is_binary());
    }

    #[test]
    fn test_serde_round_trip() {
        let mt = MediaType::ImagePng;
        let json = serde_json::to_string(&mt).unwrap();
        assert_eq!(json, r#""image/png""#);
        let back: MediaType = serde_json::from_str(&json).unwrap();
        assert_eq!(back, MediaType::ImagePng);
    }

    #[test]
    fn test_serde_unknown_round_trip() {
        let json = r#""application/x-wasm""#;
        let mt: MediaType = serde_json::from_str(json).unwrap();
        assert!(!mt.is_known());
        let back = serde_json::to_string(&mt).unwrap();
        assert_eq!(back, json);
    }

    #[test]
    fn test_display() {
        assert_eq!(format!("{}", MediaType::VideoMp4), "video/mp4");
        assert_eq!(format!("{}", MediaType::ImageSvg), "image/svg+xml");
    }

    #[test]
    fn test_count_known_types() {
        assert_eq!(MediaType::all_known().len(), 25);
    }
}
