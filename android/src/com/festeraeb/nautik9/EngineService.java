package com.festeraeb.nautik9;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.os.Build;
import android.os.IBinder;

/**
 * Keeps the app process in the foreground while the server and the on-device model run.
 * Without it Android kills the whole app ("excessive cpu" phantom-process kill) as soon as the
 * model works hard while you are in another app or the screen is off.
 */
public class EngineService extends Service {
    private static final String CHANNEL = "engines";

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        NotificationManager nm = getSystemService(NotificationManager.class);
        nm.createNotificationChannel(new NotificationChannel(CHANNEL, "NautiK9 server", NotificationManager.IMPORTANCE_LOW));
        PendingIntent open = PendingIntent.getActivity(this, 0, new Intent(this, MainActivity.class), PendingIntent.FLAG_IMMUTABLE);
        Notification n = new Notification.Builder(this, CHANNEL)
                .setContentTitle("NautiK9 is running")
                .setContentText("Server and on-device model, on this phone only")
                .setSmallIcon(getApplicationInfo().icon)
                .setContentIntent(open)
                .setOngoing(true)
                .build();
        if (Build.VERSION.SDK_INT >= 34) startForeground(1, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE);
        else startForeground(1, n);
        // Not sticky: this service only keeps the process alive for engines the activity started.
        // A sticky restart after the process died would show "running" with nothing running.
        return START_NOT_STICKY;
    }

    /** The task was swiped away: stop the native processes and this service (onDestroy of the activity is not guaranteed). */
    @Override
    public void onTaskRemoved(Intent rootIntent) {
        new Thread(() -> {
            Engines.get(this).stopAll();
            stopSelf();
        }, "engines-stop").start();
        super.onTaskRemoved(rootIntent);
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }
}
