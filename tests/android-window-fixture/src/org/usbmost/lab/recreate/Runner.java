package org.usbmost.lab.recreate;

import android.app.*;
import android.content.*;
import android.os.*;
import android.view.*;
import java.io.*;

public final class Runner extends Instrumentation {
  Bundle args;

  void touch(View view) throws Exception {
    android.graphics.Rect r = new android.graphics.Rect();
    runOnMainSync(() -> {
      if (!view.getGlobalVisibleRect(r) || r.isEmpty())
        throw new AssertionError("hidden touch target");
    });
    long t = SystemClock.uptimeMillis();
    MotionEvent d = MotionEvent.obtain(t, t, 0, r.centerX(), r.centerY(), 0),
        u = MotionEvent.obtain(t, t + 80, 1, r.centerX(), r.centerY(), 0);
    d.setSource(InputDevice.SOURCE_TOUCHSCREEN);
    u.setSource(InputDevice.SOURCE_TOUCHSCREEN);
    getUiAutomation().injectInputEvent(d, true);
    getUiAutomation().injectInputEvent(u, true);
    d.recycle();
    u.recycle();
  }

  void shell(String command) throws Exception {
    try (ParcelFileDescriptor fd = getUiAutomation().executeShellCommand(command);
        InputStream in = new FileInputStream(fd.getFileDescriptor())) {
      byte[] b = new byte[1024];
      while (in.read(b) >= 0) {}
    }
  }

  @Override
  public void onCreate(Bundle b) {
    super.onCreate(b);
    args = b;
    start();
  }

  @Override
  public void onStart() {
    Bundle result = new Bundle();
    try {
      String variant = args.getString("variant", "plain");
      String transition = args.getString("transition", "recreate");
      Screen old =
          (Screen)
              startActivitySync(
                  new Intent(getTargetContext(), Screen.class)
                      .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_CLEAR_TASK));
      waitForIdleSync();
      Thread.sleep(600);
      if (!variant.equals("plain")) {
        runOnMainSync(() -> old.open(variant.equals("keyboard")));
        Thread.sleep(1200);
      }
      if (transition.equals("font")) shell("settings put system font_scale 1.3");
      else if (transition.equals("density")) shell("wm density 200");
      else if (transition.equals("resize")) shell("wm size 320x720");
      else
        runOnMainSync(() -> {
          if (transition.equals("recreate")) old.recreate();
          else
            old.setRequestedOrientation(
                android.content.pm.ActivityInfo.SCREEN_ORIENTATION_LANDSCAPE);
        });
      long until = SystemClock.elapsedRealtime() + 10000;
      while ((Screen.current == old || !Screen.current.hasWindowFocus())
          && SystemClock.elapsedRealtime() < until) Thread.sleep(100);
      waitForIdleSync();
      Thread.sleep(1200);
      Screen next = Screen.current;
      boolean replaced = next != old;
      boolean noRelaunch = transition.equals("resize") && !replaced;
      if (noRelaunch && next.dialog != null && next.dialog.isShowing()) {
        touch(next.dialog.getButton(android.content.DialogInterface.BUTTON_NEGATIVE));
        Thread.sleep(1200);
      }
      boolean focused = next.hasWindowFocus();
      try (ParcelFileDescriptor p =
              getUiAutomation().executeShellCommand("dumpsys window windows");
          InputStream in = new FileInputStream(p.getFileDescriptor());
          OutputStream out =
              new FileOutputStream(
                  new File(
                      getTargetContext().getExternalFilesDir(null),
                      "windows-" + variant + "-" + transition + ".txt"))) {
        byte[] buf = new byte[8192];
        int n;
        while ((n = in.read(buf)) > 0) out.write(buf, 0, n);
      }
      if ((replaced || noRelaunch) && focused) {
        touch(next.button);
        Thread.sleep(500);
      }
      result.putString(
          "stream",
          "MINIMAL variant="
              + variant
              + " transition="
              + transition
              + " replaced="
              + replaced
              + " focus="
              + focused
              + " clicks="
              + next.clicks
              + "\n");
      finish((replaced || noRelaunch) && focused && next.clicks == 1 ? -1 : 0, result);
    } catch (Throwable e) {
      result.putString("stream", "FAIL " + e);
      finish(0, result);
    }
  }
}
