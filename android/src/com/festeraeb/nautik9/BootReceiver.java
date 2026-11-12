package com.festeraeb.nautik9;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;

/**
 * After a reboot (once the phone is unlocked: the app's files are in credential-encrypted
 * storage) or after this app was updated, start the server again if remote care is on, so the
 * home fleet can reach the phone without anyone opening the app.
 *
 * Android 15 forbids starting some foreground-service types (dataSync, camera, mediaPlayback,
 * phoneCall, mediaProjection, microphone) from BOOT_COMPLETED; EngineService is specialUse,
 * which is still allowed. MY_PACKAGE_REPLACED is likewise allowed to start one.
 */
public class BootReceiver extends BroadcastReceiver {
    @Override
    public void onReceive(Context c, Intent i) {
        String a = i == null ? null : i.getAction();
        if (!Intent.ACTION_BOOT_COMPLETED.equals(a) && !Intent.ACTION_MY_PACKAGE_REPLACED.equals(a)) return;
        if (!Engines.remoteCareOn(c)) return;
        try {
            c.startForegroundService(new Intent(c, EngineService.class));
        } catch (RuntimeException ignored) {
            // Not allowed right now (for example a battery restriction): the next app start covers it.
        }
    }
}
