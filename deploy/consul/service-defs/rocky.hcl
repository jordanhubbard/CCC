# Service definitions for rocky (do-host1) — CCC hub node
# Registered with the local Consul server at startup.
# These are all services that CCC requires on the hub and that other
# fleet nodes may discover via <name>.service.consul.

service {
  name = "acc-server"
  id   = "acc-server-rocky"
  port = 8789
  tags = ["hub", "api", "dashboard"]
  meta {
    node    = "rocky"
    desc    = "CCC API server (Rust/Axum) + ClawChat SPA"
  }
  check {
    http     = "http://127.0.0.1:8789/api/health"
    interval = "15s"
    timeout  = "3s"
  }
}

service {
  name = "tokenhub"
  id   = "tokenhub-rocky"
  port = 8090
  tags = ["hub", "llm-router"]
  meta {
    node = "rocky"
    desc = "LLM request router / token accounting"
  }
  check {
    http     = "http://127.0.0.1:8090/health"
    interval = "15s"
    timeout  = "3s"
  }
}

service {
  name = "qdrant"
  id   = "qdrant-rocky"
  port = 6333
  tags = ["hub", "database", "vector"]
  meta {
    node       = "rocky"
    collection = "agent_memories"
    desc       = "Qdrant vector database"
  }
  check {
    http     = "http://127.0.0.1:6333/collections"
    interval = "15s"
    timeout  = "3s"
  }
}

service {
  name = "minio"
  id   = "minio-rocky"
  port = 9000
  tags = ["hub", "storage", "s3"]
  meta {
    node    = "rocky"
    console = "9001"
    desc    = "MinIO S3-compatible object storage (AccFS backend)"
  }
  check {
    http     = "http://127.0.0.1:9000/minio/health/live"
    interval = "15s"
    timeout  = "3s"
  }
}

service {
  name = "searxng"
  id   = "searxng-rocky"
  port = 8888
  tags = ["hub", "search"]
  meta {
    node = "rocky"
    desc = "SearXNG privacy-respecting metasearch"
  }
  check {
    http     = "http://127.0.0.1:8888/"
    interval = "30s"
    timeout  = "5s"
  }
}

service {
  name = "prometheus"
  id   = "prometheus-rocky"
  port = 9090
  tags = ["hub", "metrics", "monitoring"]
  meta {
    node = "rocky"
    desc = "Prometheus metrics collection"
  }
  check {
    http     = "http://127.0.0.1:9090/-/healthy"
    interval = "30s"
    timeout  = "5s"
  }
}

service {
  name = "grafana"
  id   = "grafana-rocky"
  port = 3000
  tags = ["hub", "metrics", "dashboard"]
  meta {
    node = "rocky"
    desc = "Grafana metrics dashboards"
  }
  check {
    http     = "http://127.0.0.1:3000/api/health"
    interval = "30s"
    timeout  = "5s"
  }
}
