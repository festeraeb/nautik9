package com.festeraeb.nautik9;

import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.database.Cursor;
import android.net.Uri;
import android.provider.Telephony;
import android.webkit.JavascriptInterface;

import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

import java.io.File;

/**
 * window.NautiK9Android in the page. Only the app's own page (127.0.0.1) is ever
 * loaded in the WebView, so only that page can call these.
 */
final class Bridge {
    private final Context ctx;
    private final Engines engines;

    Bridge(Context ctx, Engines engines) {
        this.ctx = ctx;
        this.engines = engines;
    }

    /** Newest inbox rows as a JSON array of {id, sender, body, date_ms}, or {"error": ...}. */
    @JavascriptInterface
    public String readInbox(int limit) {
        if (ctx.checkSelfPermission(android.Manifest.permission.READ_SMS) != PackageManager.PERMISSION_GRANTED) {
            return "{\"error\":\"SMS permission not granted\"}";
        }
        JSONArray rows = new JSONArray();
        String[] cols = {Telephony.Sms._ID, Telephony.Sms.ADDRESS, Telephony.Sms.BODY, Telephony.Sms.DATE};
        try (Cursor c = ctx.getContentResolver().query(
                Telephony.Sms.Inbox.CONTENT_URI, cols, null, null, Telephony.Sms.DEFAULT_SORT_ORDER)) {
            while (c != null && c.moveToNext() && rows.length() < Math.max(1, Math.min(limit, 50))) {
                JSONObject o = new JSONObject();
                o.put("id", String.valueOf(c.getLong(0)));
                o.put("sender", c.getString(1) == null ? "Unknown" : c.getString(1));
                o.put("body", c.getString(2) == null ? "" : c.getString(2));
                o.put("date_ms", c.getLong(3));
                rows.put(o);
            }
        } catch (JSONException | RuntimeException e) {
            return error(e.toString());
        }
        return rows.toString();
    }

    /** Opens the phone's composer with the reply filled in. Nothing is sent: the user presses send. */
    @JavascriptInterface
    public boolean openComposer(String to, String body) {
        try {
            Intent i = new Intent(Intent.ACTION_SENDTO, Uri.parse("smsto:" + Uri.encode(to)));
            i.putExtra("sms_body", body);
            i.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
            ctx.startActivity(i);
            return true;
        } catch (RuntimeException e) {
            return false;
        }
    }

    @JavascriptInterface
    public String offlineModelStatus() {
        JSONObject o = new JSONObject();
        try {
            File dir = engines.modelsDir();
            long[] p = Download.progress(ctx, dir);
            File model = engines.modelFile();
            o.put("default_name", Download.DEFAULT_NAME);
            o.put("default_size_mb", Download.DEFAULT_SIZE / 1_000_000);
            if (p != null && p[0] >= 0) {
                o.put("state", "downloading");
                o.put("done_mb", p[1] / 1_000_000);
                o.put("size_mb", p[2] / 1_000_000);
            } else if (model != null) {
                if (!engines.llmRunning()) engines.startLlmIfModel();
                o.put("state", "ready");
                o.put("file", model.getName());
                o.put("size_mb", model.length() / 1_000_000);
                o.put("running", engines.llmRunning());
            } else {
                o.put("state", "none");
                if (p != null) o.put("last_error", "the last download failed");
            }
        } catch (Exception e) {
            return error(e.toString());
        }
        return o.toString();
    }

    @JavascriptInterface
    public void downloadOfflineModel() {
        Download.start(ctx, engines.modelsDir());
    }

    private static String error(String msg) {
        try {
            return new JSONObject().put("error", msg).toString();
        } catch (JSONException e) {
            return "{\"error\":\"unknown\"}";
        }
    }
}
