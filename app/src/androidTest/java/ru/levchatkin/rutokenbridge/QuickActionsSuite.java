package ru.levchatkin.rutokenbridge;

import android.app.*;
import android.content.*;
import android.hardware.usb.UsbDevice;
import android.os.*;
import android.view.accessibility.AccessibilityNodeInfo;
import java.io.*;
import java.lang.reflect.Field;
import java.nio.charset.StandardCharsets;
import java.util.Arrays;
import java.util.concurrent.TimeUnit;
import java.util.function.BooleanSupplier;
import org.cgutman.usbip.config.QuickActions;
import org.cgutman.usbip.connection.ComputerRepository;
import org.cgutman.usbip.service.UsbInventory;
import org.cgutman.usbip.service.UsbIpService;
import org.json.JSONObject;

/** Actual SystemUI tile, immutable PendingIntent, foreground Activity/service and synthetic USB. */
final class QuickActionsSuite {
  private static final String APP = "ru.levchatkin.rutokenbridge";
  private static final String MAIN = "org.cgutman.usbip.config.UsbIpConfig";
  private static final String TILE = APP + "/org.cgutman.usbip.config.BridgeTileService";
  private final Instrumentation test;
  private final Context context;
  private ComputerRepository repo;
  private int checks;
  private volatile Activity screen;
  private volatile Intent captured;
  private final StringBuilder lifecycle = new StringBuilder();

  private void trace(Activity activity, String event) {
    if (lifecycle.length() < 32000)
      lifecycle
          .append(SystemClock.elapsedRealtime())
          .append(' ')
          .append(event)
          .append(' ')
          .append(activity.getClass().getSimpleName())
          .append('@')
          .append(System.identityHashCode(activity))
          .append(" action=")
          .append(activity.getIntent().getIntExtra("action", -1))
          .append('\n');
  }

  private final Application.ActivityLifecycleCallbacks callbacks =
      new Application.ActivityLifecycleCallbacks() {
        public void onActivityCreated(Activity activity, Bundle state) {
          trace(activity, "created");
          if (activity.getClass().getName().endsWith(".QuickActionActivity"))
            captured = new Intent(activity.getIntent());
        }

        public void onActivityResumed(Activity activity) {
          trace(activity, "resumed");
          if (activity.getClass().getName().equals(MAIN)) screen = activity;
        }

        public void onActivityStarted(Activity a) {}

        public void onActivityPaused(Activity a) {
          trace(a, "paused");
        }

        public void onActivityStopped(Activity a) {}

        public void onActivitySaveInstanceState(Activity a, Bundle b) {}

        public void onActivityDestroyed(Activity a) {
          trace(a, "destroyed");
        }
      };

  private boolean visible(String text) {
    AccessibilityNodeInfo root = test.getUiAutomation().getRootInActiveWindow();
    AccessibilityNodeInfo node = find(root, text);
    if (root != null) root.recycle();
    if (node == null) return false;
    node.recycle();
    return true;
  }

  private void sendIntent(Intent intent) throws Exception {
    send(
        PendingIntent.getActivity(
            context, 901, intent, PendingIntent.FLAG_CANCEL_CURRENT | PendingIntent.FLAG_IMMUTABLE),
        null);
  }

  private void rotate(int orientation) throws Exception {
    Activity previous = screen;
    main(() -> previous.setRequestedOrientation(orientation));
    await(
        () -> screen != previous && screen.hasWindowFocus(),
        "orientation replacement resumes with real input focus");
  }

  private void tapView(int id) throws Exception {
    main(() -> {
      android.view.View view = screen.findViewById(id);
      check(view != null, "quick settings control exists");
      view.requestRectangleOnScreen(
          new android.graphics.Rect(0, 0, view.getWidth(), view.getHeight()), true);
    });
    Thread.sleep(300);
    android.graphics.Rect bounds = new android.graphics.Rect();
    main(() -> {
      android.view.View view = screen.findViewById(id);
      int[] origin = new int[2];
      view.getLocationOnScreen(origin);
      bounds.set(origin[0], origin[1], origin[0] + view.getWidth(), origin[1] + view.getHeight());
      android.graphics.Rect shown = new android.graphics.Rect();
      check(
          view.getGlobalVisibleRect(shown) && shown.height() == view.getHeight(),
          "whole control reachable after scrolling");
    });
    long down = SystemClock.uptimeMillis();
    for (int action :
        new int[] {android.view.MotionEvent.ACTION_DOWN, android.view.MotionEvent.ACTION_UP}) {
      android.view.MotionEvent event =
          android.view.MotionEvent.obtain(
              down,
              SystemClock.uptimeMillis(),
              action,
              bounds.exactCenterX(),
              bounds.exactCenterY(),
              0);
      event.setSource(android.view.InputDevice.SOURCE_TOUCHSCREEN);
      try {
        check(
            test.getUiAutomation().injectInputEvent(event, true), "physical settings control tap");
      } finally {
        event.recycle();
      }
      if (action == android.view.MotionEvent.ACTION_DOWN) Thread.sleep(80);
    }
    Thread.sleep(500);
  }

  private int id(String name) {
    return context.getResources().getIdentifier(name, "id", APP);
  }

  private void dump(AccessibilityNodeInfo node, StringBuilder out, int depth) {
    if (node == null || depth > 25 || out.length() > 32000) return;
    android.graphics.Rect bounds = new android.graphics.Rect();
    node.getBoundsInScreen(bounds);
    out.append(node.getViewIdResourceName())
        .append(" | ")
        .append(node.getText())
        .append(" | ")
        .append(node.getContentDescription())
        .append(" | ")
        .append(bounds)
        .append('\n');
    for (int i = 0; i < node.getChildCount(); i++) {
      AccessibilityNodeInfo child = node.getChild(i);
      dump(child, out, depth + 1);
      if (child != null) child.recycle();
    }
  }

  private void screenshot(String name) throws Exception {
    File directory = new File(context.getExternalFilesDir(null), "quick-actions-proof");
    if (!directory.isDirectory() && !directory.mkdir())
      throw new IOException("screenshot directory");
    android.graphics.Bitmap bitmap = test.getUiAutomation().takeScreenshot();
    check(bitmap != null, "SystemUI screenshot captured");
    try (FileOutputStream out = new FileOutputStream(new File(directory, name + ".png"))) {
      check(
          bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, out),
          "screenshot written");
    } finally {
      bitmap.recycle();
    }
  }

  QuickActionsSuite(Instrumentation test) {
    this.test = test;
    context = test.getTargetContext();
    android.accessibilityservice.AccessibilityServiceInfo service =
        test.getUiAutomation().getServiceInfo();
    service.flags |= android.accessibilityservice.AccessibilityServiceInfo.FLAG_REPORT_VIEW_IDS;
    test.getUiAutomation().setServiceInfo(service);
  }

  private void check(boolean value, String label) {
    checks++;
    if (!value) throw new AssertionError(label);
  }

  private void main(Runnable task) {
    Throwable[] failure = {null};
    test.runOnMainSync(() -> {
      try {
        task.run();
      } catch (Throwable error) {
        failure[0] = error;
      }
    });
    if (failure[0] != null) throw new AssertionError("quick action UI", failure[0]);
  }

  private void await(BooleanSupplier ready, String label) throws Exception {
    long end = SystemClock.elapsedRealtime() + 15000;
    while (!ready.getAsBoolean() && SystemClock.elapsedRealtime() < end) Thread.sleep(50);
    check(ready.getAsBoolean(), label);
  }

  private String shell(String command) throws Exception {
    try (InputStream in =
            new ParcelFileDescriptor.AutoCloseInputStream(
                test.getUiAutomation().executeShellCommand(command));
        ByteArrayOutputStream out = new ByteArrayOutputStream()) {
      byte[] buffer = new byte[4096];
      int size;
      while ((size = in.read(buffer)) >= 0) {
        if (out.size() + size > 262144) throw new IOException("bounded shell response");
        out.write(buffer, 0, size);
      }
      return out.toString("UTF-8").trim();
    }
  }

  private AccessibilityNodeInfo find(AccessibilityNodeInfo root, String text) {
    if (root == null) return null;
    if (root.isVisibleToUser()
        && root.isEnabled()
        && root.getText() != null
        && text.equalsIgnoreCase(root.getText().toString()))
      return AccessibilityNodeInfo.obtain(root);
    for (int i = 0; i < root.getChildCount(); i++) {
      AccessibilityNodeInfo child = root.getChild(i), found = find(child, text);
      if (child != null) child.recycle();
      if (found != null) return found;
    }
    return null;
  }

  private AccessibilityNodeInfo resource(AccessibilityNodeInfo root, String id, String text) {
    if (root == null) return null;
    if (root.isVisibleToUser()
        && id.equals(root.getViewIdResourceName())
        && (text == null || (root.getText() != null && text.contentEquals(root.getText()))))
      return AccessibilityNodeInfo.obtain(root);
    for (int i = 0; i < root.getChildCount(); i++) {
      AccessibilityNodeInfo child = root.getChild(i), found = resource(child, id, text);
      if (child != null) child.recycle();
      if (found != null) return found;
    }
    return null;
  }

  private void click(String text) throws Exception {
    long end = SystemClock.elapsedRealtime() + 12000;
    do {
      AccessibilityNodeInfo root = test.getUiAutomation().getRootInActiveWindow();
      AccessibilityNodeInfo node = find(root, text);
      if (root != null) root.recycle();
      if (node != null) {
        try {
          check(
              node.performAction(AccessibilityNodeInfo.ACTION_CLICK), "actual UI action: " + text);
        } finally {
          node.recycle();
        }
        test.waitForIdleSync();
        Thread.sleep(250);
        return;
      }
      Thread.sleep(100);
    } while (SystemClock.elapsedRealtime() < end);
    android.graphics.Bitmap screenshot = test.getUiAutomation().takeScreenshot();
    if (screenshot != null) {
      try (FileOutputStream output =
          new FileOutputStream(
              new File(context.getExternalFilesDir(null), "quick-action-failure.png"))) {
        screenshot.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, output);
      } finally {
        screenshot.recycle();
      }
    }
    StringBuilder tree = new StringBuilder();
    AccessibilityNodeInfo root = test.getUiAutomation().getRootInActiveWindow();
    dump(root, tree, 0);
    if (root != null) root.recycle();
    throw new AssertionError("visible UI action missing: " + text + "\n" + tree);
  }

  private void expandNotification() throws Exception {
    // The tile's collapse animation may still be completing after the service
    // becomes ready. Wait for that transition before opening another panel.
    test.getUiAutomation().waitForIdle(500, 5000);
    shell("cmd statusbar collapse");
    Thread.sleep(400);
    shell("cmd statusbar expand-notifications");
    Thread.sleep(700);
    AccessibilityNodeInfo node = null;
    long deadline = SystemClock.elapsedRealtime() + 10000;
    while (node == null && SystemClock.elapsedRealtime() < deadline) {
      AccessibilityNodeInfo root = test.getUiAutomation().getRootInActiveWindow();
      if (root != null) {
        for (String id : new String[] {"android:id/app_name_text", "android:id/title"})
          if (node == null) node = resource(root, id, "USB Мост");
        root.recycle();
      }
      if (node == null) Thread.sleep(100);
    }
    check(node != null, "own notification header appears in SystemUI");
    for (int depth = 0; node != null && depth < 6; depth++) {
      boolean expanded = false;
      AccessibilityNodeInfo button = resource(node, "android:id/expand_button", null);
      if (button != null) {
        if (!expanded
            && button.isVisibleToUser()
            && (button.getContentDescription() == null
                || !button
                    .getContentDescription()
                    .toString()
                    .toLowerCase(java.util.Locale.ROOT)
                    .contains("collapse"))) {
          android.graphics.Rect bounds = new android.graphics.Rect();
          button.getBoundsInScreen(bounds);
          if (!bounds.isEmpty()) {
            long down = SystemClock.uptimeMillis();
            for (int action :
                new int[] {
                  android.view.MotionEvent.ACTION_DOWN, android.view.MotionEvent.ACTION_UP
                }) {
              android.view.MotionEvent event =
                  android.view.MotionEvent.obtain(
                      down,
                      SystemClock.uptimeMillis(),
                      action,
                      bounds.exactCenterX(),
                      bounds.exactCenterY(),
                      0);
              event.setSource(android.view.InputDevice.SOURCE_TOUCHSCREEN);
              try {
                check(
                    test.getUiAutomation().injectInputEvent(event, true),
                    "physical notification expand control");
              } finally {
                event.recycle();
              }
              if (action == android.view.MotionEvent.ACTION_DOWN) Thread.sleep(80);
            }
            expanded = true;
          }
        }
        button.recycle();
      }
      AccessibilityNodeInfo parent = expanded ? null : node.getParent();
      node.recycle();
      node = parent;
      if (expanded) {
        Thread.sleep(500);
        return;
      }
    }
    if (node != null) node.recycle();
  }

  private ComputerRepository.Snapshot change(JSONObject operation) throws Exception {
    byte[] bytes = operation.toString().getBytes(StandardCharsets.UTF_8);
    try {
      return repo.change(bytes).get(30, TimeUnit.SECONDS);
    } finally {
      Arrays.fill(bytes, (byte) 0);
    }
  }

  private String add(String name, boolean active) throws Exception {
    ComputerRepository.Snapshot result =
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
    return result.computers.get(result.computers.size() - 1).id;
  }

  private void activate(String id) throws Exception {
    change(
        new JSONObject()
            .put("op", "activate")
            .put("id", id)
            .put("revision", repo.snapshot().revision));
  }

  private void send(PendingIntent action, Intent fill) throws Exception {
    ActivityOptions options = ActivityOptions.makeBasic();
    if (Build.VERSION.SDK_INT >= 34)
      options.setPendingIntentBackgroundActivityStartMode(
          ActivityOptions.MODE_BACKGROUND_ACTIVITY_START_ALLOWED);
    action.send(context, 0, fill, null, null, null, options.toBundle());
    Thread.sleep(700);
    test.waitForIdleSync();
    test.getUiAutomation().waitForIdle(200, 5000);
  }

  private void started() throws Exception {
    await(
        () -> UsbIpService.running && UsbIpService.status.startsWith("Готово."),
        "actual foreground service is ready");
  }

  private void stopped() throws Exception {
    await(
        () -> !UsbIpService.running && !repo.isBusy(), "service and owning computer lease retired");
  }

  private PendingIntent notificationAction() {
    for (android.service.notification.StatusBarNotification item :
        context.getSystemService(NotificationManager.class).getActiveNotifications())
      if (item.getId() == 100) {
        Notification notification = item.getNotification();
        check(
            notification.actions != null && notification.actions.length == 1,
            "one notification action, no extra notification UI");
        check(
            notification.visibility == Notification.VISIBILITY_PRIVATE,
            "private lock-screen notification");
        return notification.actions[0].actionIntent;
      }
    throw new AssertionError("foreground notification missing");
  }

  int run() throws Exception {
    check(shell("getprop ro.kernel.qemu").equals("1"), "owned emulator only");
    repo = ComputerRepository.get(context);
    repo.ready().get(30, TimeUnit.SECONDS);
    repo.reset().get(30, TimeUnit.SECONDS);
    String originalTiles = shell("settings get secure sysui_qs_tiles");
    boolean alreadyAdded = originalTiles.contains(TILE);
    Application app = (Application) context.getApplicationContext();
    main(() -> app.registerActivityLifecycleCallbacks(callbacks));
    Field field = app.getClass().getDeclaredField("inventory");
    field.setAccessible(true);
    UsbInventory original = (UsbInventory) field.get(app);
    UsbFixture fixture = new UsbFixture(context);
    UsbInventory[] holder = {null};
    main(() -> {
      holder[0] = new UsbInventory(fixture, fixture);
      holder[0].bindComputers(repo);
    });
    UsbInventory inventory = holder[0];
    field.set(app, inventory);
    boolean complete = false;
    try {
      context
          .getSharedPreferences("bridge", 0)
          .edit()
          .putBoolean("welcome_seen", true)
          .putBoolean("notifications_requested", true)
          .commit();
      if (Build.VERSION.SDK_INT >= 33)
        shell("pm grant " + APP + " android.permission.POST_NOTIFICATIONS");
      test.startActivitySync(
          new Intent().setClassName(APP, MAIN).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
      QuickActions.Binding absent = QuickActions.capture(context);
      check(absent.action == QuickActions.OPEN, "unconfigured tile opens setup only");
      send(QuickActions.intent(context, absent, false), null);
      check(
          !UsbIpService.running && repo.snapshot().active() == null,
          "no fallback connection without a computer");
      String a = add("Первый компьютер", true), b = add("Второй компьютер", false);
      QuickActions.Binding old = QuickActions.capture(context);
      PendingIntent staleComputer = QuickActions.intent(context, old, false);
      activate(b);
      check(!QuickActions.current(context, old), "changed computer invalidates captured action");
      send(staleComputer, null);
      check(
          !UsbIpService.running && repo.snapshot().activeId.equals(b),
          "stale action cannot start a different computer");
      activate(a);
      check(
          !context
              .getPackageManager()
              .getActivityInfo(
                  new ComponentName(APP, "org.cgutman.usbip.config.QuickActionActivity"), 0)
              .exported,
          "quick action entry is not exported; external shell denial checked by runner");
      shell("am start -n " + APP + "/" + MAIN + " --ei action 1 --ez quick_start true");
      Thread.sleep(500);
      check(!UsbIpService.running, "exported launcher extras cannot operate the bridge");
      shell("cmd statusbar add-tile " + TILE);
      shell("cmd statusbar expand-settings");
      Thread.sleep(1200);
      shell("cmd statusbar click-tile " + TILE);
      started();
      Intent firstStart = new Intent(captured);
      check(
          firstStart.getIntExtra("action", -1) == QuickActions.START,
          "actual tile delivers a captured START binding");
      check(repo.snapshot().activeId.equals(a), "tile starts the selected computer only");
      QuickActions.Binding firstSession = QuickActions.capture(context);
      PendingIntent firstStop = notificationAction();
      main(() -> context.startService(new Intent(context, UsbIpService.class)));
      test.waitForIdleSync();
      check(
          firstStop.equals(notificationAction()),
          "same-session notification refresh preserves the existing action capability");
      if (Build.VERSION.SDK_INT >= 31)
        check(firstStop.isImmutable(), "notification PendingIntent is immutable");
      shell("cmd statusbar expand-notifications");
      expandNotification();
      click("Остановить");
      stopped();
      check(
          !QuickActions.current(context, firstSession),
          "ended service invalidates old stop binding");
      QuickActions.Binding expiring = QuickActions.capture(context);
      PendingIntent expired = QuickActions.intent(context, expiring, false);
      Thread.sleep(31000);
      check(
          QuickActions.current(context, expiring),
          "expiry fixture retains destination and service generation");
      send(expired, null);
      check(!UsbIpService.running, "expired START opens screen without starting");
      QuickActions.Binding start = QuickActions.capture(context);
      PendingIntent immutableStart = QuickActions.intent(context, start, false);
      send(
          immutableStart,
          new Intent().putExtra("action", QuickActions.STOP).putExtra("computer", "forged"));
      started();
      check(
          !QuickActions.current(context, start), "service creation invalidates old start binding");
      send(immutableStart, null);
      check(UsbIpService.running, "repeated start cannot turn into stop");
      try {
        send(firstStop, null);
      } catch (PendingIntent.CanceledException expected) {
        checks++;
      }
      check(UsbIpService.running, "old notification cannot stop a replacement service");
      UsbDevice disk = UsbFixture.device(141, "Quick test disk", 8, false, 88);
      fixture.serials.put(disk.getDeviceId(), "quick-fixture-disk");
      main(() -> {
        fixture.permissions.add(disk.getDeviceId());
        fixture.connect(disk, UsbFixture.descriptor(8, false));
        inventory.refresh();
      });
      UsbInventory.Item item = inventory.item(disk.getDeviceId());
      main(() -> inventory.select(disk.getDeviceId(), item.generation, true, true));
      await(() -> inventory.item(disk.getDeviceId()).ready, "owned synthetic storage prepared");
      PendingIntent stopStorage = notificationAction();
      main(() -> trace(screen, "before_storage_stop"));
      send(stopStorage, null);
      main(() -> trace(screen, "after_storage_stop"));
      check(UsbIpService.running, "storage quick stop waits for user confirmation");
      click("Отмена");
      check(
          UsbIpService.running && inventory.item(disk.getDeviceId()).approved,
          "cancelled quick stop preserves service and current storage consent");
      Intent validStop = new Intent(captured);
      check(
          validStop.getIntExtra("action", -1) == QuickActions.STOP,
          "captured private notification entry carries STOP");
      for (Intent malformed :
          new Intent[] {
            new Intent(validStop).putExtra("action", 99),
            new Intent(validStop).putExtra("process", "bad"),
            new Intent(validStop).putExtra("epoch", -1L),
            new Intent(validStop).putExtra("computer", "wrong"),
            new Intent(validStop).putExtra("action", "wrong type")
          }) {
        sendIntent(malformed);
        check(
            UsbIpService.running && !visible("Завершить передачу?"),
            "malformed private action cannot reach storage confirmation");
      }
      send(stopStorage, null);
      await(() -> visible("Завершить передачу?"), "valid current STOP still reaches confirmation");
      rotate(android.content.pm.ActivityInfo.SCREEN_ORIENTATION_LANDSCAPE);
      rotate(android.content.pm.ActivityInfo.SCREEN_ORIENTATION_PORTRAIT);
      check(
          UsbIpService.running && !visible("Завершить передачу?"),
          "rotation retires confirmation without stopping");
      send(stopStorage, null);
      shell("input keyevent KEYCODE_HOME");
      Thread.sleep(400);
      shell("am start -n " + APP + "/" + MAIN);
      Thread.sleep(600);
      check(
          UsbIpService.running && !visible("Завершить передачу?"),
          "background confirmation cannot be replayed on return");
      send(stopStorage, null);
      await(() -> visible("Завершить передачу?"), "old-session confirmation is visible");
      main(() -> context.stopService(new Intent(context, UsbIpService.class)));
      stopped();
      main(() ->
          context.startService(
              new Intent(context, UsbIpService.class)
                  .putExtra(UsbIpService.EXTRA_COMPUTER_REVISION, repo.snapshot().revision)));
      started();
      click("Извлечён · завершить");
      check(UsbIpService.running, "old open confirmation cannot stop replacement session");
      main(() -> {
        inventory.select(disk.getDeviceId(), item.generation, false, false);
        inventory.select(disk.getDeviceId(), item.generation, true, true);
      });
      await(
          () -> inventory.item(disk.getDeviceId()).ready,
          "fresh storage consent in replacement session");
      send(notificationAction(), null);
      click("Извлечён · завершить");
      stopped();
      check(!inventory.item(disk.getDeviceId()).approved, "confirmed stop clears storage consent");
      try (FileOutputStream output =
          new FileOutputStream(new File(context.getFilesDir(), "quick-action-resume.json"))) {
        output.write(
            new JSONObject()
                .put("pid", android.os.Process.myPid())
                .put("intent", firstStart.toUri(Intent.URI_INTENT_SCHEME))
                .put("computer", a)
                .toString()
                .getBytes(StandardCharsets.UTF_8));
      }
      complete = true;
      return checks;
    } finally {
      try (FileOutputStream log =
          new FileOutputStream(
              new File(context.getExternalFilesDir(null), "quick-actions-lifecycle.txt"))) {
        String[] trace = {""};
        main(() -> trace[0] = lifecycle.toString());
        log.write(trace[0].getBytes(StandardCharsets.UTF_8));
      }
      shell("cmd statusbar collapse");
      if (!alreadyAdded) shell("cmd statusbar remove-tile " + TILE);
      main(() -> context.stopService(new Intent(context, UsbIpService.class)));
      stopped();
      field.set(app, original);
      main(inventory::close);
      main(() -> app.unregisterActivityLifecycleCallbacks(callbacks));
      if (!complete) repo.reset().get(30, TimeUnit.SECONDS);
    }
  }

  int resume() throws Exception {
    check(shell("getprop ro.kernel.qemu").equals("1"), "restart on owned emulator only");
    File marker = new File(context.getFilesDir(), "quick-action-resume.json");
    JSONObject saved;
    try (FileInputStream input = new FileInputStream(marker)) {
      byte[] buffer = new byte[8192];
      int length = input.read(buffer);
      check(
          length > 0 && length < buffer.length && input.read() == -1,
          "bounded owned restart marker");
      saved = new JSONObject(new String(buffer, 0, length, StandardCharsets.UTF_8));
    }
    repo = ComputerRepository.get(context);
    repo.ready().get(30, TimeUnit.SECONDS);
    try {
      check(saved.getInt("pid") != android.os.Process.myPid(), "actual new Android process");
      check(
          saved.getString("computer").equals(repo.snapshot().activeId),
          "destination survives process death");
      test.startActivitySync(
          new Intent().setClassName(APP, MAIN).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
      Intent old = Intent.parseUri(saved.getString("intent"), Intent.URI_INTENT_SCHEME);
      sendIntent(old);
      check(!UsbIpService.running, "old process action is not replayed");
      // Isolate the process nonce check from elapsed-time expiry. This timestamp
      // normalization is a same-UID test fixture, not an external capability.
      old.putExtra("created", SystemClock.elapsedRealtime());
      check(old.getLongExtra("epoch", -1) == 1, "first tile token matches fresh service epoch");
      sendIntent(old);
      check(
          !UsbIpService.running, "old process nonce rejected even without time or epoch mismatch");
      check(QuickActions.consume(context) == null, "no replay command retained");
      return checks;
    } finally {
      repo.reset().get(30, TimeUnit.SECONDS);
      check(marker.delete(), "remove exact owned process marker");
    }
  }

  int interfaceChecks() throws Exception {
    check(shell("getprop ro.kernel.qemu").equals("1"), "interface checks on owned emulator");
    repo = ComputerRepository.get(context);
    repo.ready().get(30, TimeUnit.SECONDS);
    repo.reset().get(30, TimeUnit.SECONDS);
    String original = shell("settings get secure sysui_qs_tiles");
    boolean added = original.contains(TILE);
    Application app = (Application) context.getApplicationContext();
    main(() -> app.registerActivityLifecycleCallbacks(callbacks));
    try {
      context.getSharedPreferences("bridge", 0).edit().putBoolean("welcome_seen", true).commit();
      add("Домашний компьютер", true);
      test.startActivitySync(
          new Intent().setClassName(APP, MAIN).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
      await(() -> screen != null && screen.hasWindowFocus(), "settings screen focused");
      tapView(id("open_settings"));
      shell("cmd statusbar remove-tile " + TILE);
      tapView(id("add_quick_tile"));
      if (Build.VERSION.SDK_INT >= 33) {
        screenshot("add-tile-prompt");
        click("Add tile");
        await(
            () -> {
              try {
                return shell("settings get secure sysui_qs_tiles").contains(TILE);
              } catch (Exception e) {
                return false;
              }
            },
            "actual SystemUI add-tile confirmation adds component");
        tapView(id("add_quick_tile"));
        check(!visible("Add tile"), "already-added tile does not ask a second time");
      } else {
        check(visible("Плитка USB Мост"), "older Android displays manual tile help");
        screenshot("tile-help");
        click("Понятно");
        shell("cmd statusbar add-tile " + TILE);
      }
      screenshot("settings");
      shell("cmd statusbar expand-settings");
      Thread.sleep(1000);
      screenshot("tile");
      check(!UsbIpService.running, "adding tile does not start the bridge");
      if (Build.VERSION.SDK_INT >= 33) {
        shell("cmd statusbar collapse");
        Thread.sleep(500);
        context
            .getSharedPreferences("bridge", 0)
            .edit()
            .putBoolean("notifications_requested", false)
            .commit();
        check(
            context.checkSelfPermission("android.permission.POST_NOTIFICATIONS") != 0,
            "fresh installation has no notification grant");
        send(QuickActions.intent(context, QuickActions.capture(context), false), null);
        await(
            () -> {
              AccessibilityNodeInfo root = test.getUiAutomation().getRootInActiveWindow();
              AccessibilityNodeInfo deny =
                  resource(
                      root, "com.android.permissioncontroller:id/permission_deny_button", null);
              if (root != null) root.recycle();
              if (deny == null) return false;
              deny.recycle();
              return true;
            },
            "actual first-use notification permission prompt");
        String replacement = add("Другой компьютер", true);
        AccessibilityNodeInfo root = test.getUiAutomation().getRootInActiveWindow();
        AccessibilityNodeInfo deny =
            resource(root, "com.android.permissioncontroller:id/permission_deny_button", null);
        if (root != null) root.recycle();
        check(deny != null, "permission prompt remains on captured request");
        try {
          check(
              deny.performAction(AccessibilityNodeInfo.ACTION_CLICK),
              "deny actual notification permission");
        } finally {
          deny.recycle();
        }
        Thread.sleep(700);
        check(
            !UsbIpService.running && repo.snapshot().activeId.equals(replacement),
            "permission result cannot start the changed computer");
        check(
            context.checkSelfPermission("android.permission.POST_NOTIFICATIONS") != 0,
            "notification permission remains denied");
        send(QuickActions.intent(context, QuickActions.capture(context), false), null);
        started();
        check(
            repo.snapshot().activeId.equals(replacement),
            "new explicit command starts selected computer after denial");
        send(QuickActions.intent(context, QuickActions.capture(context), false), null);
        stopped();
      }
      return checks;
    } finally {
      shell("cmd statusbar collapse");
      shell("cmd statusbar " + (added ? "add-tile " : "remove-tile ") + TILE);
      main(() -> context.stopService(new Intent(context, UsbIpService.class)));
      stopped();
      main(() -> app.unregisterActivityLifecycleCallbacks(callbacks));
      repo.reset().get(30, TimeUnit.SECONDS);
    }
  }

  private void unlockFixture() throws Exception {
    shell("input keyevent KEYCODE_WAKEUP");
    shell("input keyevent KEYCODE_MENU");
    Thread.sleep(400);
    android.util.DisplayMetrics display = context.getResources().getDisplayMetrics();
    shell(
        "input swipe "
            + display.widthPixels / 2
            + " "
            + display.heightPixels * 4 / 5
            + " "
            + display.widthPixels / 2
            + " "
            + display.heightPixels / 5
            + " 300");
    Thread.sleep(400);
    shell("input text 482619");
    shell("input keyevent KEYCODE_ENTER");
    try {
      await(() -> !QuickActions.locked(context), "owned secure keyguard accepts fixture PIN");
    } catch (AssertionError failure) {
      screenshot("keyguard-unlock-failure");
      throw failure;
    }
  }

  private void lockFixture() throws Exception {
    shell("input keyevent KEYCODE_SLEEP");
    Thread.sleep(400);
    shell("input keyevent KEYCODE_WAKEUP");
    await(() -> QuickActions.locked(context), "real secure keyguard is locked");
  }

  int keyguardChecks() throws Exception {
    check(shell("getprop ro.kernel.qemu").equals("1"), "keyguard changes on owned emulator only");
    KeyguardManager keyguard = context.getSystemService(KeyguardManager.class);
    check(!keyguard.isDeviceSecure(), "never replace an existing device credential");
    String disabled = shell("locksettings get-disabled");
    check(disabled.equals("true") || disabled.equals("false"), "known baseline lockscreen setting");
    boolean added = shell("settings get secure sysui_qs_tiles").contains(TILE);
    File lockMarker =
        new File(context.getExternalFilesDir(null), "quick-actions-keyguard-owned.json");
    repo = ComputerRepository.get(context);
    repo.ready().get(30, TimeUnit.SECONDS);
    repo.reset().get(30, TimeUnit.SECONDS);
    try {
      context
          .getSharedPreferences("bridge", 0)
          .edit()
          .putBoolean("welcome_seen", true)
          .putBoolean("notifications_requested", true)
          .commit();
      if (Build.VERSION.SDK_INT >= 33)
        shell("pm grant " + APP + " android.permission.POST_NOTIFICATIONS");
      String a = add("Первый компьютер", true), b = add("Второй компьютер", false);
      test.startActivitySync(
          new Intent().setClassName(APP, MAIN).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
      try (FileOutputStream out = new FileOutputStream(lockMarker)) {
        out.write(
            new JSONObject().put("disabled", disabled).toString().getBytes(StandardCharsets.UTF_8));
      }
      shell("locksettings set-pin 482619");
      check(keyguard.isDeviceSecure(), "temporary emulator PIN installed");
      shell("cmd statusbar add-tile " + TILE);
      lockFixture();
      shell("cmd statusbar expand-settings");
      Thread.sleep(600);
      shell("cmd statusbar click-tile " + TILE);
      Thread.sleep(800);
      check(
          !UsbIpService.running && QuickActions.locked(context),
          "tile cannot start before secure unlock");
      activate(b);
      unlockFixture();
      Thread.sleep(800);
      check(
          !UsbIpService.running, "destination changed during unlock cannot start the replacement");
      activate(a);
      lockFixture();
      shell("cmd statusbar expand-settings");
      Thread.sleep(600);
      shell("cmd statusbar click-tile " + TILE);
      Thread.sleep(31000);
      unlockFixture();
      Thread.sleep(800);
      check(!UsbIpService.running, "delayed tile unlock expires without starting");
      shell("cmd statusbar expand-settings");
      Thread.sleep(600);
      shell("cmd statusbar click-tile " + TILE);
      started();
      lockFixture();
      // Dedicated current capability avoids a SystemUI notification refresh
      // cancelling the token while the test is locking the screen.
      PendingIntent stop = QuickActions.intent(context, QuickActions.capture(context), false);
      send(stop, null);
      check(UsbIpService.running, "notification entry cannot stop while keyguard remains locked");
      unlockFixture();
      Thread.sleep(500);
      check(UsbIpService.running, "blocked lockscreen action is not replayed after unlock");
      return checks;
    } finally {
      shell("input keyevent KEYCODE_WAKEUP");
      shell("locksettings clear --old 482619");
      check(!keyguard.isDeviceSecure(), "remove only the fixture credential");
      shell("locksettings set-disabled " + disabled);
      check(!lockMarker.exists() || lockMarker.delete(), "remove exact owned keyguard marker");
      shell("wm dismiss-keyguard");
      shell("cmd statusbar collapse");
      shell("cmd statusbar " + (added ? "add-tile " : "remove-tile ") + TILE);
      main(() -> context.stopService(new Intent(context, UsbIpService.class)));
      stopped();
      repo.reset().get(30, TimeUnit.SECONDS);
    }
  }
}
