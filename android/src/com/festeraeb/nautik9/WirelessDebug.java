package com.festeraeb.nautik9;

import android.app.AlertDialog;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.PackageManager;
import android.net.ConnectivityManager;
import android.net.NetworkCapabilities;
import android.net.nsd.NsdManager;
import android.net.nsd.NsdServiceInfo;
import android.os.Handler;
import android.os.Looper;
import android.provider.Settings;

import org.json.JSONObject;

import java.io.BufferedReader;
import java.io.File;
import java.io.InputStreamReader;
import java.net.InetAddress;
import java.net.NetworkInterface;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.util.Collections;
import java.util.HashSet;
import java.util.Set;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.LinkedBlockingQueue;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicInteger;

/**
 * Wireless debugging on demand. It stays off; this class turns it on in exactly two cases (the
 * fleet asked for a button-debugging session and the owner answered Yes, or the app's own button
 * self-test failed) and turns it off again after an idle time. Both times are state, not code:
 * config/debug.json {idle_minutes (30), prompt_minutes (10)}, written through the server's
 * /api/admin/debug/config.
 *
 * Turning it on is Settings.Global "adb_wifi_enabled" = 1, which needs
 * android.permission.WRITE_SECURE_SETTINGS (adb shell pm grant, once; it survives reboots and
 * updates). Without it nothing is turned on and every answer says so.
 *
 * "Idle" means: no new request from the fleet. An adb session itself cannot be seen by an app, so
 * the fleet keeps the session alive by asking again (POST /api/admin/debug/session while it is on).
 * Wireless debugging that the owner turned on by hand is never turned off by this class.
 */
final class WirelessDebug {
    static final String PERMISSION = "android.permission.WRITE_SECURE_SETTINGS";
    static final String SETTING = "adb_wifi_enabled";
    private static final String CHANNEL = "debug_prompt";
    private static final int NOTE_ID = 3;
    static final String NO_PERMISSION = "WRITE_SECURE_SETTINGS is not granted to the app, so it cannot turn Wireless debugging on. "
            + "Grant it once over adb: adb shell pm grant com.festeraeb.nautik9 android.permission.WRITE_SECURE_SETTINGS";

    /** The foreground activity, when there is one: the prompt is also shown there as a dialog. */
    static volatile android.app.Activity visible;
    private static final Object LOCK = new Object();
    private static Boolean lastPermission;

    private WirelessDebug() {}

    private static SharedPreferences prefs(Context c) {
        return c.getSharedPreferences("wireless_debug", Context.MODE_PRIVATE);
    }

    static boolean hasPermission(Context c) {
        return c.checkSelfPermission(PERMISSION) == PackageManager.PERMISSION_GRANTED;
    }

    static boolean isOn(Context c) {
        try {
            return Settings.Global.getInt(c.getContentResolver(), SETTING, 0) == 1;
        } catch (RuntimeException e) {
            return false;
        }
    }

    /** null on success, else a plain reason. */
    static String set(Context c, boolean on) {
        if (!hasPermission(c)) return NO_PERMISSION;
        try {
            if (!Settings.Global.putInt(c.getContentResolver(), SETTING, on ? 1 : 0)) return "Android refused to change " + SETTING;
            return null;
        } catch (SecurityException e) {
            return NO_PERMISSION;
        } catch (RuntimeException e) {
            return "could not change " + SETTING + ": " + e;
        }
    }

    // ---- config (state, not code) ----

    private static long minutes(Context c, String key, long dflt, long max) {
        try {
            File f = new File(new File(c.getFilesDir(), "config"), "debug.json");
            long m = new JSONObject(new String(Files.readAllBytes(f.toPath()), StandardCharsets.UTF_8)).optLong(key, dflt);
            return m >= 1 && m <= max ? m : dflt;
        } catch (Exception e) {
            return dflt;
        }
    }

    static long idleMinutes(Context c) { return minutes(c, "idle_minutes", 30, 1440); }
    static long promptMinutes(Context c) { return minutes(c, "prompt_minutes", 10, 240); }

    // ---- reporting to the local server (which tells the fleet) ----

    /** Merge a status patch into what GET /api/admin/debug shows. Network: call off the main thread. */
    static void post(Context c, JSONObject patch) {
        Engines.get(c).adminPost("/api/admin/device/debug_state", patch.toString());
    }

    private static boolean wifiConnected(Context c) {
        try {
            ConnectivityManager cm = (ConnectivityManager) c.getSystemService(Context.CONNECTIVITY_SERVICE);
            for (android.net.Network n : cm.getAllNetworks()) {
                NetworkCapabilities nc = cm.getNetworkCapabilities(n);
                if (nc != null && nc.hasTransport(NetworkCapabilities.TRANSPORT_WIFI)) return true;
            }
        } catch (RuntimeException ignored) {
        }
        return false;
    }

    /** Called every poll: report the permission when it changes, expire a prompt, switch off when idle. */
    static void tick(Context c) {
        try {
            boolean perm = hasPermission(c);
            if (lastPermission == null || lastPermission != perm) {
                lastPermission = perm;
                post(c, new JSONObject().put("permission_granted", perm).put("wireless_debugging", isOn(c)));
            }
            SharedPreferences p = prefs(c);
            long now = System.currentTimeMillis();
            String pid = p.getString("prompt_id", "");
            if (!pid.isEmpty() && now >= p.getLong("prompt_expires", 0)) {
                p.edit().remove("prompt_id").apply();
                c.getSystemService(NotificationManager.class).cancel(NOTE_ID);
                post(c, new JSONObject().put("prompt", new JSONObject().put("id", pid).put("answer", "expired")
                        .put("detail", "nobody answered within " + promptMinutes(c) + " minutes; Wireless debugging stays off")));
                EngineService.reportDone(c, pid, false, "prompt expired unanswered");
            }
            if (p.getBoolean("on_by_app", false)) {
                if (!isOn(c)) {
                    // Switched off by someone else (or the phone rebooted): nothing left for us to do.
                    p.edit().remove("on_by_app").remove("off_at").apply();
                    post(c, new JSONObject().put("wireless_debugging", false).put("idle_off_at", JSONObject.NULL)
                            .put("adb_port", JSONObject.NULL).put("on_reason", JSONObject.NULL));
                } else if (now >= p.getLong("off_at", Long.MAX_VALUE)) {
                    turnOff(c, "idle for " + idleMinutes(c) + " minutes");
                }
            }
        } catch (Exception ignored) {
        }
    }

    // ---- the fleet's requests ----

    /** wireless_debug_prompt and wireless_debug_off actions from /api/admin/pending. */
    static void handleAction(Context c, JSONObject a) throws Exception {
        String id = a.optString("id");
        if ("wireless_debug_off".equals(a.optString("kind"))) {
            if (isOn(c) && !prefs(c).getBoolean("on_by_app", false)) {
                post(c, new JSONObject().put("last_event", "off request ignored: the owner turned Wireless debugging on by hand"));
                EngineService.reportDone(c, id, false, "the owner turned it on by hand; left on");
                return;
            }
            turnOff(c, "the fleet asked");
            EngineService.reportDone(c, id, true, "turned off");
            return;
        }
        if (!hasPermission(c)) {
            post(c, new JSONObject().put("permission_granted", false).put("last_error", NO_PERMISSION)
                    .put("prompt", new JSONObject().put("id", id).put("answer", "no_permission").put("detail", NO_PERMISSION)));
            EngineService.reportDone(c, id, false, "no WRITE_SECURE_SETTINGS");
            return;
        }
        SharedPreferences p = prefs(c);
        if (isOn(c)) {
            boolean ours = p.getBoolean("on_by_app", false);
            if (ours) p.edit().putLong("off_at", System.currentTimeMillis() + idleMinutes(c) * 60_000).apply();
            post(c, new JSONObject().put("wireless_debugging", true)
                    .put("idle_off_at", ours ? p.getLong("off_at", 0) / 1000 : JSONObject.NULL)
                    .put("prompt", new JSONObject().put("id", id).put("answer", "already_on").put("detail",
                            ours ? "already on; the idle timer was restarted (" + idleMinutes(c) + " min)" : "you turned it on yourself; the app will not turn it off")));
            EngineService.reportDone(c, id, true, "already on");
            return;
        }
        p.edit().putString("prompt_id", id).putLong("prompt_expires", System.currentTimeMillis() + promptMinutes(c) * 60_000).apply();
        if (!showPrompt(c, id)) {
            p.edit().remove("prompt_id").apply();
            String why = "the phone could not show the prompt (notifications are blocked and the app is not open)";
            post(c, new JSONObject().put("prompt", new JSONObject().put("id", id).put("answer", "failed").put("detail", why)));
            EngineService.reportDone(c, id, false, why);
        }
    }

    /** True if the owner can see the prompt (a notification with Yes / Not now, and a dialog if the app is open). */
    private static boolean showPrompt(Context c, String id) {
        NotificationManager nm = c.getSystemService(NotificationManager.class);
        boolean noted = false;
        if (nm.areNotificationsEnabled()) {
            nm.createNotificationChannel(new NotificationChannel(CHANNEL, "Button test requests", NotificationManager.IMPORTANCE_HIGH));
            Notification.Action yes = action(c, id, true), no = action(c, id, false);
            nm.notify(NOTE_ID, new Notification.Builder(c, CHANNEL)
                    .setContentTitle("Turn on Wireless debugging for a button test?")
                    .setContentText("Your home fleet asked. It turns itself off after " + idleMinutes(c) + " idle minutes.")
                    .setSmallIcon(c.getApplicationInfo().icon)
                    .setCategory(Notification.CATEGORY_STATUS)
                    .addAction(yes).addAction(no)
                    .setOngoing(true)
                    .build());
            noted = true;
        }
        final android.app.Activity a = visible;
        if (a != null) {
            new Handler(Looper.getMainLooper()).post(() -> {
                try {
                    new AlertDialog.Builder(a)
                            .setMessage("Turn on Wireless debugging for a button test?")
                            .setPositiveButton("Yes", (d, w) -> answer(a.getApplicationContext(), id, true))
                            .setNegativeButton("Not now", (d, w) -> answer(a.getApplicationContext(), id, false))
                            .setCancelable(false)
                            .show();
                } catch (RuntimeException ignored) {
                }
            });
            return true;
        }
        return noted;
    }

    private static Notification.Action action(Context c, String id, boolean yes) {
        Intent i = new Intent(c, DebugAnswer.class).putExtra(DebugAnswer.EXTRA_ID, id).putExtra(DebugAnswer.EXTRA_YES, yes)
                .setAction("answer-" + yes + "-" + id);
        PendingIntent pi = PendingIntent.getBroadcast(c, yes ? 1 : 2, i, PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
        return new Notification.Action.Builder(null, yes ? "Yes" : "Not now", pi).build();
    }

    /** The owner's answer (notification button or dialog). Runs the network part on its own thread. */
    static void answer(Context c, String id, boolean yes) {
        synchronized (LOCK) {
            SharedPreferences p = prefs(c);
            if (!id.equals(p.getString("prompt_id", ""))) return; // already answered, expired, or from an older request
            p.edit().remove("prompt_id").apply();
        }
        c.getSystemService(NotificationManager.class).cancel(NOTE_ID);
        new Thread(() -> {
            try {
                if (!yes) {
                    post(c, new JSONObject().put("prompt", new JSONObject().put("id", id).put("answer", "not_now").put("detail", "the owner pressed Not now")));
                    EngineService.reportDone(c, id, true, "owner said not now");
                    return;
                }
                String err = turnOn(c, "fleet_request", id);
                EngineService.reportDone(c, id, err == null, err == null ? "turned on" : err);
            } catch (Exception ignored) {
            }
        }, "debug-answer").start();
    }

    // ---- on and off ----

    /**
     * Turn Wireless debugging on, start the idle timer, read the adb port and tell the fleet.
     * Returns null on success or the plain reason it could not be turned on (the fleet is told that too).
     * Call off the main thread.
     */
    static String turnOn(Context c, String reason, String promptId) throws Exception {
        SharedPreferences p = prefs(c);
        boolean wasOn = isOn(c);
        String err = set(c, true);
        if (err != null) {
            JSONObject patch = new JSONObject().put("permission_granted", hasPermission(c)).put("wireless_debugging", wasOn)
                    .put("on_reason", reason).put("last_error", err).put("announce", true);
            if (promptId != null) patch.put("prompt", new JSONObject().put("id", promptId).put("answer", "no_permission").put("detail", err));
            post(c, patch);
            return err;
        }
        long offAt = System.currentTimeMillis() + idleMinutes(c) * 60_000;
        // A session the owner had already switched on stays theirs: we do not take over turning it off.
        SharedPreferences.Editor e = p.edit().putLong("off_at", offAt);
        if (!wasOn) e.putBoolean("on_by_app", true);
        e.apply();
        JSONObject patch = new JSONObject().put("permission_granted", true).put("wireless_debugging", true).put("on_reason", reason)
                .put("on_since", System.currentTimeMillis() / 1000).put("idle_off_at", p.getBoolean("on_by_app", false) ? offAt / 1000 : JSONObject.NULL)
                .put("last_error", JSONObject.NULL).put("wifi_connected", wifiConnected(c));
        if (promptId != null) patch.put("prompt", new JSONObject().put("id", promptId).put("answer", "yes").put("detail", "the owner pressed Yes"));
        post(c, patch); // the page of the fleet sees "on" at once; the port follows
        String[] found = discoverPort(c, 25_000);
        JSONObject second = new JSONObject();
        if (found[0] != null) second.put("adb_port", Integer.parseInt(found[0])).put("adb_port_note", JSONObject.NULL);
        else second.put("adb_port", JSONObject.NULL).put("adb_port_note", found[1]);
        second.put("announce", true);
        post(c, second);
        return null;
    }

    static void turnOff(Context c, String why) {
        String err = set(c, false);
        prefs(c).edit().remove("on_by_app").remove("off_at").apply();
        try {
            JSONObject patch = new JSONObject().put("wireless_debugging", err == null ? false : isOn(c)).put("idle_off_at", JSONObject.NULL)
                    .put("adb_port", JSONObject.NULL).put("adb_port_note", JSONObject.NULL).put("on_reason", JSONObject.NULL);
            if (err != null) patch.put("last_error", err);
            else patch.put("last_event", "turned off: " + why);
            post(c, patch);
        } catch (Exception ignored) {
        }
    }

    // ---- the adb TLS port ----

    /**
     * {port, null} or {null, why}. Tries the system property service.adb.tls.port (if an app may read
     * it), then looks for this phone's own _adb-tls-connect._tcp service with NsdManager. Wireless
     * debugging only listens while the phone is on Wi-Fi.
     */
    static String[] discoverPort(Context c, long timeoutMs) {
        long end = System.currentTimeMillis() + timeoutMs;
        String propNote = "service.adb.tls.port not readable by an app";
        for (int i = 0; i < 6 && System.currentTimeMillis() < end; i++) {
            String v = getprop("service.adb.tls.port");
            if (v != null && v.matches("\\d{2,5}") && Integer.parseInt(v) > 0) return new String[] {v, null};
            if (v != null && !v.isEmpty()) propNote = "service.adb.tls.port = " + v;
            if (v == null || v.isEmpty()) break; // property hidden from apps: do not wait for it
            sleep(1000);
        }
        if (!wifiConnected(c)) return new String[] {null, "the phone is not on Wi-Fi, so Wireless debugging is not listening yet"};
        long left = Math.max(3000, end - System.currentTimeMillis());
        String[] nsd = nsdPort(c, left);
        if (nsd[0] != null) return nsd;
        return new String[] {null, nsd[1] + "; " + propNote};
    }

    private static String getprop(String name) {
        try {
            Process pr = new ProcessBuilder("getprop", name).redirectErrorStream(true).start();
            try (BufferedReader r = new BufferedReader(new InputStreamReader(pr.getInputStream()))) {
                String s = r.readLine();
                pr.waitFor(2, TimeUnit.SECONDS);
                return s == null ? "" : s.trim();
            }
        } catch (Exception e) {
            return null;
        }
    }

    private static void sleep(long ms) {
        try {
            Thread.sleep(ms);
        } catch (InterruptedException ignored) {
            Thread.currentThread().interrupt();
        }
    }

    private static Set<String> myAddresses() {
        Set<String> s = new HashSet<>();
        try {
            for (NetworkInterface ni : Collections.list(NetworkInterface.getNetworkInterfaces())) {
                for (InetAddress a : Collections.list(ni.getInetAddresses())) s.add(a.getHostAddress());
            }
        } catch (Exception ignored) {
        }
        return s;
    }

    private static String[] nsdPort(Context c, long timeoutMs) {
        NsdManager nsd = (NsdManager) c.getSystemService(Context.NSD_SERVICE);
        if (nsd == null) return new String[] {null, "no NsdManager on this phone"};
        final LinkedBlockingQueue<NsdServiceInfo> seen = new LinkedBlockingQueue<>();
        final AtomicInteger startError = new AtomicInteger(0);
        final Set<String> mine = myAddresses();
        NsdManager.DiscoveryListener dl = new NsdManager.DiscoveryListener() {
            @Override public void onStartDiscoveryFailed(String t, int e) { startError.set(e); }
            @Override public void onStopDiscoveryFailed(String t, int e) {}
            @Override public void onDiscoveryStarted(String t) {}
            @Override public void onDiscoveryStopped(String t) {}
            @Override public void onServiceFound(NsdServiceInfo i) { seen.offer(i); }
            @Override public void onServiceLost(NsdServiceInfo i) {}
        };
        int others = 0;
        try {
            nsd.discoverServices("_adb-tls-connect._tcp", NsdManager.PROTOCOL_DNS_SD, dl);
            long end = System.currentTimeMillis() + timeoutMs;
            while (System.currentTimeMillis() < end) {
                if (startError.get() != 0) return new String[] {null, "mDNS discovery would not start (NSD error " + startError.get() + ")"};
                NsdServiceInfo i = seen.poll(500, TimeUnit.MILLISECONDS);
                if (i == null) continue;
                final CountDownLatch done = new CountDownLatch(1);
                final int[] port = {0};
                final boolean[] own = {false};
                nsd.resolveService(i, new NsdManager.ResolveListener() {
                    @Override public void onResolveFailed(NsdServiceInfo s, int e) { done.countDown(); }
                    @Override public void onServiceResolved(NsdServiceInfo s) {
                        if (s.getHost() != null && mine.contains(s.getHost().getHostAddress())) {
                            own[0] = true;
                            port[0] = s.getPort();
                        }
                        done.countDown();
                    }
                });
                done.await(3, TimeUnit.SECONDS);
                if (own[0]) return new String[] {String.valueOf(port[0]), null};
                others++;
            }
        } catch (InterruptedException ie) {
            Thread.currentThread().interrupt();
        } catch (RuntimeException e) {
            return new String[] {null, "mDNS discovery failed: " + e};
        } finally {
            try {
                nsd.stopServiceDiscovery(dl);
            } catch (RuntimeException ignored) {
            }
        }
        return new String[] {null, others == 0 ? "no _adb-tls-connect service was announced on Wi-Fi within " + timeoutMs / 1000 + " s"
                : "saw " + others + " _adb-tls-connect service(s), none at this phone's address"};
    }
}
