package com.festeraeb.nautik9;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Intent;
import android.content.pm.PackageInstaller;
import android.content.pm.ServiceInfo;
import android.os.Build;
import android.os.IBinder;

import java.io.File;
import java.io.FileInputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.security.MessageDigest;
import java.util.Set;
import java.util.concurrent.ConcurrentHashMap;

/**
 * Keeps the app process in the foreground while the server and the on-device model run.
 * Without it Android kills the whole app ("excessive cpu" phantom-process kill) as soon as the
 * model works hard while you are in another app or the screen is off.
 *
 * It also starts the server itself (so it can run with no screen open: after a reboot with remote
 * care on, see BootReceiver) and every 20 s asks the server for device actions the home fleet
 * queued (GET /api/admin/pending). An install_apk action opens an installer session; Android shows
 * its own confirmation and InstallResult reports the outcome.
 */
public class EngineService extends Service {
    private static final String CHANNEL = "engines";
    private static final long POLL_MS = 20_000;
    /** Action ids already handed to the installer by this process (not started twice). */
    private static final Set<String> handed = ConcurrentHashMap.newKeySet();
    private Thread poller;
    private Boolean shownRemoteCare;

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        NotificationManager nm = getSystemService(NotificationManager.class);
        nm.createNotificationChannel(new NotificationChannel(CHANNEL, "NautiK9 server", NotificationManager.IMPORTANCE_LOW));
        boolean rc = Engines.remoteCareOn(this);
        shownRemoteCare = rc;
        if (Build.VERSION.SDK_INT >= 34) startForeground(1, notification(rc), ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE);
        else startForeground(1, notification(rc));
        new Thread(() -> {
            try {
                Engines.get(this).startAll();
            } catch (Exception ignored) {
                // MainActivity shows the error when it is open; the poll below retries nothing by itself.
            }
        }, "engines-start").start();
        synchronized (this) {
            if (poller == null || !poller.isAlive()) {
                poller = new Thread(this::pollLoop, "remote-care-poll");
                poller.setDaemon(true);
                poller.start();
            }
        }
        // With remote care on the service starts the server itself, so a restart after the process died is
        // wanted. With it off, nothing is left to restart (the activity starts the engines): not sticky.
        return rc ? START_STICKY : START_NOT_STICKY;
    }

    /**
     * The task was swiped away (onDestroy of the activity is not guaranteed). With remote care on the
     * server and this service stay up so the home fleet can still reach the phone and only the
     * on-device model stops; otherwise stop the native processes and the service.
     */
    @Override
    public void onTaskRemoved(Intent rootIntent) {
        new Thread(() -> {
            if (Engines.remoteCareOn(this)) {
                Engines.get(this).stopLlm();
            } else {
                Engines.get(this).stopAll();
                stopSelf();
            }
        }, "engines-stop").start();
        super.onTaskRemoved(rootIntent);
    }

    private Notification notification(boolean remoteCare) {
        PendingIntent open = PendingIntent.getActivity(this, 0, new Intent(this, MainActivity.class), PendingIntent.FLAG_IMMUTABLE);
        return new Notification.Builder(this, CHANNEL)
                .setContentTitle("NautiK9 is running")
                .setContentText(remoteCare ? "Remote care on: answers the home fleet over Tailscale" : "Server and on-device model, on this phone only")
                .setSmallIcon(getApplicationInfo().icon)
                .setContentIntent(open)
                .setOngoing(true)
                .build();
    }

    private void pollLoop() {
        while (!Thread.currentThread().isInterrupted()) {
            try {
                Thread.sleep(POLL_MS);
            } catch (InterruptedException e) {
                return;
            }
            try {
                boolean rc = Engines.remoteCareOn(this);
                if (shownRemoteCare == null || rc != shownRemoteCare) {
                    shownRemoteCare = rc;
                    getSystemService(NotificationManager.class).notify(1, notification(rc));
                }
                pollOnce();
            } catch (Exception ignored) {
                // Server not up yet, or a malformed answer: try again next round.
            }
        }
    }

    private void pollOnce() throws Exception {
        String body = Engines.get(this).adminGet("/api/admin/pending");
        if (body == null) return;
        org.json.JSONArray actions = new org.json.JSONObject(body).optJSONArray("actions");
        if (actions == null) return;
        for (int k = 0; k < actions.length(); k++) {
            org.json.JSONObject a = actions.getJSONObject(k);
            String id = a.optString("id");
            if (id.isEmpty() || !"install_apk".equals(a.optString("kind")) || !handed.add(id)) continue;
            try {
                install(id, a.getString("path"), a.getString("sha256"));
            } catch (Exception e) {
                reportDone(this, id, false, "could not start the install: " + e);
            }
        }
    }

    /** Copy the APK into an installer session, checking its sha256 again on the way, and commit. */
    private void install(String id, String path, String sha256) throws Exception {
        File f = new File(path);
        PackageInstaller pi = getPackageManager().getPackageInstaller();
        PackageInstaller.SessionParams params = new PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL);
        params.setAppPackageName(getPackageName());
        params.setSize(f.length());
        int sid = pi.createSession(params);
        try (PackageInstaller.Session s = pi.openSession(sid)) {
            MessageDigest md = MessageDigest.getInstance("SHA-256");
            try (InputStream in = new FileInputStream(f); OutputStream out = s.openWrite("base.apk", 0, f.length())) {
                byte[] b = new byte[1 << 16];
                for (int n; (n = in.read(b)) > 0; ) {
                    md.update(b, 0, n);
                    out.write(b, 0, n);
                }
                s.fsync(out);
            }
            StringBuilder hex = new StringBuilder();
            for (byte x : md.digest()) hex.append(String.format("%02x", x));
            if (!hex.toString().equalsIgnoreCase(sha256)) {
                s.abandon();
                reportDone(this, id, false, "sha256 mismatch on the phone; not installed");
                return;
            }
            Intent result = new Intent(this, InstallResult.class).putExtra(InstallResult.EXTRA_ACTION_ID, id);
            // Mutable: the installer fills in the status extras. The intent is explicit, so this is allowed.
            PendingIntent pend = PendingIntent.getBroadcast(this, sid, result, PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_MUTABLE);
            s.commit(pend.getIntentSender());
        }
    }

    /** POST /api/admin/pending/{id}/done. Network: call off the main thread. */
    static void reportDone(android.content.Context c, String id, boolean ok, String detail) {
        try {
            String json = new org.json.JSONObject().put("ok", ok).put("detail", detail).toString();
            Engines.get(c).adminPost("/api/admin/pending/" + java.net.URLEncoder.encode(id, "UTF-8") + "/done", json);
        } catch (Exception ignored) {
        }
    }

    @Override
    public void onDestroy() {
        synchronized (this) {
            if (poller != null) poller.interrupt();
        }
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }
}
