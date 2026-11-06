package ru.levchatkin.rutokenbridge;

import android.app.*;
import android.content.*;
import android.graphics.Rect;
import android.os.*;
import android.view.*;
import android.view.accessibility.AccessibilityNodeInfo;
import android.view.inputmethod.InputMethodManager;
import android.widget.*;
import java.io.*;
import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;
import java.security.KeyStore;
import java.util.*;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicReference;
import javax.crypto.*;
import javax.crypto.spec.GCMParameterSpec;
import org.cgutman.usbip.connection.ComputerRepository;
import org.json.*;

/** Test APK only. Real R8 UI/Keystore/JNI/SSH/bootstrap, only an isolated QEMU target. */
final class ConnectionSuite {
  private static final String APP = "ru.levchatkin.rutokenbridge";
  private static final String MAIN = "org.cgutman.usbip.config.UsbIpConfig";
  private static final String SETUP = "org.cgutman.usbip.connection.ConnectionSetup";
  private static final String SERVICE = "org.cgutman.usbip.service.UsbIpService";
  private final Instrumentation test;
  private final Context context;
  private final AtomicReference<Activity> resumed = new AtomicReference<>();
  private int checks;

  ConnectionSuite(Instrumentation test, Activity main) {
    this.test = test;
    context = test.getTargetContext();
    resumed.set(main);
  }

  private void check(boolean ok, String label) {
    checks++;
    if (!ok) throw new AssertionError(label);
  }

  private void phase(String label) {
    Bundle b = new Bundle();
    b.putString("stream", "PASS " + label + "\n");
    test.sendStatus(0, b);
  }

  private void main(Runnable action) {
    Throwable[] error = new Throwable[1];
    test.runOnMainSync(() -> {
      try {
        action.run();
      } catch (Throwable e) {
        error[0] = e;
      }
    });
    if (error[0] != null) throw new AssertionError("connection UI action failed", error[0]);
  }

  private Activity await(String name, Activity except) throws Exception {
    long until = SystemClock.elapsedRealtime() + 10000;
    while (SystemClock.elapsedRealtime() < until) {
      Activity a = resumed.get();
      if (a != null && a != except && a.getClass().getName().equals(name)) {
        test.waitForIdleSync();
        return a;
      }
      Thread.sleep(20);
    }
    throw new AssertionError("activity did not resume: " + name);
  }

  private View find(View root, Class<?> type, String text) {
    if (type.isInstance(root)
        && root instanceof TextView
        && ((TextView) root).getText().toString().contains(text)) return root;
    if (root instanceof ViewGroup) {
      ViewGroup group = (ViewGroup) root;
      for (int n = 0; n < group.getChildCount(); n++) {
        View found = find(group.getChildAt(n), type, text);
        if (found != null) return found;
      }
    }
    return null;
  }

  private void edits(View root, List<EditText> out) {
    if (root instanceof EditText) out.add((EditText) root);
    if (root instanceof ViewGroup) {
      ViewGroup group = (ViewGroup) root;
      for (int n = 0; n < group.getChildCount(); n++) edits(group.getChildAt(n), out);
    }
  }

  private EditText input(Activity a, String hint) {
    List<EditText> fields = new ArrayList<>();
    edits(a.getWindow().getDecorView(), fields);
    for (EditText field : fields)
      if (field.getTag() instanceof String && ((String) field.getTag()).startsWith(hint))
        return field;
    throw new AssertionError("missing input: " + hint);
  }

  private void set(Activity a, String hint, String value) {
    main(() -> input(a, hint).setText(value));
  }

  private void click(Activity a, String label) {
    main(() -> {
      View v = find(a.getWindow().getDecorView(), Button.class, label);
      check(v != null && v.performClick(), "UI button: " + label);
    });
  }

  private boolean has(Activity a, String text) {
    boolean[] found = {false};
    main(() -> found[0] = find(a.getWindow().getDecorView(), TextView.class, text) != null);
    return found[0];
  }

  private void waitText(Activity a, String text, long timeout) throws Exception {
    long until = SystemClock.elapsedRealtime() + timeout;
    while (SystemClock.elapsedRealtime() < until) {
      if (has(a, text)) {
        checks++;
        return;
      }
      Thread.sleep(100);
    }
    throw new AssertionError("connection UI state missing: " + text);
  }

  private AccessibilityNodeInfo node(AccessibilityNodeInfo root, String text) {
    if (root == null) return null;
    if (root.getText() != null
        && root.getText()
            .toString()
            .toLowerCase(Locale.ROOT)
            .contains(text.toLowerCase(Locale.ROOT))) return root;
    for (int n = 0; n < root.getChildCount(); n++) {
      AccessibilityNodeInfo found = node(root.getChild(n), text);
      if (found != null) return found;
    }
    return null;
  }

  private String dialog(String text) throws Exception {
    long until = SystemClock.elapsedRealtime() + 15000;
    while (SystemClock.elapsedRealtime() < until) {
      AccessibilityNodeInfo found = node(test.getUiAutomation().getRootInActiveWindow(), text);
      if (found != null) return found.getText().toString();
      Thread.sleep(100);
    }
    String[] status = {"unavailable"};
    main(() -> {
      Activity a = resumed.get();
      if (a != null) {
        TextView view =
            a.findViewById(context.getResources().getIdentifier("bridge_status", "id", APP));
        if (view != null) status[0] = view.getText().toString();
      }
    });
    throw new AssertionError("dialog missing: " + text + "; bridge status: " + status[0]);
  }

  private AccessibilityNodeInfo buttonNode(AccessibilityNodeInfo root, String label) {
    if (root == null) return null;
    if (root.isClickable()
        && root.getText() != null
        && root.getText().toString().equalsIgnoreCase(label)) return root;
    for (int n = 0; n < root.getChildCount(); n++) {
      AccessibilityNodeInfo found = buttonNode(root.getChild(n), label);
      if (found != null) return found;
    }
    return null;
  }

  private void dialogClick(String label) throws Exception {
    dialog(label);
    AccessibilityNodeInfo n = buttonNode(test.getUiAutomation().getRootInActiveWindow(), label);
    check(
        n != null && n.performAction(AccessibilityNodeInfo.ACTION_CLICK),
        "dialog action: " + label);
  }

  private byte[] bytes(InputStream input, int maximum) throws Exception {
    ByteArrayOutputStream out = new ByteArrayOutputStream();
    byte[] block = new byte[4096];
    int n;
    while ((n = input.read(block)) != -1) {
      if (out.size() + n > maximum) throw new IOException("test input bound");
      out.write(block, 0, n);
    }
    return out.toByteArray();
  }

  private byte[] read(File path) throws Exception {
    try (InputStream input = new FileInputStream(path)) {
      return bytes(input, 131200);
    }
  }

  private void writeAtomic(File path, byte[] value) throws Exception {
    android.util.AtomicFile file = new android.util.AtomicFile(path);
    FileOutputStream output = null;
    try {
      output = file.startWrite();
      output.write(value);
      file.finishWrite(output);
      output = null;
    } finally {
      if (output != null) file.failWrite(output);
    }
  }

  private Intent service() {
    return new Intent().setClassName(APP, SERVICE);
  }

  private String shell(String command) throws Exception {
    try (ParcelFileDescriptor fd = test.getUiAutomation().executeShellCommand(command);
        InputStream in = new ParcelFileDescriptor.AutoCloseInputStream(fd)) {
      return new String(bytes(in, 131200), StandardCharsets.UTF_8);
    }
  }

  private void stop() throws Exception {
    main(() -> context.stopService(service()));
    long until = SystemClock.elapsedRealtime() + 10000;
    while (SystemClock.elapsedRealtime() < until) {
      if (!shell("dumpsys activity services " + APP).contains("ServiceRecord{")
          && !ComputerRepository.get(context).isBusy()) {
        Activity a = resumed.get();
        if (a != null
            && a.getClass().getName().equals(MAIN)
            && ComputerRepository.get(context).snapshot().active() != null)
          waitText(a, "Запустить мост", 3000);
        return;
      }
      Thread.sleep(100);
    }
    throw new AssertionError("secure foreground service did not stop");
  }

  private void importKey(Activity a, String key) throws Exception {
    File path = new File(context.getCacheDir(), "receiver-fixture-" + UUID.randomUUID() + ".key");
    check(path.createNewFile(), "unique private key fixture path");
    try {
      try (FileOutputStream out = new FileOutputStream(path)) {
        out.write(key.getBytes(StandardCharsets.UTF_8));
      }
      Method result =
          a.getClass().getDeclaredMethod("onActivityResult", int.class, int.class, Intent.class);
      result.setAccessible(true);
      main(() -> {
        try {
          result.invoke(
              a, 24, Activity.RESULT_OK, new Intent().setData(android.net.Uri.fromFile(path)));
        } catch (Exception e) {
          throw new AssertionError(e);
        }
      });
      waitText(a, "SSH-ключ выбран", 15000);
    } finally {
      check(path.delete(), "only owned private-key fixture removed");
    }
  }

  private JSONObject decrypt(File file) throws Exception {
    byte[] encoded = read(file);
    check(
        encoded.length >= 30 && encoded[0] == 2 && encoded[1] == 12,
        "actual vault envelope/GCM format");
    KeyStore store = KeyStore.getInstance("AndroidKeyStore");
    store.load(null);
    SecretKey key = (SecretKey) store.getKey("usb-most.computers.v2", null);
    check(key != null && key.getEncoded() == null, "actual Android Keystore key is non-exportable");
    Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
    cipher.init(Cipher.DECRYPT_MODE, key, new GCMParameterSpec(128, encoded, 2, 12));
    cipher.updateAAD("usb-most/computers/v2".getBytes(StandardCharsets.UTF_8));
    byte[] plain = cipher.doFinal(encoded, 14, encoded.length - 14);
    try {
      JSONObject catalog = new JSONObject(new String(plain, StandardCharsets.UTF_8));
      JSONArray computers = catalog.getJSONArray("computers");
      for (int i = 0; i < computers.length(); i++) {
        JSONObject entry = computers.getJSONObject(i);
        if (entry.getString("id").equals(catalog.getString("active_id")))
          return entry.getJSONObject("connection");
      }
      throw new AssertionError("active computer missing from encrypted catalog");
    } finally {
      Arrays.fill(plain, (byte) 0);
      Arrays.fill(encoded, (byte) 0);
    }
  }

  private void layout(Activity a) {
    main(() -> {
      View root = a.getWindow().getDecorView();
      List<EditText> fields = new ArrayList<>();
      edits(root, fields);
      check(root.getWidth() > 0, "connection viewport measured");
      for (EditText field : fields)
        if (field.getVisibility() == View.VISIBLE) {
          check(
              field.getWidth() > 0 && field.getWidth() <= root.getWidth(),
              "connection field fits viewport");
          check(
              field.getMeasuredHeight() >= field.getLineHeight(),
              "connection field text is not vertically clipped");
        }
      InputMethodManager keyboard =
          (InputMethodManager) context.getSystemService(Context.INPUT_METHOD_SERVICE);
      keyboard.hideSoftInputFromWindow(root.getWindowToken(), 0);
      View save = find(root, Button.class, "Сохранить компьютер");
      check(save != null, "save remains in scrollable form");
      save.requestRectangleOnScreen(new Rect(0, 0, save.getWidth(), save.getHeight()), true);
    });
  }

  int run(boolean retainOwnedFixture) throws Exception {
    JSONObject fixture;
    try (InputStream input = test.getContext().getAssets().open("receiver-fixture.json")) {
      fixture = new JSONObject(new String(bytes(input, 131200), StandardCharsets.UTF_8));
    }
    check(
        fixture.getString("scope").equals("isolated-QEMU-only")
            && fixture.getString("host").equals("10.0.2.2"),
        "SSH target is our isolated emulator/QEMU fixture");
    SharedPreferences prefs = context.getSharedPreferences("bridge", 0);
    check(shell("getprop ro.kernel.qemu").trim().equals("1"), "disposable emulator only");
    ComputerRepository repo = ComputerRepository.get(context);
    repo.ready().get(30, TimeUnit.SECONDS);
    File vault = new File(context.getNoBackupFilesDir(), "computers.enc");
    Application application = (Application) context.getApplicationContext();
    Application.ActivityLifecycleCallbacks callbacks =
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
    main(() -> application.registerActivityLifecycleCallbacks(callbacks));
    boolean completed = false;
    try {
      stop();
      repo.reset().get(30, TimeUnit.SECONDS);
      Activity main = await(MAIN, null);
      View later = find(main.getWindow().getDecorView(), Button.class, "Позже");
      if (later != null && later.isShown()) click(main, "Позже");
      waitText(main, "Настроить подключение", 5000);
      click(main, "Настроить подключение");
      click(main, "Через SSH");
      Activity setup = await(SETUP, null);
      click(setup, "Ключ и дополнительные параметры");
      set(setup, "Адрес компьютера", fixture.getString("host"));
      set(setup, "Порт SSH", Integer.toString(fixture.getInt("port")));
      set(setup, "Имя пользователя", fixture.getString("user"));
      set(setup, "Пароль SSH", "rotation-only-secret");
      set(setup, "Пароль администратора", "rotation-only-sudo");
      set(setup, "Пароль ключа", "rotation-only-keypass");
      importKey(setup, fixture.getString("private_key"));
      layout(setup);
      Activity original = setup;
      EditText[] oldPassword = new EditText[1];
      main(() -> oldPassword[0] = input(original, "Пароль SSH"));
      main(original::recreate);
      setup = await(SETUP, original);
      Activity rotated = setup;
      main(() -> {
        check(
            input(rotated, "Адрес компьютера").getText().toString().equals("10.0.2.2"),
            "host survives rotation");
        check(
            input(rotated, "Пароль SSH").getText().toString().equals("rotation-only-secret"),
            "password survives rotation in memory");
        check(
            input(rotated, "Пароль администратора")
                .getText()
                .toString()
                .equals("rotation-only-sudo"),
            "separate sudo password survives rotation");
        check(
            input(rotated, "Пароль ключа").getText().toString().equals("rotation-only-keypass"),
            "key passphrase survives rotation");
        check(
            oldPassword[0].getText().length() == 0,
            "destroyed Activity clears old editable secret");
      });
      layout(setup);
      set(setup, "Пароль SSH", "");
      set(setup, "Пароль ключа", "");
      set(setup, "Пароль администратора", "intentionally-wrong-fixture-password");
      click(setup, "Сохранить компьютер");
      main = await(MAIN, null);
      JSONObject envelope = decrypt(vault);
      check(
          envelope.getJSONObject("profile").toString().equals(repo.snapshot().active().profile),
          "sealed profile bound to public profile cache");
      check(
          envelope
              .getJSONObject("credentials")
              .getString("private_key")
              .equals(fixture.getString("private_key")),
          "imported key survives rotation and is actually sealed");
      phase("R8 connection UI, bounded key import, rotation, geometry and actual Keystore/GCM");
      click(main, "Запустить мост");
      dialog("Доверять компьютеру?");
      String fingerprint = dialog("SHA256:");
      boolean known = false;
      JSONArray pins = fixture.getJSONArray("host_pins");
      for (int n = 0; n < pins.length(); n++)
        known |= fingerprint.contains(pins.getString(n).replace('-', '+').replace('_', '/'));
      check(known, "displayed SSH fingerprint matches isolated VM public host key");
      dialogClick("Отмена");
      waitText(main, "ключ компьютера", 15000);
      stop();
      check(
          new JSONObject(repo.snapshot().active().profile).isNull("host_key"),
          "denied host is not persisted");
      click(main, "Запустить мост");
      dialog("Доверять компьютеру?");
      dialogClick("Доверять");
      waitText(main, "подготовка не завершена", 45000);
      stop();
      check(
          !new JSONObject(repo.snapshot().active().profile).isNull("host_key"),
          "explicitly approved host saved before preparation");
      phase("actual SSH trust denial/approval and wrong sudo password fail closed");
      click(main, "Изменить компьютер");
      Activity list = await("org.cgutman.usbip.config.ComputersActivity", null);
      click(list, "⋮");
      dialogClick("Изменить подключение");
      setup = await(SETUP, null);
      long loadedUntil = SystemClock.elapsedRealtime() + 10000;
      while (input(setup, "Пароль администратора").length() == 0
          && SystemClock.elapsedRealtime() < loadedUntil) Thread.sleep(50);
      check(
          input(setup, "Пароль администратора").length() > 0,
          "bound editor loaded saved credentials");
      click(setup, "Ключ и дополнительные параметры");
      set(setup, "Пароль администратора", fixture.getString("sudo_password"));
      click(setup, "Сохранить компьютер");
      list = await("org.cgutman.usbip.config.ComputersActivity", null);
      click(list, "Назад");
      main = await(MAIN, null);
      click(main, "Запустить мост");
      waitText(main, "Защищённое подключение", 120000);
      envelope = decrypt(vault);
      check(
          envelope
              .getJSONObject("credentials")
              .getString("sudo_password")
              .equals(fixture.getString("sudo_password")),
          "separate sudo credential sealed and reused");
      phase("actual optimized APK -> SSH -> password sudo -> cold Linux stack installation/health");
      stop();
      click(main, "Запустить мост");
      waitText(main, "Защищённое подключение", 120000);
      stop();
      phase("repeat start reuses sealed credentials and trusted host without another approval");
      byte[] valid = read(vault);
      byte[] corrupt = valid.clone();
      corrupt[corrupt.length - 1] ^= 1;
      writeAtomic(vault, corrupt);
      Arrays.fill(corrupt, (byte) 0);
      try {
        click(main, "Запустить мост");
        waitText(main, "Сохранённые данные недоступны", 15000);
        stop();
      } finally {
        writeAtomic(vault, valid);
        Arrays.fill(valid, (byte) 0);
        repo.reload().get(30, TimeUnit.SECONDS);
      }
      waitText(main, "Запустить мост", 5000);
      click(main, "Запустить мост");
      waitText(main, "Защищённое подключение", 120000);
      stop();
      phase("tampered GCM is refused before SSH and restored sealed state reconnects");
      completed = true;
      return checks;
    } finally {
      stop();
      // An explicit test-only preparation can retain this verified QEMU profile.
      // The host runner then starts a normal, uninstrumented application process
      // for a foreground-service soak. Failed preparation always resets it.
      if (!retainOwnedFixture || !completed) repo.reset().get(30, TimeUnit.SECONDS);
      main(() -> {
        Activity current = resumed.get();
        if (current != null && current.getClass().getName().equals(SETUP)) current.finish();
        application.unregisterActivityLifecycleCallbacks(callbacks);
      });
    }
  }
}
