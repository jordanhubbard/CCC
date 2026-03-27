# SPEC: Project Issues & PRs Panel

**Requested by:** jkh, 2026-03-23  
**Design:** Rocky  
**UI:** Natasha  
**Target:** `/projects/:id` detail page in RCC API (`rcc/api/index.mjs`)

---

## Goal

On the project detail page, show a live list of **open GitHub issues** and **open pull requests** for the project's repo — inline, without leaving RCC.

---

## New API Endpoint

### `GET /api/projects/:owner/:repo/github`

Fetches live GitHub data for a project via `gh` CLI. Public, no auth required (read-only).

**Response:**
```json
{
  "issues": [
    {
      "number": 42,
      "title": "Bug: scout dedup breaks on empty tags",
      "state": "open",
      "url": "https://github.com/your-org/your-repo/issues/42",
      "labels": ["bug", "priority:high"],
      "author": "your-github-username",
      "createdAt": "2026-03-20T10:00:00Z",
      "updatedAt": "2026-03-22T14:30:00Z",
      "commentCount": 3
    }
  ],
  "prs": [
    {
      "number": 7,
      "title": "feat: add per-agent capability registry",
      "state": "open",
      "url": "https://github.com/your-org/your-repo/pull/7",
      "author": "rocky-bot",
      "isDraft": false,
      "reviewDecision": "REVIEW_REQUIRED",
      "mergeable": "MERGEABLE",
      "createdAt": "2026-03-21T08:00:00Z",
      "updatedAt": "2026-03-23T09:00:00Z",
      "labels": []
    }
  ],
  "fetchedAt": "2026-03-23T17:48:00Z",
  "repo": "your-org/your-repo"
}
```

**Implementation notes:**
- Uses `gh issue list --repo <repo> --state open --limit 50 --json number,title,labels,url,author,createdAt,updatedAt,comments`
- Uses `gh pr list --repo <repo> --state open --limit 30 --json number,title,author,url,isDraft,reviewDecision,mergeable,createdAt,updatedAt,labels`
- If `gh` fails or repo is inaccessible: return `{ issues: [], prs: [], error: "..." }`
- Cache response for 5 minutes (in-memory) to avoid hammering GitHub API
- Endpoint is unauthenticated (same as other GET project endpoints)

---

## UI Changes (Natasha's domain)

### Layout: new "GitHub Activity" section on project detail page

Insert **between** the project header and the queue section:

```
┌─────────────────────────────────────────────────────────┐
│  GitHub Activity                          [↻ refresh]   │
│                                                         │
│  ● Issues (3 open)          ● Pull Requests (1 open)   │
│  ┌──────────────────────┐   ┌──────────────────────┐   │
│  │ #42 Bug: scout dedup │   │ #7 feat: capability  │   │
│  │ 🔴 bug  3 days ago   │   │ 🟡 Review required   │   │
│  ├──────────────────────┤   ├──────────────────────┤   │
│  │ #38 Add retry logic  │   │                      │   │
│  │ ✨ enhancement 5d ago│   │   (no more PRs)      │   │
│  └──────────────────────┘   └──────────────────────┘   │
│  View all on GitHub →                                   │
└─────────────────────────────────────────────────────────┘
```

### Visual design requirements (for Natasha):

**Issues list:**
- Each row: `#number title` + label badges + relative timestamp (`3 days ago`)
- Label colors: match GitHub label colors where possible; fallback to neutral pill
- Bug labels → red tint; enhancement → purple; good first issue → green
- Click row → opens GitHub issue in new tab
- Empty state: "No open issues 🎉"

**PR list:**
- Each row: `#number title` + review status chip + draft indicator
- Review status chips:
  - `APPROVED` → green ✓
  - `REVIEW_REQUIRED` → yellow ⚠
  - `CHANGES_REQUESTED` → red ✗
  - Draft → grey italic "(draft)"
- Click row → opens GitHub PR in new tab
- Empty state: "No open PRs"

**Section behavior:**
- Loads in parallel with existing queue fetch (third `Promise.all` leg)
- Loading state: spinner/skeleton in the section
- Error state: "GitHub data unavailable" with link to repo directly
- Refresh button: re-fetches `/api/projects/:id/github` (busts server cache)
- If project has no `github_url`: section is hidden entirely

**Columns:**
- Two-column layout on desktop (issues left, PRs right)
- Single column stacked on mobile/narrow (<640px)
- Max ~5 items shown per column; "Show N more →" link to full list

---

## Data flow

```
Browser
  └─ fetch /api/projects/:id          → project metadata
  └─ fetch /api/queue                 → work queue items  
  └─ fetch /api/projects/:id/github   → issues + PRs (NEW)
       └─ RCC API
            └─ gh issue list + gh pr list
```

---

## Files to modify

| File | Change |
|------|--------|
| `rcc/api/index.mjs` | Add `GET /api/projects/:owner/:repo/github` handler + 5-min cache |
| `rcc/api/index.mjs` | Update `projectDetailHtml()` to fetch + render GitHub section |

All UI work (CSS, HTML template, responsive layout) goes in `projectDetailHtml()` — that's where Natasha works her magic.

---

## Out of scope (v1)

- Closed issues/PRs (open only for now)
- Issue comments inline
- Filtering/searching issues within the panel
- Creating issues from RCC
- CI/check status on PRs (already partially handled by scout queue items)

---

## Acceptance criteria

- [ ] `/api/projects/:id/github` returns valid JSON for any registered repo with a `github_url`
- [ ] Issues and PRs appear on project detail page without page reload
- [ ] Clicking any issue/PR row opens the correct GitHub URL in a new tab
- [ ] Empty states render correctly (no open issues, no PRs, GitHub unavailable)
- [ ] No regressions to existing queue display or project header
- [ ] Works on mobile (single column)
