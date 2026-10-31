package com.festeraeb.nautik9;

import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.content.pm.PackageManager;
import android.database.Cursor;
import android.hardware.camera2.CameraCharacteristics;
import android.hardware.camera2.CameraManager;
import android.location.Location;
import android.location.LocationManager;
import android.net.Uri;
import android.os.BatteryManager;
import android.os.Build;
import android.os.VibrationEffect;
import android.os.Vibrator;
import android.os.VibratorManager;
import android.provider.Telephony;
import android.webkit.JavascriptInterface;
import android.webkit.WebView;

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

    private WebView web;
    private boolean torchOn = false;

    Bridge(Context ctx, Engines engines) {
        this.ctx = ctx;
        this.engines = engines;
    }

    void attach(WebView web) {
        this.web = web;
    }

    /** Vibrate with a pattern of on/off milliseconds ("60,40,60"). {"ok":true} or {"error":...}. */
    @JavascriptInterface
    public String vibrate(String pattern) {
        try {
            String[] parts = pattern.split(",");
            long[] ms = new long[parts.length + 1];
            for (int i = 0; i < parts.length; i++) ms[i + 1] = Math.max(0, Math.min(2000, Long.parseLong(parts[i].trim())));
            Vibrator v = Build.VERSION.SDK_INT >= 31
                    ? ((VibratorManager) ctx.getSystemService(Context.VIBRATOR_MANAGER_SERVICE)).getDefaultVibrator()
                    : (Vibrator) ctx.getSystemService(Context.VIBRATOR_SERVICE);
            if (v == null || !v.hasVibrator()) return error("this phone reports no vibrator");
            v.vibrate(VibrationEffect.createWaveform(ms, -1));
            return "{\"ok\":true}";
        } catch (RuntimeException e) {
            return error(e.toString());
        }
    }

    /** Turn the back camera's flash on or off. {"ok":true,"on":bool} or {"error":...}. */
    @JavascriptInterface
    public String torch(boolean on) {
        try {
            CameraManager cm = (CameraManager) ctx.getSystemService(Context.CAMERA_SERVICE);
            for (String id : cm.getCameraIdList()) {
                CameraCharacteristics c = cm.getCameraCharacteristics(id);
                Boolean flash = c.get(CameraCharacteristics.FLASH_INFO_AVAILABLE);
                Integer facing = c.get(CameraCharacteristics.LENS_FACING);
                if (Boolean.TRUE.equals(flash) && facing != null && facing == CameraCharacteristics.LENS_FACING_BACK) {
                    cm.setTorchMode(id, on);
                    torchOn = on;
                    return new JSONObject().put("ok", true).put("on", on).toString();
                }
            }
            return error("no back camera with a flash was found");
        } catch (Exception e) {
            return error(e.toString());
        }
    }

    @JavascriptInterface
    public boolean torchState() {
        return torchOn;
    }

    /** {"level":0-100,"charging":bool,"temp_c":x} from the system battery broadcast. */
    @JavascriptInterface
    public String battery() {
        try {
            Intent b = ctx.registerReceiver(null, new IntentFilter(Intent.ACTION_BATTERY_CHANGED));
            if (b == null) return error("no battery broadcast");
            int level = b.getIntExtra(BatteryManager.EXTRA_LEVEL, -1), scale = b.getIntExtra(BatteryManager.EXTRA_SCALE, 100);
            int status = b.getIntExtra(BatteryManager.EXTRA_STATUS, -1);
            boolean charging = status == BatteryManager.BATTERY_STATUS_CHARGING || status == BatteryManager.BATTERY_STATUS_FULL;
            return new JSONObject().put("level", level < 0 ? JSONObject.NULL : Math.round(100f * level / scale))
                    .put("charging", charging).put("temp_c", b.getIntExtra(BatteryManager.EXTRA_TEMPERATURE, 0) / 10.0).toString();
        } catch (Exception e) {
            return error(e.toString());
        }
    }

    /**
     * Ask for one GPS fix. The answer arrives later as window.n9OnLocation({lat, lon, accuracy_m,
     * altitude_m, provider, time_ms}) or ({error}). Asks for the location permission first if needed.
     */
    @JavascriptInterface
    public void requestLocation() {
        if (ctx.checkSelfPermission(android.Manifest.permission.ACCESS_FINE_LOCATION) != PackageManager.PERMISSION_GRANTED) {
            ((Activity) ctx).runOnUiThread(() -> ((Activity) ctx).requestPermissions(new String[] {
                    android.Manifest.permission.ACCESS_FINE_LOCATION, android.Manifest.permission.ACCESS_COARSE_LOCATION}, 2));
            return;
        }
        fetchLocation();
    }

    /** Called by the activity when the location permission answer comes back. */
    void onLocationPermission(boolean granted) {
        if (granted) fetchLocation();
        else sendLocation(error("location permission was refused"));
    }

    private void fetchLocation() {
        try {
            LocationManager lm = (LocationManager) ctx.getSystemService(Context.LOCATION_SERVICE);
            if (!lm.isLocationEnabled()) {
                sendLocation(error("location is switched off on the phone"));
                return;
            }
            // 1. A fix from the last two minutes, from any provider, answers at once.
            Location best = null;
            for (String p : lm.getProviders(true)) {
                Location l = lm.getLastKnownLocation(p);
                if (l != null && (best == null || l.getTime() > best.getTime())) best = l;
            }
            final Location last = best;
            if (last != null && System.currentTimeMillis() - last.getTime() < 120_000) {
                sendLocation(describe(last, last.getProvider()));
                return;
            }
            // 2. A live fix: fused (Android 12+), else network, else GPS (GPS alone rarely fixes indoors).
            String provider = Build.VERSION.SDK_INT >= 31 && lm.isProviderEnabled(LocationManager.FUSED_PROVIDER) ? LocationManager.FUSED_PROVIDER
                    : lm.isProviderEnabled(LocationManager.NETWORK_PROVIDER) ? LocationManager.NETWORK_PROVIDER : LocationManager.GPS_PROVIDER;
            if (Build.VERSION.SDK_INT >= 30) {
                lm.getCurrentLocation(provider, null, ctx.getMainExecutor(), loc -> {
                    // 3. No live fix: the last known one, marked with its age by the page.
                    if (loc == null && last != null) sendLocation(describe(last, last.getProvider()));
                    else sendLocation(loc == null ? error("the " + provider + " provider gave no location in time and the phone has no earlier fix") : describe(loc, provider));
                });
            } else {
                sendLocation(last == null ? error("the phone has no fix yet (Android 10 uses the last known location only)") : describe(last, last.getProvider()));
            }
        } catch (SecurityException e) {
            sendLocation(error("location permission was refused"));
        } catch (RuntimeException e) {
            sendLocation(error(e.toString()));
        }
    }

    private static String describe(Location loc, String provider) {
        try {
            return new JSONObject().put("lat", loc.getLatitude()).put("lon", loc.getLongitude())
                    .put("accuracy_m", loc.hasAccuracy() ? loc.getAccuracy() : JSONObject.NULL)
                    .put("altitude_m", loc.hasAltitude() ? loc.getAltitude() : JSONObject.NULL)
                    .put("provider", provider).put("time_ms", loc.getTime()).toString();
        } catch (JSONException e) {
            return error(e.toString());
        }
    }

    private void sendLocation(String json) {
        if (web == null) return;
        web.post(() -> web.evaluateJavascript("window.n9OnLocation&&window.n9OnLocation(" + json + ")", null));
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

    /** Start the on-device model if it is not running (returns at once; poll offlineModelStatus). */
    @JavascriptInterface
    public void ensureOfflineModel() {
        engines.ensureLlm();
    }

    /** Stop the on-device model now (it restarts on demand). */
    @JavascriptInterface
    public void stopOfflineModel() {
        engines.stopLlm();
    }

    /**
     * The phone's connectivity: {"online":bool,"transport":"wifi|cell|ethernet|vpn|none",
     * "metered":bool}. "online" means the active network reached the internet (validated).
     */
    @JavascriptInterface
    public String network() {
        return networkJson(ctx);
    }

    static String networkJson(Context ctx) {
        try {
            android.net.ConnectivityManager cm = (android.net.ConnectivityManager) ctx.getSystemService(Context.CONNECTIVITY_SERVICE);
            JSONObject o = new JSONObject();
            // A VPN (Tailscale) is "validated" long after the real link is gone (airplane mode still
            // showed ONLINE VPN), so online means: some non-VPN network reaches the internet.
            boolean internet = false, vpn = false;
            String t = "none";
            for (android.net.Network n : cm.getAllNetworks()) {
                android.net.NetworkCapabilities c = cm.getNetworkCapabilities(n);
                if (c == null) continue;
                if (c.hasTransport(android.net.NetworkCapabilities.TRANSPORT_VPN)) { vpn = true; continue; }
                boolean ok = c.hasCapability(android.net.NetworkCapabilities.NET_CAPABILITY_INTERNET)
                        && c.hasCapability(android.net.NetworkCapabilities.NET_CAPABILITY_VALIDATED);
                if (!ok) continue;
                String tt = c.hasTransport(android.net.NetworkCapabilities.TRANSPORT_WIFI) ? "wifi"
                        : c.hasTransport(android.net.NetworkCapabilities.TRANSPORT_CELLULAR) ? "cell"
                        : c.hasTransport(android.net.NetworkCapabilities.TRANSPORT_ETHERNET) ? "ethernet" : "other";
                // Prefer Wi-Fi over cell when both are up.
                if (!internet || tt.equals("wifi")) { t = tt; o.put("metered", !c.hasCapability(android.net.NetworkCapabilities.NET_CAPABILITY_NOT_METERED)); }
                internet = true;
            }
            if (!internet) o.put("metered", false);
            return o.put("online", internet).put("transport", t)
                    .put("tailscale", vpn).put("tailscale_installed", installed(ctx, "com.tailscale.ipn")).toString();
        } catch (Exception e) {
            return error(e.toString());
        }
    }

    /**
     * True when a VPN network (Tailscale) is up. Interface addresses are not enough: a mobile
     * carrier's own CGNAT address is also in 100.64.0.0/10.
     */
    static boolean tailscaleUp(Context ctx) {
        try {
            android.net.ConnectivityManager cm = (android.net.ConnectivityManager) ctx.getSystemService(Context.CONNECTIVITY_SERVICE);
            for (android.net.Network n : cm.getAllNetworks()) {
                android.net.NetworkCapabilities c = cm.getNetworkCapabilities(n);
                if (c != null && c.hasTransport(android.net.NetworkCapabilities.TRANSPORT_VPN)) return true;
            }
        } catch (RuntimeException ignored) {
        }
        return false;
    }

    static boolean installed(Context ctx, String pkg) {
        try {
            ctx.getPackageManager().getPackageInfo(pkg, 0);
            return true;
        } catch (PackageManager.NameNotFoundException e) {
            return false;
        }
    }

    /**
     * HTTP to the app's own server from Java, for when the WebView refuses loopback requests
     * (airplane mode: Chromium reports "Failed to fetch" even for 127.0.0.1). The page gets the
     * answer as window.n9OnHttp(id, status, bodyText). Only paths on the local server are allowed.
     */
    @JavascriptInterface
    public void httpAsync(String id, String method, String path, String contentType, String body) {
        new Thread(() -> {
            int status = 0;
            String text;
            try {
                if (!path.startsWith("/")) throw new java.io.IOException("only local paths");
                HttpURLConnection c = (HttpURLConnection) new URL("http://127.0.0.1:" + Engines.SERVER_PORT + path).openConnection();
                c.setRequestMethod(method);
                c.setConnectTimeout(3000);
                c.setReadTimeout(600_000);
                c.setRequestProperty("Authorization", "Bearer " + engines.token());
                if (body != null && !body.isEmpty()) {
                    c.setDoOutput(true);
                    c.setRequestProperty("Content-Type", contentType == null || contentType.isEmpty() ? "application/json" : contentType);
                    try (java.io.OutputStream out = c.getOutputStream()) {
                        out.write(body.getBytes(java.nio.charset.StandardCharsets.UTF_8));
                    }
                }
                status = c.getResponseCode();
                java.io.InputStream in = status >= 400 ? c.getErrorStream() : c.getInputStream();
                java.io.ByteArrayOutputStream buf = new java.io.ByteArrayOutputStream();
                if (in != null) {
                    byte[] b = new byte[8192];
                    for (int n; (n = in.read(b)) > 0; ) buf.write(b, 0, n);
                    in.close();
                }
                text = buf.toString("UTF-8");
            } catch (Exception e) {
                text = e.toString();
            }
            final int st = status;
            final String payload;
            try {
                payload = new JSONObject().put("id", id).put("status", st).put("body", text).toString();
            } catch (JSONException e) {
                return;
            }
            if (web != null) web.post(() -> web.evaluateJavascript("window.n9OnHttp&&window.n9OnHttp(" + payload + ")", null));
        }, "http-" + id).start();
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
