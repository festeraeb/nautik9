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

    private static Engines instance;

    private final Context ctx;
    private final File files;
    private final File configDir;
    private final String nativeDir;
    private Process server;
    private Process llm;
    private String llmModel;

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
        startLlmIfModel();
        if (!alive(SERVER_PORT, "/api/health")) {
            ProcessBuilder pb = new ProcessBuilder(
                    nativeDir + "/libnautik9.so",
                    "--device", "s25ultra",
                    "--vault-path", new File(files, "vault.db").getPath(),
                    "--config-dir", configDir.getPath(),
                    "serve", "--bind", "127.0.0.1", "--port", String.valueOf(SERVER_PORT),
                    "--sms-bridge", "app");
            pb.environment().put("HOME", files.getPath());
            pb.redirectErrorStream(true);
            pb.redirectOutput(ProcessBuilder.Redirect.appendTo(new File(files, "server.log")));
            server = pb.start();
        }
        for (int i = 0; i < 100 && !alive(SERVER_PORT, "/api/health"); i++) {
            sleep(100);
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
                "-c", "8192", "-ctk", "q8_0", "-ctv", "q8_0", "-t", "6", "--jinja");
        pb.redirectErrorStream(true);
        pb.redirectOutput(ProcessBuilder.Redirect.appendTo(new File(files, "llm.log")));
        llm = pb.start();
        llmModel = model.getName();
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
