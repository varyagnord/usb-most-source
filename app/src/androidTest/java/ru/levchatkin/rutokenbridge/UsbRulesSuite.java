package ru.levchatkin.rutokenbridge;

import android.app.*;
import android.content.*;
import android.hardware.usb.UsbDevice;
import android.os.*;
import android.view.accessibility.AccessibilityNodeInfo;
import java.io.*;
import java.lang.reflect.Field;
import java.nio.charset.StandardCharsets;
import java.util.*;
import java.util.concurrent.TimeUnit;
import java.util.function.BooleanSupplier;
import org.cgutman.usbip.connection.ComputerRepository;
import org.cgutman.usbip.service.UsbInventory;
import org.json.*;

/** Optimized UI + actual Rust/JNI/Keystore, with owned synthetic Android USB objects. */
final class UsbRulesSuite {
  private static final String APP = "ru.levchatkin.rutokenbridge";
  private final Instrumentation test;
  private final Context context;
  private int checks;
  private ComputerRepository repo;
  private UsbFixture fixture;
  private UsbInventory inventory;

  UsbRulesSuite(Instrumentation test) {
    this.test = test;
    context = test.getTargetContext();
    android.accessibilityservice.AccessibilityServiceInfo service =
        test.getUiAutomation().getServiceInfo();
    service.flags |=
        android.accessibilityservice.AccessibilityServiceInfo.FLAG_RETRIEVE_INTERACTIVE_WINDOWS;
    test.getUiAutomation().setServiceInfo(service);
  }

  private void check(boolean value, String label) {
    checks++;
    if (!value) throw new AssertionError(label);
  }

  private void main(Runnable action) {
    Throwable[] failure = {null};
    test.runOnMainSync(() -> {
      try {
        action.run();
      } catch (Throwable error) {
        failure[0] = error;
      }
    });
    if (failure[0] != null) throw new AssertionError("USB rules UI", failure[0]);
  }

  private void await(BooleanSupplier ready, String label) throws Exception {
    long end = SystemClock.elapsedRealtime() + 12000;
    while (!ready.getAsBoolean() && SystemClock.elapsedRealtime() < end) Thread.sleep(50);
    check(ready.getAsBoolean(), label);
  }

  private AccessibilityNodeInfo find(AccessibilityNodeInfo node, String text, String type) {
    if (node == null) return null;
    if (node.isVisibleToUser()
        && node.isEnabled()
        && (type == null || type.contentEquals(node.getClassName()))
        && (text == null
            || text.equalsIgnoreCase(node.getText() == null ? "" : node.getText().toString())
            || text.equalsIgnoreCase(
                node.getContentDescription() == null
                    ? ""
                    : node.getContentDescription().toString())))
      return AccessibilityNodeInfo.obtain(node);
    for (int i = 0; i < node.getChildCount(); i++) {
      AccessibilityNodeInfo child = node.getChild(i);
      AccessibilityNodeInfo found = find(child, text, type);
      if (child != null) child.recycle();
      if (found != null) return found;
    }
    return null;
  }

  private AccessibilityNodeInfo node(String text, String type) throws Exception {
    long end = SystemClock.elapsedRealtime() + 10000;
    int attempt = 0;
    do {
      AccessibilityNodeInfo root = root();
      AccessibilityNodeInfo found = find(root, text, type);
      if (found != null && !fullyVisible(found)) {
        found.performAction(
            AccessibilityNodeInfo.AccessibilityAction.ACTION_SHOW_ON_SCREEN.getId());
        found.recycle();
        found = null;
      }
      if (found == null && root != null && attempt++ > 1) {
        AccessibilityNodeInfo scroll = scrollable(root);
        if (scroll != null) {
          scroll.performAction(
              attempt % 20 < 10
                  ? AccessibilityNodeInfo.ACTION_SCROLL_FORWARD
                  : AccessibilityNodeInfo.ACTION_SCROLL_BACKWARD);
          scroll.recycle();
        }
      }
      if (root != null) root.recycle();
      if (found != null) return found;
      Thread.sleep(100);
    } while (SystemClock.elapsedRealtime() < end);
    screenshot("failure");
    StringBuilder tree = new StringBuilder();
    AccessibilityNodeInfo root = root();
    dump(root, tree, 0);
    if (root != null) root.recycle();
    System.out.println("Owned fixture accessibility tree:\n" + tree);
    throw new AssertionError("UI node not visible: " + text + " " + type);
  }

  private AccessibilityNodeInfo root() {
    AccessibilityNodeInfo root = test.getUiAutomation().getRootInActiveWindow();
    if (root != null) return root;
    // Older Android may retain the removed dialog as its active window after
    // recreation. Retrieve the actually focused app window, as accessibility
    // services with interactive-window support do; never click hidden controls.
    for (android.view.accessibility.AccessibilityWindowInfo window :
        test.getUiAutomation().getWindows()) {
      if (window.isFocused()) {
        AccessibilityNodeInfo candidate = window.getRoot();
        if (candidate != null && APP.contentEquals(candidate.getPackageName())) root = candidate;
        else if (candidate != null) candidate.recycle();
      }
      window.recycle();
    }
    return root;
  }

  private void dump(AccessibilityNodeInfo node, StringBuilder out, int depth) {
    if (node == null || depth > 20 || out.length() > 32768) return;
    android.graphics.Rect bounds = new android.graphics.Rect();
    node.getBoundsInScreen(bounds);
    out.append(node.getClassName())
        .append(' ')
        .append(node.getText())
        .append(' ')
        .append(node.getContentDescription())
        .append(' ')
        .append(bounds)
        .append(" visible=")
        .append(node.isVisibleToUser())
        .append(" enabled=")
        .append(node.isEnabled())
        .append(" full=")
        .append(fullyVisible(node))
        .append('\n');
    for (int i = 0; i < node.getChildCount(); i++) {
      AccessibilityNodeInfo child = node.getChild(i);
      dump(child, out, depth + 1);
      if (child != null) child.recycle();
    }
  }

  private boolean fullyVisible(AccessibilityNodeInfo node) {
    android.graphics.Rect bounds = new android.graphics.Rect();
    node.getBoundsInScreen(bounds);
    android.graphics.Rect allowed =
        new android.graphics.Rect(
            0,
            0,
            context.getResources().getDisplayMetrics().widthPixels,
            context.getResources().getDisplayMetrics().heightPixels);
    AccessibilityNodeInfo parent = node.getParent();
    while (parent != null) {
      android.graphics.Rect clip = new android.graphics.Rect();
      parent.getBoundsInScreen(clip);
      boolean intersects = allowed.intersect(clip);
      AccessibilityNodeInfo next = parent.getParent();
      parent.recycle();
      parent = next;
      if (!intersects) {
        if (parent != null) parent.recycle();
        return false;
      }
    }
    boolean touchHeight =
        !"android.widget.Button".contentEquals(node.getClassName())
            || bounds.height() >= 40 * context.getResources().getDisplayMetrics().density;
    return !bounds.isEmpty() && allowed.contains(bounds) && touchHeight;
  }

  private AccessibilityNodeInfo scrollable(AccessibilityNodeInfo node) {
    if (node == null) return null;
    if (node.isVisibleToUser() && node.isScrollable()) return AccessibilityNodeInfo.obtain(node);
    for (int i = 0; i < node.getChildCount(); i++) {
      AccessibilityNodeInfo child = node.getChild(i), result = scrollable(child);
      if (child != null) child.recycle();
      if (result != null) return result;
    }
    return null;
  }

  private void click(String text) throws Exception {
    AccessibilityNodeInfo node = node(text, null);
    try {
      check(fullyVisible(node), "entire action is physically visible: " + text);
      check(node.performAction(AccessibilityNodeInfo.ACTION_CLICK), "UI click " + text);
    } finally {
      node.recycle();
    }
    test.waitForIdleSync();
    Thread.sleep(200); // Accessibility click is dispatched asynchronously before the next frame.
  }

  private android.view.View described(android.view.View view, String label) {
    if (view.getContentDescription() != null && label.contentEquals(view.getContentDescription()))
      return view;
    if (view instanceof android.view.ViewGroup) {
      android.view.ViewGroup group = (android.view.ViewGroup) view;
      for (int i = 0; i < group.getChildCount(); i++) {
        android.view.View found = described(group.getChildAt(i), label);
        if (found != null) return found;
      }
    }
    return null;
  }

  private void tapDeviceMenu(Activity activity, String label) throws Exception {
    // Real pointer input verifies the recreated view's hit target, including
    // status/navigation-bar insets on Android versions without edge-to-edge.
    android.view.View[] target = {null};
    main(() -> {
      target[0] = described(activity.getWindow().getDecorView(), label);
      check(target[0] != null && target[0].isShown(), "recreated labelled menu is shown");
      target[0].requestRectangleOnScreen(
          new android.graphics.Rect(0, 0, target[0].getWidth(), target[0].getHeight()), true);
    });
    Thread.sleep(300);
    android.graphics.Rect bounds = new android.graphics.Rect();
    main(() -> {
      check(
          target[0].getGlobalVisibleRect(bounds)
              && bounds.width() == target[0].getWidth()
              && bounds.height() == target[0].getHeight(),
          "recreated menu touch target is entirely visible");
      int[] location = new int[2];
      target[0].getLocationOnScreen(location);
      bounds.offsetTo(location[0], location[1]);
    });
    long now = SystemClock.uptimeMillis();
    for (int action :
        new int[] {android.view.MotionEvent.ACTION_DOWN, android.view.MotionEvent.ACTION_UP}) {
      android.view.MotionEvent event =
          android.view.MotionEvent.obtain(
              now,
              SystemClock.uptimeMillis(),
              action,
              bounds.exactCenterX(),
              bounds.exactCenterY(),
              0);
      event.setSource(android.view.InputDevice.SOURCE_TOUCHSCREEN);
      try {
        check(
            test.getUiAutomation().injectInputEvent(event, true),
            "actual pointer opens recreated menu");
        if (action == android.view.MotionEvent.ACTION_DOWN) Thread.sleep(80);
      } finally {
        event.recycle();
      }
    }
    test.waitForIdleSync();
    Thread.sleep(200);
  }

  private void name(String text) throws Exception {
    AccessibilityNodeInfo node = node(null, "android.widget.EditText");
    Bundle argument = new Bundle();
    argument.putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, text);
    try {
      check(
          node.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, argument),
          "device label entered");
    } finally {
      node.recycle();
    }
  }

  private void screenshot(String name) throws Exception {
    Thread.sleep(300); // Capture settled checkbox/window animations, not an intermediate frame.
    File directory = new File(context.getExternalFilesDir(null), "usb-rules-proof");
    check(directory.isDirectory() || directory.mkdir(), "screenshot directory");
    android.graphics.Bitmap bitmap = test.getUiAutomation().takeScreenshot();
    check(bitmap != null, "actual rendered screenshot available");
    try (FileOutputStream output = new FileOutputStream(new File(directory, name + ".png"))) {
      check(
          bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, output),
          "screenshot saved");
    } finally {
      bitmap.recycle();
    }
  }

  private ComputerRepository.Snapshot change(JSONObject command) throws Exception {
    byte[] bytes = command.toString().getBytes(StandardCharsets.UTF_8);
    try {
      return repo.change(bytes).get(30, TimeUnit.SECONDS);
    } finally {
      Arrays.fill(bytes, (byte) 0);
    }
  }

  private String add(String name, boolean active) throws Exception {
    ComputerRepository.Snapshot snapshot =
        change(
            new JSONObject()
                .put("op", "add")
                .put("revision", repo.snapshot().revision)
                .put("name", name)
                .put("activate", active)
                .put(
                    "connection",
                    new JSONObject()
                        .put("kind", "legacy")
                        .put("host", "192.168.1.2")
                        .put("mode", 1)));
    return snapshot.computers.get(snapshot.computers.size() - 1).id;
  }

  private void activate(String id) throws Exception {
    change(
        new JSONObject()
            .put("op", "activate")
            .put("revision", repo.snapshot().revision)
            .put("id", id));
    test.waitForIdleSync();
  }

  private void connect(UsbDevice d, String serial, int cls, boolean permission) {
    main(() -> {
      if (serial != null) fixture.serials.put(d.getDeviceId(), serial);
      if (permission) fixture.permissions.add(d.getDeviceId());
      fixture.connect(d, UsbFixture.descriptor(cls, false));
    });
  }

  private UsbInventory.Item item(UsbDevice d) {
    return inventory.item(d.getDeviceId());
  }

  private void disconnect(UsbDevice d) {
    main(() -> fixture.disconnect(d));
  }

  private void setRule(UsbDevice device, String name, boolean automatic) throws Exception {
    change(
        new JSONObject()
            .put("op", "set_usb_rule")
            .put("id", repo.snapshot().activeId)
            .put("revision", repo.snapshot().revision)
            .put(
                "rule",
                new JSONObject()
                    .put("identity", new JSONObject(item(device).identity))
                    .put("name", name)
                    .put("automatic", automatic)
                    .put("storage", item(device).storage)));
  }

  private void ownedEmulator() throws Exception {
    try (InputStream in =
        new ParcelFileDescriptor.AutoCloseInputStream(
            test.getUiAutomation().executeShellCommand("getprop ro.kernel.qemu"))) {
      byte[] bytes = new byte[64];
      int count = in.read(bytes);
      check(
          count > 0 && new String(bytes, 0, count, StandardCharsets.UTF_8).trim().equals("1"),
          "owned emulator only");
    }
  }

  int run(String transition) throws Exception {
    ownedEmulator();
    repo = ComputerRepository.get(context);
    repo.ready().get(30, TimeUnit.SECONDS);
    repo.reset().get(30, TimeUnit.SECONDS);
    String a = add("Рабочий компьютер", true), b = add("Другой компьютер", false);
    Object app = context.getApplicationContext();
    Field field = app.getClass().getDeclaredField("inventory");
    field.setAccessible(true);
    UsbInventory original = (UsbInventory) field.get(app);
    fixture = new UsbFixture(context);
    main(() -> {
      inventory = new UsbInventory(fixture, fixture);
      inventory.bindComputers(repo);
    });
    field.set(app, inventory);
    Activity screen = null;
    boolean passed = false;
    try {
      context.getSharedPreferences("bridge", 0).edit().putBoolean("welcome_seen", true).commit();
      screen =
          test.startActivitySync(
              new Intent()
                  .setClassName(APP, "org.cgutman.usbip.config.UsbIpConfig")
                  .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_CLEAR_TASK));
      UsbDevice mouse = UsbFixture.device(121, "Test Mouse", 3, false, 55);
      connect(mouse, "fixture-a", 3, true);
      await(() -> item(mouse).identity != null, "identity obtained without opening USB");
      check(
          !item(mouse).selected && fixture.requests.isEmpty(),
          "new device manual; no hidden permission requests");
      main(() -> {
        fixture.permissions.remove(mouse.getDeviceId());
        inventory.refresh();
      });
      check(
          item(mouse).identity == null && item(mouse).ruleReason.equals("permission"),
          "permission loss clears recognition even before device preparation");
      main(() -> {
        fixture.permissions.add(mouse.getDeviceId());
        inventory.refresh();
      });
      await(() -> item(mouse).identity != null, "fresh permission re-reads identity");
      click("Настройки устройства: Test Mouse");
      name("Отменённое имя");
      click("Отмена");
      check(
          new JSONArray(repo.snapshot().active().usbRules).length() == 0
              && item(mouse).name.equals("Test Mouse")
              && !item(mouse).selected,
          "cancelled dialog cannot rename or select the device");
      click("Настройки устройства: Test Mouse");
      name("Несохранённое имя");
      Activity previous = screen;
      java.util.concurrent.atomic.AtomicReference<Activity> resumed =
          new java.util.concurrent.atomic.AtomicReference<>();
      Application application = (Application) context.getApplicationContext();
      Application.ActivityLifecycleCallbacks callbacks =
          new Application.ActivityLifecycleCallbacks() {
            public void onActivityResumed(Activity a) {
              if (a.getClass() == previous.getClass()) resumed.set(a);
            }

            public void onActivityCreated(Activity a, Bundle b) {}

            public void onActivityStarted(Activity a) {}

            public void onActivityPaused(Activity a) {}

            public void onActivityStopped(Activity a) {}

            public void onActivitySaveInstanceState(Activity a, Bundle b) {}

            public void onActivityDestroyed(Activity a) {}
          };
      main(() -> application.registerActivityLifecycleCallbacks(callbacks));
      try {
        if (transition.equals("rotate")) {
          main(() ->
              previous.setRequestedOrientation(
                  android.content.pm.ActivityInfo.SCREEN_ORIENTATION_LANDSCAPE));
          await(
              () -> resumed.get() != null && resumed.get() != previous,
              "actual Activity recreation reaches resumed state with menu open");
          screen = resumed.get();
          Activity landscape = screen;
          main(() ->
              landscape.setRequestedOrientation(
                  android.content.pm.ActivityInfo.SCREEN_ORIENTATION_PORTRAIT));
          await(
              () -> resumed.get() != null && resumed.get() != landscape,
              "actual orientation change returns to portrait");
          screen = resumed.get();
        } else {
          android.content.res.Configuration before =
              new android.content.res.Configuration(previous.getResources().getConfiguration());
          String command;
          switch (transition) {
            case "font":
              command = "settings put system font_scale 1.3";
              break;
            case "density":
              command = "wm density 200";
              break;
            case "resize":
              command = "wm size 320x720";
              break;
            case "split":
              command = "am stack move-task " + previous.getTaskId() + " 3 true";
              break;
            default:
              throw new AssertionError("Unknown configuration fixture");
          }
          try (ParcelFileDescriptor fd = test.getUiAutomation().executeShellCommand(command);
              InputStream input = new FileInputStream(fd.getFileDescriptor())) {
            byte[] bytes = new byte[4096];
            while (input.read(bytes) >= 0) {}
          }
          await(
              () -> {
                Activity current = resumed.get() == null ? previous : resumed.get();
                android.content.res.Configuration now = current.getResources().getConfiguration();
                return before.fontScale != now.fontScale
                    || before.densityDpi != now.densityDpi
                    || before.screenWidthDp != now.screenWidthDp
                    || before.screenHeightDp != now.screenHeightDp;
              },
              "system configuration actually changed: " + transition);
          test.waitForIdleSync();
          Thread.sleep(1500);
          screen = resumed.get() == null ? previous : resumed.get();
          if (screen == previous) click("Отмена");
          if (transition.equals("split")) {
            check(screen.isInMultiWindowMode(), "actual split-screen window");
            // Android opens Recents in the second pane and deliberately focuses
            // it. A real user must tap the app pane before typing there.
            Activity pane = screen;
            android.graphics.Rect bounds = new android.graphics.Rect();
            main(() -> {
              android.view.View content = pane.findViewById(android.R.id.content);
              int[] location = new int[2];
              content.getLocationOnScreen(location);
              bounds.set(
                  location[0],
                  location[1],
                  location[0] + content.getWidth(),
                  location[1] + content.getHeight());
            });
            long now = SystemClock.uptimeMillis();
            for (int action : new int[] {0, 1}) {
              android.view.MotionEvent event =
                  android.view.MotionEvent.obtain(
                      now, SystemClock.uptimeMillis(), action, bounds.left + 8, bounds.top + 8, 0);
              event.setSource(android.view.InputDevice.SOURCE_TOUCHSCREEN);
              try {
                check(test.getUiAutomation().injectInputEvent(event, true), "focus app split pane");
              } finally {
                event.recycle();
              }
              Thread.sleep(80);
            }
            Thread.sleep(1000);
          }
          screenshot("configuration-" + transition);
        }
      } finally {
        main(() -> application.unregisterActivityLifecycleCallbacks(callbacks));
      }
      test.waitForIdleSync();
      Activity recreated = screen;
      long focusDeadline = SystemClock.elapsedRealtime() + 10000;
      while (!recreated.hasWindowFocus() && SystemClock.elapsedRealtime() < focusDeadline)
        Thread.sleep(100);
      if (!recreated.hasWindowFocus()) {
        try (ParcelFileDescriptor fd =
                test.getUiAutomation().executeShellCommand("dumpsys window windows");
            FileInputStream input = new FileInputStream(fd.getFileDescriptor());
            FileOutputStream output =
                new FileOutputStream(
                    new File(context.getExternalFilesDir(null), "window-focus-fixture.txt"))) {
          byte[] buffer = new byte[8192];
          int length, total = 0;
          while ((length = input.read(buffer)) > 0 && total < 262144) {
            output.write(buffer, 0, length);
            total += length;
          }
        }
      }
      check(
          recreated.hasWindowFocus(),
          "recreated app window receives focus before real pointer input");
      check(
          new JSONArray(repo.snapshot().active().usbRules).length() == 0
              && item(mouse).name.equals("Test Mouse")
              && !item(mouse).selected,
          "destroyed menu cannot commit its draft or start automatic selection");
      tapDeviceMenu(screen, "Настройки устройства: Test Mouse");
      name("Рабочая мышь");
      click("Выбирать при подключении");
      screenshot("device-menu");
      node("Сохранить", null).recycle();
      screenshot("device-actions");
      click("Сохранить");
      await(
          () ->
              item(mouse).name.equals("Рабочая мышь") && item(mouse).selected && item(mouse).ready,
          "real menu saves encrypted rule and selects recognized device");
      screenshot("device-home");
      click("Рабочая мышь");
      await(() -> !item(mouse).selected, "manual uncheck stops automatic choice");
      main(() -> {
        inventory.refresh();
        inventory.bindComputer(repo.snapshot().active());
        inventory.refresh();
      });
      check(!item(mouse).selected, "refresh and same profile do not undo manual uncheck");
      long oldGeneration = item(mouse).generation;
      disconnect(mouse);
      UsbDevice moved = UsbFixture.device(122, "Test Mouse", 3, false, 55);
      connect(moved, "fixture-a", 3, true);
      await(
          () -> item(moved).selected && item(moved).ready,
          "same identity in another port selects again");
      main(() -> inventory.select(moved.getDeviceId(), oldGeneration, false, false));
      check(item(moved).selected, "old attachment generation cannot change replacement selection");
      main(() -> inventory.select(moved.getDeviceId(), item(moved).generation, false, false));
      UsbDevice twin = UsbFixture.device(123, "Duplicate Mouse", 3, false, 55);
      connect(twin, "fixture-a", 3, true);
      await(
          () -> item(twin).identity != null && item(moved).ruleReason.equals("ambiguous"),
          "duplicate serial recognized as ambiguous");
      check(!item(twin).selected && !item(moved).selected, "duplicate does not inherit selection");
      disconnect(twin);
      disconnect(moved);
      UsbDevice unknown = UsbFixture.device(124, "Unknown Mouse", 3, false, 55);
      connect(unknown, null, 3, false);
      connect(moved, "fixture-a", 3, true);
      await(
          () -> item(moved).ruleReason.equals("ambiguous"),
          "unreadable identical model blocks automatic guess");
      check(
          !item(moved).selected && fixture.requests.isEmpty(),
          "ambiguity does not open Android permission prompts");
      disconnect(unknown);
      await(
          () -> item(moved).selected && item(moved).ready,
          "unique known device can be selected when ambiguity disappears");
      Object owner = new Object();
      check(
          inventory.acquire(moved.getDeviceId(), item(moved).generation, owner),
          "synthetic current USB ownership acquired");
      check(
          !inventory.bindComputer(repo.snapshot().computers.get(1)),
          "computer binding refuses live USB ownership");
      inventory.released(moved.getDeviceId(), item(moved).generation, owner);
      UsbDevice disk = UsbFixture.device(125, "Test Disk", 8, false, 77);
      connect(disk, "fixture-disk", 8, true);
      await(() -> item(disk).identity != null, "storage identity available for label only");
      setRule(disk, "Рабочая флешка", false);
      await(() -> item(disk).name.equals("Рабочая флешка"), "persistent storage label applied");
      check(!item(disk).selected && !item(disk).approved, "storage label never supplies consent");
      try {
        setRule(disk, "Invalid", true);
        throw new AssertionError("automatic storage accepted");
      } catch (java.util.concurrent.ExecutionException expected) {
        checks++;
      }
      main(() -> inventory.select(disk.getDeviceId(), item(disk).generation, true, true));
      await(
          () -> item(disk).ready && item(disk).selected, "explicit current storage consent works");
      activate(b);
      await(
          () -> !item(moved).selected && !item(disk).selected,
          "switch computer clears shared selection");
      check(
          !item(disk).approved && !item(moved).name.equals("Рабочая мышь"),
          "no inherited consent or label on other computer");
      activate(a);
      await(
          () -> item(moved).selected && item(moved).name.equals("Рабочая мышь"),
          "return uses only that computer's explicit rule");
      check(
          !item(disk).selected && !item(disk).approved,
          "return to previous PC does not restore disk consent");
      disconnect(disk);
      UsbDevice plain = UsbFixture.device(126, "Plain USB", 3, false, 88);
      connect(plain, null, 3, true);
      click("Настройки устройства: Plain USB");
      name("Временное имя");
      click("Сохранить");
      await(
          () -> item(plain).name.equals("Временное имя"),
          "attachment-only name works without serial");
      disconnect(plain);
      connect(plain, null, 3, true);
      check(item(plain).name.equals("Plain USB"), "temporary name is not inherited after replug");
      disconnect(plain);
      disconnect(moved);
      fixture.serialEntered = new java.util.concurrent.CountDownLatch(1);
      fixture.serialRelease = new java.util.concurrent.CountDownLatch(1);
      fixture.blockedSerialId = moved.getDeviceId();
      try {
        connect(moved, "fixture-a", 3, true);
        check(
            fixture.serialEntered.await(5, TimeUnit.SECONDS),
            "metadata read is deliberately in flight");
        disconnect(moved);
        UsbDevice replacement = UsbFixture.device(122, "Replacement Mouse", 3, false, 55);
        connect(replacement, "fixture-other", 3, true);
        fixture.serialRelease.countDown();
        await(
            () -> item(replacement).identity != null,
            "replacement identity completes after stale metadata");
        check(
            !item(replacement).selected && item(replacement).name.equals("Replacement Mouse"),
            "late serial from former attachment cannot rename or select replacement");
        disconnect(replacement);
      } finally {
        fixture.serialRelease.countDown();
        fixture.blockedSerialId = -1;
      }
      connect(moved, "fixture-a", 3, true);
      await(() -> item(moved).selected, "intended recognized device restored after metadata race");
      check(
          !fixture.serialOnMain && !fixture.serialGateTimedOut,
          "serial provider never runs on UI thread");
      check(
          !repo.snapshot().active().usbRules.contains("fixture-a"),
          "public rules contain no raw serial");
      click("Изменить компьютер");
      click("Доступ и настройки: Рабочий компьютер");
      click("Правила USB · 2");
      click("Рабочая флешка · ручной выбор");
      click("Забыть");
      await(
          () -> {
            try {
              return new JSONArray(repo.snapshot().active().usbRules).length() == 1;
            } catch (JSONException error) {
              return false;
            }
          },
          "offline rule can be forgotten through computer settings");
      check(
          item(moved).selected,
          "forgetting an offline disk rule preserves current mouse selection");
      check(!org.cgutman.usbip.service.UsbIpService.running, "rules never start network bridge");
      click("Назад");
      click("Настройки устройства: Рабочая мышь");
      name("Не должно сохраниться");
      click("Выбирать при подключении");
      File sealed = new File(context.getNoBackupFilesDir(), "computers.enc");
      File pending = new File(context.getNoBackupFilesDir(), "computers.enc.new");
      File sentinel = new File(pending, "owned-write-failure-fixture");
      byte[] beforeWrite = java.nio.file.Files.readAllBytes(sealed.toPath());
      check(
          !pending.exists() && pending.mkdir(),
          "owned write-failure directory created without replacing existing state");
      // openRead retires an empty .new directory. Keep one exact owned sentinel
      // so the obstruction survives that recovery step and reaches startWrite.
      try (FileOutputStream output = new FileOutputStream(sentinel)) {
        output.write(1);
      }
      try {
        click("Сохранить");
        await(
            () -> repo.snapshot().status == ComputerRepository.Status.FAILED,
            "failed rule write invalidates public snapshot instead of applying candidate");
        check(
            Arrays.equals(beforeWrite, java.nio.file.Files.readAllBytes(sealed.toPath())),
            "failed rule write preserves previous authenticated ciphertext");
        click("Отмена");
      } finally {
        Arrays.fill(beforeWrite, (byte) 0);
        check(sentinel.delete(), "exact owned write-failure sentinel removed");
        if (pending.exists())
          check(
              pending.isDirectory() && pending.delete(),
              "only owned empty write-failure directory removed");
        repo.reload().get(30, TimeUnit.SECONDS);
      }
      await(
          () -> item(moved).selected && item(moved).name.equals("Рабочая мышь"),
          "recovery reloads old durable rule, never the unsaved name or automatic flag");
      try (FileOutputStream output =
          new FileOutputStream(new File(context.getFilesDir(), "usb-rules-resume-fixture.json"))) {
        output.write(
            new JSONObject()
                .put("scope", "owned-emulator-usb-rules")
                .put("pid", android.os.Process.myPid())
                .put("active", a)
                .put("other", b)
                .toString()
                .getBytes(StandardCharsets.UTF_8));
      }
      passed = true;
      return checks;
    } finally {
      if (screen != null) {
        Activity owned = screen;
        main(owned::finish);
      }
      field.set(app, original);
      main(inventory::close);
      if (!passed) repo.reset().get(30, TimeUnit.SECONDS);
    }
  }

  int resume() throws Exception {
    ownedEmulator();
    File marker = new File(context.getFilesDir(), "usb-rules-resume-fixture.json");
    JSONObject probe;
    try (FileInputStream input = new FileInputStream(marker)) {
      byte[] bytes = new byte[1024];
      int count = input.read(bytes);
      check(
          count > 0 && count < bytes.length && input.read() == -1, "bounded owned restart marker");
      probe = new JSONObject(new String(bytes, 0, count, StandardCharsets.UTF_8));
    }
    check(probe.getString("scope").equals("owned-emulator-usb-rules"), "owned rule fixture scope");
    check(
        probe.getInt("pid") != android.os.Process.myPid(),
        "actual new Android process reads persisted rules");
    repo = ComputerRepository.get(context);
    repo.ready().get(30, TimeUnit.SECONDS);
    check(
        repo.snapshot().activeId.equals(probe.getString("active"))
            && repo.snapshot().computers.size() == 2,
        "same selected computer restored from encrypted catalog");
    check(
        new JSONArray(repo.snapshot().active().usbRules).length() == 1,
        "offline rule deletion survived process death");
    fixture = new UsbFixture(context);
    main(() -> {
      inventory = new UsbInventory(fixture, fixture);
      inventory.bindComputers(repo);
    });
    try {
      UsbDevice mouse = UsbFixture.device(131, "Restart Mouse", 3, false, 55);
      connect(mouse, "fixture-a", 3, true);
      await(
          () ->
              item(mouse).selected && item(mouse).ready && item(mouse).name.equals("Рабочая мышь"),
          "persisted rule names and selects only the recognized attachment after restart");
      UsbDevice disk = UsbFixture.device(132, "Restart Disk", 8, false, 77);
      connect(disk, "fixture-disk", 8, true);
      await(() -> item(disk).identity != null, "restart storage identity obtained");
      check(!item(disk).selected && !item(disk).approved, "disk consent is never persisted");
      activate(probe.getString("other"));
      await(() -> !item(mouse).selected, "persisted second computer does not inherit rule");
      activate(probe.getString("active"));
      await(
          () -> item(mouse).selected && item(mouse).state.equals("Ожидает подключения ПК"),
          "already prepared device has truthful waiting state after destination switch");
      check(!org.cgutman.usbip.service.UsbIpService.running, "restart rule does not start bridge");
      return checks;
    } finally {
      main(inventory::close);
      repo.reset().get(30, TimeUnit.SECONDS);
      check(marker.delete(), "exact owned restart marker deleted");
    }
  }
}
