package ru.levchatkin.rutokenbridge;

import android.app.*;
import android.content.*;
import android.hardware.usb.UsbDevice;
import android.os.*;
import android.view.accessibility.AccessibilityNodeInfo;
import java.io.*;
import java.lang.reflect.Field;
import java.net.Socket;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.*;
import java.util.concurrent.TimeUnit;
import java.util.function.BooleanSupplier;
import org.cgutman.usbip.connection.ComputerRepository;
import org.cgutman.usbip.core.ManagedImports;
import org.cgutman.usbip.jni.NativeManagement;
import org.cgutman.usbip.service.UsbInventory;
import org.json.JSONObject;

/** Optimized Android/JNI/inventory/UI; OS replies and USB hardware are explicitly synthetic. */
final class ManagedEjectionSuite {
  private static final String APP = "ru.levchatkin.rutokenbridge";
  private final Instrumentation test;
  private final Context context;
  private int checks;
  private UsbInventory inventory;
  private final Map<Socket, UsbDevice> owners = new HashMap<>();

  ManagedEjectionSuite(Instrumentation test) {
    this.test = test;
    context = test.getTargetContext();
  }

  private void check(boolean value, String label) {
    checks++;
    if (!value) throw new AssertionError(label);
  }

  private void main(Runnable action) {
    Throwable[] error = {null};
    test.runOnMainSync(() -> {
      try {
        action.run();
      } catch (Throwable failure) {
        error[0] = failure;
      }
    });
    if (error[0] != null) throw new AssertionError("managed UI/inventory", error[0]);
  }

  private void await(BooleanSupplier ready, String label) throws Exception {
    long end = SystemClock.elapsedRealtime() + 10000;
    while (!ready.getAsBoolean() && SystemClock.elapsedRealtime() < end) Thread.sleep(30);
    check(ready.getAsBoolean(), label);
  }

  private UsbInventory.Item item(UsbDevice device) {
    return inventory.item(device.getDeviceId());
  }

  private ManagedImports adapter(long timeout) {
    byte[] nonce = new byte[16];
    new java.security.SecureRandom().nextBytes(nonce);
    long handle = NativeManagement.create(nonce);
    check(handle > 0, "actual ART native model");
    return new ManagedImports(
        new ManagedImports.Model() {
          public byte[] execute(int action, byte[] input) {
            return NativeManagement.execute(handle, action, input);
          }

          public void close() {
            NativeManagement.close(handle);
          }
        },
        (socket, request) -> {
          UsbDevice device = owners.get(socket);
          return device == null
              ? null
              : inventory.bindEjection(
                  device.getDeviceId(), item(device).generation, socket, request);
        },
        timeout);
  }

  private byte[] bind(ManagedImports model, UsbDevice device, Socket owner) throws Exception {
    main(() ->
        inventory.select(
            device.getDeviceId(), item(device).generation, true, item(device).storage));
    await(() -> item(device).ready, "synthetic descriptor ready");
    check(
        inventory.acquire(device.getDeviceId(), item(device).generation, owner),
        "exact synthetic owner acquired");
    inventory.state(
        device.getDeviceId(), item(device).generation, owner, "Подключено к компьютеру");
    owners.put(owner, device);
    byte[] key = model.reserve();
    check(key != null && model.attached(owner, key), "managed key bound to inventory generation");
    return key;
  }

  private byte[] poll(ManagedImports model, byte[] key, long sequence, int state) {
    byte[] reply =
        model.synchronize(
            ByteBuffer.allocate(40).put(key).putLong(sequence).putInt(state).putInt(0).array());
    check(reply != null, "accepted exact synthetic receiver report");
    return reply;
  }

  private void released(ManagedImports model, UsbDevice device, Socket owner, byte[] key) {
    inventory.released(device.getDeviceId(), item(device).generation, owner);
    model.released(key);
    owners.remove(owner);
  }

  private AccessibilityNodeInfo find(AccessibilityNodeInfo root, String text) {
    if (root == null) return null;
    if (text.contentEquals(root.getText() == null ? "" : root.getText()))
      return AccessibilityNodeInfo.obtain(root);
    for (int i = 0; i < root.getChildCount(); i++) {
      AccessibilityNodeInfo child = root.getChild(i), found = find(child, text);
      if (child != null) child.recycle();
      if (found != null) return found;
    }
    return null;
  }

  private void click(String text) throws Exception {
    visible(text, true);
  }

  private void show(String text) throws Exception {
    visible(text, false);
  }

  private void visible(String text, boolean click) throws Exception {
    long end = SystemClock.elapsedRealtime() + 10000;
    while (SystemClock.elapsedRealtime() < end) {
      AccessibilityNodeInfo root = test.getUiAutomation().getRootInActiveWindow(),
          node = find(root, text);
      if (root != null) root.recycle();
      if (node != null) {
        try {
          node.performAction(
              AccessibilityNodeInfo.AccessibilityAction.ACTION_SHOW_ON_SCREEN.getId());
          if (node.isVisibleToUser() && (!click || node.isEnabled())) {
            check(
                !click || node.performAction(AccessibilityNodeInfo.ACTION_CLICK),
                click ? "actual accessible eject click" : "actual visible status: " + text);
            return;
          }
        } finally {
          node.recycle();
        }
      }
      Thread.sleep(100);
    }
    screenshot("missing-action");
    throw new AssertionError("visible action missing: " + text);
  }

  private void screenshot(String name) throws Exception {
    // Wait for the actual visible-state assertion first; then let system
    // rotation/scroll animations finish instead of capturing an intermediate frame.
    Thread.sleep(1200);
    File directory = new File(context.getExternalFilesDir(null), "managed-ejection-proof");
    check(directory.isDirectory() || directory.mkdir(), "owned screenshot directory");
    android.graphics.Bitmap bitmap = test.getUiAutomation().takeScreenshot();
    check(bitmap != null, "actual screen capture");
    try (FileOutputStream output = new FileOutputStream(new File(directory, name + ".png"))) {
      check(
          bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, output),
          "saved rendered screen");
    } finally {
      bitmap.recycle();
    }
  }

  int run() throws Exception {
    try (InputStream input =
        new ParcelFileDescriptor.AutoCloseInputStream(
            test.getUiAutomation().executeShellCommand("getprop ro.kernel.qemu"))) {
      byte[] bytes = new byte[64];
      int n = input.read(bytes);
      check(
          n > 0 && new String(bytes, 0, n, StandardCharsets.UTF_8).trim().equals("1"),
          "owned emulator only");
    }
    ComputerRepository repo = ComputerRepository.get(context);
    repo.ready().get(30, TimeUnit.SECONDS);
    repo.reset().get(30, TimeUnit.SECONDS);
    repo.change(
            new JSONObject()
                .put("op", "add")
                .put("revision", repo.snapshot().revision)
                .put("name", "Тестовый компьютер")
                .put("activate", true)
                .put(
                    "connection",
                    new JSONObject()
                        .put("kind", "legacy")
                        .put("host", "192.168.10.2")
                        .put("mode", 1))
                .toString()
                .getBytes(StandardCharsets.UTF_8))
        .get(30, TimeUnit.SECONDS);
    Object app = context.getApplicationContext();
    Field field = app.getClass().getDeclaredField("inventory");
    field.setAccessible(true);
    UsbInventory original = (UsbInventory) field.get(app);
    UsbFixture fixture = new UsbFixture(context);
    main(() -> {
      inventory = new UsbInventory(fixture, fixture);
      inventory.bindComputers(repo);
    });
    field.set(app, inventory);
    Activity screen = null;
    try (ManagedImports model = adapter(30000)) {
      context.getSharedPreferences("bridge", 0).edit().putBoolean("welcome_seen", true).commit();
      screen =
          test.startActivitySync(
              new Intent()
                  .setClassName(APP, "org.cgutman.usbip.config.UsbIpConfig")
                  .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_CLEAR_TASK));
      UsbDevice disk = UsbFixture.device(181, "Тестовая флешка", 8, false, 81);
      UsbDevice second = UsbFixture.device(182, "Вторая флешка", 8, false, 82);
      UsbDevice mouse = UsbFixture.device(183, "Test Mouse", 3, false, 83);
      UsbDevice token = UsbFixture.device(184, "Test Token", 11, false, 84);
      for (UsbDevice device : Arrays.asList(disk, second, mouse, token))
        main(() -> {
          fixture.permissions.add(device.getDeviceId());
          fixture.connect(
              device, UsbFixture.descriptor(device == mouse ? 3 : device == token ? 11 : 8, false));
        });
      try (Socket owner = new Socket();
          Socket other = new Socket();
          Socket mouseOwner = new Socket();
          Socket tokenOwner = new Socket()) {
        byte[] key = bind(model, disk, owner), otherKey = bind(model, second, other);
        byte[] mouseKey = bind(model, mouse, mouseOwner);
        byte[] tokenKey = bind(model, token, tokenOwner);
        poll(model, mouseKey, 0, 0);
        poll(model, tokenKey, 0, 0);
        check(
            !item(mouse).canEject && !item(token).canEject,
            "storage action is not offered for HID or token");
        check(!item(disk).canEject, "action hidden until receiver supports current import");
        poll(model, key, 0, 0);
        await(() -> item(disk).canEject, "action available on exact ready storage");
        show("Извлечь на компьютере");
        screenshot("ready-portrait");
        click("Извлечь на компьютере");
        await(() -> item(disk).ejectionState == 1, "pending UI after tap");
        check(item(disk).selected && !item(disk).canEject, "selection retained while OS pending");
        check(
            !inventory.requestEjection(disk.getDeviceId(), item(disk).generation),
            "repeated tap cannot allocate new operation");
        test.getUiAutomation().setRotation(UiAutomation.ROTATION_FREEZE_90);
        Thread.sleep(1200);
        show("Завершение работы с накопителем…");
        screenshot("pending-landscape");
        test.getUiAutomation().setRotation(UiAutomation.ROTATION_FREEZE_0);
        Thread.sleep(1200);
        check(
            item(disk).ejectionState == 1 && item(disk).selected,
            "rotation preserves in-flight lease");
        poll(model, key, 1, 3);
        await(() -> item(disk).canEject, "Busy enables explicit retry");
        show("Закройте файлы на компьютере и повторите извлечение");
        screenshot("busy-portrait");
        click("Извлечь на компьютере");
        await(() -> item(disk).ejectionState == 1, "retry starts next sequence");
        byte[] retry = poll(model, key, 1, 3);
        check(
            ByteBuffer.wrap(retry).getLong(24) == 2
                && ByteBuffer.wrap(retry).getInt(32) == 1
                && item(disk).ejectionState == 1,
            "previous Busy snapshot delivers the next explicit request");
        check(
            Arrays.equals(retry, poll(model, key, 1, 3)),
            "lost retry directive preserves the same new operation");
        byte[] done = poll(model, key, 2, 2);
        check(
            ByteBuffer.wrap(done).getInt(32) == 2,
            "Safe acknowledged after applying exact inventory result");
        check(
            !item(disk).selected && !item(disk).approved && item(disk).ejectionState == 2,
            "Safe deselects and clears Android storage consent");
        check(
            item(second).selected && item(mouse).selected && item(token).selected,
            "neighbor storage/token/mouse untouched");
        released(model, disk, owner, key);
        check(item(disk).ejectionState == 2, "USB cleanup preserves confirmed UI result");
        show("Передача завершена · для повторной выберите снова");
        screenshot("safe-portrait");
        check(
            ByteBuffer.wrap(poll(model, key, 2, 2)).getInt(32) == 2,
            "repeated safe acknowledgement is Retire");
        // Physical address unchanged; explicit new selection must still invalidate the old result.
        poll(model, otherKey, 0, 0);
        check(
            inventory.requestEjection(second.getDeviceId(), item(second).generation),
            "second exact operation");
        main(() -> inventory.select(second.getDeviceId(), item(second).generation, true, true));
        poll(model, otherKey, 1, 2);
        check(
            item(second).selected && item(second).approved && item(second).ejectionState == -1,
            "late Safe cannot clear new explicit selection");
        released(model, second, other, otherKey);
        released(model, mouse, mouseOwner, mouseKey);
        released(model, token, tokenOwner, tokenKey);
      }
      try (ManagedImports late = adapter(120);
          Socket owner = new Socket()) {
        byte[] key = bind(late, disk, owner);
        poll(late, key, 0, 0);
        check(
            inventory.requestEjection(disk.getDeviceId(), item(disk).generation),
            "uncertainty test starts exact operation");
        await(
            () -> item(disk).ejectionState == 7,
            "actual timer marks uncertain without clearing selection");
        check(
            item(disk).selected && !item(disk).canEject,
            "timeout is not success or replay permission");
        released(late, disk, owner, key);
        poll(late, key, 1, 2);
        check(
            item(disk).ejectionState == 2, "late Safe after USB EOF applies to original selection");
      }
      try (ManagedImports replaced = adapter(30000);
          Socket owner = new Socket()) {
        byte[] key = bind(replaced, disk, owner);
        poll(replaced, key, 0, 0);
        check(
            inventory.requestEjection(disk.getDeviceId(), item(disk).generation),
            "detach test request");
        long old = item(disk).generation;
        main(() -> fixture.disconnect(disk));
        main(() -> {
          fixture.permissions.add(disk.getDeviceId());
          fixture.connect(disk, UsbFixture.descriptor(8, false));
        });
        check(item(disk).generation != old, "same USB address gets new generation");
        main(() -> inventory.select(disk.getDeviceId(), item(disk).generation, true, true));
        poll(replaced, key, 1, 2);
        check(
            item(disk).selected && item(disk).ejectionState == -1,
            "replacement USB unaffected by old Safe");
      }
      try (ManagedImports permission = adapter(30000);
          Socket owner = new Socket()) {
        byte[] key = bind(permission, disk, owner);
        poll(permission, key, 0, 0);
        check(
            inventory.requestEjection(disk.getDeviceId(), item(disk).generation),
            "permission test request");
        main(() -> {
          fixture.permissions.remove(disk.getDeviceId());
          inventory.refresh();
        });
        poll(permission, key, 1, 2);
        check(
            item(disk).ejectionState != 2 && !item(disk).canEject,
            "revoked USB permission invalidates old result");
        released(permission, disk, owner, key);
        main(() -> {
          fixture.permissions.add(disk.getDeviceId());
          inventory.refresh();
        });
      }
      try (ManagedImports selection = adapter(30000);
          Socket owner = new Socket()) {
        byte[] key = bind(selection, disk, owner);
        poll(selection, key, 0, 0);
        check(
            inventory.requestEjection(disk.getDeviceId(), item(disk).generation),
            "select-all test request");
        main(() -> inventory.selectAll(true, Collections.emptyList()));
        poll(selection, key, 1, 2);
        check(
            item(disk).selected && item(disk).ejectionState == -1,
            "explicit select-all invalidates prior result");
        released(selection, disk, owner, key);
      }
      try (ManagedImports destination = adapter(30000);
          Socket owner = new Socket()) {
        byte[] key = bind(destination, disk, owner);
        poll(destination, key, 0, 0);
        check(
            inventory.requestEjection(disk.getDeviceId(), item(disk).generation),
            "computer scope test request");
        check(!inventory.bindComputer(null), "cannot replace computer with USB still owned");
        released(destination, disk, owner, key);
        check(inventory.bindComputer(null), "computer scope may change after release");
        poll(destination, key, 1, 2);
        check(item(disk).ejectionState == -1, "late prior-computer result is discarded");
        check(
            inventory.bindComputer(repo.snapshot().active()), "restore owned test computer scope");
      }
      try (ManagedImports stopping = adapter(30000);
          Socket owner = new Socket()) {
        byte[] key = bind(stopping, disk, owner);
        poll(stopping, key, 0, 0);
        check(
            inventory.requestEjection(disk.getDeviceId(), item(disk).generation),
            "stop test request");
        stopping.close();
        check(
            !item(disk).canEject && item(disk).ejectionState == 7,
            "model shutdown cannot report success");
        check(
            stopping.synchronize(
                    ByteBuffer.allocate(40).put(key).putLong(1).putInt(2).putInt(0).array())
                == null,
            "closed service ignores late result");
        inventory.released(disk.getDeviceId(), item(disk).generation, owner);
      }
      return checks;
    } finally {
      test.getUiAutomation().setRotation(UiAutomation.ROTATION_UNFREEZE);
      if (screen != null) {
        Activity owned = screen;
        main(owned::finish);
      }
      context.stopService(new Intent().setClassName(APP, "org.cgutman.usbip.service.UsbIpService"));
      field.set(app, original);
      main(inventory::close);
      owners.clear();
    }
  }
}
