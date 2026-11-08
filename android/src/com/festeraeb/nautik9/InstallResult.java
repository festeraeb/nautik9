package com.festeraeb.nautik9;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageInstaller;

/**
 * The installer's answer for an update session (EngineService.install). When Android needs the
 * user's confirmation it hands back its dialog: we open it, and also post a notification that
 * opens it, because Android may not let a background app bring up a screen. Any final status is
 * reported to the server (POST /api/admin/pending/{id}/done). A successful self-update replaces
 * this process first, so success is usually seen as the new version answering, not as a report.
 */
public class InstallResult extends BroadcastReceiver {
    static final String EXTRA_ACTION_ID = "action_id";
    private static final String CHANNEL = "updates";
    private static final int NOTE_ID = 2;

    @Override
    public void onReceive(Context c, Intent i) {
        int status = i.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE);
        String id = i.getStringExtra(EXTRA_ACTION_ID);
        NotificationManager nm = c.getSystemService(NotificationManager.class);
        if (status == PackageInstaller.STATUS_PENDING_USER_ACTION) {
            Intent confirm = android.os.Build.VERSION.SDK_INT >= 33
                    ? i.getParcelableExtra(Intent.EXTRA_INTENT, Intent.class)
                    : i.getParcelableExtra(Intent.EXTRA_INTENT);
            if (confirm == null) return;
            confirm.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
            nm.createNotificationChannel(new NotificationChannel(CHANNEL, "NautiK9 updates", NotificationManager.IMPORTANCE_HIGH));
            PendingIntent tap = PendingIntent.getActivity(c, 0, confirm, PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
            nm.notify(NOTE_ID, new Notification.Builder(c, CHANNEL)
                    .setContentTitle("NautiK9 update ready")
                    .setContentText("Your home fleet sent an update. Tap to install it.")
                    .setSmallIcon(c.getApplicationInfo().icon)
                    .setContentIntent(tap)
                    .setAutoCancel(true)
                    .build());
            try {
                c.startActivity(confirm);
            } catch (RuntimeException ignored) {
                // Blocked from the background: the notification is the way in.
            }
            return;
        }
        nm.cancel(NOTE_ID);
        if (id == null) return;
        String msg = i.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE);
        boolean ok = status == PackageInstaller.STATUS_SUCCESS;
        String detail = statusName(status) + (msg == null ? "" : ": " + msg);
        PendingResult pr = goAsync();
        new Thread(() -> {
            try {
                EngineService.reportDone(c, id, ok, detail);
            } finally {
                pr.finish();
            }
        }, "install-result").start();
    }

    static String statusName(int s) {
        switch (s) {
            case PackageInstaller.STATUS_SUCCESS: return "STATUS_SUCCESS";
            case PackageInstaller.STATUS_FAILURE_ABORTED: return "STATUS_FAILURE_ABORTED (declined or cancelled)";
            case PackageInstaller.STATUS_FAILURE_BLOCKED: return "STATUS_FAILURE_BLOCKED";
            case PackageInstaller.STATUS_FAILURE_CONFLICT: return "STATUS_FAILURE_CONFLICT (signed with another key?)";
            case PackageInstaller.STATUS_FAILURE_INCOMPATIBLE: return "STATUS_FAILURE_INCOMPATIBLE";
            case PackageInstaller.STATUS_FAILURE_INVALID: return "STATUS_FAILURE_INVALID";
            case PackageInstaller.STATUS_FAILURE_STORAGE: return "STATUS_FAILURE_STORAGE";
            default: return "STATUS_FAILURE (" + s + ")";
        }
    }
}
