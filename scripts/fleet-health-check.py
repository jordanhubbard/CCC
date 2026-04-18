#!/usr/bin/env python3
"""
CCC Fleet Health Check — runs every 10 minutes via Hermes cron.
Validates all core services from the perspective of every registered agent.
Places a unique sentinel on MinIO each pass; agents must confirm they can read it.
Outputs a JSON report to stdout for the cron job prompt to parse.
"""

import json
import os
import sys
import time
import hashlib
import urllib.request
import urllib.error
from datetime import datetime, timezone

# ── Config ─────────────────────────────────────────────────────────
CCC_API = os.environ.get("CCC_API", "http://localhost:8789")
QDRANT_URL = os.environ.get("QDRANT_URL", "http://localhost:6333")
MINIO_ENDPOINT = os.environ.get("MINIO_ENDPOINT", "http://localhost:9000")
TOKENHUB_URL = os.environ.get("TOKENHUB_URL", "http://localhost:8090")
SEARXNG_URL = os.environ.get("SEARXNG_URL", "http://localhost:8888")
OMGJKH_WEBHOOK = os.environ.get("OMGJKH_WEBHOOK", "")

# Load .ccc/.env for tokens
def load_env(path):
    if not os.path.exists(path):
        return
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith('#'):
                continue
            if '=' in line:
                k, v = line.split('=', 1)
                if k not in os.environ:
                    os.environ[k] = v

load_env(os.path.expanduser("~/.ccc/.env"))
load_env(os.path.expanduser("~/.hermes/.env"))

QDRANT_API_KEY = ""
try:
    import subprocess
    r = subprocess.run(["docker", "inspect", "qdrant"], capture_output=True, text=True, timeout=10)
    for env in json.loads(r.stdout)[0]["Config"]["Env"]:
        if "API_KEY" in env:
            QDRANT_API_KEY = env.split("=", 1)[1]
except Exception:
    pass

def http_get(url, headers=None, timeout=5):
    """Simple GET, returns (status_code, body_str) or (0, error_str)."""
    hdrs = headers or {}
    req = urllib.request.Request(url, headers=hdrs)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return (resp.status, resp.read().decode('utf-8', errors='replace'))
    except urllib.error.HTTPError as e:
        return (e.code, e.read().decode('utf-8', errors='replace') if e.fp else str(e))
    except Exception as e:
        return (0, str(e))

def http_post(url, data, headers=None, timeout=5):
    hdrs = {"Content-Type": "application/json"}
    if headers:
        hdrs.update(headers)
    body = json.dumps(data).encode()
    req = urllib.request.Request(url, data=body, headers=hdrs, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return (resp.status, resp.read().decode('utf-8', errors='replace'))
    except urllib.error.HTTPError as e:
        return (e.code, e.read().decode('utf-8', errors='replace') if e.fp else str(e))
    except Exception as e:
        return (0, str(e))

# ── Service Probes ─────────────────────────────────────────────────
def probe_service(name, url, headers=None, expect_in_body=None):
    t0 = time.monotonic()
    status, body = http_get(url, headers=headers, timeout=5)
    latency_ms = round((time.monotonic() - t0) * 1000)
    ok = status in (200, 204)
    if ok and expect_in_body and expect_in_body not in body:
        ok = False
    return {
        "name": name,
        "url": url,
        "ok": ok,
        "status": status,
        "latency_ms": latency_ms,
        "error": body[:200] if not ok else None,
    }

def check_core_services():
    """Probe all core infrastructure services on do-host1."""
    results = []
    results.append(probe_service("ccc-api", f"{CCC_API}/api/health", expect_in_body="ok"))
    results.append(probe_service("agentbus", f"{CCC_API}/api/health", expect_in_body="ok"))
    results.append(probe_service("qdrant", f"{QDRANT_URL}/healthz",
                                 headers={"api-key": QDRANT_API_KEY} if QDRANT_API_KEY else None))
    results.append(probe_service("minio", f"{MINIO_ENDPOINT}/minio/health/live"))
    results.append(probe_service("tokenhub", f"{TOKENHUB_URL}/v1/models"))
    results.append(probe_service("searxng", f"{SEARXNG_URL}/healthz"))

    # Redis — required by AccFS (JuiceFS metadata store)
    try:
        r = subprocess.run(["redis-cli", "-h", "127.0.0.1", "-p", "6379", "ping"],
                          capture_output=True, text=True, timeout=5)
        redis_ok = r.returncode == 0 and "PONG" in r.stdout
        results.append({
            "name": "redis",
            "url": "redis://127.0.0.1:6379",
            "ok": redis_ok,
            "status": 200 if redis_ok else 0,
            "latency_ms": 0,
            "error": None if redis_ok else f"redis-cli ping failed: {r.stdout.strip()} {r.stderr.strip()}",
        })
    except Exception as e:
        results.append({"name": "redis", "url": "redis://127.0.0.1:6379",
                        "ok": False, "status": 0, "latency_ms": 0, "error": str(e)})

    # AccFS S3 gateway (port 9100) — how remote nodes access shared filesystem
    results.append(probe_service("accfs-gateway", "http://127.0.0.1:9100/minio/health/live"))

    # AccFS FUSE mount — local POSIX access on rocky
    accfs_mounted = os.path.ismount("/mnt/accfs")
    results.append({
        "name": "accfs-fuse",
        "url": "/mnt/accfs",
        "ok": accfs_mounted,
        "status": "mounted" if accfs_mounted else "not mounted",
        "latency_ms": 0,
        "error": None if accfs_mounted else "/mnt/accfs is not mounted",
    })

    # Docker containers
    try:
        r = subprocess.run(["docker", "ps", "--format", "{{.Names}}|{{.Status}}"],
                          capture_output=True, text=True, timeout=10)
        expected = {"qdrant", "searxng"}
        running = {}
        for line in r.stdout.strip().split('\n'):
            if '|' in line:
                name, status = line.split('|', 1)
                running[name] = status
        for c in expected:
            ok = c in running and "Up" in running.get(c, "")
            results.append({
                "name": f"docker:{c}",
                "url": "docker",
                "ok": ok,
                "status": running.get(c, "not found"),
                "latency_ms": 0,
                "error": None if ok else f"container {c} not running",
            })
    except Exception as e:
        results.append({"name": "docker", "url": "docker", "ok": False, "status": 0,
                        "latency_ms": 0, "error": str(e)})

    # Tailscale
    try:
        r = subprocess.run(["tailscale", "status", "--json"], capture_output=True, text=True, timeout=10)
        ts = json.loads(r.stdout)
        results.append({
            "name": "tailscaled",
            "url": "tailscale",
            "ok": ts.get("BackendState") == "Running",
            "status": ts.get("BackendState", "unknown"),
            "latency_ms": 0,
            "error": None if ts.get("BackendState") == "Running" else "tailscale not running",
        })
    except Exception as e:
        results.append({"name": "tailscaled", "url": "tailscale", "ok": False,
                        "status": 0, "latency_ms": 0, "error": str(e)})

    return results

# ── Agent Health ───────────────────────────────────────────────────
def get_registered_agents():
    """Get agents from CCC API."""
    status, body = http_get(f"{CCC_API}/api/agents")
    if status != 200:
        return []
    data = json.loads(body)
    return data.get("agents", [])

def check_agent_health(agents):
    """Check each agent's heartbeat recency and connectivity."""
    results = []
    now = datetime.now(timezone.utc)
    for agent in agents:
        name = agent["name"]
        last_seen = agent.get("lastSeen", "")
        online = agent.get("online", False)

        # Parse lastSeen
        stale_minutes = None
        if last_seen:
            try:
                # Handle various ISO formats
                ls = last_seen.replace("+00:00", "+0000").replace("Z", "+0000")
                if "." in ls:
                    ls = ls[:ls.index(".")] + ls[ls.index("+"):]
                dt = datetime.strptime(ls[:24], "%Y-%m-%dT%H:%M:%S%z")
                stale_minutes = (now - dt).total_seconds() / 60
            except Exception:
                pass

        agent_ok = True
        issues = []

        # Agent is stale if no heartbeat in 10 minutes
        if stale_minutes is not None and stale_minutes > 10:
            agent_ok = False
            issues.append(f"last heartbeat {stale_minutes:.0f}m ago")

        if not online:
            # Not necessarily a problem — agent may be legitimately offline
            issues.append("marked offline")

        results.append({
            "name": name,
            "host": agent.get("host", "unknown"),
            "online": online,
            "ok": agent_ok,
            "stale_minutes": round(stale_minutes) if stale_minutes else None,
            "issues": issues,
        })
    return results

# ── Sentinel File ──────────────────────────────────────────────────
def write_sentinel():
    """Write a sentinel file to MinIO for agents to verify."""
    sentinel_id = hashlib.md5(str(time.time()).encode()).hexdigest()[:12]
    ts = datetime.now(timezone.utc).isoformat()
    sentinel = {"id": sentinel_id, "ts": ts, "from": "rocky"}
    try:
        r = subprocess.run(
            ["mc", "pipe", "local/agents/shared/health-sentinel.json"],
            input=json.dumps(sentinel).encode(),
            capture_output=True, timeout=10
        )
        return sentinel_id if r.returncode == 0 else None
    except Exception:
        return None

def read_sentinel():
    """Read the sentinel back from MinIO to verify storage is working."""
    try:
        r = subprocess.run(
            ["mc", "cat", "local/agents/shared/health-sentinel.json"],
            capture_output=True, text=True, timeout=10
        )
        if r.returncode == 0:
            data = json.loads(r.stdout)
            return data.get("id")
    except Exception:
        pass
    return None

# ── Remote Node AccFS Access ─────────────────────────────────────
FLEET_NODES = {
    "sparky": {"ip": "100.87.229.125", "user": "jkh", "mc_path": "~/bin/mc"},
    "puck": {"ip": "100.87.68.11", "user": "jkh", "mc_path": "~/bin/mc"},
}

def check_remote_accfs():
    """Verify remote fleet nodes can reach the AccFS S3 gateway."""
    results = []
    for node, cfg in FLEET_NODES.items():
        try:
            # Use a command that outputs a known marker on success
            cmd = f"{cfg['mc_path']} ls accfs/accfs/ >/dev/null 2>&1 && echo ACCFS_OK || echo ACCFS_FAIL"
            r = subprocess.run(
                ["ssh", "-o", "ConnectTimeout=5", "-o", "StrictHostKeyChecking=no",
                 f"{cfg['user']}@{cfg['ip']}", cmd],
                capture_output=True, text=True, timeout=15,
            )
            output = r.stdout.strip()
            ok = "ACCFS_OK" in output
            error = None
            if not ok:
                # Grab stderr for diagnostics
                error = r.stderr.strip()[:200] or output[:200] or "mc ls failed (no output)"
            results.append({
                "name": f"accfs-access:{node}",
                "ok": ok,
                "error": error,
            })
        except subprocess.TimeoutExpired:
            results.append({
                "name": f"accfs-access:{node}",
                "ok": False,
                "error": f"SSH to {node} timed out",
            })
        except Exception as e:
            results.append({
                "name": f"accfs-access:{node}",
                "ok": False,
                "error": str(e)[:200],
            })
    return results

# ── Tokenhub Provider Health ──────────────────────────────────────
def check_tokenhub_providers():
    """Check tokenhub provider health via admin API."""
    admin_token = os.environ.get("TOKENHUB_ADMIN_TOKEN", "")
    if not admin_token:
        # Try loading from tokenhub env
        tokenhub_env = os.path.expanduser("~/.tokenhub/.env")
        if os.path.exists(tokenhub_env):
            with open(tokenhub_env) as f:
                for line in f:
                    if line.startswith("TOKENHUB_ADMIN_TOKEN="):
                        admin_token = line.strip().split("=", 1)[1]
                        break
    if not admin_token:
        return []

    status, body = http_get(
        f"{TOKENHUB_URL}/admin/v1/health",
        headers={"Authorization": f"Bearer {admin_token}"},
        timeout=10,
    )
    if status != 200:
        return []

    try:
        data = json.loads(body)
        results = []
        for prov in data.get("providers", data.get("items", [])):
            name = prov.get("provider_id", prov.get("name", prov.get("id", "unknown")))
            state = prov.get("state", "")
            ok = state == "healthy"
            results.append({
                "name": name,
                "ok": ok,
                "state": state,
                "error": prov.get("last_error") if not ok else None,
            })
        return results
    except Exception:
        return []

# ── Main ───────────────────────────────────────────────────────────
def main():
    report = {
        "timestamp": datetime.now(timezone.utc).isoformat(),
        "hub": "rocky@do-host1",
    }

    # 1. Core services
    services = check_core_services()
    report["services"] = services
    svc_ok = sum(1 for s in services if s["ok"])
    svc_total = len(services)

    # 2. Sentinel file round-trip
    sentinel_id = write_sentinel()
    if sentinel_id:
        readback = read_sentinel()
        sentinel_ok = readback == sentinel_id
    else:
        sentinel_ok = False
    report["sentinel"] = {
        "ok": sentinel_ok,
        "id": sentinel_id,
        "verified": sentinel_ok,
    }

    # 3. Agent health
    agents = get_registered_agents()
    agent_results = check_agent_health(agents)
    report["agents"] = agent_results
    agents_ok = sum(1 for a in agent_results if a["ok"])
    agents_total = len(agent_results)

    # 4. Summary
    all_ok = all(s["ok"] for s in services) and sentinel_ok
    failed_services = [s["name"] for s in services if not s["ok"]]
    stale_agents = [a["name"] for a in agent_results if not a["ok"]]

    report["summary"] = {
        "all_ok": all_ok,
        "services_ok": f"{svc_ok}/{svc_total}",
        "agents_ok": f"{agents_ok}/{agents_total}",
        "sentinel_ok": sentinel_ok,
        "failed_services": failed_services,
        "stale_agents": stale_agents,
    }

    # 5. Tokenhub provider health
    tokenhub_providers = check_tokenhub_providers()
    if tokenhub_providers:
        report["tokenhub_providers"] = tokenhub_providers

    # 6. Remote node AccFS access
    remote_accfs = check_remote_accfs()
    if remote_accfs:
        report["remote_accfs"] = remote_accfs
        failed_remote = [r["name"] for r in remote_accfs if not r["ok"]]
        if failed_remote:
            report["summary"]["failed_services"].extend(failed_remote)

    # Output JSON for the cron prompt
    print(json.dumps(report, indent=2))

if __name__ == "__main__":
    main()
