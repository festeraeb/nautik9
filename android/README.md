# NautiK9 for Android

The app runs the NautiK9 server inside the phone and shows its page in a WebView. Nothing needs a
computer or adb once it is installed.

- **Server**: `libnautik9.so` is the `nautik9-app` binary built for arm64. The app starts it as
  `serve --bind 127.0.0.1 --sms-bridge app`; the vault and config folder are in the app's private
  storage.
- **Offline model**: `libllama_server.so` is llama.cpp's `llama-server`. The app starts it on
  127.0.0.1:8341 when a `.gguf` is in the app's `models` folder. The page offers a download of
  Qwen3-4B-Instruct-2507 Q4_K_M (2.5 GB) through the system download manager. On a Galaxy S25
  Ultra (CPU only) it generated about 20 tokens/s and read prompts at about 80 tokens/s.
- **Texts**: the app holds READ_SMS and pushes the newest inbox rows to the server
  (`/api/sms/ingest`). Approving a draft opens the phone's composer with it; you press send. The app
  never sends a text.
- **Providers**: on first start `providers.json` lists only the on-device model. To ask a home
  Lemonade first when the phone can reach it, add it above the on-device entry (see below).

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
adb install -r android/build/nautik9-0.2.0.apk
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

An endpoint that cannot be reached fails in about 3 s and the next one is asked.

## Not done yet

- The server runs while the app is open; there is no background service yet, so texts are
  drafted when you open the app, not as they arrive.
- No NPU or GPU use; the model runs on the CPU.
- One model at a time (the first `.gguf` in the folder).
