# Service definitions for do-host1 (100.89.199.14)
# Loaded by Consul server on startup.

services {
  name = "rcc-hub"
  id   = "rcc-hub-do-host1"
  port = 8789
  tags = ["infrastructure", "api", "dashboard"]
  meta {
    host    = "do-host1"
    version = "1.0"
  }
  check {
    http     = "http://127.0.0.1:8789/health"
    interval = "15s"
    timeout  = "3s"
  }
}

services {
  name = "clawbus"
  id   = "clawbus-do-host1"
  port = 8789
  tags = ["infrastructure", "messaging"]
  meta {
    host     = "do-host1"
    endpoint = "/api/bus/send"
  }
  check {
    http     = "http://127.0.0.1:8789/api/bus/stream"
    method   = "GET"
    interval = "30s"
    timeout  = "5s"
  }
}

services {
  name = "tokenhub"
  id   = "tokenhub-do-host1"
  port = 8090
  tags = ["infrastructure", "llm-router"]
  meta {
    host  = "do-host1"
    admin = "/admin/"
  }
  check {
    http     = "http://127.0.0.1:8090/health"
    interval = "15s"
    timeout  = "3s"
  }
}

services {
  name = "qdrant"
  id   = "qdrant-do-host1"
  port = 6333
  tags = ["database", "vector"]
  meta {
    host       = "do-host1"
    collection = "agent_memories"
  }
  check {
    http     = "http://127.0.0.1:6333/collections"
    interval = "15s"
    timeout  = "3s"
  }
}

services {
  name = "minio"
  id   = "minio-do-host1"
  port = 9000
  tags = ["storage", "s3"]
  meta {
    host = "do-host1"
  }
  check {
    http     = "http://127.0.0.1:9000/minio/health/live"
    interval = "15s"
    timeout  = "3s"
  }
}

services {
  name = "searxng"
  id   = "searxng-do-host1"
  port = 8888
  tags = ["search", "web"]
  meta {
    host = "do-host1"
  }
  check {
    http     = "http://127.0.0.1:8888/"
    interval = "30s"
    timeout  = "5s"
  }
}

# ClawChat is a static WASM SPA served by nginx at /clawchat/ — no server process to register.
