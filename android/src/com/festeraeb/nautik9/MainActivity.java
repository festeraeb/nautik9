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
    private android.net.ConnectivityManager.NetworkCallback netWatch;
    private android.net.ConnectivityManager.NetworkCallback vpnWatch;

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
        // The page talks to a server on this phone; it must keep working with no network at all.
        web.setNetworkAvailable(true);
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

        java.util.List<String> ask = new java.util.ArrayList<>();
        if (checkSelfPermission(android.Manifest.permission.READ_SMS) != PackageManager.PERMISSION_GRANTED) ask.add(android.Manifest.permission.READ_SMS);
        // Android 13+: the "NautiK9 is running" notification needs this; the service runs either way.
        if (android.os.Build.VERSION.SDK_INT >= 33 && checkSelfPermission(android.Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
            ask.add(android.Manifest.permission.POST_NOTIFICATIONS);
        }
        if (!ask.isEmpty()) requestPermissions(ask.toArray(new String[0]), 1);
        startForegroundService(new Intent(this, EngineService.class));
        watchNetwork();
        start();
    }

    /** Tell the page when the phone goes online or offline: window.n9OnNetwork({online, transport, metered}). */
    private void watchNetwork() {
        android.net.ConnectivityManager cm = (android.net.ConnectivityManager) getSystemService(CONNECTIVITY_SERVICE);
        netWatch = new android.net.ConnectivityManager.NetworkCallback() {
            private void push() {
                String json = Bridge.networkJson(MainActivity.this);
                runOnUiThread(() -> { if (web != null) web.evaluateJavascript("window.n9OnNetwork&&window.n9OnNetwork(" + json + ")", null); });
            }
            @Override public void onAvailable(android.net.Network n) { push(); }
            @Override public void onLost(android.net.Network n) { push(); }
            @Override public void onCapabilitiesChanged(android.net.Network n, android.net.NetworkCapabilities c) { push(); }
        };
        try {
            cm.registerDefaultNetworkCallback(netWatch);
            // A VPN (Tailscale) coming or going does not change the default network: watch it too.
            vpnWatch = new android.net.ConnectivityManager.NetworkCallback() {
                private void push() {
                    String json = Bridge.networkJson(MainActivity.this);
                    runOnUiThread(() -> { if (web != null) web.evaluateJavascript("window.n9OnNetwork&&window.n9OnNetwork(" + json + ")", null); });
                }
                @Override public void onAvailable(android.net.Network n) { push(); }
                @Override public void onLost(android.net.Network n) { push(); }
            };
            cm.registerNetworkCallback(new android.net.NetworkRequest.Builder()
                    .addTransportType(android.net.NetworkCapabilities.TRANSPORT_VPN)
                    .removeCapability(android.net.NetworkCapabilities.NET_CAPABILITY_NOT_VPN).build(), vpnWatch);
        } catch (RuntimeException e) {
            netWatch = null;
        }
    }

    @Override
    protected void onDestroy() {
        android.net.ConnectivityManager cm = (android.net.ConnectivityManager) getSystemService(CONNECTIVITY_SERVICE);
        for (android.net.ConnectivityManager.NetworkCallback cb : new android.net.ConnectivityManager.NetworkCallback[] {netWatch, vpnWatch}) {
            if (cb == null) continue;
            try {
                cm.unregisterNetworkCallback(cb);
            } catch (RuntimeException ignored) {
            }
        }
        // The user closed the app (swiped it away or pressed back out): stop the model and the
        // server so nothing runs in the background. A rotation or theme change keeps them.
        if (isFinishing()) {
            Engines.get(this).stopAll();
            stopService(new Intent(this, EngineService.class));
        }
        super.onDestroy();
    }

    private final java.util.concurrent.atomic.AtomicBoolean starting = new java.util.concurrent.atomic.AtomicBoolean();

    /** Start the engines if needed and show the page. Safe to call again: one start at a time. */
    private void start() {
        if (!starting.compareAndSet(false, true)) return;
        new Thread(() -> {
            try {
                Engines e = Engines.get(this);
                e.startAll();
                if (!Engines.alive(Engines.SERVER_PORT, "/api/health")) {
                    throw new java.io.IOException("it did not answer within 60 s (see files/server.log)");
                }
                String url = HOME + "/?token=" + e.token();
                runOnUiThread(() -> web.loadUrl(url));
            } catch (Exception ex) {
                String msg = String.valueOf(ex.getMessage()).replace("<", "&lt;");
                runOnUiThread(() -> web.loadData("<body style='background:#0b1220;color:#fca5a5;font-family:sans-serif;padding:40px'>"
                        + "The NautiK9 server did not start: " + msg
                        + "<p>Switch away and back to NautiK9 to try again.</p></body>", "text/html", "utf-8"));
            } finally {
                starting.set(false);
            }
        }).start();
    }

    @Override
    protected void onResume() {
        super.onResume();
        // Coming back to the app while it still shows the waiting or error screen (the first load
        // was lost, or the server was not up yet): try again instead of waiting forever.
        String u = web == null ? null : web.getUrl();
        if (u == null || !u.startsWith(HOME + "/")) start();
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
