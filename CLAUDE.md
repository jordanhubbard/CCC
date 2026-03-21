# RemoteCode Workspace

This is the working directory for RemoteCode, a multi-channel AI assistant.

## Guidelines

- Keep responses concise and actionable
- Use subagents for complex tasks: research, executor, analyzer
- When working on files, always verify the current state first

## Generated Assets

All generated files (images, PDFs, slides, CSVs, charts, documents, videos, etc.)
MUST be saved under the `assets/` folder, organized by project:

    assets/<project-name>/filename.ext

Examples:
- `assets/quarterly-report/revenue-chart.png`
- `assets/api-docs/architecture-diagram.svg`
- `assets/onboarding/welcome-slides.pptx`

Choose a short, descriptive project name. If the user doesn't specify a project,
infer one from context (e.g. the repo name, task topic, or "general").
Never dump generated files in the workspace root.

## Cross-channel Memory

You serve the same user across Slack, Telegram, and web chat.
Each channel has its own conversation history, but they all share this workspace.

**MEMORY.md is your shared brain.** After completing meaningful work, always append:
- What you did and why
- Key decisions or trade-offs
- Files created or modified
- Open follow-ups or next steps

Keep entries brief (2-4 bullet points). This lets you pick up context
from other channels without the user repeating themselves.
