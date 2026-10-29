# Porting Jade Vault's intent to NautiK9

NautiK9 follows the design of **Jade Vault** (MIT, lucky-edgelord/jade-vault; v2.16.1, agent work checked against v2.20.0). It is a rewrite in Rust for a home-lab setup. No Jade source code is copied; Jade's README, SECURITY.md and docs were read for intent. Thanks to the Jade Vault authors.

## Intent principles the port honours

1. **Your data stays yours.** Nothing leaves except to the model endpoint you chose. The API needs a token (except `/api/health`), the server warns that it speaks plain HTTP, and it is meant for a trusted LAN or VPN only.
2. **Owner versus others.** Messages from other people never get your vault notes or tools. Messages for the owner land in an inbox (the Work SMS list) for review.
3. **Any OpenAI-compatible provider, with a fallback chain you configure.** Honest errors: when nothing answers the API returns 502 and the SMS draft is left empty. Nothing is invented.
4. **Notes are markdown** with `[[links]]`, backlinks and `#tags`.
5. **Reports and UI say what ran.** The answer shows the model and endpoint that produced it. Hints are labelled as hints. Planned features are labelled planned.

## Feature table

| Jade Vault feature | NautiK9 status | Notes |
| :-- | :-- | :-- |
| Markdown notes | done | SQLite vault, create/list/delete via API and UI |
| `[[links]]`, aliases `[[A\|label]]`, `[[A#heading]]` | done | parsed from content; code spans and fences ignored |
| Backlinks, unresolved (missing) links | done | `GET /api/notes/{id}/backlinks`; matched by title or id |
| `#tags`, nested tags (`#a/b`), tag filter | done | `GET /api/tags`, `GET /api/notes?tag=`; parent tag finds nested |
| Note search | partial | keyword search only. The vector part is a character-hash fingerprint, not semantic |
| Semantic search / embeddings | planned | via an OpenAI-compatible `/embeddings` endpoint (embeddinggemma) |
| Graph view, KaTeX, `![[image]]`, embeds, split editor | not planned | |
| Token-protected API, health open | done | bearer or `?token=`, constant-time compare, token file mode 0600 |
| Plain-HTTP warning, LAN-only guidance | done | printed on start; `--bind 127.0.0.1` for local only |
| TLS | planned | |
| Any OpenAI-compatible provider | done | `providers.json`, optional per-provider key and model |
| Provider fallback chain | done | ordered; first that answers wins |
| Honest model errors (502) | done | `/api/chat` 502 with attempts; SMS status `NoModel` |
| Provider/model label on answers | done | model field returned plus endpoint |
| Context guard (trims old turns, cuts long tool results, learns the limit) | done | `nautik9-agent` `fit_context`; summarising compaction not yet |
| Chat sessions, history, Retry, 👍/👎 | done (API) | `/api/agent/sessions*`; the web UI keeps one session and shows tools used; Retry/thumbs buttons not in the UI yet |
| Owner versus other callers | partial | SMS: other people get no notes in the prompt; owner numbers configurable. No tool use exists to restrict yet |
| Secretary inbox (messages for the owner) | partial | Work SMS list with drafts you approve; not a general inbox |
| Messenger bridges (Vector, Discord) | not planned | the phone SMS bridge over adb is the equivalent here |
| Agent tools | partial | notes search/read/list/write/edit, memory, web_search (SearXNG, a home server, DuckDuckGo), web_fetch (HTML to Markdown), http_request, chat_history, skill, delegate, and every tool of configured MCP servers (the CESARops fleet MCP; a phone reaches it through its home server). No local shell/node/Python sandbox |
| Memory, SOUL/AGENTS/SYSTEM steering, skills | done | `Jade/Memory.md` bullets (keyword-ranked when long), `Jade/SOUL.md`, `AGENTS.md` + `Jade/AGENTS.md`, `Jade/SYSTEM.md`, `Jade/skills/<name>/SKILL.md`. Journal and embedding-ranked memory not yet |
| Schedules, crontab, heartbeat | not planned | |
| Flows (n8n-style automations) | not planned | |
| Scripting (`jade.*`), note scripts, templates | not planned | |
| Trajectory log / fine-tuning export | done | every model call in `trajectories.jsonl`; `/api/agent/export?format=openai\|sharegpt&rated=up` |
| Webxdc mini apps | not planned | |
| Live2D avatar, TTS | not planned | NautiK9 has the Mack picture and mood text only |
| Android wrapper app | done (CPU) | `android/`: WebView over the server running in the app, on-device llama-server for offline answers, inbox read by the app, no adb needed |
| Secrets as `{{NAME}}` for http_request | done | `secrets.json` in the config dir; values are masked in every tool result |
| Tree-of-thought reasoner (not in Jade) | planned | `/api/tot` answers 501; the old output was a fixed template |

## Known gaps and honest caveats

- `providers.json` may hold API keys in plain text. The file is not created automatically; keep it mode 0600.
- Tags and links are computed from note content when listed, so older notes get them too.
- The device profile (`--device`) is a hint chosen by the user; the app does not probe the phone or run models on it.
- `decompose` is a fixed 8-step checklist with the task name filled in. It does not call a model.
- Fleet telemetry posts to the configured endpoint if reachable; it has no auth of its own.
- SMS replies are only ever opened in the phone's composer. The user presses send.
