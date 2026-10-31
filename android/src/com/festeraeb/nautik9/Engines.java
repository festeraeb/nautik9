package com.festeraeb.nautik9;

import android.content.Context;

import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.net.HttpURLConnection;
import java.net.URL;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;

/**
 * Starts the two processes the app runs on the phone, both bound to 127.0.0.1:
 * the NautiK9 server (libnautik9.so) and, when a model file is present, llama.cpp's
 * llama-server (libllama_server.so) as the on-device, offline model endpoint.
 * Both binaries ship in the APK's lib/arm64-v8a and run from nativeLibraryDir.
 */
final class Engines {
    static final int SERVER_PORT = 8340;
    static final int LLM_PORT = 8341;
    static final String LLM_ALIAS = "on-device";
    /** Threads for the on-device model: the big cores. More than this fights the UI and burns battery. */
    static final int LLM_THREADS = 6;
    /** Stop the on-device model after this long without a request; it restarts on demand. */
    static final long LLM_IDLE_STOP_MS = 10 * 60_000L;

    private static Engines instance;

    private final Context ctx;
    private final File files;
    private final File configDir;
    private final String nativeDir;
    private Process server;
    private Process llm;
    private String llmModel;
    private long llmStartedAt;
    private Thread idleWatcher;

    static synchronized Engines get(Context c) {
        if (instance == null) instance = new Engines(c.getApplicationContext());
        return instance;
    }

    private Engines(Context c) {
        ctx = c;
        files = c.getFilesDir();
        configDir = new File(files, "config");
        nativeDir = c.getApplicationInfo().nativeLibraryDir;
    }

    File modelsDir() {
        File d = new File(ctx.getExternalFilesDir(null), "models");
        d.mkdirs();
        return d;
    }

    /** The first complete .gguf in the models folder (adb push or the in-app download). */
    File modelFile() {
        File[] ggufs = modelsDir().listFiles((dir, name) -> name.endsWith(".gguf"));
        if (ggufs == null || ggufs.length == 0) return null;
        java.util.Arrays.sort(ggufs);
        return ggufs[0];
    }

    /** Start whatever is not already answering. Blocks until the server answers or times out. */
    synchronized void startAll() throws IOException {
        configDir.mkdirs();
        writeDefaultProviders();
        Download.progress(ctx, modelsDir()); // finishes a download that completed while the app was away
        // The model is started on demand (the page asks for it when it needs an answer), not here:
        // cold start stays short and an idle phone does not hold a 2.5 GB model.
        if (!alive(SERVER_PORT, "/api/health")) {
            java.util.List<String> cmd = new java.util.ArrayList<>(java.util.Arrays.asList(
                    nativeDir + "/libnautik9.so",
                    "--device", "s25ultra",
                    "--vault-path", new File(files, "vault.db").getPath(),
                    "--config-dir", configDir.getPath(),
                    "serve", "--bind", "127.0.0.1", "--port", String.valueOf(SERVER_PORT),
                    "--sms-bridge", "app"));
            // config/fleet_url (one line) names the home fleet aggregator for the Fleet Deck; without
            // it the server's default (this phone's loopback) is asked and answers "unreachable".
            File fleetUrl = new File(configDir, "fleet_url");
            if (fleetUrl.exists()) {
                String u = new String(Files.readAllBytes(fleetUrl.toPath()), StandardCharsets.UTF_8).trim();
                if (!u.isEmpty()) { cmd.add("--fleet-url"); cmd.add(u); }
            }
            ProcessBuilder pb = new ProcessBuilder(cmd);
            pb.environment().put("HOME", files.getPath());
            pb.redirectErrorStream(true);
            pb.redirectOutput(ProcessBuilder.Redirect.appendTo(new File(files, "server.log")));
            server = pb.start();
        }
        // Up to 60 s: a first start can be slow while the phone is busy.
        for (int i = 0; i < 300 && !alive(SERVER_PORT, "/api/health"); i++) {
            sleep(200);
        }
        startIdleWatcher();
    }

    /** Start the on-device model in the background if a model file is present. */
    void ensureLlm() {
        new Thread(() -> {
            try {
                startLlmIfModel();
            } catch (IOException ignored) {
            }
        }, "llm-start").start();
    }

    /** Stop the on-device model (ours, or an orphan from an earlier app process). */
    synchronized void stopLlm() {
        if (llm != null) {
            llm.destroy();
            llm = null;
        }
        for (int pid : pidsOf("libllama_server.so")) android.os.Process.killProcess(pid);
        llmModel = null;
    }

    /** Stop everything the app started. Used when the user closes the app. */
    synchronized void stopAll() {
        stopLlm();
        if (server != null) {
            server.destroy();
            server = null;
        }
        for (int pid : pidsOf("libnautik9.so")) android.os.Process.killProcess(pid);
    }

    /** Our own child processes whose command line names this binary. */
    private static java.util.List<Integer> pidsOf(String binary) {
        java.util.List<Integer> out = new java.util.ArrayList<>();
        File[] procs = new File("/proc").listFiles((d, n) -> n.chars().allMatch(Character::isDigit));
        if (procs == null) return out;
        for (File p : procs) {
            try {
                byte[] cmd = Files.readAllBytes(new File(p, "cmdline").toPath());
                if (new String(cmd, StandardCharsets.UTF_8).contains(binary)) out.add(Integer.parseInt(p.getName()));
            } catch (IOException | RuntimeException ignored) {
                // another app's process: unreadable, skip
            }
        }
        return out;
    }

    /**
     * Every minute: if the model has answered nothing for LLM_IDLE_STOP_MS (the server keeps
     * the time of the last local request), stop it. It comes back in a few seconds when asked.
     */
    private synchronized void startIdleWatcher() {
        if (idleWatcher != null && idleWatcher.isAlive()) return;
        idleWatcher = new Thread(() -> {
            while (true) {
                sleep(60_000);
                try {
                    if (!llmRunning()) continue;
                    long idleMs = System.currentTimeMillis() - llmStartedAt;
                    Long fromServer = serverIdleSeconds();
                    if (fromServer != null) idleMs = Math.min(idleMs, fromServer * 1000);
                    if (idleMs > LLM_IDLE_STOP_MS) stopLlm();
                } catch (RuntimeException ignored) {
                }
            }
        }, "llm-idle");
        idleWatcher.setDaemon(true);
        idleWatcher.start();
    }

    /** Seconds since the server last asked the on-device model, or null if unknown / never. */
    private Long serverIdleSeconds() {
        try {
            HttpURLConnection c = (HttpURLConnection) new URL("http://127.0.0.1:" + SERVER_PORT + "/api/net").openConnection();
            c.setRequestProperty("Authorization", "Bearer " + token());
            c.setConnectTimeout(500);
            c.setReadTimeout(1500);
            if (c.getResponseCode() != 200) return null;
            java.io.ByteArrayOutputStream buf = new java.io.ByteArrayOutputStream();
            try (java.io.InputStream in = c.getInputStream()) {
                byte[] b = new byte[4096];
                for (int n; (n = in.read(b)) > 0; ) buf.write(b, 0, n);
            }
            String body = new String(buf.toByteArray(), StandardCharsets.UTF_8);
            org.json.JSONObject o = new org.json.JSONObject(body);
            return o.isNull("local_model_idle_seconds") ? null : o.getLong("local_model_idle_seconds");
        } catch (Exception e) {
            return null;
        }
    }

    synchronized void startLlmIfModel() throws IOException {
        File model = modelFile();
        if (model == null || alive(LLM_PORT, "/health")) return;
        if (Download.inProgress(ctx)) return;
        ProcessBuilder pb = new ProcessBuilder(
                nativeDir + "/libllama_server.so",
                "-m", model.getPath(),
                "-a", LLM_ALIAS,
                "--host", "127.0.0.1", "--port", String.valueOf(LLM_PORT),
                "-c", "8192", "-ctk", "q8_0", "-ctv", "q8_0",
                "-t", String.valueOf(LLM_THREADS), "-tb", String.valueOf(LLM_THREADS),
                // Keep the system prompt and earlier turns in the KV cache between requests.
                "--cache-reuse", "256",
                // No model warm-up pass: the first real request pays it instead, so wake is quick.
                "--no-warmup",
                "--jinja");
        pb.redirectErrorStream(true);
        pb.redirectOutput(ProcessBuilder.Redirect.appendTo(new File(files, "llm.log")));
        llm = pb.start();
        llmModel = model.getName();
        llmStartedAt = System.currentTimeMillis();
    }

    boolean llmRunning() {
        return alive(LLM_PORT, "/health");
    }

    String token() throws IOException {
        return new String(Files.readAllBytes(new File(configDir, "token").toPath()), StandardCharsets.UTF_8).trim();
    }

    /**
     * First start only: the chain is the on-device model. Edit config/providers.json
     * (adb or the app's data) to put a home Lemonade first; the user's file is never overwritten.
     */
    private void writeDefaultProviders() throws IOException {
        File f = new File(configDir, "providers.json");
        if (f.exists()) return;
        String json = "{\n"
                + "  \"providers\": [\n"
                + "    {\"name\": \"on-device\", \"base_url\": \"http://127.0.0.1:" + LLM_PORT + "/v1\", \"offline\": true}\n"
                + "  ],\n"
                + "  \"timeout_seconds\": 180,\n"
                + "  \"max_tokens\": 400\n"
                + "}\n";
        try (FileOutputStream out = new FileOutputStream(f)) {
            out.write(json.getBytes(StandardCharsets.UTF_8));
        }
    }

    static boolean alive(int port, String path) {
        try {
            HttpURLConnection c = (HttpURLConnection) new URL("http://127.0.0.1:" + port + path).openConnection();
            c.setConnectTimeout(300);
            c.setReadTimeout(1000);
            int code = c.getResponseCode();
            c.disconnect();
            return code == 200;
        } catch (IOException e) {
            return false;
        }
    }

    private static void sleep(long ms) {
        try {
            Thread.sleep(ms);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        }
    }
}
