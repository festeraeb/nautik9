# Porting Jade Vault's intent to NautiK9

NautiK9 follows the design of **Jade Vault** (MIT, lucky-edgelord/jade-vault; v2.16.1, agent work checked against v2.20.0). It is a rewrite in Rust for a home-lab setup. No Jade source code is copied; Jade's README, SECURITY.md and docs were read for intent. Thanks to the Jade Vault authors.

## Intent principles the port honours

1. **Your data stays yours.** Nothing leaves except to the model endpoint you chose. The API needs a token (except `/api/health` and the compiled-in `/static/` files), the server warns that it speaks plain HTTP unless you start it with `--tls`, and it is meant for a trusted LAN or VPN only.
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
| Note search | done (meaning part needs an endpoint you configure) | `GET /api/notes?q=&tag=&k=&mode=` (module `notes_api.rs`): keyword ranking over title/content/tags (title 3, tag 2, content 1 per hit); with `embeddings` set in `agent.json` the notes are also ranked by meaning through `nautik9-agent` `memory.rs` (collection `notes`, kept equal to the vault on each search; embeds only new or changed chunks) and the two lists are fused with its reciprocal rank fusion. The reply says `semantic: true/false`; a dead endpoint falls back to keyword with a `note`; `mode=keyword` turns the meaning part off. The old character-hash fingerprint is deleted, so nothing claims to be semantic when it is not. Not done: the agent's `notes` tool search and the Work SMS draft context are still keyword only; no automatic re-index on edit (it happens on the next search) |
| Hybrid memory search + `embeddings` tool | done (needs an endpoint you configure) | `nautik9-agent` `memory.rs`: any OpenAI-compatible `/v1/embeddings` endpoint set in `agent.json` `embeddings: {url, model, api_key_env?}`; without it semantic is OFF and every report says "keyword only" (no fake vectors). SQLite index `vectors.db` (`index_path`): chunks + FTS5 bm25 (porter) + f32 BLOB vectors, brute-force cosine in Rust (no sqlite-vec), reciprocal rank fusion k=60, modes hybrid/semantic/keyword. Collections `memory`, `journal`, `notes` (index action) and any name. Actions search/add/index/remove/compare/status, owner only; `POST /api/agent/embeddings`. Unlike Jade: no automatic re-index a few seconds after a note changes (memory resyncs when the prompt is built, notes on `index`); no `nomic` query prefixes; an embedding-model change drops the stored vectors and re-embeds on the next index/sync. Checked live against a local embeddinggemma 768-D server on 10 test notes |
| Graph view | done | `GET /api/notes/graph`: note, tag and missing-note nodes; link and tag edges (links matched by id or title; self links and `![[image]]` embeds make no edge). The Vault tab draws it on a canvas with a small force layout in plain JS (drag, click to open; best 500 nodes when larger). No local-graph (neighbours of one note) or filters |
| Math (KaTeX) | done | `$...$` and `$$...$$` in the preview and in note embeds. KaTeX 0.16.11 (MIT, licence file included) is compiled into the binary under `/static/katex/` (JS, CSS, woff2 fonts only), so it works offline; it loads when a note first has math. No `\(...\)` delimiters, no `\ce` chemistry |
| `![[image.png]]`, `![[note]]` embeds | done | `POST /api/attachments?name=` (raw body, 25 MB, png/jpg/gif/webp/bmp/avif/pdf/audio/video/txt; svg refused) and `GET /api/attachments/{name}` store in `attachments/` next to the vault; token-protected, one plain file name only (traversal tested), never overwrites (`a-2.png`), served `nosniff` with a sandbox CSP. The preview fetches files with the token and shows images (`\|300` width), audio, video, PDF links and whole notes (two levels deep, self-embed stops). No attachment list or delete, no `![[note#heading]]` partial embed, no image paste or camera capture |
| Split editor | done | Vault tab workspace: Edit, Edit + preview (side by side on wide screens), Two notes (two editors with previews side by side), Graph. Live preview while typing. Plain JS in `static/notes-plus.js`; the Markdown preview is a small renderer (headings, lists, quotes, code, bold/italic, links; no tables) |
| Token-protected API, health open | done | bearer or `?token=`, constant-time compare, token file mode 0600 |
| Plain-HTTP warning, LAN-only guidance | done | printed on start; `--bind 127.0.0.1` for local only |
| TLS | done (opt-in) | `serve --tls` uses `tls-cert.pem` / `tls-key.pem` in the config dir and makes a self-signed pair (localhost, 127.0.0.1, ::1 and this machine's LAN address) on first run, key mode 0600; `--tls-cert F --tls-key F` use your own PEM files. rustls with the ring provider (`axum-server`). Plain HTTP stays the default with its warning. A self-signed certificate encrypts but must be trusted once by the browser or phone; the Android app is not set up to pin it. Tested: the TLS listener answers `/api/health` and still wants the token elsewhere. No certificate renewal or reload without a restart |
| Any OpenAI-compatible provider | done | `providers.json`, optional per-provider key and model |
| Provider fallback chain | done | ordered; first that answers wins |
| Honest model errors (502) | done | `/api/chat` 502 with attempts; SMS status `NoModel` |
| Provider/model label on answers | done | model field returned plus endpoint |
| Context guard (trims old turns, cuts long tool results, learns the limit) | done | `nautik9-agent` `fit_context`; summarising compaction not yet |
| Chat sessions, history, Retry, 👍/👎 | done | `/api/agent/sessions*`; the web chat keeps one session, shows tools used, and puts 👍 / 👎 and Retry under the latest agent reply (commit 38f9edf); no session list or switcher in the UI |
| Owner versus other callers | done | SMS: other people get no notes in the prompt; owner numbers configurable. Agent tools are owner-only (`Tool::owner_only` defaults to true; `ToolCtx.owner` and the registry offer none to a non-owner caller), and only the token holder reaches `/api/agent/*` |
| Secretary inbox (messages for the owner) | partial | Work SMS list with drafts you approve; not a general inbox |
| Messenger bridges (Vector, Discord) | not planned | the phone SMS bridge over adb is the equivalent here |
| Agent tools | partial | notes search/read/list/write/edit, memory, web_search (SearXNG, a home server, DuckDuckGo), web_fetch (HTML to Markdown), http_request, chat_history, skill, delegate, and every tool of configured MCP servers (the CESARops fleet MCP; a phone reaches it through its home server). No local shell/node/Python sandbox |
| Schedules (prompt runs) | done (API + tool) | `schedule` tool and `/api/agent/schedules*`; 5-field cron or simple English (`every N minutes\|hours`, `hourly`, `daily at`, `weekdays at`, `every monday at`), unknown text rejected; local time zone; each schedule has its own session "⏰ <spec>". Only kind `prompt`: a `flow` kind is not built. Missed runs while the server is down are not replayed. DST: a skipped wall-clock time runs once just after the gap, a repeated one runs at its first occurrence. No UI yet |
| `Jade/crontab` note | done | lines `<5 cron fields> prompt <text>`, re-read every 30 s tick; bad lines go to `Jade/Logs/cron.log` once, never skipped silently. Entries are in memory only: a restart recomputes their next run from "now" |
| Heartbeat (`Jade/HEARTBEAT.md`) | done (opt-in) | `heartbeat_minutes` in `agent.json` (default 0 = off); one turn in the session "♥ Heartbeat" with the note as prompt, skipped while the note is empty. First beat comes N minutes after start |
| Run log `Jade/Logs/cron.log` | done | one line per run (ok/ERROR) appended by the runner, newest 500 lines kept; one run at a time |
| Memory, SOUL/AGENTS/SYSTEM steering, skills | done | `Jade/Memory.md` bullets (keyword-ranked when long), `Jade/SOUL.md`, `AGENTS.md` + `Jade/AGENTS.md`, `Jade/SYSTEM.md`, `Jade/skills/<name>/SKILL.md`. With an embedding endpoint a long Memory.md is ranked by hybrid search on the last two user messages (`memory_k`, default 8); without one, by shared words. The `memory` tool's own `search` action is still plain keyword |
| Journal | done (model-assisted) | `POST /api/agent/journal {date?}` (default yesterday): one model summary per chat from the stored user/assistant messages into `Jade/Journal/YYYY-MM-DD.md` (with session ids), linked from `Jade/Journal.md`, entries also in the `journal` search collection. No model answering = nothing written, error returned. Not a model-facing tool; no automatic nightly run yet (a schedule can ask for one); a summary is the model's, not a transcript |
| Memory, SOUL/AGENTS/SYSTEM steering, skills | done | `Jade/Memory.md` bullets (keyword-ranked when long), `Jade/SOUL.md`, `AGENTS.md` + `Jade/AGENTS.md`, `Jade/SYSTEM.md`, `Jade/skills/<name>/SKILL.md`. Journal and embedding-ranked memory not yet |
| Flows (n8n-style automations) | not planned | Playbooks (next row) cover the deterministic part; there is no flow editor or trigger graph |
| Playbooks (deterministic JSON automations) | partial | `nautik9-agent` `playbook.rs`: notes `Jade/Playbooks/<name>.json`; tasks `do` = any tool, or `set`, `fail`, `assert`, `ask`, `block`; `when`, `loop` (cap 200), `register`, `ignore_errors`, `timeout_s`, `tags`/`skip_tags`; safe expression language and `{{ }}` templates (undeclared name = error; regex on a linear-time engine with size caps); approval = SHA-256 of the JSON in the sessions DB, a changed file needs re-approval, only approved playbooks run with trigger `schedule`; `playbook` tool and `/api/agent/playbooks*`. Not done: `retries/until`, `failed_when`, `rescue/always`, `loop_var`, `debug/wait/save`, browser actions, `allow_hosts`, a Playbooks tab in the UI, and no scheduler calls it yet (schedules are not ported); Jade's playbooks forbid model calls, here `ask` is allowed |
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
