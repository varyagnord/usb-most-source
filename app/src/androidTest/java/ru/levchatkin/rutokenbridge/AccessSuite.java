package ru.levchatkin.rutokenbridge;

import android.app.*;
import android.content.*;
import android.graphics.*;
import android.os.*;
import android.view.*;
import android.view.accessibility.AccessibilityNodeInfo;
import android.widget.*;
import java.io.*;
import java.net.*;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.security.MessageDigest;
import java.util.*;
import java.util.concurrent.*;
import java.util.concurrent.atomic.*;
import org.cgutman.usbip.connection.ComputerRepository;
import org.cgutman.usbip.service.UsbIpService;
import org.json.*;

/** Saved identity and local deletion through the optimized UI, on owned emulators only. */
final class AccessSuite {
  private static final String APP = "ru.levchatkin.rutokenbridge";
  private static final String NAME = "Рабочий компьютер";
  private final Instrumentation test;
  private final Context context;
  private final AtomicReference<Activity> resumed = new AtomicReference<>();
  private int checks;
  private ComputerRepository repo;

  AccessSuite(Instrumentation test) {
    this.test = test;
    context = test.getTargetContext();
  }

  private void check(boolean ok, String label) {
    if (!ok) throw new AssertionError(label);
    checks++;
  }

  private void main(Runnable action) {
    Throwable[] error = {null};
    test.runOnMainSync(() -> {
      try {
        action.run();
      } catch (Throwable e) {
        error[0] = e;
      }
    });
    if (error[0] != null) throw new AssertionError("access UI", error[0]);
    test.waitForIdleSync();
  }

  private String shell(String command) throws Exception {
    try (InputStream input =
        new ParcelFileDescriptor.AutoCloseInputStream(
            test.getUiAutomation().executeShellCommand(command))) {
      ByteArrayOutputStream output = new ByteArrayOutputStream();
      byte[] data = new byte[1024];
      int count;
      while ((count = input.read(data)) != -1) output.write(data, 0, count);
      return output.toString("UTF-8").trim();
    }
  }

  private void owned() throws Exception {
    check(shell("getprop ro.kernel.qemu").equals("1"), "owned emulator only");
    repo = ComputerRepository.get(context);
    repo.ready().get(30, TimeUnit.SECONDS);
  }

  private View find(View v, String text) {
    if (!v.isShown()) return null;
    if (v instanceof Button && ((Button) v).getText().toString().equals(text)) return v;
    if (v instanceof ViewGroup)
      for (int i = 0; i < ((ViewGroup) v).getChildCount(); i++) {
        View found = find(((ViewGroup) v).getChildAt(i), text);
        if (found != null) return found;
      }
    return null;
  }

  private void click(String text) throws Exception {
    for (int n = 0; n < 100; n++) {
      boolean[] clicked = {false};
      main(() -> {
        Activity a = resumed.get();
        View v = a == null ? null : find(a.getWindow().getDecorView(), text);
        if (v != null && v.isEnabled()) {
          v.requestRectangleOnScreen(new Rect(0, 0, v.getWidth(), v.getHeight()), true);
          if (!v.getGlobalVisibleRect(new Rect())) return;
          check(v.performClick(), "click " + text);
          clicked[0] = true;
        }
      });
      if (clicked[0]) return;
      Thread.sleep(50);
    }
    throw new AssertionError("missing button " + text);
  }

  private AccessibilityNodeInfo node(AccessibilityNodeInfo root, String text) {
    if (root == null) return null;
    if (root.isVisibleToUser()
        && root.isClickable()
        && root.getText() != null
        && root.getText().toString().equalsIgnoreCase(text)) return root;
    for (int i = 0; i < root.getChildCount(); i++) {
      AccessibilityNodeInfo found = node(root.getChild(i), text);
      if (found != null) return found;
    }
    return null;
  }

  private boolean scroll(AccessibilityNodeInfo root) {
    if (root == null) return false;
    if (root.isScrollable() && root.performAction(AccessibilityNodeInfo.ACTION_SCROLL_FORWARD))
      return true;
    for (int i = 0; i < root.getChildCount(); i++) if (scroll(root.getChild(i))) return true;
    return false;
  }

  private void dialog(String text) throws Exception {
    for (int n = 0; n < 60; n++) {
      AccessibilityNodeInfo root = test.getUiAutomation().getRootInActiveWindow(),
          found = node(root, text);
      if (found != null) {
        check(found.performAction(AccessibilityNodeInfo.ACTION_CLICK), "dialog " + text);
        Thread.sleep(120);
        test.waitForIdleSync();
        return;
      }
      scroll(root);
      Thread.sleep(100);
    }
    throw new AssertionError("missing dialog action " + text);
  }

  private void collect(AccessibilityNodeInfo node, StringBuilder text) {
    if (node == null) return;
    if (node.getText() != null) text.append(node.getText()).append('\n');
    for (int i = 0; i < node.getChildCount(); i++) collect(node.getChild(i), text);
  }

  private String visibleText() {
    StringBuilder text = new StringBuilder();
    collect(test.getUiAutomation().getRootInActiveWindow(), text);
    return text.toString();
  }

  private void details(String id) throws Exception {
    for (int n = 0; n < 100; n++) {
      boolean[] opened = {false};
      main(() -> {
        Activity a = resumed.get();
        View select =
            a == null ? null : a.getWindow().getDecorView().findViewWithTag("computer:" + id);
        if (select == null) return;
        ViewGroup row = (ViewGroup) select.getParent();
        View more = row.getChildAt(row.getChildCount() - 1);
        more.requestRectangleOnScreen(new Rect(0, 0, more.getWidth(), more.getHeight()), true);
        Rect bounds = new Rect();
        if (more.isEnabled() && more.getGlobalVisibleRect(bounds)) {
          check(more.performClick(), "open exact record despite equal names and endpoints");
          opened[0] = true;
        }
      });
      if (opened[0]) {
        Thread.sleep(150);
        return;
      }
      Thread.sleep(50);
    }
    throw new AssertionError("computer record is not reachable");
  }

  private void launch() {
    test.startActivitySync(
        new Intent()
            .setClassName(APP, "org.cgutman.usbip.config.UsbIpConfig")
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_CLEAR_TOP));
    test.waitForIdleSync();
  }

  private ComputerRepository.Snapshot change(JSONObject command) throws Exception {
    byte[] value = command.toString().getBytes(StandardCharsets.UTF_8);
    try {
      return repo.change(value).get(30, TimeUnit.SECONDS);
    } finally {
      Arrays.fill(value, (byte) 0);
    }
  }

  private byte[] edit(String id) throws Exception {
    return repo.edit(id, repo.snapshot().revision).get(30, TimeUnit.SECONDS);
  }

  private JSONObject record(String id) throws Exception {
    byte[] data = edit(id);
    try { return new JSONObject(new String(data, StandardCharsets.UTF_8)); }
    finally { Arrays.fill(data, (byte) 0); }
  }

  private EditText field(String tag) throws Exception {
    for (int i = 0; i < 100; i++) {
      View[] value = {null};
      main(() -> {
        Activity a = resumed.get();
        if (a != null) value[0] = a.getWindow().getDecorView().findViewWithTag(tag);
      });
      if (value[0] instanceof EditText && value[0].isShown() && value[0].isEnabled())
        return (EditText) value[0];
      Thread.sleep(50);
    }
    throw new AssertionError("editable field missing: " + tag);
  }

  private void waitForComputers() throws Exception {
    for (int i = 0; i < 100; i++) {
      boolean[] ready = {false};
      main(() -> {
        Activity a = resumed.get();
        ready[0] = a != null && find(a.getWindow().getDecorView(), "Добавить компьютер") != null;
      });
      if (ready[0]) return;
      Thread.sleep(50);
    }
    throw new AssertionError("saved editor did not return to computer list");
  }

  int endpoints(boolean resume) throws Exception {
    owned();
    check(proof("endpoint-state.json").getParentFile().isDirectory()
        || proof("endpoint-state.json").getParentFile().mkdir(), "endpoint proof directory");
    if (resume) {
      JSONObject state = new JSONObject(new String(Files.readAllBytes(proof("endpoint-state.json").toPath()), StandardCharsets.UTF_8));
      check(Objects.equals(repo.snapshot().activeId, state.getString("active")), "active profile survives fresh process");
      JSONArray entries = state.getJSONArray("entries");
      for (int i = 0; i < entries.length(); i++) {
        JSONObject entry = entries.getJSONObject(i);
        byte[] bytes = edit(entry.getString("id"));
        try { check(digest(bytes).equals(entry.getString("digest")), "edited identity and credentials survive process restart"); }
        finally { Arrays.fill(bytes, (byte) 0); }
      }
      check(!repo.isBusy() && !UsbIpService.running, "edited profiles do not auto-start in new process");
      repo.reset().get(30, TimeUnit.SECONDS);
      return checks;
    }
    stop();
    repo.reset().get(30, TimeUnit.SECONDS);
    context.getSharedPreferences("bridge", 0).edit().putBoolean("welcome_seen", true).commit();
    Application app = (Application) context.getApplicationContext();
    Application.ActivityLifecycleCallbacks callbacks = new Application.ActivityLifecycleCallbacks() {
      public void onActivityResumed(Activity a) { resumed.set(a); }
      public void onActivityDestroyed(Activity a) { resumed.compareAndSet(a, null); }
      public void onActivityCreated(Activity a, Bundle b) {}
      public void onActivityStarted(Activity a) {}
      public void onActivityPaused(Activity a) {}
      public void onActivityStopped(Activity a) {}
      public void onActivitySaveInstanceState(Activity a, Bundle b) {}
    };
    main(() -> app.registerActivityLifecycleCallbacks(callbacks));
    try (WaitingPeer peer = new WaitingPeer()) {
      String[] ids = {add(0, peer.listener.getLocalPort()), add(1, peer.listener.getLocalPort()), add(2, peer.listener.getLocalPort())};
      String[] addresses = {"192.0.2.30", "2001:db8::20", "pc-renamed.example"};
      launch(); click("Изменить компьютер");
      for (int i = 0; i < ids.length; i++) {
        final int index = i;
        JSONObject before = record(ids[i]);
        JSONObject connection = before.getJSONObject("connection"), profile = connection.getJSONObject("profile");
        byte[][] neighbors = {edit(ids[0]), edit(ids[1]), edit(ids[2])};
        try {
          details(ids[i]); dialog("Изменить подключение");
          EditText host = field("Адрес компьютера");
          if (i == 2) {
            long revision = repo.snapshot().revision;
            main(() -> host.setText(""));
            click("Сохранить компьютер");
            check(host.getError() != null && repo.snapshot().revision == revision,
                "empty paired address is explained without changing the saved computer");
          }
          main(() -> host.setText(addresses[index]));
          click("Назад"); waitForComputers();
          byte[] cancelled = edit(ids[i]);
          try { check(Arrays.equals(cancelled, neighbors[i]), "cancel address edit preserves exact identity and credentials"); }
          finally { Arrays.fill(cancelled, (byte) 0); }
          details(ids[i]); dialog("Изменить подключение");
          EditText editedHost = field("Адрес компьютера");
          main(() -> editedHost.setText(addresses[index]));
          int expectedPort = profile.getJSONObject("endpoint").getInt("port");
          if (i < 2) {
            expectedPort += i + 1;
            EditText port = field("Порт SSH"); final int value = expectedPort;
            main(() -> port.setText(Integer.toString(value)));
          } else {
            Activity a = resumed.get(); main(a::recreate);
            check(field("Адрес компьютера").getText().toString().equals(addresses[i]), "paired address draft survives recreation");
          }
          click("Сохранить компьютер"); waitForComputers();
          JSONObject after = record(ids[i]).getJSONObject("connection"), next = after.getJSONObject("profile");
          check(next.getJSONObject("endpoint").getString("host").equals(addresses[i]), "edited endpoint host committed");
          check(next.getJSONObject("endpoint").getInt("port") == expectedPort, "endpoint port edited for SSH and retained for TLS");
          String pin = i < 2 ? "host_key" : "server_pin";
          check(!next.isNull(pin) && next.getString(pin).equals(profile.getString(pin)), "endpoint edit preserves confirmed " + pin);
          check(after.getJSONObject("credentials").toString().equals(connection.getJSONObject("credentials").toString()), "endpoint edit preserves only its own credentials");
          check(repo.snapshot().activeId.equals(ids[2]), "inactive edit cannot select another profile");
          for (int j = 0; j < ids.length; j++) if (j != i) {
            byte[] untouched = edit(ids[j]);
            try { check(Arrays.equals(untouched, neighbors[j]), "equal-name/equal-address neighbor remains byte-identical"); }
            finally { Arrays.fill(untouched, (byte) 0); }
          }
          details(ids[i]);
          String shown = i < 2 ? "SHA256:" + b64(251 + i, false) : b64(253, true);
          check(visibleText().contains(shown), "identity dialog retains exact pin after address edit");
          screenshot("endpoint-identity-" + i); dialog("Закрыть");
          check(peer.accepted.get() == 0, "editing endpoints never initiates a connection");
        } finally { for (byte[] bytes : neighbors) Arrays.fill(bytes, (byte) 0); }
      }
      JSONObject oldDirect = record(ids[2]).getJSONObject("connection");
      JSONObject pairing = new JSONObject().put("endpoint", oldDirect.getJSONObject("profile").getJSONObject("endpoint"))
          .put("server_pin", b64(254, true)).put("claim", b64(77, true));
      String code = "usbmost:1:" + android.util.Base64.encodeToString(pairing.toString().getBytes(StandardCharsets.UTF_8), android.util.Base64.URL_SAFE | android.util.Base64.NO_WRAP | android.util.Base64.NO_PADDING);
      details(ids[2]); dialog("Изменить подключение");
      EditText claim = field("Код подключения с компьютера"); main(() -> claim.setText(code));
      main(() -> check(!resumed.get().getWindow().getDecorView()
          .findViewWithTag("Адрес компьютера").isShown(), "new QR is authoritative without a conflicting address field"));
      click("Назад"); waitForComputers();
      check(record(ids[2]).getJSONObject("connection").toString().equals(oldDirect.toString()), "cancel new QR preserves old pin and token");
      details(ids[2]); dialog("Изменить подключение");
      EditText replacement = field("Код подключения с компьютера"); main(() -> replacement.setText(code));
      click("Сохранить компьютер"); waitForComputers();
      JSONObject replaced = record(ids[2]).getJSONObject("connection");
      check(replaced.getJSONObject("profile").getString("server_pin").equals(b64(254, true)), "explicit new code replaces pin even at same address");
      check(replaced.getJSONObject("credentials").isNull("token") && replaced.getJSONObject("credentials").getString("pair_code").equals(code), "new claim retires previous token");
      details(ids[2]);
      check(visibleText().contains(b64(254, true)) && !visibleText().contains(b64(253, true)),
          "same-address replacement displays its new identity only");
      screenshot("endpoint-replaced-identity"); dialog("Закрыть");
      check(peer.accepted.get() == 0 && !repo.isBusy(), "new claim cannot implicitly connect");
      JSONArray entries = new JSONArray();
      for (String id : ids) {
        byte[] bytes = edit(id);
        try { entries.put(new JSONObject().put("id", id).put("digest", digest(bytes))); }
        finally { Arrays.fill(bytes, (byte) 0); }
      }
      Files.write(proof("endpoint-state.json").toPath(), new JSONObject().put("active", ids[2]).put("entries", entries).toString().getBytes(StandardCharsets.UTF_8));
      Files.write(proof("endpoint-result.json").toPath(), new JSONObject().put("status", "passed").put("checks", checks).put("api", Build.VERSION.SDK_INT).put("peer_accepted", peer.accepted.get()).toString(2).getBytes(StandardCharsets.UTF_8));
    } finally {
      stop(); main(() -> app.unregisterActivityLifecycleCallbacks(callbacks));
    }
    return checks;
  }

  private void absent(String id) throws Exception {
    try {
      byte[] unexpected = edit(id);
      Arrays.fill(unexpected, (byte) 0);
      throw new AssertionError("forgotten credentials still accessible");
    } catch (ExecutionException expected) {
      checks++;
    }
    for (ComputerRepository.Computer c : repo.snapshot().computers)
      check(!c.id.equals(id), "forgotten identity and its USB rules absent from catalog");
  }

  private void stop() throws Exception {
    main(() -> context.stopService(new Intent(context, UsbIpService.class)));
    waitStopped();
  }

  private void waitStopped() throws Exception {
    for (int n = 0; n < 300 && (UsbIpService.running || repo.isBusy()); n++) Thread.sleep(50);
    check(
        !UsbIpService.running && !repo.isBusy(),
        "service releases repository before local mutation");
  }

  private static String b64(int value, boolean url) {
    byte[] data = new byte[32];
    Arrays.fill(data, (byte) value);
    return android.util.Base64.encodeToString(
        data,
        android.util.Base64.NO_WRAP
            | android.util.Base64.NO_PADDING
            | (url ? android.util.Base64.URL_SAFE : 0));
  }

  private static String digest(byte[] value) throws Exception {
    return android.util.Base64.encodeToString(
        MessageDigest.getInstance("SHA-256").digest(value), android.util.Base64.NO_WRAP);
  }

  private File proof(String name) {
    return new File(new File(context.getExternalFilesDir(null), "access-proof"), name);
  }

  private void screenshot(String name) throws Exception {
    test.waitForIdleSync();
    // The editor is secure. API 26 can retain its protected SurfaceFlinger layer
    // briefly after returning to the non-secret identity dialog. Wait for the
    // transition; never remove FLAG_SECURE to make the test capture succeed.
    Bitmap bitmap = null;
    for (int attempt = 0; attempt < 20 && bitmap == null; attempt++) {
      bitmap = test.getUiAutomation().takeScreenshot();
      if (bitmap == null) Thread.sleep(150);
    }
    check(bitmap != null, "screenshot available");
    try (OutputStream out = new FileOutputStream(proof(name + ".png"))) {
      check(bitmap.compress(Bitmap.CompressFormat.PNG, 100, out), "screenshot saved");
    } finally {
      bitmap.recycle();
    }
  }

  private String add(int index, int port) throws Exception {
    JSONObject profile =
        new JSONObject()
            .put("transport", index == 2 ? "direct" : "ssh")
            .put("endpoint", new JSONObject().put("host", "127.0.0.1").put("port", port));
    JSONObject credentials = new JSONObject();
    if (index == 2) {
      profile.put("server_pin", b64(253, true));
      credentials.put("token", b64(65, true));
    } else {
      profile
          .put("user", "owned-fixture")
          .put("host_key", b64(251 + index, true))
          .put("reverse_port", 0)
          .put("prepare", false);
      credentials
          .put("password", "SYNTHETIC-ACCESS-SECRET-" + index)
          .put(
              "client_id",
              index == 0 ? "11111111111111111111111111111111" : "22222222222222222222222222222222");
    }
    ComputerRepository.Snapshot result =
        change(
            new JSONObject()
                .put("op", "add")
                .put("revision", repo.snapshot().revision)
                .put("name", NAME)
                .put("activate", true)
                .put(
                    "connection",
                    new JSONObject()
                        .put("kind", "secure")
                        .put("profile", profile)
                        .put("credentials", credentials)));
    String id = result.activeId;
    char[] a = new char[64], b = new char[64];
    Arrays.fill(a, 'a');
    Arrays.fill(b, 'b');
    change(
        new JSONObject()
            .put("op", "set_usb_rule")
            .put("revision", repo.snapshot().revision)
            .put("id", id)
            .put(
                "rule",
                new JSONObject()
                    .put(
                        "identity",
                        new JSONObject()
                            .put("vendor", 4660)
                            .put("product", 22136)
                            .put("serial_hash", new String(a))
                            .put("layout_hash", new String(b)))
                    .put("name", "Устройство " + index)
                    .put("automatic", true)
                    .put("storage", false)));
    return id;
  }

  private static final class WaitingPeer implements AutoCloseable {
    final ServerSocket listener = new ServerSocket(0, 1, InetAddress.getByName("127.0.0.1"));
    final AtomicInteger accepted = new AtomicInteger(), closed = new AtomicInteger();
    volatile boolean stopped;
    volatile Socket client;
    volatile Throwable failure;
    final Thread worker;

    WaitingPeer() throws Exception {
      listener.setSoTimeout(200);
      worker =
          new Thread(
              () -> {
                try {
                  while (!stopped) {
                    try {
                      client = listener.accept();
                    } catch (SocketTimeoutException wait) {
                      continue;
                    }
                    accepted.incrementAndGet();
                    client.setSoTimeout(200);
                    try (Socket owned = client) {
                      byte[] bytes = new byte[1024];
                      while (!stopped) {
                        try {
                          if (owned.getInputStream().read(bytes) == -1) break;
                        } catch (SocketTimeoutException wait) {
                        } catch (SocketException closedByClient) {
                          break;
                        }
                      }
                    } finally {
                      closed.incrementAndGet();
                      client = null;
                    }
                  }
                } catch (Throwable error) {
                  if (!stopped) failure = error;
                }
              },
              "owned-access-peer");
      worker.start();
    }

    public void close() throws Exception {
      stopped = true;
      listener.close();
      Socket socket = client;
      if (socket != null) socket.close();
      worker.join(3000);
      if (worker.isAlive() || failure != null)
        throw new AssertionError("owned peer cleanup", failure);
    }
  }

  int run() throws Exception {
    owned();
    stop();
    repo.reset().get(30, TimeUnit.SECONDS);
    check(
        proof("state.json").getParentFile().isDirectory()
            || proof("state.json").getParentFile().mkdir(),
        "proof directory");
    context.getSharedPreferences("bridge", 0).edit().putBoolean("welcome_seen", true).commit();
    if (Build.VERSION.SDK_INT >= 33)
      shell("pm grant " + APP + " android.permission.POST_NOTIFICATIONS");
    Application app = (Application) context.getApplicationContext();
    Application.ActivityLifecycleCallbacks callbacks =
        new Application.ActivityLifecycleCallbacks() {
          public void onActivityResumed(Activity a) {
            resumed.set(a);
          }

          public void onActivityDestroyed(Activity a) {
            resumed.compareAndSet(a, null);
          }

          public void onActivityCreated(Activity a, Bundle b) {}

          public void onActivityStarted(Activity a) {}

          public void onActivityPaused(Activity a) {}

          public void onActivityStopped(Activity a) {}

          public void onActivitySaveInstanceState(Activity a, Bundle b) {}
        };
    main(() -> app.registerActivityLifecycleCallbacks(callbacks));
    try (WaitingPeer peer = new WaitingPeer()) {
      String[] ids = {
        add(0, peer.listener.getLocalPort()),
        add(1, peer.listener.getLocalPort()),
        add(2, peer.listener.getLocalPort())
      };
      byte[][] originals = {edit(ids[0]), edit(ids[1]), edit(ids[2])};
      String neighborRules = repo.snapshot().computers.get(2).usbRules;
      try {
        launch();
        click("Изменить компьютер");
        for (int index = 0; index < 3; index++) {
          details(ids[index]);
          String expected = index == 2 ? b64(253, true) : "SHA256:" + b64(251 + index, false);
          String text = visibleText();
          check(text.contains(expected), "exact saved pin shown for this record");
          check(
              text.contains("Удаление здесь не меняет настройки доступа на компьютере."),
              "local deletion is distinct from remote revocation");
          check(
              !text.contains("SYNTHETIC-ACCESS-SECRET-") && !text.contains(b64(65, true)),
              "no password or token in identity dialog");
          screenshot("identity-" + index);
          dialog("Копировать отпечаток");
          main(() -> {
            ClipData clip = context.getSystemService(ClipboardManager.class).getPrimaryClip();
            check(
                clip != null
                    && clip.getItemCount() == 1
                    && clip.getItemAt(0).coerceToText(context).toString().equals(expected),
                "actual clipboard matches public fingerprint only");
          });
          dialog("Закрыть");
        }
        check(
            peer.accepted.get() == 0,
            "viewing and copying identities performs no network connection");
        details(ids[0]);
        dialog("Забыть на этом устройстве");
        dialog("Отмена");
        check(
            repo.snapshot().computers.size() == 3 && repo.snapshot().activeId.equals(ids[2]),
            "cancel forgetting preserves catalog and active target");
        byte[] cancelled = edit(ids[0]);
        try {
          check(Arrays.equals(cancelled, originals[0]), "cancel preserves credentials and rules");
        } finally {
          Arrays.fill(cancelled, (byte) 0);
        }
        details(ids[0]);
        dialog("Забыть на этом устройстве");
        dialog("Забыть");
        for (int n = 0; n < 100 && repo.snapshot().computers.size() != 2; n++) Thread.sleep(50);
        absent(ids[0]);
        check(repo.snapshot().activeId.equals(ids[2]), "inactive forget does not switch target");
        check(
            peer.accepted.get() == 0,
            "local forget makes no remote connection or revocation claim");
        change(
            new JSONObject()
                .put("op", "activate")
                .put("revision", repo.snapshot().revision)
                .put("id", ids[1]));
        launch();
        click("Запустить мост");
        for (int n = 0; n < 100 && peer.accepted.get() == 0; n++) Thread.sleep(50);
        check(
            peer.accepted.get() == 1 && repo.isBusy() && UsbIpService.running,
            "actual foreground connection owns the saved secret");
        long staleRevision = repo.snapshot().revision;
        click("Изменить компьютер");
        details(ids[1]);
        dialog("Забыть на этом устройстве");
        check(
            visibleText().contains("Остановить и забыть компьютер?"),
            "active forgetting requires explicit stop");
        dialog("Отмена");
        check(repo.isBusy() && UsbIpService.running, "cancel active forget leaves service running");
        details(ids[1]);
        dialog("Забыть на этом устройстве");
        dialog("Продолжить");
        waitStopped();
        for (int n = 0; n < 100 && repo.snapshot().computers.size() != 1; n++) Thread.sleep(50);
        absent(ids[1]);
        check(
            repo.snapshot().activeId == null,
            "active forget does not silently select another computer");
        for (int n = 0; n < 100 && peer.closed.get() != 1; n++) Thread.sleep(50);
        check(
            peer.closed.get() == 1 && peer.accepted.get() == 1,
            "forget closes its actual TCP connection without reconnecting");
        main(() ->
            context.startForegroundService(
                new Intent(context, UsbIpService.class)
                    .putExtra(UsbIpService.EXTRA_COMPUTER_REVISION, staleRevision)));
        Thread.sleep(500);
        waitStopped();
        Thread.sleep(1000);
        check(
            peer.accepted.get() == 1,
            "stale service request cannot reconnect forgotten credentials");
        byte[] remaining = edit(ids[2]);
        try {
          check(
              Arrays.equals(remaining, originals[2]),
              "neighbor credentials pin and rules remain byte-identical");
        } finally {
          Arrays.fill(remaining, (byte) 0);
        }
        check(
            new JSONArray(repo.snapshot().computers.get(0).usbRules).length() == 1,
            "neighbor USB rule retained");
        check(
            repo.snapshot().computers.get(0).usbRules.equals(neighborRules),
            "exact neighbor USB rule unchanged");
        screenshot("forgotten-active");
        JSONObject state =
            new JSONObject()
                .put("deleted", new JSONArray().put(ids[0]).put(ids[1]))
                .put("remaining", ids[2])
                .put("remaining_digest", digest(originals[2]))
                .put("remaining_rules", neighborRules);
        Files.write(
            proof("state.json").toPath(), state.toString().getBytes(StandardCharsets.UTF_8));
        Files.write(
            proof("result.json").toPath(),
            new JSONObject()
                .put("status", "passed")
                .put("checks", checks)
                .put("api", Build.VERSION.SDK_INT)
                .put("peer_accepted", peer.accepted.get())
                .put("peer_closed", peer.closed.get())
                .put(
                    "scope",
                    "Actual identity/forget UI and encrypted repository, loopback waiting SSH peer,"
                        + " no USB or remote revoke")
                .toString(2)
                .getBytes(StandardCharsets.UTF_8));
      } finally {
        for (byte[] value : originals) Arrays.fill(value, (byte) 0);
      }
    } finally {
      stop();
      main(() -> app.unregisterActivityLifecycleCallbacks(callbacks));
    }
    return checks;
  }

  int resume() throws Exception {
    owned();
    JSONObject state =
        new JSONObject(
            new String(Files.readAllBytes(proof("state.json").toPath()), StandardCharsets.UTF_8));
    JSONArray deleted = state.getJSONArray("deleted");
    for (int i = 0; i < deleted.length(); i++) absent(deleted.getString(i));
    check(
        repo.snapshot().computers.size() == 1 && repo.snapshot().activeId == null,
        "process restart preserves local deletion without selecting a neighbor");
    byte[] retained = edit(state.getString("remaining"));
    try {
      check(
          digest(retained).equals(state.getString("remaining_digest")),
          "neighbor secret and pin durable across process restart");
    } finally {
      Arrays.fill(retained, (byte) 0);
    }
    check(
        new JSONArray(repo.snapshot().computers.get(0).usbRules).length() == 1,
        "neighbor rule durable across process restart");
    check(
        repo.snapshot().computers.get(0).usbRules.equals(state.getString("remaining_rules")),
        "exact neighbor rule durable");
    try {
      repo.resume().get(30, TimeUnit.SECONDS);
      throw new AssertionError("forgotten session resumed");
    } catch (ExecutionException expected) {
      checks++;
    }
    repo.reset().get(30, TimeUnit.SECONDS);
    return checks;
  }
}
