# NautiK9 for Android

The app runs the NautiK9 server inside the phone and shows its page in a WebView. Nothing needs a
computer or adb once it is installed.

- **Server**: `libnautik9.so` is the `nautik9-app` binary built for arm64. The app starts it as
  `serve --bind 0.0.0.0 --allow-from loopback,tailnet --sms-bridge app`; the vault and config
  folder are in the app's private storage. The bind is wide but the server's source gate answers
  only this phone, and the tailnet while remote care is on (see "Remote care").
- **Offline model**: `libllama_server.so` is llama.cpp's `llama-server`. The page starts it on
  127.0.0.1:8341 when an answer is needed and a `.gguf` is in the app's `models` folder, and the
  app stops it after 10 idle minutes (it wakes in a few seconds when asked again). Swiping the
  app away stops the model, the server and the foreground service (with remote care on, only the
  model). The page offers a download of
  Qwen3-4B-Instruct-2507 Q4_K_M (2.5 GB) through the system download manager. On a Galaxy S25
  Ultra (CPU only) it generated about 20 tokens/s and read prompts at about 80 tokens/s.
- **Texts**: the app holds READ_SMS and pushes the newest inbox rows to the server
  (`/api/sms/ingest`). Approving a draft opens the phone's composer with it; you press send. The app
  never sends a text.
- **Providers**: on first start `providers.json` lists only the on-device model. To ask a home
  Lemonade first when the phone can reach it, add it above the on-device entry (see below).

## Online and offline

The app tells the page the phone's connectivity (`window.n9OnNetwork`, badge `ONLINE WIFI` /
`ONLINE CELL` / `OFFLINE`) and the page tells the server (`POST /api/net {"online": bool}`).
Offline: notes, search, texts, GPS, torch, haptic and the on-device model work; remote endpoints
are skipped at once ("skipped, the phone is offline"); `web_search`, `web_fetch`, `weather` and
`http_request` answer "This needs an internet connection and the phone is offline right now.";
the Fleet Deck and the model download say they need a connection. Nothing is invented offline.
`GET /api/net` reports `online` and `local_model_idle_seconds`.

## Nexus

The Nexus tab joins a Lemonade Nexus network (crate `nexus-join` over the Nexus SDK's C API,
`nexus_api.rs`, panel `static/nexus-panel.js`, the same panel Jade uses).

- **SDK**: `liblemonade_nexus_sdk.so` for android-arm64-v8a (lemonade-nexus
  `scripts/build-sdk-mobile.sh`), copied into the APK when `NEXUS_SDK_SO` points at it. The server
  loads it at run time; without it the tab says the SDK is missing.
- **Invite**: paste the invite file, load it, or have it pushed with `POST /api/nexus/offer` (over
  remote care). The tab shows network, server, expiry and grants; nothing joins until Join is pressed.
  The invite is single-use, kept 0600 until the join, and never returned by any route.
- **Address map**: a phone that reaches LAN-only servers over Tailscale needs an invite with
  `address_map` (advertised name or LAN address -> tailnet address) and an SDK with
  `ln_set_address_map`.
- **Network change**: the app posts `/api/nexus/network` when the phone moves between Wi-Fi and
  cellular; the SDK opens a new socket and handshakes on the new path (no rejoin).
- **Mesh route for models**: a `providers.json` entry with `"nexus": {"peer", "port", "path"}` is
  reached through the mesh when joined and skipped with the reason otherwise; keep the tailnet entry
  after it as the fallback.
- Test: `e2e/nexus-check.mjs --invite FILE [--leave] [--shots DIR]` presses Check invite, Join and Leave.

## Remote care

Remote care lets the home fleet reach the phone over Tailscale when adb cannot (Samsung turns
Wireless debugging off at every reboot, and away from home there is no LAN). It is a switch in
Settings > Remote care, off by default, saved as `files/config/remote_care.json`.

- **Who gets in**: the server checks the peer address before the token. This phone (127.0.0.1)
  always; a tailnet peer (100.64.0.0/10, fd7a:115c:a1e0::/48) only while the switch is on; Wi-Fi,
  LAN and carrier peers never (403, empty body). Every route but `/api/health` still needs the
  token. `/api/health` answers a tailnet peer even with the switch off, so the fleet can tell the
  app is up.
- **The card** shows the switch, the phone's Tailscale address and port, the running build, and
  "Copy pairing line" (`http://<tailscale addr>:8340/?token=...` on the clipboard; the token is
  not shown).
- **After a reboot** (once the phone has been unlocked) or an app update, the app starts its
  server again by itself when the switch is on, and keeps a notification "Remote care on: answers
  the home fleet over Tailscale". Swiping the app away keeps the server running; only the
  on-device model stops. The app must have been opened once since install, and a force stop
  (Settings > Apps > Force stop) disables the boot start until it is opened again. If Samsung
  puts NautiK9 in "Sleeping apps", the boot start can be blocked; set its battery use to
  Unrestricted.
- **Pushing an update** from t440 (the phone needs the switch on and Tailscale up):

  ```bash
  cd android/build && python3 -m http.server 8399 --bind 100.72.182.77 &   # serve the APK on the tailnet only
  curl -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
       -d "{\"url\": \"http://100.72.182.77:8399/nautik9-0.9.0.apk\", \"sha256\": \"$(sha256sum nautik9-0.9.0.apk | cut -d' ' -f1)\"}" \
       http://100.72.127.73:8340/api/admin/update
  ```

  The phone downloads it (plain http only to loopback or a tailnet address, https anywhere,
  200 MB cap), checks the sha256 (a mismatch deletes it and answers 422), and queues an install.
  Within 20 s the app opens Android's install confirmation and posts a notification "NautiK9
  update ready"; **one tap on the phone installs it**. The first time, Android also asks to allow
  "Install unknown apps" for NautiK9. Android only accepts an APK signed with the same key as the
  installed app. Check the result with `GET /api/admin/version` (`app` is the APK version).
  `GET /api/admin/pending` lists what is still waiting; a declined or failed install is reported
  to `server.log` and removed.
- **Still needs adb**: reading `server.log` / `llm.log`, and reading the token the first time
  (`adb shell run-as com.festeraeb.nautik9 cat files/config/token`, or the pairing line from the
  card sent to yourself).
- **Open**: a carrier that lets subscribers in the same CGNAT pool reach each other could deliver
  a 100.64/10 source over cellular; such a peer passes the gate but still needs the token. Checking
  that the connection arrived on the Tailscale interface's own address would close that.

## Button debugging on demand (Wireless debugging off by default)

Samsung turns Wireless debugging off at every reboot, and leaving it on costs battery. So updates
go through remote care (no adb needed, see above; `android/push-update.sh` does the push), and
Wireless debugging is turned on by the app only when it is needed, then off again.

It is turned on in exactly two cases:

1. **The fleet asks.** `POST /api/admin/debug/session` (token, over Tailscale with remote care on).
   The phone shows "Turn on Wireless debugging for a button test?" as a notification with
   **Yes / Not now** (and as a dialog when the app is open). Only Yes turns it on. Read the answer
   at `GET /api/admin/debug` (`device.prompt.answer`: waiting, yes, not_now, expired,
   no_permission, already_on). Asking again while it is on restarts the idle timer.
2. **The app's own button self-test fails.** It then turns Wireless debugging on by itself and
   posts one line to Buzz #ops (through the fleet address in `config/fleet_url`; same host, port
   5561) with the reason, the adb TLS port when the app could read it, the phone's addresses and
   when it switches off again.

It switches itself off after an idle time (default 30 minutes; `POST /api/admin/debug/config`
`{"idle_minutes": 30, "prompt_minutes": 10}`, kept in `config/debug.json`). The app cannot see an
adb session, so "idle" means no new request from the fleet. Wireless debugging that the owner
turned on by hand is never turned off by the app, and `POST /api/admin/debug/off` ignores it.

**One-time grant over adb** (survives reboots and updates; without it the app says so plainly in
the prompt result and in the API answer, and turns nothing on):

```bash
adb shell pm grant com.festeraeb.nautik9 android.permission.WRITE_SECURE_SETTINGS
```

**The port** changes every time. The app tries `getprop service.adb.tls.port`, then looks for its
own `_adb-tls-connect._tcp` service with NsdManager (Wireless debugging only listens while the
phone is on Wi-Fi). If it cannot read it, the report says why and the fleet scans the phone's LAN
address over 1024-65535.

**Internal self-test** (`SelfTest.java` plus the server's own checks): haptic, torch on and off,
battery, GPS fix or a stated reason, local server health, theme apply (saved and put back), Nexus
SDK loaded. It runs on the first start after an update and on `POST /api/admin/selftest`; the
result is at `GET /api/admin/selftest`. The phone vibrates once and the torch flashes for half a
second. A failure cannot be forced through the API (that would be a way around the Yes prompt);
debuggable builds honour an empty file `config/selftest_force_fail` once, to prove the failure path
on a real phone.

## Build

Needs `ANDROID_HOME` with `platforms;android-35`, `build-tools;35.0.0` and `ndk;27.2.12479018`,
a JDK 17+, `zip`, and `rustup target add aarch64-linux-android`.

```bash
android/build-llama.sh /path/to/llama.cpp /tmp/llama-android   # once per llama.cpp update
LLAMA_SERVER=/tmp/llama-android/bin/llama-server android/build-apk.sh
```

The APK is `android/build/nautik9-<version>.apk`. The first build creates a signing key in
`~/.config/nautik9-android/`; keep it, because an update must be signed with the same key.
Both binaries are linked for 16 KB pages.

`DEBUGGABLE=1` (the default for now) allows `adb run-as` and WebView DevTools over adb.
Build with `DEBUGGABLE=0` for a copy you give to someone else.

## Working on it over wireless debugging

```bash
adb connect <phone-ip>:<port>              # Developer options > Wireless debugging
adb install -r android/build/nautik9-0.9.0.apk
adb shell run-as com.festeraeb.nautik9 tail files/server.log files/llm.log
adb shell run-as com.festeraeb.nautik9 cat files/config/providers.json
# drive the page from this machine (Playwright / Chrome DevTools):
adb forward tcp:9229 localabstract:webview_devtools_remote_$(adb shell pidof com.festeraeb.nautik9)
node android/e2e/app-check.mjs
```

A model can also be copied instead of downloaded:
`adb push model.gguf /sdcard/Android/data/com.festeraeb.nautik9/files/models/`.

To put a home Lemonade first, edit `files/config/providers.json` (with `run-as`) and restart the app:

```json
{"providers": [
  {"name": "home-lemonade", "base_url": "http://<lan-or-tailscale-ip>:13305/v1"},
  {"name": "on-device", "base_url": "http://127.0.0.1:8341/v1", "offline": true}
]}
```

An endpoint that cannot be reached fails in about 3 s and the next one is asked. A key-gated
endpoint (for example `https://cesarops.com/lemonade/v1`) takes `"api_key": "..."` and should name
its `"model"`, because the first model a Lemonade lists may be an embedding model. Keep the file
mode 600; `/api/providers` never returns keys.

## Not done yet

- Texts are drafted when the page pushes the inbox (app open), not as they arrive.
- No NPU or GPU use; the model runs on the CPU.
- One model at a time (the first `.gguf` in the folder).
