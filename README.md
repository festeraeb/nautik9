# NautiK9

A Rust notes-and-assistant server with a phone-friendly web UI. It is a rewrite of the design of Jade Vault (MIT) for a home-lab setup: your notes stay in a local SQLite vault, a chain of OpenAI-compatible model endpoints that you configure answers questions and drafts text replies, and the phone is reached over adb. See [PORTING.md](PORTING.md) for what is done and what is not.

This README describes what the code does today. Anything not built yet is under "Planned" and is marked that way.

## What it does today

- **Vault notes** (SQLite, `vault.db`): markdown text with `[[links]]` and `#tags` read from the content when you save. API and UI for backlinks, unresolved links, tag list and tag filter. Search is keyword matching over title, content and tags.
- **Model endpoints**: an ordered list in `~/.config/nautik9/providers.json` (default: a local Lemonade server at `127.0.0.1:13305/v1`; add more, such as a Paddler route or a hosted provider, to the list). Each is OpenAI-compatible. The first that answers wins. The answer is labelled with the model name the endpoint returned and the endpoint that answered. If none answer, the API returns HTTP 502 with the attempts, and the UI says so. There is no canned fallback text.
- **Work SMS** (Android phone over adb): reads the inbox, drafts a reply per new text with the model chain, and, when you approve, opens the phone's SMS composer with the reply filled in. It never sends; you press send on the phone. Status names are `Drafted`, `Approved`, `OpenedInComposer`, `NoModel`, `Ignored`. If no model answers there is no draft and the status is `NoModel`. The owner name is configurable (`workmode.json`, default "Thom") and replies use the name, not pronouns. Texts from other people are never given vault notes.
- **Fleet tab**: shows what the fleet aggregator (`--fleet-url`, default `http://127.0.0.1:9122`) returns, or the error if it cannot be reached.
- **Access control**: every `/api/*` route except `/api/health` needs the access token (`Authorization: Bearer <token>` or `?token=`). The token is created on first run in `~/.config/nautik9/token` (mode 0600). Open the UI once with `/?token=...`; the browser remembers it.
- **Mack avatar**: the companion picture and mood text in the UI (cosmetic).

## Run it

```bash
cargo build --release -p nautik9-app
# default: binds 0.0.0.0:8340 so the phone on the LAN can reach it
./target/release/nautik9-app --device s25ultra serve --port 8340
# this machine only:
./target/release/nautik9-app serve --bind 127.0.0.1
```

On start it prints the local and LAN URL and the token file path. On a terminal (or with `--print-token`) it also prints the URL with the token, once. The server speaks plain HTTP, so the token and your data cross the network unencrypted: use it on a trusted LAN or VPN and never port-forward it.

Config folder (`--config-dir`, default `~/.config/nautik9`):

| File | Purpose |
| :-- | :-- |
| `token` | access token, created on first run |
| `providers.json` | `{"providers":[{"name","base_url","model"?,"api_key"?,"offline"?}], "timeout_seconds", "max_tokens"}` |
| `workmode.json` | `owner_name`, `owner_numbers`, `custom_instructions`, `auto_reply`, ... |

If `model` is omitted the endpoint's loaded model is used (Lemonade `/api/v1/health`, else the first model from `/models`). A provider marked `"offline": true` is a real local endpoint (for example a Bonsai or burn server) that the "offline only" option may use.

`serve --sms-bridge adb` (default) reads texts from a phone on this machine's adb (USB or wireless debugging); `--sms-bridge app` is used by the Android app, which reads the inbox itself and opens the composer.

## Android app

`android/` builds an APK that runs this same server inside the phone on 127.0.0.1, with the vault on the phone, and llama.cpp's `llama-server` as an on-device model, so it works with no network. Texts are read by the app (READ_SMS); replies open in the phone's composer and you press send. Measured on a Galaxy S25 Ultra (CPU only) with Qwen3-4B-Instruct-2507 Q4_K_M: about 20 tokens/s generating, 80 tokens/s reading the prompt. See [android/README.md](android/README.md).

CLI: `status`, `search`, `chat`, `index`, `decompose` (a fixed checklist, not model output), `serve`. `--device` is a profile hint (`s25ultra`, `s24`, `baseline`, `desktop`); it is not probed from hardware.

## Layout

```
crates/
  nautik9-core/       module trait, event bus, SMS types, device profile hint, avatar state
  nautik9-wayfinder/  SQLite vault, links/tags/backlinks, keyword search
  nautik9-lemonade/   OpenAI-compatible provider chain, SMS prompts
  nautik9-fleet/      posts telemetry to a configured endpoint
  nautik9-app/        CLI, HTTP server, web UI (src/index.html)
```

## Planned (not built)

These appeared in earlier versions of this README. There is no code behind them yet.

- Fast on-device vector search. Today's "embedding" is a character-hash fingerprint, not semantic; real embeddings via an OpenAI-compatible `/embeddings` endpoint (embeddinggemma) are planned.
- Running models on the phone's NPU (the Android app runs them on the CPU).
- Streaming answers over SSE; a hosted endpoint.
- Real tree-of-thought reasoning (the endpoint now answers 501).
- TLS, a login for more than one user, a Discord/Vector bridge as in Jade.

## Tests

```bash
cargo test --workspace
```

Covers token auth, the 502 when no endpoint answers, fleet errors, SMS status names and prompts, and notes links/tags/backlinks. Tests never open the phone composer or read the phone.

## Credit

Design follows Jade Vault (MIT); this is a rewrite, not copied code. See NOTICE.
