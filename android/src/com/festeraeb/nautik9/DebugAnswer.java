package com.festeraeb.nautik9;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;

/** The Yes / Not now buttons on the "Turn on Wireless debugging for a button test?" notification. */
public class DebugAnswer extends BroadcastReceiver {
    static final String EXTRA_ID = "prompt_id";
    static final String EXTRA_YES = "yes";

    @Override
    public void onReceive(Context c, Intent i) {
        String id = i.getStringExtra(EXTRA_ID);
        if (id == null) return;
        WirelessDebug.answer(c.getApplicationContext(), id, i.getBooleanExtra(EXTRA_YES, false));
    }
}
