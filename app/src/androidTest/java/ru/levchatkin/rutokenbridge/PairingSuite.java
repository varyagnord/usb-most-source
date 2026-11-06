package ru.levchatkin.rutokenbridge;

import android.app.*;
import android.content.*;
import android.content.pm.ActivityInfo;
import android.net.nsd.*;
import android.os.*;
import android.view.*;
import android.view.accessibility.AccessibilityNodeInfo;
import android.widget.*;
import java.io.*;
import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;
import java.util.*;
import java.util.concurrent.*;
import java.util.concurrent.atomic.AtomicReference;

/** Optimized APK camera lifecycle, ART QR and real Android NSD, without USB. */
final class PairingSuite {
  private static final String APP = "ru.levchatkin.rutokenbridge";
  private static final String SETUP = "org.cgutman.usbip.connection.ConnectionSetup";
  private static final String SCANNER = "org.cgutman.usbip.connection.PairScanner";
  private final Instrumentation test;
  private final AtomicReference<Activity> resumed = new AtomicReference<>();
  private int checks;

  PairingSuite(Instrumentation test) {
    this.test = test;
  }

  private void check(boolean value, String message) {
    checks++;
    if (!value) throw new AssertionError(message);
  }

  private void main(Runnable action) {
    Throwable[] failure = new Throwable[1];
    test.runOnMainSync(() -> {
      try {
        action.run();
      } catch (Throwable error) {
        failure[0] = error;
      }
    });
    if (failure[0] != null) throw new AssertionError("pairing UI action failed", failure[0]);
  }

  private void phase(String name) {
    Bundle b = new Bundle();
    b.putString("stream", "PASS " + name + "\n");
    test.sendStatus(0, b);
  }

  private View find(View root, Class<?> type, String text) {
    if (type.isInstance(root)
        && (text == null
            || root instanceof TextView && ((TextView) root).getText().toString().contains(text)))
      return root;
    if (root instanceof ViewGroup)
      for (int i = 0; i < ((ViewGroup) root).getChildCount(); i++) {
        View found = find(((ViewGroup) root).getChildAt(i), type, text);
        if (found != null) return found;
      }
    return null;
  }

  private void click(Activity a, String label) {
    main(() -> {
      View v = find(a.getWindow().getDecorView(), Button.class, label);
      check(v != null && v.performClick(), label);
    });
  }

  private Activity await(String name, Activity old) throws Exception {
    long end = SystemClock.elapsedRealtime() + 15000;
    while (SystemClock.elapsedRealtime() < end) {
      Activity a = resumed.get();
      if (a != null && a != old && a.getClass().getName().equals(name)) {
        test.waitForIdleSync();
        return a;
      }
      Thread.sleep(50);
    }
    throw new AssertionError("Activity not resumed: " + name);
  }

  private boolean contains(AccessibilityNodeInfo root, String value) {
    if (root == null) return false;
    if (root.getText() != null && root.getText().toString().contains(value)) return true;
    for (int i = 0; i < root.getChildCount(); i++)
      if (contains(root.getChild(i), value)) return true;
    return false;
  }

  private void waitDialog(String value, boolean expected) throws Exception {
    long end = SystemClock.elapsedRealtime() + 20000;
    do {
      if (contains(test.getUiAutomation().getRootInActiveWindow(), value) == expected) {
        checks++;
        return;
      }
      Thread.sleep(100);
    } while (SystemClock.elapsedRealtime() < end);
    throw new AssertionError("NSD UI expectation: " + value + "=" + expected);
  }

  private int cameraThreads() {
    int n = 0;
    for (Thread t : Thread.getAllStackTraces().keySet())
      if (t.isAlive() && t.getName().equals("usb-most-qr")) n++;
    return n;
  }

  private void cameraClosed() throws Exception {
    long end = SystemClock.elapsedRealtime() + 10000;
    while (cameraThreads() != 0 && SystemClock.elapsedRealtime() < end) Thread.sleep(100);
    check(cameraThreads() == 0, "camera worker released after activity close");
  }

  private byte[] asset(String name) throws Exception {
    try (InputStream input = test.getContext().getAssets().open(name);
        ByteArrayOutputStream out = new ByteArrayOutputStream()) {
      byte[] block = new byte[8192];
      int n;
      while ((n = input.read(block)) != -1) {
        if (out.size() + n > 1048576) throw new IOException("fixture bound");
        out.write(block, 0, n);
      }
      return out.toByteArray();
    }
  }

  private void qr() throws Exception {
    Class<?> api =
        Class.forName(
            "org.cgutman.usbip.jni.NativeConnection",
            true,
            test.getTargetContext().getClassLoader());
    Method decode = api.getMethod("decodeQr", byte[].class, int.class, int.class);
    byte[] pgm = asset("pairing.pgm");
    int offset = 0, lines = 0;
    while (lines < 3) if (pgm[offset++] == '\n') lines++;
    String[] header = new String(pgm, 0, offset, StandardCharsets.US_ASCII).split("\\s+");
    int width = Integer.parseInt(header[1]);
    byte[] gray = Arrays.copyOfRange(pgm, offset, pgm.length);
    String expected = new String(asset("pairing.txt"), StandardCharsets.UTF_8);
    for (int rotation = 0; rotation < 4; rotation++) {
      check(
          expected.equals(decode.invoke(null, gray, width, width)),
          "ART QR decode rotation " + rotation);
      byte[] next = new byte[gray.length];
      for (int y = 0; y < width; y++)
        for (int x = 0; x < width; x++) next[x * width + width - 1 - y] = gray[y * width + x];
      gray = next;
    }
    check(decode.invoke(null, null, width, width) == null, "null frame rejected");
    check(decode.invoke(null, gray, Integer.MAX_VALUE, width) == null, "overflow frame rejected");
    check(decode.invoke(null, new byte[32 * 32], 32, 32) == null, "blank camera frame rejected");
    phase("real ART/R8 QR decoding: four rotations, blank, null and overflow");
  }

  private AccessibilityNodeInfo denyButton(AccessibilityNodeInfo node) {
    if (node == null) return null;
    String id = node.getViewIdResourceName();
    String text = node.getText() == null ? "" : node.getText().toString();
    if (node.isEnabled()
        && node.isClickable()
        && (id != null && id.endsWith(":id/permission_deny_button")
            || text.equalsIgnoreCase("Deny")
            || text.equalsIgnoreCase("Don't allow"))) return node;
    for (int i = 0; i < node.getChildCount(); i++) {
      AccessibilityNodeInfo found = denyButton(node.getChild(i));
      if (found != null) return found;
    }
    return null;
  }

  private void visibleNodes(AccessibilityNodeInfo node, StringBuilder out) {
    if (node == null || out.length() > 4096) return;
    out.append(node.getViewIdResourceName()).append(':').append(node.getText()).append(';');
    for (int i = 0; i < node.getChildCount(); i++) visibleNodes(node.getChild(i), out);
  }

  private void deniedCamera(Activity setup) throws Exception {
    click(setup, "Сканировать QR-код");
    Activity scanner = await(SCANNER, null);
    long end = SystemClock.elapsedRealtime() + 10000;
    AccessibilityNodeInfo deny = null;
    while (deny == null && SystemClock.elapsedRealtime() < end) {
      deny = denyButton(test.getUiAutomation().getRootInActiveWindow());
      if (deny == null)
        for (android.view.accessibility.AccessibilityWindowInfo window :
            test.getUiAutomation().getWindows()) {
          deny = denyButton(window.getRoot());
          if (deny != null) break;
        }
      if (deny == null) Thread.sleep(100);
    }
    if (deny == null) {
      StringBuilder nodes = new StringBuilder();
      visibleNodes(test.getUiAutomation().getRootInActiveWindow(), nodes);
      throw new AssertionError("camera permission dialog not found: " + nodes);
    }
    boolean clicked = deny.performAction(AccessibilityNodeInfo.ACTION_CLICK);
    if (!clicked) {
      android.graphics.Rect bounds = new android.graphics.Rect();
      deny.getBoundsInScreen(bounds);
      long now = SystemClock.uptimeMillis();
      MotionEvent down =
          MotionEvent.obtain(
              now, now, MotionEvent.ACTION_DOWN, bounds.centerX(), bounds.centerY(), 0);
      MotionEvent up =
          MotionEvent.obtain(
              now, now + 50, MotionEvent.ACTION_UP, bounds.centerX(), bounds.centerY(), 0);
      down.setSource(android.view.InputDevice.SOURCE_TOUCHSCREEN);
      up.setSource(android.view.InputDevice.SOURCE_TOUCHSCREEN);
      try {
        clicked =
            test.getUiAutomation().injectInputEvent(down, true)
                && test.getUiAutomation().injectInputEvent(up, true);
      } finally {
        down.recycle();
        up.recycle();
      }
    }
    check(clicked, "camera permission denied by actual Android dialog");
    Thread.sleep(700);
    cameraClosed();
    check(
        (scanner.getWindow().getAttributes().flags & WindowManager.LayoutParams.FLAG_SECURE) != 0,
        "denied scanner remains protected");
    click(scanner, "Ввести код вручную");
    await(SETUP, null);
    phase("actual camera permission denial retains manual input and creates no capture worker");
  }

  int run(boolean denied, boolean cameraQr) throws Exception {
    qr();
    android.accessibilityservice.AccessibilityServiceInfo accessibility =
        test.getUiAutomation().getServiceInfo();
    final int priorFlags = accessibility.flags;
    accessibility.flags |=
        android.accessibilityservice.AccessibilityServiceInfo.FLAG_REPORT_VIEW_IDS
            | android.accessibilityservice.AccessibilityServiceInfo
                .FLAG_RETRIEVE_INTERACTIVE_WINDOWS;
    test.getUiAutomation().setServiceInfo(accessibility);
    Application app = (Application) test.getTargetContext().getApplicationContext();
    Application.ActivityLifecycleCallbacks callback =
        new Application.ActivityLifecycleCallbacks() {
          public void onActivityResumed(Activity a) {
            resumed.set(a);
          }

          public void onActivityCreated(Activity a, Bundle b) {}

          public void onActivityStarted(Activity a) {}

          public void onActivityPaused(Activity a) {}

          public void onActivityStopped(Activity a) {}

          public void onActivitySaveInstanceState(Activity a, Bundle b) {}

          public void onActivityDestroyed(Activity a) {
            resumed.compareAndSet(a, null);
          }
        };
    main(() -> app.registerActivityLifecycleCallbacks(callback));
    NsdManager nsd = (NsdManager) app.getSystemService(Context.NSD_SERVICE);
    CountDownLatch registered = new CountDownLatch(1);
    AtomicReference<String> serviceName = new AtomicReference<>();
    NsdManager.RegistrationListener registration =
        new NsdManager.RegistrationListener() {
          public void onServiceRegistered(NsdServiceInfo s) {
            serviceName.set(s.getServiceName());
            registered.countDown();
          }

          public void onRegistrationFailed(NsdServiceInfo s, int code) {
            registered.countDown();
          }

          public void onServiceUnregistered(NsdServiceInfo s) {}

          public void onUnregistrationFailed(NsdServiceInfo s, int code) {}
        };
    boolean published = false;
    try {
      Activity setup =
          test.startActivitySync(
              new Intent().setClassName(APP, SETUP).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
      Activity initial = setup;
      main(() ->
          ((Spinner) find(initial.getWindow().getDecorView(), Spinner.class, null))
              .setSelection(1));
      test.waitForIdleSync();
      check(
          (setup.getWindow().getAttributes().flags & WindowManager.LayoutParams.FLAG_SECURE) != 0,
          "pairing code protected against screenshots");
      if (denied) {
        deniedCamera(setup);
        return checks;
      }
      if (cameraQr) {
        click(setup, "Сканировать QR-код");
        String expected = new String(asset("pairing.txt"), StandardCharsets.UTF_8);
        boolean[] delivered = {false};
        long deadline = SystemClock.elapsedRealtime() + 20000;
        while (!delivered[0] && SystemClock.elapsedRealtime() < deadline) {
          main(() -> {
            Activity current = resumed.get();
            if (current != null && current.getClass().getName().equals(SETUP))
              delivered[0] =
                  find(current.getWindow().getDecorView(), EditText.class, expected) != null;
          });
          if (!delivered[0]) Thread.sleep(50);
        }
        check(
            delivered[0],
            "virtual camera QR decoded through actual Camera2 plane, Rust JNI and Activity result");
        cameraClosed();
        phase("imagefile camera pixels -> YUV -> Rust QR -> manual confirmation field");
        return checks;
      }
      // The emulator runner grants CAMERA explicitly, without changing real-device permissions.
      for (int round = 0; round < 3; round++) {
        click(setup, "Сканировать QR-код");
        Activity scanner = await(SCANNER, null);
        Thread.sleep(1500);
        check(
            (scanner.getWindow().getAttributes().flags & WindowManager.LayoutParams.FLAG_SECURE)
                != 0,
            "camera preview protected");
        check(cameraThreads() == 1, "exactly one camera capture worker");
        if (round == 1) {
          main(() -> scanner.setRequestedOrientation(ActivityInfo.SCREEN_ORIENTATION_LANDSCAPE));
          Activity rotated = await(SCANNER, scanner);
          Thread.sleep(1500);
          check(cameraThreads() == 1, "old camera worker released on rotation");
          click(rotated, "Ввести код вручную");
        } else click(scanner, "Ввести код вручную");
        setup = await(SETUP, null);
        cameraClosed();
      }
      phase("camera opens, rotates, closes and reopens without orphan workers");
      NsdServiceInfo service = new NsdServiceInfo();
      service.setServiceName("USB Most E2E " + SystemClock.elapsedRealtime());
      service.setServiceType("_usbmost._tcp.");
      service.setPort(43333);
      service.setAttribute("v", "1");
      nsd.registerService(service, NsdManager.PROTOCOL_DNS_SD, registration);
      published = true;
      check(
          registered.await(10, TimeUnit.SECONDS) && serviceName.get() != null,
          "actual Android NSD registration");
      click(setup, "Найти компьютер в локальной сети");
      waitDialog(serviceName.get(), true);
      check(
          find(setup.getWindow().getDecorView(), EditText.class, null) != null,
          "discovery does not replace configuration without QR");
      nsd.unregisterService(registration);
      published = false;
      waitDialog(serviceName.get(), false);
      test.getUiAutomation()
          .performGlobalAction(
              android.accessibilityservice.AccessibilityService.GLOBAL_ACTION_BACK);
      test.waitForIdleSync();
      click(setup, "Найти компьютер в локальной сети");
      test.getUiAutomation()
          .performGlobalAction(
              android.accessibilityservice.AccessibilityService.GLOBAL_ACTION_BACK);
      phase("actual Android NSD discovery, withdrawal and repeated dialog lifecycle");
      Activity done = setup;
      main(done::finish);
    } finally {
      accessibility.flags = priorFlags;
      test.getUiAutomation().setServiceInfo(accessibility);
      if (published) nsd.unregisterService(registration);
      main(() -> {
        Activity a = resumed.get();
        if (a != null
            && (a.getClass().getName().equals(SETUP) || a.getClass().getName().equals(SCANNER)))
          a.finish();
        app.unregisterActivityLifecycleCallbacks(callback);
      });
    }
    return checks;
  }
}
