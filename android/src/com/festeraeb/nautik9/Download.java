package com.festeraeb.nautik9;

import android.app.DownloadManager;
import android.content.Context;
import android.content.SharedPreferences;
import android.database.Cursor;
import android.net.Uri;

import java.io.File;

/** Fetches the default on-device model with the system DownloadManager (resumes, shows a notification). */
final class Download {
    static final String DEFAULT_NAME = "Qwen3-4B-Instruct-2507-Q4_K_M.gguf";
    static final String DEFAULT_URL =
            "https://huggingface.co/unsloth/Qwen3-4B-Instruct-2507-GGUF/resolve/main/" + DEFAULT_NAME;
    static final long DEFAULT_SIZE = 2497281120L;

    private static final String PREFS = "download";
    private static final String KEY_ID = "id";

    private Download() {}

    /** Downloads to <name>.part and renames on success, so a partial file is never loaded. */
    static void start(Context ctx, File modelsDir) {
        if (inProgress(ctx)) return;
        File part = new File(modelsDir, DEFAULT_NAME + ".part");
        part.delete();
        DownloadManager dm = (DownloadManager) ctx.getSystemService(Context.DOWNLOAD_SERVICE);
        DownloadManager.Request r = new DownloadManager.Request(Uri.parse(DEFAULT_URL))
                .setTitle("NautiK9 offline model")
                .setDescription(DEFAULT_NAME)
                .setDestinationUri(Uri.fromFile(part))
                .setNotificationVisibility(DownloadManager.Request.VISIBILITY_VISIBLE_NOTIFY_COMPLETED);
        long id = dm.enqueue(r);
        prefs(ctx).edit().putLong(KEY_ID, id).apply();
    }

    /** {state, done, total}; also finishes a completed download (rename .part to .gguf). */
    static long[] progress(Context ctx, File modelsDir) {
        long id = prefs(ctx).getLong(KEY_ID, -1);
        if (id < 0) return null;
        DownloadManager dm = (DownloadManager) ctx.getSystemService(Context.DOWNLOAD_SERVICE);
        try (Cursor c = dm.query(new DownloadManager.Query().setFilterById(id))) {
            if (c == null || !c.moveToFirst()) {
                prefs(ctx).edit().remove(KEY_ID).apply();
                return null;
            }
            int status = c.getInt(c.getColumnIndexOrThrow(DownloadManager.COLUMN_STATUS));
            long done = c.getLong(c.getColumnIndexOrThrow(DownloadManager.COLUMN_BYTES_DOWNLOADED_SO_FAR));
            long total = c.getLong(c.getColumnIndexOrThrow(DownloadManager.COLUMN_TOTAL_SIZE_BYTES));
            if (status == DownloadManager.STATUS_SUCCESSFUL) {
                new File(modelsDir, DEFAULT_NAME + ".part").renameTo(new File(modelsDir, DEFAULT_NAME));
                prefs(ctx).edit().remove(KEY_ID).apply();
                return null;
            }
            if (status == DownloadManager.STATUS_FAILED) {
                prefs(ctx).edit().remove(KEY_ID).apply();
                return new long[] {-1, done, total};
            }
            return new long[] {status, done, total > 0 ? total : DEFAULT_SIZE};
        }
    }

    static boolean inProgress(Context ctx) {
        return prefs(ctx).getLong(KEY_ID, -1) >= 0;
    }

    private static SharedPreferences prefs(Context ctx) {
        return ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE);
    }
}
