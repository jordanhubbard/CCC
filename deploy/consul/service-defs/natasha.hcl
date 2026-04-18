# Service definitions for natasha (sparky) — CCC GPU worker node
# Registered with the local Consul agent at startup.

service {
  name = "whisper-api"
  id   = "whisper-api-natasha"
  port = 8792
  tags = ["gpu", "ml", "speech-to-text"]
  meta {
    node  = "natasha"
    model = "whisper-large-v3"
    desc  = "OpenAI-compatible Whisper transcription API"
  }
  check {
    http     = "http://127.0.0.1:8792/health"
    interval = "15s"
    timeout  = "3s"
  }
}

service {
  name = "accfs"
  id   = "accfs-natasha"
  port = 8791
  tags = ["storage", "fuse"]
  meta {
    node = "natasha"
    desc = "AccFS FUSE filesystem (mounts /mnt/accfs)"
  }
  check {
    http     = "http://127.0.0.1:8791/health"
    interval = "15s"
    timeout  = "3s"
  }
}

service {
  name = "ollama"
  id   = "ollama-natasha"
  port = 11434
  tags = ["gpu", "ml", "inference"]
  meta {
    node = "natasha"
    desc = "Ollama local LLM inference (GB10 GPU)"
  }
  check {
    http     = "http://127.0.0.1:11434/"
    interval = "15s"
    timeout  = "3s"
  }
}
