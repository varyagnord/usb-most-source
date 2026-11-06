package org.cgutman.usbip.config;

import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;

/** Private immutable-PendingIntent destination. Never starts USB from the background. */
public final class QuickActionActivity extends Activity {
  @Override
  public void onCreate(Bundle saved) {
    super.onCreate(saved);
    // A restored Activity opens the current screen; it never replays an uncertain action.
    QuickActions.accept(this, saved == null ? QuickActions.read(getIntent()) : null);
    startActivity(
        new Intent(this, UsbIpConfig.class)
            .addFlags(
                Intent.FLAG_ACTIVITY_NEW_TASK
                    | Intent.FLAG_ACTIVITY_CLEAR_TOP
                    | Intent.FLAG_ACTIVITY_SINGLE_TOP));
    finish();
  }
}
