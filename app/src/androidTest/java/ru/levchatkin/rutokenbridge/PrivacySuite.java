package ru.levchatkin.rutokenbridge;

import android.app.*;
import android.content.*;
import android.graphics.Bitmap;
import android.os.*;
import android.view.*;
import android.widget.*;
import java.io.*;
import java.util.concurrent.atomic.AtomicReference;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import org.cgutman.usbip.service.UsbIpService;

/** Real settings navigation, offline text, recreation and visible narrow layout. */
final class PrivacySuite {
  private static final String PRIVACY = "org.cgutman.usbip.config.PrivacyActivity";
  private final Instrumentation test;
  private final AtomicReference<Activity> resumed = new AtomicReference<>();
  private int checks;

  PrivacySuite(Instrumentation test) { this.test = test; }

  private void check(boolean ok, String message) {
    checks++;
    if (!ok) throw new AssertionError(message);
  }

  private void main(Runnable action) {
    Throwable[] error = {null};
    test.runOnMainSync(() -> { try { action.run(); } catch (Throwable t) { error[0] = t; } });
    if (error[0] != null) throw new AssertionError("privacy UI", error[0]);
    test.waitForIdleSync();
  }

  private View find(View view, String text) {
    if (!view.isShown()) return null;
    if (view instanceof Button && ((Button) view).getText().toString().equals(text)) return view;
    if (view instanceof ViewGroup) {
      ViewGroup group = (ViewGroup) view;
      for (int i = 0; i < group.getChildCount(); i++) {
        View result = find(group.getChildAt(i), text);
        if (result != null) return result;
      }
    }
    return null;
  }

  private View find(String text) {
    View[] found = {null};
    main(() -> found[0] = find(resumed.get().getWindow().getDecorView(), text));
    return found[0];
  }

  private void click(String text) throws Exception {
    View target = find(text);
    check(target != null, "control exists: " + text);
    main(() -> target.requestRectangleOnScreen(new android.graphics.Rect(0, 0, target.getWidth(), target.getHeight()), true));
    Thread.sleep(150);
    main(() -> {
      android.graphics.Rect visible = new android.graphics.Rect();
      check(target.getGlobalVisibleRect(visible) && visible.width() > 0 && visible.height() >= target.getHeight() - 2, "control fully reachable: " + text);
      check(target.performClick(), "control clicked: " + text);
    });
  }

  private Activity await(String name) throws Exception {
    long until = SystemClock.elapsedRealtime() + 5000;
    while (SystemClock.elapsedRealtime() < until) {
      Activity activity = resumed.get();
      if (activity != null && activity.getClass().getName().equals(name)) {
        test.waitForIdleSync();
        return activity;
      }
      Thread.sleep(50);
    }
    throw new AssertionError("activity not resumed: " + name);
  }

  private void geometry(View view) {
    if (!view.isShown()) return;
    if (view instanceof TextView) {
      TextView text = (TextView) view;
      android.text.Layout layout = text.getLayout();
      if (text.length() > 0) {
        check(layout != null && text.getWidth() > 0, "text measured");
        check(text.getHeight() >= layout.getHeight() + text.getCompoundPaddingTop() + text.getCompoundPaddingBottom(), "text height fits");
        for (int line = 0; line < layout.getLineCount(); line++) {
          check(layout.getEllipsisCount(line) == 0, "text not ellipsized");
          // getLineWidth includes the trailing space at a wrapped line. It
          // can exceed the viewport even when every visible glyph fits.
          check(layout.getLineMax(line) <= layout.getWidth() + 2,
              "visible line fits: " + line + " extent=" + layout.getLineMax(line)
                  + " width=" + layout.getWidth());
        }
      }
    }
    if (view instanceof ViewGroup) {
      ViewGroup group = (ViewGroup) view;
      for (int i = 0; i < group.getChildCount(); i++) geometry(group.getChildAt(i));
    }
  }

  private void screenshot(String stage) throws Exception {
    File folder = new File(test.getTargetContext().getExternalFilesDir(null), "privacy-proof");
    check(folder.isDirectory() || folder.mkdir(), "proof directory");
    // Activity resume/layout can precede the final window transition frame.
    test.getUiAutomation().waitForIdle(500, 5000);
    Bitmap bitmap = null;
    long deadline = SystemClock.elapsedRealtime() + 2000;
    while (bitmap == null && SystemClock.elapsedRealtime() < deadline) {
      bitmap = test.getUiAutomation().takeScreenshot();
      if (bitmap == null) Thread.sleep(100);
    }
    String capture = "display";
    if (bitmap == null && Build.VERSION.SDK_INT == 26) {
      // API26's full-display screenshot can fail after a virtual-display size
      // change. PixelCopy reads this actual Activity surface, not a redraw of
      // an offscreen View. Record the narrower capture scope explicitly.
      Activity current = resumed.get();
      View root = current.getWindow().getDecorView();
      Bitmap surface = Bitmap.createBitmap(root.getWidth(), root.getHeight(), Bitmap.Config.ARGB_8888);
      CountDownLatch done = new CountDownLatch(1);
      AtomicInteger result = new AtomicInteger(-1);
      main(() -> PixelCopy.request(current.getWindow(), surface,
          status -> { result.set(status); done.countDown(); }, new Handler(Looper.getMainLooper())));
      boolean success = done.await(5, TimeUnit.SECONDS) && result.get() == PixelCopy.SUCCESS;
      if (!success) {
        // PixelCopy may still hold the bitmap if its callback has not run.
        // Keep it live in that case; do not recycle memory used by the system.
        if (done.getCount() == 0) surface.recycle();
        throw new AssertionError("Activity surface capture failed: " + result.get());
      }
      bitmap = surface;
      capture = "activity_surface_pixelcopy";
    }
    check(bitmap != null, "real display or owned Activity surface captured");
    try (FileOutputStream file = new FileOutputStream(new File(folder, stage + ".png"))) {
      check(bitmap.compress(Bitmap.CompressFormat.PNG, 100, file), "screenshot saved");
    } finally { bitmap.recycle(); }
    try (FileOutputStream file = new FileOutputStream(new File(folder, stage + ".json"))) {
      file.write(("{\"capture\":\"" + capture + "\",\"api\":" + Build.VERSION.SDK_INT + "}\n")
          .getBytes(java.nio.charset.StandardCharsets.UTF_8));
    }
  }

  int run(Activity home) throws Exception {
    Application app = (Application) test.getTargetContext().getApplicationContext();
    Application.ActivityLifecycleCallbacks lifecycle = new Application.ActivityLifecycleCallbacks() {
      public void onActivityResumed(Activity a) { resumed.set(a); }
      public void onActivityCreated(Activity a, Bundle b) {}
      public void onActivityStarted(Activity a) {}
      public void onActivityPaused(Activity a) {}
      public void onActivityStopped(Activity a) {}
      public void onActivitySaveInstanceState(Activity a, Bundle b) {}
      public void onActivityDestroyed(Activity a) {}
    };
    resumed.set(home); app.registerActivityLifecycleCallbacks(lifecycle);
    try {
      Context context = test.getTargetContext();
      int camera = context.checkSelfPermission(android.Manifest.permission.CAMERA);
      check(!context.getPackageManager().getActivityInfo(
          new ComponentName(context.getPackageName(), PRIVACY), 0).exported,
          "privacy activity is internal");
      if (find("Позже") != null) click("Позже");
      click("Настройки");
      click("Данные и защита");
      Activity notice = await(PRIVACY);
      screenshot("top");
      main(() -> geometry(notice.getWindow().getDecorView()));
      check(!UsbIpService.running, "reading notice does not start bridge");
      int scrollId = notice.getResources().getIdentifier("privacy_scroll", "id", notice.getPackageName());
      ScrollView scroll = notice.findViewById(scrollId);
      check(scroll != null && scroll.canScrollVertically(1), "long notice is scrollable");
      main(() -> scroll.scrollTo(0, scroll.getChildAt(0).getHeight()));
      Thread.sleep(200);
      screenshot("bottom");
      int y = scroll.getScrollY();
      check(y > 0, "lower notice reached");
      main(notice::recreate);
      long until = SystemClock.elapsedRealtime() + 5000;
      while (resumed.get() == notice && SystemClock.elapsedRealtime() < until) Thread.sleep(50);
      Activity restored = await(PRIVACY);
      check(restored != notice, "activity recreated");
      Thread.sleep(200);
      main(() -> {
        ScrollView after = restored.findViewById(scrollId);
        check(Math.abs(after.getScrollY() - y) <= 2, "scroll position survives recreation");
        geometry(restored.getWindow().getDecorView());
      });
      click("Компьютеры и доступ");
      Activity computers = await("org.cgutman.usbip.config.ComputersActivity");
      main(computers::finish);
      await(PRIVACY);
      click("Назад");
      await("org.cgutman.usbip.config.UsbIpConfig");
      check(find("Данные и защита") != null, "back returns to settings");
      check(!UsbIpService.running, "access navigation leaves bridge stopped");
      check(context.checkSelfPermission(android.Manifest.permission.CAMERA) == camera,
          "reading notice does not change camera permission");
      return checks;
    } finally {
      app.unregisterActivityLifecycleCallbacks(lifecycle);
      Activity current = resumed.get();
      if (current != null && current != home) main(current::finish);
    }
  }
}
