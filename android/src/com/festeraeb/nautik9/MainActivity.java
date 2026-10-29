package com.festeraeb.nautik9;

import android.app.Activity;
import android.content.Intent;
import android.content.pm.ApplicationInfo;
import android.content.pm.PackageManager;
import android.net.Uri;
import android.os.Bundle;
import android.webkit.WebResourceRequest;
import android.webkit.WebSettings;
import android.webkit.WebView;
import android.webkit.WebViewClient;

/** Starts the on-phone server (and the offline model, if present) and shows its page. */
public class MainActivity extends Activity {
    private static final String HOME = "http://127.0.0.1:" + Engines.SERVER_PORT;
    private WebView web;
    private Bridge bridge;

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        // Debuggable builds only: the page can be inspected and driven over adb (DevTools / Playwright).
        if ((getApplicationInfo().flags & ApplicationInfo.FLAG_DEBUGGABLE) != 0) {
            WebView.setWebContentsDebuggingEnabled(true);
        }
        web = new WebView(this);
        WebSettings s = web.getSettings();
        s.setJavaScriptEnabled(true);
        s.setDomStorageEnabled(true);
        s.setAllowFileAccess(false);
        bridge = new Bridge(this, Engines.get(this));
        bridge.attach(web);
        web.addJavascriptInterface(bridge, "NautiK9Android");
        web.setWebViewClient(new WebViewClient() {
            @Override
            public boolean shouldOverrideUrlLoading(WebView view, WebResourceRequest req) {
                Uri u = req.getUrl();
                if (u.toString().startsWith(HOME + "/")) return false;
                // Anything else (sms:, web links) goes to the phone's own apps, never into this WebView.
                try {
                    startActivity(new Intent(Intent.ACTION_VIEW, u));
                } catch (RuntimeException ignored) {
                }
                return true;
            }
        });
        web.loadData("<body style='background:#0b1220;color:#9fb3c8;font-family:sans-serif;padding:40px'>"
                + "Mack is waking up...</body>", "text/html", "utf-8");
        setContentView(web);

        if (checkSelfPermission(android.Manifest.permission.READ_SMS) != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(new String[] {android.Manifest.permission.READ_SMS}, 1);
        }
        start();
    }

    private void start() {
        new Thread(() -> {
            String url;
            try {
                Engines e = Engines.get(this);
                e.startAll();
                url = HOME + "/?token=" + e.token();
            } catch (Exception ex) {
                String msg = String.valueOf(ex.getMessage()).replace("<", "&lt;");
                runOnUiThread(() -> web.loadData("<body style='background:#0b1220;color:#fca5a5;font-family:sans-serif;padding:40px'>"
                        + "The NautiK9 server did not start: " + msg + "</body>", "text/html", "utf-8"));
                return;
            }
            runOnUiThread(() -> web.loadUrl(url));
        }).start();
    }

    @Override
    public void onRequestPermissionsResult(int code, String[] perms, int[] results) {
        if (code == 2) {
            boolean granted = false;
            for (int r : results) granted |= r == PackageManager.PERMISSION_GRANTED;
            if (bridge != null) bridge.onLocationPermission(granted);
            return;
        }
        // Re-read the inbox now that the permission answer is in.
        if (web != null) web.evaluateJavascript("typeof loadSmsInbox==='function'&&loadSmsInbox(true)", null);
    }

    @Override
    public void onBackPressed() {
        if (web != null && web.canGoBack()) web.goBack();
        else super.onBackPressed();
    }
}
