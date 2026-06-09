# External Agents (Claude Code / Codex / Gemini)

Zedium supports external AI coding agents over ACP (Agent Client Protocol).
These are **local CLI tools** you install yourself — they run on your machine,
use your own API keys, and never route through any Zed-owned service.

## What changed in Zedium

Upstream Zed discovered agents through an online **ACP registry** (an "Add More
Agents" page that fetched an index and downloaded agent binaries on demand).
Zedium **removed the registry entirely** (patch 0036): it was an empty,
network-backed discovery surface and carried a dormant binary-download path.

Instead, you add agents directly as **custom agent servers** in `settings.json`.
This is fully offline — Zedium only launches the local command you specify.

## Setup

1. Install the agent's CLI yourself (e.g. via `npm`, `brew`, or the vendor's
   installer). Make sure it's on your `PATH`.
2. Add an entry under `agent_servers` in your `settings.json`. The key is the
   agent id; `command` is the executable to launch; `env` carries your BYO key.

```jsonc
{
  "agent_servers": {
    "claude-acp": {
      "type": "custom",
      "command": "claude-code-acp",
      "env": { "ANTHROPIC_API_KEY": "sk-ant-..." }
    },
    "codex-acp": {
      "type": "custom",
      "command": "codex",
      "args": ["acp"],
      "env": { "OPENAI_API_KEY": "sk-..." }
    },
    "gemini": {
      "type": "custom",
      "command": "gemini",
      "env": { "GEMINI_API_KEY": "..." }
    }
  }
}
```

Adjust `command`/`args` to match the binary names and ACP invocation your
installed CLIs actually use — Zedium passes them through verbatim.

3. Open the agent panel; your configured agents appear under **External Agents**.

## Notes

- **No Zed account, no sign-in.** Sign-in was removed (GAP-7 / patch 0037).
- **Keys are yours.** Set them in `env` above or export them in your shell before
  launching Zedium; nothing is sent anywhere except to the endpoint your CLI
  itself contacts.
- **Extension-provided agents** (agents shipped by an installed Zed extension)
  still work as before. Only the online registry discovery/install path was
  removed.
