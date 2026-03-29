# Projects as First-Class Objects in RCC

**Status:** SPEC — Natasha, 2026-03-28  
**Priority:** High  
**Owner:** Natasha  
**Crew review:** Rocky, Bullwinkle

---

## Problem

All agent activity flows through `#agent-shared` (C0AMNRSN9EZ), intermingled. You can't join a channel and get a clean picture of a single project's state — commits, work items, agent commentary, completions — all in one place over time.

---

## What's Already There

RCC already has most of the plumbing:

- **`repos.json`** — repo registry with `ownership.slack_channel` field (currently unused for routing)
- **`projects.json`** — overlay with `slack_channels[]` array (workspace + channel_id pairs)
- **`buildProjectFromRepo()`** — merges both into a project view
- **`PROJECTS_PATH`** → `./projects.json` — read/write helpers exist
- **`notifyJkhCompletion()`** — already posts to Slack on queue item completion
- **`/api/projects/:id/channel`** POST endpoint — registers a Slack channel to a project
- **`slackPost()`** — posts to any Slack channel via bot token
- **SquirrelBus** — fan-out message bus agents already use

What's missing: **routing**. When something happens on a project, nobody fans out to the project's channel.

---

## Solution

### 1. Project Slack Channel Fan-out

Add a `fanoutToProjectChannels(projectId, text)` helper in `index.mjs`:

```js
async function fanoutToProjectChannels(projectId, text) {
  if (!SLACK_BOT_TOKEN || !projectId) return;
  const projects = await readProjects();
  const project = projects.find(p => p.id === projectId);
  if (!project?.slack_channels?.length) return;
  for (const ch of project.slack_channels) {
    slackPost('chat.postMessage', {
      channel: ch.channel_id,
      text,
      mrkdwn: true,
    }).catch(e => console.warn(`[fanout] ${ch.channel_id}: ${e.message}`));
  }
}
```

### 2. Hook it into existing events

**Queue item completed** → in `notifyJkhCompletion` and `POST /api/item/:id/complete`, call:
```js
fanoutToProjectChannels(item.project || item.repo, 
  `✅ *${item.title}* completed by ${agent}\n${resolution}`);
```

**Queue item created** → in `POST /api/queue`, if item has `project` field:
```js
fanoutToProjectChannels(item.project, 
  `📋 New task: *${item.title}* (${item.priority})\n${item.description}`);
```

**Agent comments / journal entries** → in `POST /api/item/:id/comment`, if item has `project`:
```js
fanoutToProjectChannels(item.project,
  `💬 *${body.author}* on *${item.title}*: ${text}`);
```

**GitHub issues/PRs** (optional, phase 2) → Scout already polls repos; when it files items, include `project` field → automatic fan-out.

### 3. Project field on queue items

Add `project` as an optional field to queue items:
```js
const item = {
  ...
  project: body.project || null,  // e.g. "jordanhubbard/agentos"
  repo: body.repo || null,
};
```

Agents tag their work with `project: "owner/repo"` when posting to the queue.

### 4. Channel registration flow

Already exists at `POST /api/projects/:owner/:repo/channel`. 

Create channels in Slack, then register them:
```bash
# Register #project-agentos as the channel for jordanhubbard/agentos
curl -X POST http://146.190.134.110:8789/api/projects/jordanhubbard%2Fagentos/channel \
  -H "Authorization: Bearer $RCC_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"channel_id": "C...", "workspace": "omgjkh", "channel_name": "project-agentos"}'
```

### 5. Dashboard: project detail page update

`/projects/:id` already renders a project detail page with queue items and GitHub data.  
Add a "Slack Channels" section showing registered channels.

---

## Existing Projects to Wire Up

Based on `repos.json` and active work:

| Project | Suggested Channel |
|---------|------------------|
| jordanhubbard/agentos | #project-agentos |
| jordanhubbard/rockyandfriends | #project-rcc |
| (dashboard) | #project-dashboard |

---

## Implementation Plan

### Phase 1 — Core routing (1-2 hours)
1. Add `project` field to queue item schema
2. Add `fanoutToProjectChannels()` helper
3. Hook into `complete`, `create`, `comment` endpoints
4. Register channels for active projects

### Phase 2 — Scout integration (next session)
- Scout pump: include `project` field when filing items from repos
- GitHub webhook or poll: fan-out new issues/PRs to project channel

### Phase 3 — Dashboard (stretch)
- Add "Channels" tab to project detail page
- Show recent fan-out messages inline

---

## What Agents Need to Do

When posting a queue item, include `project`:
```json
{
  "title": "Fix WASM hot-swap pipeline",
  "project": "jordanhubbard/agentos",
  "assignee": "natasha",
  "priority": "high"
}
```

That's it. RCC handles the rest.

---

## Notes

- Fan-out is fire-and-forget (like `notifyJkhCompletion`) — don't block on Slack API
- Start with the three active projects; add more as work begins on them
- SquirrelBus could eventually be a subscriber too (agents get project updates via bus)
