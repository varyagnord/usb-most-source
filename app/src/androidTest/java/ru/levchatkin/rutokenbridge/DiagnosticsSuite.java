package ru.levchatkin.rutokenbridge;

import android.app.*;
import android.content.*;
import android.graphics.Bitmap;
import android.os.*;
import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyProperties;
import android.view.*;
import android.view.accessibility.AccessibilityNodeInfo;
import android.widget.*;
import java.io.*;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.security.KeyStore;
import java.util.*;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicReference;
import javax.crypto.Cipher;
import javax.crypto.KeyGenerator;
import org.cgutman.usbip.connection.ComputerRepository;
import org.cgutman.usbip.connection.ConnectionObservation;
import org.cgutman.usbip.service.UsbIpService;
import org.json.*;

/** Actual foreground service, encrypted repository and diagnosis UI; owned emulators only. */
final class DiagnosticsSuite {
  private static final String APP = "ru.levchatkin.rutokenbridge";
  private final Instrumentation test;
  private final Context context;
  private final AtomicReference<Activity> resumed = new AtomicReference<>();
  private int checks;

  DiagnosticsSuite(Instrumentation test) {
    this.test = test;
    context = test.getTargetContext();
  }

  private void check(boolean ok, String message) {
    if (!ok) throw new AssertionError(message);
    checks++;
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
    if (error[0] != null) throw new AssertionError("diagnosis UI", error[0]);
    test.waitForIdleSync();
  }

  private String shell(String command) throws Exception {
    try (InputStream in =
        new ParcelFileDescriptor.AutoCloseInputStream(
            test.getUiAutomation().executeShellCommand(command))) {
      ByteArrayOutputStream out = new ByteArrayOutputStream();
      byte[] buffer = new byte[1024];
      int count;
      while ((count = in.read(buffer)) != -1) out.write(buffer, 0, count);
      return out.toString("UTF-8").trim();
    }
  }

  private View find(View view, String text) {
    if (!view.isShown()) return null;
    if (view instanceof Button && ((Button) view).getText().toString().equals(text)) return view;
    if (view instanceof ViewGroup) {
      ViewGroup group = (ViewGroup) view;
      for (int i = 0; i < group.getChildCount(); i++) {
        View found = find(group.getChildAt(i), text);
        if (found != null) return found;
      }
    }
    return null;
  }

  private void click(String text) throws Exception {
    for (int i = 0; i < 450; i++) {
      boolean[] done = {false};
      main(() -> {
        Activity a = resumed.get();
        View v = a == null ? null : find(a.getWindow().getDecorView(), text);
        if (v != null && v.isEnabled()) {
          check(v.performClick(), "click " + text);
          done[0] = true;
        }
      });
      if (done[0]) return;
      Thread.sleep(100);
    }
    throw new AssertionError("missing action " + text);
  }

  private void summary(String expected) throws Exception {
    for (int i = 0; i < 100; i++) {
      boolean[] found = {false};
      main(() -> {
        Activity a = resumed.get();
            int id = context.getResources().getIdentifier("diagnosis_summary", "id", APP);
            TextView v = a == null ? null : a.findViewById(id);
        found[0] = v != null && v.isShown() && v.getText().toString().equals(expected);
      });
      if (found[0]) {
        checks++;
        return;
      }
      Thread.sleep(100);
    }
    throw new AssertionError("diagnostic summary did not match expected public explanation");
  }

  private void collect(AccessibilityNodeInfo node, StringBuilder text) {
    if (node == null) return;
    if (node.getText() != null) text.append(node.getText()).append('\n');
    if (node.getContentDescription() != null)
      text.append(node.getContentDescription()).append('\n');
    for (int i = 0; i < node.getChildCount(); i++) collect(node.getChild(i), text);
  }

  private void noSecrets(JSONObject credentials) throws Exception {
    StringBuilder text = new StringBuilder();
    collect(test.getUiAutomation().getRootInActiveWindow(), text);
    for (Iterator<String> keys = credentials.keys(); keys.hasNext(); ) {
      String value = credentials.optString(keys.next(), "");
      if (value.length() >= 12)
        check(
            !text.toString().contains(value), "credential absent from visible accessibility tree");
    }
  }

  private void stop(ComputerRepository repo) throws Exception {
    main(() -> context.stopService(new Intent(context, UsbIpService.class)));
    for (int i = 0; i < 300 && (UsbIpService.running || repo.isBusy()); i++) Thread.sleep(50);
    check(
        !UsbIpService.running && !repo.isBusy(), "foreground service and repository lease stopped");
  }

  private void launch() throws Exception {
    test.startActivitySync(
        new Intent()
            .setClassName(APP, "org.cgutman.usbip.config.UsbIpConfig")
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_CLEAR_TOP));
    test.waitForIdleSync();
  }

  private void screenshot(File directory, String name) throws Exception {
    Bitmap bitmap = test.getUiAutomation().takeScreenshot();
    check(bitmap != null, "diagnostic screenshot available");
    try (OutputStream out = new FileOutputStream(new File(directory, name + ".png"))) {
      check(bitmap.compress(Bitmap.CompressFormat.PNG, 100, out), "screenshot saved");
    } finally {
      bitmap.recycle();
    }
  }

  private void networkRetry(ComputerRepository repo, File proof, String name,
      JSONObject credentials, byte[] original) throws Exception {
    long deadline = SystemClock.elapsedRealtime() + 65000;
    while (!ConnectionObservation.forProfile(repo.snapshot().connectionKey())
                .phase.equals("reconnecting")) {
      if (SystemClock.elapsedRealtime() >= deadline)
        throw new AssertionError("actual network failure did not reach retry");
      Thread.sleep(100);
    }
    check(UsbIpService.running && repo.isBusy(), "network retry retains service and destination lease");
    check(!ConnectionObservation.forProfile(repo.snapshot().connectionKey()).receiverVerified,
        "unreachable peer never verifies receiver");
    click("Проверить снова");
    // No USB is attached: its separate instruction remains the summary. The
    // computer section must still describe the actual ongoing retry truthfully.
    summary("Подключите USB-устройство к телефону или планшету.");
    Activity activity = resumed.get();
    StringBuilder visible = new StringBuilder();
    main(() -> collectViews(activity.getWindow().getDecorView(), visible));
    check(visible.toString().contains("Связь восстанавливается автоматически."),
        "diagnosis describes ongoing retry rather than terminal failure");
    for (String absent : new String[] {"Защищённое соединение с выбранным компьютером установлено.",
        "Приёмник и стек USB/IP проверены при подключении.", "низкая скорость", "брандмауэр"})
      check(!visible.toString().contains(absent), "no unobserved success or guessed network cause");
    noSecrets(credentials);
    screenshot(proof, name);
    main(activity::recreate);
    for (int n = 0; n < 100 && resumed.get() == activity; n++) Thread.sleep(50);
    check(resumed.get() != null && resumed.get() != activity, "retry diagnosis survives recreation");
    check(UsbIpService.running && repo.isBusy(), "recreation preserves ongoing connection attempt");
    launch();
    click("Остановить мост");
    for (int n = 0; n < 300 && (UsbIpService.running || repo.isBusy()); n++) Thread.sleep(50);
    check(!UsbIpService.running && !repo.isBusy(), "explicit stop cancels retry and releases lease");
    Thread.sleep(4500);
    check(!UsbIpService.running && !repo.isBusy(), "cancelled retry cannot restart service");
    byte[] after = repo.edit(repo.snapshot().activeId, repo.snapshot().revision).get(30, TimeUnit.SECONDS);
    try { check(Arrays.equals(original, after), "network fault preserves exact saved profile and secret"); }
    finally { Arrays.fill(after, (byte) 0); }
    click("Настройки");
    click("Проверить подключение");
    visible.setLength(0);
    main(() -> collectViews(resumed.get().getWindow().getDecorView(), visible));
    check(visible.toString().contains("Соединение с компьютером сейчас не подтверждено."),
        "stopped diagnosis clears stale retry state");
  }

  private void collectViews(View view, StringBuilder text) {
    if (!view.isShown()) return;
    if (view instanceof TextView) text.append(((TextView) view).getText()).append('\n');
    if (view instanceof ViewGroup) {
      ViewGroup group = (ViewGroup) view;
      for (int i = 0; i < group.getChildCount(); i++) collectViews(group.getChildAt(i), text);
    }
  }

  private void upgradePhase(String label) {
    Bundle status = new Bundle();
    status.putString("stream", "PASS " + label + "\n");
    test.sendStatus(0, status);
  }

  private Application.ActivityLifecycleCallbacks callbacks() {
    return new Application.ActivityLifecycleCallbacks() {
      public void onActivityResumed(Activity a) { resumed.set(a); }
      public void onActivityDestroyed(Activity a) { resumed.compareAndSet(a, null); }
      public void onActivityCreated(Activity a, Bundle b) {}
      public void onActivityStarted(Activity a) {}
      public void onActivityPaused(Activity a) {}
      public void onActivityStopped(Activity a) {}
      public void onActivitySaveInstanceState(Activity a, Bundle b) {}
    };
  }

  private KeyStore keys() throws Exception {
    KeyStore store = KeyStore.getInstance("AndroidKeyStore");
    store.load(null);
    return store;
  }

  private void sameValues(JSONObject expected, JSONObject actual) throws Exception {
    for (Iterator<String> it = expected.keys(); it.hasNext();) {
      String name = it.next();
      Object value = expected.get(name);
      check(actual.has(name), "saved field retained");
      if (value instanceof JSONObject)
        sameValues((JSONObject) value, actual.getJSONObject(name));
      else check(value.equals(actual.get(name)), "exact saved value retained");
    }
    for (Iterator<String> it = actual.keys(); it.hasNext();) {
      String name = it.next();
      check(expected.has(name) || actual.isNull(name), "no foreign non-null credential or destination");
    }
  }

  private AccessibilityNodeInfo actionNode(AccessibilityNodeInfo node, String text) {
    if (node == null) return null;
    if (node.isClickable() && (text.equalsIgnoreCase(node.getText() == null ? "" : node.getText().toString())
        || text.contentEquals(node.getContentDescription() == null ? "" : node.getContentDescription())))
      return node;
    for (int i = 0; i < node.getChildCount(); i++) {
      AccessibilityNodeInfo result = actionNode(node.getChild(i), text);
      if (result != null) return result;
    }
    return null;
  }

  private void accessibleClick(String text) throws Exception {
    for (int i = 0; i < 100; i++) {
      AccessibilityNodeInfo node = actionNode(test.getUiAutomation().getRootInActiveWindow(), text);
      if (node != null && node.isEnabled()) {
        check(node.performAction(AccessibilityNodeInfo.ACTION_CLICK), "accessible action " + text);
        Thread.sleep(250);
        return;
      }
      Thread.sleep(100);
    }
    throw new AssertionError("missing accessible action " + text);
  }

  /** Two live owned receivers, actual catalog/service/UI; deliberately no selected USB. */
  int runLiveProfiles(String stage) throws Exception {
    check(stage.equals("first") || stage.equals("resume"), "known profiles stage");
    check(shell("getprop ro.kernel.qemu").equals("1"), "owned emulator only");
    File input = new File(context.getExternalFilesDir(null), "live-profiles-fixture.json");
    check(input.isFile() && input.length() > 0 && input.length() < 32768, "bounded profiles input");
    byte[] raw = Files.readAllBytes(input.toPath());
    JSONObject config;
    try { config = new JSONObject(new String(raw, StandardCharsets.UTF_8)); }
    finally { Arrays.fill(raw, (byte) 0); }
    check(input.delete() && config.getBoolean("owned_emulator_only"), "owned fixture consumed");
    String marker = config.getString("marker");
    check(marker.matches("[a-f0-9]{32}"), "bounded host barrier identity");
    JSONArray items = config.getJSONArray("computers");
    check(items.length() == 2, "two independent computers");
    File proof = new File(context.getExternalFilesDir(null), "live-profiles-" + marker);
    check(proof.isDirectory() || proof.mkdir(), "owned proof directory");
    ComputerRepository repo = ComputerRepository.get(context);
    repo.ready().get(30, TimeUnit.SECONDS);
    check(!UsbIpService.running && !repo.isBusy(), "fresh idle process");
    String[] ids = new String[2];
    byte[][] saved = new byte[2][];
    if (stage.equals("first")) {
      check(repo.snapshot().computers.isEmpty(), "fresh cleared catalog");
      check(context.getSharedPreferences("bridge", 0).edit().putBoolean("welcome_seen", true)
          .putBoolean("notifications_requested", true).commit(), "onboarding already covered");
      for (int i = 1; i >= 0; i--) {
        JSONObject item = items.getJSONObject(i);
        check(item.getJSONObject("profile").getJSONObject("endpoint").getString("host").equals("10.0.2.2"),
            "owned guest host forward");
        byte[] command = new JSONObject().put("op", "add").put("revision", repo.snapshot().revision)
            .put("name", item.getString("name")).put("activate", true)
            .put("connection", new JSONObject().put("kind", "secure")
                .put("profile", item.getJSONObject("profile")).put("credentials", item.getJSONObject("credentials")))
            .toString().getBytes(StandardCharsets.UTF_8);
        try { repo.change(command).get(30, TimeUnit.SECONDS); }
        finally { Arrays.fill(command, (byte) 0); }
        ids[i] = repo.snapshot().activeId;
      }
    } else {
      JSONObject prior = new JSONObject(new String(Files.readAllBytes(new File(proof, "first.json").toPath()), StandardCharsets.UTF_8));
      check(prior.getInt("pid") != android.os.Process.myPid(), "actual process restart");
      for (int i = 0; i < 2; i++) ids[i] = prior.getJSONArray("ids").getString(i);
    }
    check(repo.snapshot().computers.size() == 2 && !ids[0].equals(ids[1])
        && ids[0].equals(repo.snapshot().activeId), "two durable independent identities");
    for (int i = 0; i < 2; i++) {
      saved[i] = repo.edit(ids[i], repo.snapshot().revision).get(30, TimeUnit.SECONDS);
      JSONObject connection = new JSONObject(new String(saved[i], StandardCharsets.UTF_8)).getJSONObject("connection");
      sameValues(items.getJSONObject(i).getJSONObject("profile"), connection.getJSONObject("profile"));
      sameValues(items.getJSONObject(i).getJSONObject("credentials"), connection.getJSONObject("credentials"));
    }
    if (Build.VERSION.SDK_INT >= 33) shell("pm grant " + APP + " android.permission.POST_NOTIFICATIONS");
    Application app = (Application) context.getApplicationContext();
    Application.ActivityLifecycleCallbacks observer = callbacks();
    main(() -> app.registerActivityLifecycleCallbacks(observer));
    try {
      for (int round = 0; round < 3; round++) {
        int index = round % 2;
        check(repo.snapshot().activeId.equals(ids[index]), "expected UI-selected identity");
        launch(); click("Запустить мост");
        long until = SystemClock.elapsedRealtime() + 120000;
        ConnectionObservation.State state;
        do {
          state = ConnectionObservation.forProfile(repo.snapshot().connectionKey());
          check(state.failure == null, "live profile has no terminal failure");
          if (state.phase.equals("connected")) break;
          Thread.sleep(100);
        } while (SystemClock.elapsedRealtime() < until);
        check(state.active && state.phase.equals("connected") && repo.isBusy() && UsbIpService.running,
            "actual authenticated foreground session");
        check(state.receiverVerified == items.getJSONObject(index).getJSONObject("profile").getString("transport").equals("ssh"),
            "preflight belongs to selected transport");
        until = SystemClock.elapsedRealtime() + 10000;
        boolean displayed = false;
        while (!displayed && SystemClock.elapsedRealtime() < until) {
          StringBuilder visible = new StringBuilder();
          main(() -> collectViews(resumed.get().getWindow().getDecorView(), visible));
          displayed = visible.toString().contains("Защищённое подключение");
          if (!displayed) Thread.sleep(100);
        }
        check(displayed, "main screen displays actual authenticated state");
        for (int i = 0; i < 2; i++) noSecrets(items.getJSONObject(i).getJSONObject("credentials"));
        screenshot(proof, stage + "-connected-" + round);
        File ack = new File(proof.getParentFile(), "profiles-" + marker + "-" + stage + "-" + round + ".go");
        check(!ack.exists(), "fresh independent receiver barrier");
        upgradePhase("PROFILES_CONNECTED stage=" + stage + " round=" + round);
        until = SystemClock.elapsedRealtime() + 60000;
        while (!ack.exists() && SystemClock.elapsedRealtime() < until) {
          state = ConnectionObservation.forProfile(repo.snapshot().connectionKey());
          check(state.active && state.phase.equals("connected") && state.failure == null, "connection survives observation");
          Thread.sleep(100);
        }
        check(ack.isFile() && ack.length() == 1, "both receivers independently observed");
        try (InputStream stream = new FileInputStream(ack)) {
          check(stream.read() == 1 && stream.read() == -1, "exact host acknowledgement");
        }
        check(ack.delete(), "owned acknowledgement consumed");
        if (round < 2) {
          click("Изменить компьютер");
          String next = items.getJSONObject(1 - index).getString("name");
          accessibleClick(next); accessibleClick("Отмена");
          check(repo.snapshot().activeId.equals(ids[index]) && repo.isBusy() && UsbIpService.running,
              "cancel switch preserves live profile");
          accessibleClick(next); accessibleClick("Продолжить");
          until = SystemClock.elapsedRealtime() + 30000;
          while ((!repo.snapshot().activeId.equals(ids[1 - index]) || repo.isBusy() || UsbIpService.running)
              && SystemClock.elapsedRealtime() < until) Thread.sleep(100);
          check(repo.snapshot().activeId.equals(ids[1 - index]) && !repo.isBusy() && !UsbIpService.running,
              "UI switch waits for session teardown");
          check(!context.getSharedPreferences("bridge", 0).contains("computer_resume"), "switch does not auto-start");
        } else { click("Остановить мост"); stop(repo); }
        for (int i = 0; i < 2; i++) {
          byte[] after = repo.edit(ids[i], repo.snapshot().revision).get(30, TimeUnit.SECONDS);
          try { check(Arrays.equals(saved[i], after), "each profile preserves exact independent credentials"); }
          finally { Arrays.fill(after, (byte) 0); }
        }
      }
      Files.write(new File(proof, stage + ".json").toPath(), new JSONObject().put("status", "passed")
          .put("pid", android.os.Process.myPid()).put("ids", new JSONArray(Arrays.asList(ids)))
          .put("checks", checks).put("rounds", 3).toString().getBytes(StandardCharsets.UTF_8));
      return checks;
    } finally {
      for (byte[] value : saved) if (value != null) Arrays.fill(value, (byte) 0);
      stop(repo);
      main(() -> app.unregisterActivityLifecycleCallbacks(observer));
    }
  }

  /** Real v1 AES-GCM fixture, actual migration and service; called before any Activity. */
  int runMigration(String stage) throws Exception {
    check(Arrays.asList("seed", "connect", "resume").contains(stage), "known migration stage");
    check(shell("getprop ro.kernel.qemu").equals("1"), "owned emulator only");
    check(!UsbIpService.running, "fresh process has no service");
    File fixture = new File(context.getExternalFilesDir(null), "migration-fixture.json");
    check(fixture.isFile() && fixture.length() > 0 && fixture.length() < 16384,
        "bounded private migration input");
    byte[] raw = Files.readAllBytes(fixture.toPath());
    JSONObject config;
    try { config = new JSONObject(new String(raw, StandardCharsets.UTF_8)); }
    finally { Arrays.fill(raw, (byte) 0); }
    check(fixture.delete(), "consumed fixture removed");
    check(config.getBoolean("owned_emulator_only"), "explicit owned fixture");
    String marker = config.getString("marker");
    check(marker.matches("[a-f0-9]{32}"), "bounded migration identity");
    JSONObject profile = config.getJSONObject("profile"), credentials = config.getJSONObject("credentials");
    check(profile.getJSONObject("endpoint").getString("host").equals("10.0.2.2"), "owned guest target");
    boolean ssh = profile.getString("transport").equals("ssh");
    check(ssh || profile.getString("transport").equals("direct"), "protected transport");
    File proof = new File(context.getExternalFilesDir(null), "migration-" + marker);
    check(proof.isDirectory() || proof.mkdir(), "private proof directory");
    File old = new File(context.getNoBackupFilesDir(), "connection.enc");
    File catalog = new File(context.getNoBackupFilesDir(), "computers.enc");
    SharedPreferences prefs = context.getSharedPreferences("bridge", 0);
    if (stage.equals("seed")) {
      check(!old.exists() && !catalog.exists(), "fresh cleared app files");
      check(!keys().containsAlias("usb-most.connection.v1")
          && !keys().containsAlias("usb-most.computers.v2"), "fresh Keystore");
      check(prefs.edit().clear().putBoolean("secure_connection", true)
          .putString("secure_profile", profile.toString())
          .putString("connection_client_id", config.getString("legacy_client_id"))
          .putBoolean("welcome_seen", true).putBoolean("notifications_requested", true).commit(),
          "exact legacy preferences committed");
      KeyGenerator generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore");
      generator.init(new KeyGenParameterSpec.Builder("usb-most.connection.v1",
          KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
          .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
          .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE).setKeySize(256).build());
      Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
      cipher.init(Cipher.ENCRYPT_MODE, generator.generateKey());
      byte[] plain = new JSONObject().put("profile", profile.toString()).put("credentials", credentials)
          .toString().getBytes(StandardCharsets.UTF_8);
      try (FileOutputStream output = new FileOutputStream(old)) {
        check(cipher.getIV().length == 12, "legacy GCM nonce length");
        output.write(1); output.write(12); output.write(cipher.getIV()); output.write(cipher.doFinal(plain));
        output.getFD().sync();
      } finally { Arrays.fill(plain, (byte) 0); }
      check(old.isFile() && !catalog.exists() && keys().containsAlias("usb-most.connection.v1")
          && !keys().containsAlias("usb-most.computers.v2"), "v1 persists without eager migration");
      Files.write(new File(proof, "seed.json").toPath(), new JSONObject().put("stage", stage)
          .put("status", "passed").put("pid", android.os.Process.myPid())
          .put("checks", checks).toString().getBytes(StandardCharsets.UTF_8));
      return checks;
    }
    JSONObject seeded = new JSONObject(new String(Files.readAllBytes(new File(proof, "seed.json").toPath()), StandardCharsets.UTF_8));
    check(seeded.getInt("pid") != android.os.Process.myPid(), "new process after v1 seed");
    if (stage.equals("connect"))
      check(old.isFile() && !catalog.exists() && keys().containsAlias("usb-most.connection.v1"),
          "legacy files survive replacement APK installation");
    else check(!old.exists() && catalog.isFile(), "v2 survives process death");
    ComputerRepository repo = ComputerRepository.get(context);
    repo.ready().get(30, TimeUnit.SECONDS);
    check(repo.snapshot().status == ComputerRepository.Status.READY
        && repo.snapshot().computers.size() == 1 && repo.snapshot().active() != null,
        "one active migrated computer");
    check(repo.snapshot().active().name.equals("Мой компьютер"), "migration retains familiar default name");
    for (String suffix : new String[] {"", ".bak", ".new"})
      check(!new File(old.getPath() + suffix).exists(), "legacy ciphertext retired");
    check(!keys().containsAlias("usb-most.connection.v1")
        && keys().containsAlias("usb-most.computers.v2"), "old key retired only after durable catalog");
    for (String name : new String[] {"secure_connection", "secure_profile", "connection_client_id"})
      check(!prefs.contains(name), "legacy preference retired");
    String id = repo.snapshot().activeId;
    if (stage.equals("resume")) {
      JSONObject previous = new JSONObject(new String(Files.readAllBytes(new File(proof, "connect.json").toPath()), StandardCharsets.UTF_8));
      check(previous.getString("computer_id").equals(id), "restart does not duplicate migrated profile");
      check(previous.getInt("pid") != android.os.Process.myPid(), "actual process restart after connection");
    }
    byte[] original = repo.edit(id, repo.snapshot().revision).get(30, TimeUnit.SECONDS);
    JSONObject saved = new JSONObject(new String(original, StandardCharsets.UTF_8)).getJSONObject("connection");
    sameValues(profile, saved.getJSONObject("profile"));
    sameValues(credentials, saved.getJSONObject("credentials"));
    if (Build.VERSION.SDK_INT >= 33) shell("pm grant " + APP + " android.permission.POST_NOTIFICATIONS");
    Application app = (Application) context.getApplicationContext();
    Application.ActivityLifecycleCallbacks observer = callbacks();
    main(() -> app.registerActivityLifecycleCallbacks(observer));
    int rounds = stage.equals("connect") ? 2 : 1;
    try {
      for (int round = 0; round < rounds; round++) {
        launch();
        click("Запустить мост");
        long until = SystemClock.elapsedRealtime() + 120000;
        ConnectionObservation.State state;
        do {
          state = ConnectionObservation.forProfile(repo.snapshot().connectionKey());
          check(state.failure == null, "migration connection has no terminal failure");
          if (state.phase.equals("connected")) break;
          Thread.sleep(100);
        } while (SystemClock.elapsedRealtime() < until);
        check(state.active && state.phase.equals("connected") && UsbIpService.running && repo.isBusy(),
            "migrated saved profile starts actual protected service");
        check(state.receiverVerified == ssh, "receiver preflight accurately reflects transport");
        click("Настройки"); click("Проверить подключение");
        noSecrets(credentials);
        screenshot(proof, stage + "-connected-" + round);
        File release = new File(proof.getParentFile(), "migration-" + marker + "-" + stage + "-" + round + ".go");
        check(!release.exists(), "fresh host observation barrier");
        upgradePhase("MIGRATION_CONNECTED stage=" + stage + " round=" + round);
        until = SystemClock.elapsedRealtime() + 60000;
        while (!release.exists() && SystemClock.elapsedRealtime() < until) {
          state = ConnectionObservation.forProfile(repo.snapshot().connectionKey());
          check(state.active && state.phase.equals("connected") && state.failure == null
              && UsbIpService.running && repo.isBusy(), "actual session survives independent host observation");
          Thread.sleep(100);
        }
        check(release.isFile() && release.length() == 1, "host observed authenticated guest session");
        try (InputStream input = new FileInputStream(release)) {
          check(input.read() == 1 && input.read() == -1, "exact observation acknowledgement");
        }
        check(release.delete(), "exact owned marker consumed");
        launch(); click("Остановить мост");
        for (int n = 0; n < 300 && (UsbIpService.running || repo.isBusy()); n++) Thread.sleep(50);
        check(!UsbIpService.running && !repo.isBusy(), "UI stop releases migrated service");
        check(repo.snapshot().activeId.equals(id) && repo.snapshot().computers.size() == 1,
            "start stop does not duplicate or switch migrated computer");
        byte[] after = repo.edit(id, repo.snapshot().revision).get(30, TimeUnit.SECONDS);
        try { check(Arrays.equals(original, after), "exact saved identity and credentials unchanged"); }
        finally { Arrays.fill(after, (byte) 0); }
      }
      Files.write(new File(proof, stage + ".json").toPath(), new JSONObject().put("stage", stage)
          .put("status", "passed").put("pid", android.os.Process.myPid()).put("computer_id", id)
          .put("rounds", rounds).put("transport", ssh ? "ssh" : "direct").put("checks", checks)
          .toString().getBytes(StandardCharsets.UTF_8));
      return checks;
    } finally {
      Arrays.fill(original, (byte) 0);
      stop(repo);
      main(() -> app.unregisterActivityLifecycleCallbacks(observer));
    }
  }

  private void upgrade(ComputerRepository repo, JSONObject config, File proof,
      JSONObject credentials, byte[] original) throws Exception {
    String marker = config.getString("upgrade_marker");
    check(marker.matches("[a-f0-9]{32}"), "bounded owned upgrade marker");
    for (int round = 0; round < 2; round++) {
      long until = SystemClock.elapsedRealtime() + 180000;
      ConnectionObservation.State state;
      do {
        state = ConnectionObservation.forProfile(repo.snapshot().connectionKey());
        check(state.failure == null, "upgrade has no terminal failure");
        if (state.phase.equals("connected")) break;
        Thread.sleep(100);
      } while (SystemClock.elapsedRealtime() < until);
      check(state.active && state.phase.equals("connected") && state.receiverVerified
          && UsbIpService.running && repo.isBusy(), "actual service verifies upgraded receiver");
      click("Проверить снова");
      StringBuilder visible = new StringBuilder();
      main(() -> collectViews(resumed.get().getWindow().getDecorView(), visible));
      check(visible.toString().contains("Приёмник и стек USB/IP проверены при подключении."),
          "diagnosis shows actual receiver verification");
      noSecrets(credentials);
      screenshot(proof, "receiver-upgrade-" + round);
      File release = new File(proof.getParentFile(), "upgrade-" + marker + "-" + round + ".go");
      check(!release.exists(), "fresh host upgrade verification marker");
      upgradePhase("UPGRADE_CONNECTED round=" + round);
      until = SystemClock.elapsedRealtime() + 90000;
      while (!release.exists() && SystemClock.elapsedRealtime() < until) {
        state = ConnectionObservation.forProfile(repo.snapshot().connectionKey());
        check(state.active && state.receiverVerified && state.phase.equals("connected")
            && state.failure == null && UsbIpService.running && repo.isBusy(),
            "service remains connected during host verification");
        Thread.sleep(100);
      }
      check(release.isFile() && release.length() == 1, "bounded host verification completed");
      try (FileInputStream input = new FileInputStream(release)) {
        check(input.read() == 1 && input.read() == -1, "exact upgrade marker payload");
      }
      check(release.delete(), "owned upgrade marker consumed");
      launch();
      click("Остановить мост");
      for (int n = 0; n < 300 && (UsbIpService.running || repo.isBusy()); n++) Thread.sleep(50);
      check(!UsbIpService.running && !repo.isBusy(), "UI stop releases upgraded service");
      byte[] saved = repo.edit(repo.snapshot().activeId, repo.snapshot().revision).get(30, TimeUnit.SECONDS);
      try { check(Arrays.equals(original, saved), "upgrade preserves exact saved profile and secrets"); }
      finally { Arrays.fill(saved, (byte) 0); }
      if (round == 0) {
        click("Запустить мост");
        click("Настройки");
        click("Проверить подключение");
      }
    }
    upgradePhase("UPGRADE_FINISHED");
  }

  int run() throws Exception {
    check(shell("getprop ro.kernel.qemu").equals("1"), "owned emulator only");
    File fixture = new File(context.getExternalFilesDir(null), "diagnosis-fixture.json");
    check(
        fixture.isFile() && fixture.length() > 0 && fixture.length() <= 65536,
        "bounded owned fixture");
    byte[] encoded = Files.readAllBytes(fixture.toPath());
    JSONObject config;
    try {
      config = new JSONObject(new String(encoded, StandardCharsets.UTF_8));
    } finally {
      Arrays.fill(encoded, (byte) 0);
    }
    check(config.getBoolean("owned_emulator_only"), "explicit fixture ownership");
    check(fixture.delete(), "remove consumed fixture secrets");
    JSONArray cases = config.getJSONArray("cases");
    boolean networkOnly = config.optBoolean("network_only", false);
    boolean preparationOnly = config.optBoolean("preparation_only", false);
    boolean upgradeOnly = config.optBoolean("upgrade_only", false);
    check((networkOnly ? 1 : 0) + (preparationOnly ? 1 : 0) + (upgradeOnly ? 1 : 0) <= 1,
        "one diagnostic fixture mode");
    check(preparationOnly ? cases.length() >= 1 && cases.length() <= 2
                         : cases.length() == (upgradeOnly ? 1 : networkOnly ? 2 : 6),
        "complete owned diagnostic fixture");
    File proof = new File(context.getExternalFilesDir(null),
        upgradeOnly ? "diagnosis-upgrade-proof" : networkOnly ? "diagnosis-network-proof" :
            preparationOnly ? "diagnosis-preparation-proof" : "diagnosis-proof");
    check(proof.isDirectory() || proof.mkdir(), "proof directory");
    if (Build.VERSION.SDK_INT >= 33)
      shell("pm grant " + APP + " android.permission.POST_NOTIFICATIONS");
    ComputerRepository repo = ComputerRepository.get(context);
    repo.ready().get(30, TimeUnit.SECONDS);
    stop(repo);
    repo.reset().get(30, TimeUnit.SECONDS);
    check(
        context.getSharedPreferences("bridge", 0).edit().putBoolean("welcome_seen", true).commit(),
        "fixture skips already-covered onboarding");
    Application application = (Application) context.getApplicationContext();
    Application.ActivityLifecycleCallbacks callbacks = callbacks();
    main(() -> application.registerActivityLifecycleCallbacks(callbacks));
    JSONArray results = new JSONArray();
    try {
      for (int index = 0; index < cases.length(); index++) {
        JSONObject item = cases.getJSONObject(index);
        String name = item.getString("name"), expected = upgradeOnly ? "" : item.getString("failure");
        check(name.matches("[a-z0-9-]{1,48}"), "public case name");
        JSONObject profile = item.getJSONObject("profile"),
            credentials = item.getJSONObject("credentials");
        check(
            profile.getJSONObject("endpoint").getString("host").equals("10.0.2.2"),
            "only host-owned fixture endpoints");
        byte[] command =
            new JSONObject()
                .put("op", "add")
                .put("revision", repo.snapshot().revision)
                .put("name", name)
                .put("activate", true)
                .put(
                    "connection",
                    new JSONObject()
                        .put("kind", "secure")
                        .put("profile", profile)
                        .put("credentials", credentials))
                .toString()
                .getBytes(StandardCharsets.UTF_8);
        try {
          repo.change(command).get(30, TimeUnit.SECONDS);
        } finally {
          Arrays.fill(command, (byte) 0);
        }
        ComputerRepository.Snapshot before = repo.snapshot();
        byte[] original = repo.edit(before.activeId, before.revision).get(30, TimeUnit.SECONDS);
        try {
          launch();
          click("Настройки");
          click("Проверить подключение");
          // Observe the real screen before connecting: the previous computer's
          // failure must not leak into this newly selected profile.
          summary("Подключите USB-устройство к телефону или планшету.");
          launch();
          click("Запустить мост");
          click("Настройки");
          click("Проверить подключение");
          if (upgradeOnly) {
            upgrade(repo, config, proof, credentials, original);
            results.put(new JSONObject().put("case", name).put("status", "passed")
                .put("receiver_verified", true).put("rounds", 2));
            continue;
          }
          if (networkOnly) {
            networkRetry(repo, proof, name, credentials, original);
            results.put(new JSONObject().put("case", name).put("status", "passed")
                .put("expected_failure", expected));
            continue;
          }
          String message = item.getString("summary");
          // Diagnosis is a read-only snapshot, so refresh until the real
          // transport has delivered its terminal error to the service.
          for (int n = 0; n < 450; n++) {
            if (UsbIpService.status.startsWith("Не удалось подключиться:")) break;
            Thread.sleep(100);
          }
          click("Проверить снова");
          summary(message);
          for (int n = 0; n < 200 && (UsbIpService.running || repo.isBusy()); n++) Thread.sleep(50);
          check(!UsbIpService.running && !repo.isBusy(), "terminal error releases actual service");
          ConnectionObservation.State observation =
              ConnectionObservation.forProfile(repo.snapshot().connectionKey());
          check(expected.equals(observation.failure), "exact observed terminal failure code");
          check(!observation.active && !observation.receiverVerified,
              "failed preparation or authentication never verifies receiver");
          noSecrets(credentials);
          screenshot(proof, name);
          Activity previous = resumed.get();
          main(previous::recreate);
          for (int n = 0; n < 100 && resumed.get() == previous; n++) Thread.sleep(50);
          check(
              resumed.get() != previous && resumed.get() != null,
              "actual diagnostic Activity recreation");
          summary(message);
          click("Проверить снова");
          summary(message);
          Thread.sleep(3500);
          check(
              !UsbIpService.running && !repo.isBusy(),
              "terminal failure has no auto-retry");
          check(
              repo.snapshot().activeId.equals(before.activeId),
              "diagnosis preserves selected computer");
          byte[] after =
              repo.edit(before.activeId, repo.snapshot().revision).get(30, TimeUnit.SECONDS);
          try {
            check(
                Arrays.equals(original, after),
                "failure and refresh preserve saved pin and credentials");
          } finally {
            Arrays.fill(after, (byte) 0);
          }
          results.put(
              new JSONObject()
                  .put("case", name)
                  .put("status", "passed")
                  .put("expected_failure", expected)
                  .put("observed_failure", observation.failure));
          Bundle status = new Bundle();
          status.putString("stream", "PASS diagnosis " + name + "\n");
          test.sendStatus(0, status);
        } finally {
          Arrays.fill(original, (byte) 0);
        }
      }
      Files.write(
          new File(proof, "result.json").toPath(),
          new JSONObject()
              .put("status", "passed")
              .put("api", Build.VERSION.SDK_INT)
              .put("checks", checks)
              .put("cases", results)
              .toString(2)
              .getBytes(StandardCharsets.UTF_8));
      return checks;
    } finally {
      stop(repo);
      repo.reset().get(30, TimeUnit.SECONDS);
      main(() -> application.unregisterActivityLifecycleCallbacks(callbacks));
    }
  }
}
