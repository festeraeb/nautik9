package com.festeraeb.nautik9;

import android.content.Context;
import android.content.SharedPreferences;
import android.content.pm.ApplicationInfo;
import android.content.pm.PackageManager;
import android.location.Location;
import android.location.LocationManager;
import android.os.Build;

import org.json.JSONArray;
import org.json.JSONObject;

import java.io.File;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicReference;

/**
 * The part of android/e2e/buttons-check.mjs that can run inside the app with no adb: haptic, torch
 * on and off, battery, GPS fix or a stated reason. The local server adds its own checks (it
 * answers, a theme applies and is saved, the Nexus SDK is loaded) and stores the whole result,
 * readable at GET /api/admin/selftest.
 *
 * It runs once on the first start after an update, and whenever the fleet asks
 * (POST /api/admin/selftest). If a check fails the app turns Wireless debugging on by itself and
 * tells the fleet (see WirelessDebug), so an agent can connect and look.
 *
 * The torch is switched on for half a second and off, and the phone vibrates once. Debuggable
 * builds honour config/selftest_force_fail (a file, deleted after one use) to prove the failure
 * path on a real phone; it cannot be set through the API.
 */
final class SelfTest {
    private static final AtomicBoolean RUNNING = new AtomicBoolean();

    private SelfTest() {}

    private static JSONObject check(String name, boolean ok, String detail) throws Exception {
        return new JSONObject().put("name", name).put("ok", ok).put("detail", detail);
    }

    /** First start of a new app version: run once (the version is remembered, pass or fail). */
    static void afterUpdateIfNeeded(Context c) {
        SharedPreferences p = c.getSharedPreferences("self_test", Context.MODE_PRIVATE);
        String v = Engines.get(c).appVersion();
        if (v.equals(p.getString("tested_version", ""))) return;
        p.edit().putString("tested_version", v).apply();
        run(c, "after_update", null);
    }

    /** Run the test and report. Blocks (up to ~20 s): call off the main thread. */
    static void run(Context c, String trigger, String actionId) {
        if (!RUNNING.compareAndSet(false, true)) {
            if (actionId != null) EngineService.reportDone(c, actionId, false, "a self-test is already running");
            return;
        }
        try {
            JSONArray checks = new JSONArray();
            Bridge b = new Bridge(c, Engines.get(c));
            String r = b.vibrate("40");
            checks.put(check("haptic pulse", r.contains("\"ok\":true"), r));
            r = b.torch(true);
            boolean on = r.contains("\"ok\":true");
            sleep(500);
            String off = b.torch(false);
            checks.put(check("torch turns on then off", on && off.contains("\"ok\":true"), on ? off : r));
            String batt = b.battery();
            int level = -1;
            try {
                level = new JSONObject(batt).optInt("level", -1);
            } catch (Exception ignored) {
            }
            checks.put(check("battery shows a real level", level >= 0 && level <= 100, batt));
            checks.put(gps(c));
            if (debuggable(c)) {
                File f = new File(new File(c.getFilesDir(), "config"), "selftest_force_fail");
                if (f.exists()) {
                    f.delete();
                    checks.put(check("forced failure (debug test hook)", false, "config/selftest_force_fail was present"));
                }
            }
            JSONObject body = new JSONObject().put("trigger", trigger).put("app", Engines.get(c).appVersion()).put("checks", checks);
            String answer = Engines.get(c).adminPost("/api/admin/device/selftest", body.toString());
            boolean failed;
            String summary;
            if (answer == null) {
                failed = true;
                summary = "the local server did not answer the self-test";
                tellFleetDirect(c, "NautiK9 on the phone: the button self-test could not reach its own local server, so it cannot report through it. Turning Wireless debugging on if it can.");
            } else {
                JSONArray f = new JSONObject(answer).getJSONObject("result").getJSONArray("failed");
                failed = f.length() > 0;
                summary = failed ? "failed: " + f : "all passed";
            }
            if (failed) WirelessDebug.turnOn(c, "self_test_failed", null);
            if (actionId != null) EngineService.reportDone(c, actionId, !failed, summary);
        } catch (Exception e) {
            if (actionId != null) EngineService.reportDone(c, actionId, false, "self-test crashed: " + e);
        } finally {
            RUNNING.set(false);
        }
    }

    private static boolean debuggable(Context c) {
        return (c.getApplicationInfo().flags & ApplicationInfo.FLAG_DEBUGGABLE) != 0;
    }

    /** A live fix, an older fix, or a stated reason all pass; only an exception fails (never a made-up position). */
    private static JSONObject gps(Context c) throws Exception {
        try {
            if (c.checkSelfPermission(android.Manifest.permission.ACCESS_FINE_LOCATION) != PackageManager.PERMISSION_GRANTED) {
                return check("gps fix or a stated reason", true, "No fix: location permission not granted");
            }
            LocationManager lm = (LocationManager) c.getSystemService(Context.LOCATION_SERVICE);
            if (!lm.isLocationEnabled()) return check("gps fix or a stated reason", true, "No fix: location is switched off on the phone");
            Location best = null;
            for (String p : lm.getProviders(true)) {
                Location l = lm.getLastKnownLocation(p);
                if (l != null && (best == null || l.getTime() > best.getTime())) best = l;
            }
            if (best != null && System.currentTimeMillis() - best.getTime() < 120_000) {
                return check("gps fix or a stated reason", true, "Live fix, accuracy " + Math.round(best.getAccuracy()) + " m (" + best.getProvider() + ")");
            }
            if (Build.VERSION.SDK_INT >= 30) {
                String provider = Build.VERSION.SDK_INT >= 31 && lm.isProviderEnabled(LocationManager.FUSED_PROVIDER) ? LocationManager.FUSED_PROVIDER
                        : lm.isProviderEnabled(LocationManager.NETWORK_PROVIDER) ? LocationManager.NETWORK_PROVIDER : LocationManager.GPS_PROVIDER;
                final AtomicReference<Location> got = new AtomicReference<>();
                final CountDownLatch done = new CountDownLatch(1);
                lm.getCurrentLocation(provider, null, c.getMainExecutor(), loc -> { got.set(loc); done.countDown(); });
                done.await(12, TimeUnit.SECONDS);
                if (got.get() != null) return check("gps fix or a stated reason", true, "Live fix, accuracy " + Math.round(got.get().getAccuracy()) + " m (" + provider + ")");
            }
            if (best != null) return check("gps fix or a stated reason", true, "Old fix, " + (System.currentTimeMillis() - best.getTime()) / 60000 + " min old");
            return check("gps fix or a stated reason", true, "No fix: the provider gave no location in 12 s and the phone has no earlier fix");
        } catch (SecurityException e) {
            return check("gps fix or a stated reason", true, "No fix: location permission refused");
        } catch (RuntimeException e) {
            return check("gps fix or a stated reason", false, e.toString());
        }
    }

    /** Only when the local server cannot be reached: post one Buzz line straight to the fleet's Buzz door (same host as config/fleet_url, port 5561). */
    private static void tellFleetDirect(Context c, String text) {
        try {
            File f = new File(new File(c.getFilesDir(), "config"), "fleet_url");
            String u = new String(java.nio.file.Files.readAllBytes(f.toPath()), java.nio.charset.StandardCharsets.UTF_8).trim();
            String host = u.substring(u.indexOf("://") + 3).split("/")[0].replaceAll(":\\d+$", "");
            java.net.HttpURLConnection h = (java.net.HttpURLConnection) new java.net.URL("http://" + host + ":5561/board/message").openConnection();
            h.setRequestMethod("POST");
            h.setRequestProperty("Content-Type", "application/json");
            h.setConnectTimeout(3000);
            h.setReadTimeout(5000);
            h.setDoOutput(true);
            h.getOutputStream().write(new JSONObject().put("sender", "nautik9-phone").put("channel", "ops").put("body", text).toString()
                    .getBytes(java.nio.charset.StandardCharsets.UTF_8));
            h.getResponseCode();
            h.disconnect();
        } catch (Exception ignored) {
        }
    }

    private static void sleep(long ms) {
        try {
            Thread.sleep(ms);
        } catch (InterruptedException ignored) {
            Thread.currentThread().interrupt();
        }
    }
}
