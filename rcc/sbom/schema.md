# Agent SBOM Schema

Each agent node declares its software dependencies in a `<agent>.sbom.json` file. The SBOM is:
- Human-editable
- Mutable by agents themselves (via API or direct edit + commit)
- Enforced by `install-sbom.sh` on each sync
- Aggregated by the CCC hub for visibility

## Schema

```json
{
  "agent": "rocky",
  "version": "1.0.0",
  "updated": "2026-03-31T00:00:00Z",
  "description": "Optional human-readable description of this agent node",
  "packages": {
    "apt": ["git", "curl", "nodejs"],
    "npm": ["-g pm2", "@anthropic-ai/sdk"],
    "pip": ["requests", "numpy"],
    "brew": [],
    "yum": []
  },
  "tools": {
    "gh": { "version": ">=2.0", "check": "gh --version", "install": "apt" },
    "claude": { "version": "latest", "check": "claude --version", "install": "npm -g" }
  },
  "skills": ["gemini", "github", "weather"],
  "env_required": ["CCC_AGENT_TOKEN"],
  "env_optional": ["NVIDIA_API_KEY", "GITHUB_TOKEN", "SLACK_TOKEN"],
  "platform": "linux",
  "notes": "Optional freeform notes"
}
```

## Fields

| Field | Required | Description |
|---|---|---|
| `agent` | yes | Agent name (matches CCC registry) |
| `version` | yes | SBOM document version (semver) |
| `updated` | yes | ISO timestamp of last update |
| `packages.apt` | no | Debian/Ubuntu packages |
| `packages.npm` | no | Node.js packages (prefix with `-g` for global) |
| `packages.pip` | no | Python packages |
| `packages.brew` | no | Homebrew packages (macOS) |
| `packages.yum` | no | RPM packages (RHEL/CentOS) |
| `tools` | no | Named tool specs with version requirement + install method |
| `skills` | no | OpenClaw skills to install |
| `env_required` | no | Environment variables that must be set |
| `env_optional` | no | Optional environment variables |
| `platform` | no | `linux` or `macos` (default: auto-detect) |
| `notes` | no | Human-readable notes |

## Enforcement

Run `install-sbom.sh` to apply the SBOM for the current agent:

```bash
AGENT_NAME=rocky bash /path/to/rcc/sbom/install-sbom.sh
```

Or via API:
```bash
curl http://localhost:8789/api/sbom/rocky/install | bash
```

## Agent self-modification

Agents can propose additions to their own SBOM:

```bash
POST /api/sbom/rocky/propose
{"package_type": "npm", "name": "some-package", "reason": "needed for X task"}
```

This adds the package to the SBOM JSON and commits to git.
