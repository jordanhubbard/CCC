# Service definitions for bullwinkle (puck) — CCC macOS developer node
# Registered with the local Consul agent at startup.
# Bullwinkle is a client-only node; services here are locally available
# but discoverable by the rest of the fleet.

service {
  name = "accfs"
  id   = "accfs-bullwinkle"
  port = 8791
  tags = ["storage", "fuse", "macos"]
  meta {
    node = "bullwinkle"
    desc = "AccFS FUSE filesystem (mounts /mnt/accfs)"
  }
  check {
    http     = "http://127.0.0.1:8791/health"
    interval = "15s"
    timeout  = "3s"
  }
}
