package ru.levchatkin.rutokenbridge;

import android.app.*;
import android.content.*;
import android.hardware.usb.UsbDevice;
import android.net.ConnectivityManager;
import android.net.LinkProperties;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.os.*;
import android.view.accessibility.AccessibilityNodeInfo;
import java.io.*;
import java.lang.reflect.Field;
import java.nio.charset.StandardCharsets;
import java.util.*;
import java.util.concurrent.*;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.function.BooleanSupplier;
import org.cgutman.usbip.connection.ComputerRepository;
import org.cgutman.usbip.connection.ConnectionObservation;
import org.cgutman.usbip.connection.ReceiverAssets;
import org.cgutman.usbip.jni.NativeConnection;
import org.cgutman.usbip.server.UsbIpServer;
import org.cgutman.usbip.service.UsbInventory;
import org.cgutman.usbip.service.UsbIpService;
import org.json.JSONArray;
import org.json.JSONObject;

/** UI -> production management/JNI/TLS -> actual Linux/UDisks2. Only usbfs is replaced. */
final class OsEjectionSuite {
  private static final String APP = "ru.levchatkin.rutokenbridge";
  private final Instrumentation test;
  private final Context context;
  private int checks;
  private long session;
  private boolean connected;
  private boolean foregroundService;
  private boolean remoteRevocation, revoking;
  private boolean manualFallback;
  private boolean localOnlyNetwork;
  private String profileAction, neighborId;
  private ComputerRepository repository;
  private QemuUsbBackend backend;
  private File directory;
  private String marker;
  private final JSONArray outcomes = new JSONArray();

  OsEjectionSuite(Instrumentation test) {
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
      } catch (Throwable e) {
        error[0] = e;
      }
    });
    if (error[0] != null) throw new AssertionError("inventory/UI", error[0]);
  }

  private void phase(String label) {
    Bundle out = new Bundle();
    out.putString("stream", "PASS OS_EJECT_" + label + "\n");
    test.sendStatus(0, out);
  }

  private void poll() throws Exception {
    if (backend != null && backend.failure != null)
      throw new AssertionError("owned USB backend", backend.failure);
    if (foregroundService) {
      // Service lifecycle and ConnectionController callbacks run on main.
      // Reading their separate volatile snapshots from the instrumentation
      // thread can splice connected-before-onDestroy with running-after-stop.
      ConnectionObservation.State[] observed = new ConnectionObservation.State[1];
      boolean[] owned = new boolean[2];
      main(() -> {
        observed[0] = ConnectionObservation.forProfile(repository.snapshot().connectionKey());
        owned[0] = UsbIpService.running;
        owned[1] = repository.isBusy();
      });
      ConnectionObservation.State state = observed[0];
      if ((state.failure != null && !(revoking && state.failure.equals("pairing_required")))
          || (!revoking && state.phase.equals("reconnecting")))
        throw new AssertionError("actual service connection " + state.phase + ":" + state.failure);
      if (state.phase.equals("connected")) {
        if (!owned[0] || !owned[1])
          throw new AssertionError("connected observation without service/repository owner");
        connected = true;
      }
      return;
    }
    if (session == 0) return;
    String event;
    while ((event = NativeConnection.poll(session)) != null) {
      JSONObject e = new JSONObject(event);
      switch (e.getString("event")) {
        case "paired":
          check(
              NativeConnection.saved(session, e.getLong("save_id"), true), "owned test token ack");
          break;
        case "state":
          String code = e.getString("code");
          if (code.equals("connected")) {
            check(!connected, "single TLS connection");
            connected = true;
          } else check(!code.equals("reconnecting"), "no reconnect/replay in storage scenario");
          break;
        default:
          throw new AssertionError(
              "unexpected native event " + e.optString("event") + ":" + e.optString("code"));
      }
    }
  }

  private UsbIpService service() throws Exception {
    Class<?> thread = Class.forName("android.app.ActivityThread");
    Object current = thread.getDeclaredMethod("currentActivityThread").invoke(null);
    Field services = thread.getDeclaredField("mServices");
    services.setAccessible(true);
    UsbIpService result = null;
    for (Object candidate : ((Map<?, ?>) services.get(current)).values()) {
      if (candidate instanceof UsbIpService) {
        if (result != null) throw new AssertionError("multiple bridge services");
        result = (UsbIpService) candidate;
      }
    }
    return result;
  }

  private void startService(UsbInventory inventory, UsbDevice disk, JSONObject config)
      throws Exception {
    Field queue = ComputerRepository.class.getDeclaredField("queue");
    queue.setAccessible(true);
    CountDownLatch entered = new CountDownLatch(1), release = new CountDownLatch(1);
    AtomicBoolean expired = new AtomicBoolean();
    ((ExecutorService) queue.get(repository))
        .execute(() -> {
          entered.countDown();
          try {
            expired.set(!release.await(25, TimeUnit.SECONDS));
          } catch (InterruptedException interruption) {
            expired.set(true);
            Thread.currentThread().interrupt();
          }
        });
    try {
      check(entered.await(5, TimeUnit.SECONDS), "repository admission barrier entered");
      visible("Запустить мост", true);
      UsbIpService[] owner = new UsbIpService[1];
      await(
          () -> {
            main(() -> {
              try {
                owner[0] = service();
              } catch (Exception error) {
                throw new AssertionError("owned service lookup", error);
              }
            });
            return owner[0] != null && UsbIpService.running;
          },
          "actual foreground service created",
          10);
      check(!repository.isBusy(), "no native connection before adapter publication");
      Field server = UsbIpService.class.getDeclaredField("server");
      Field requests = UsbIpService.class.getDeclaredField("usbRequests");
      server.setAccessible(true);
      requests.setAccessible(true);
      check(requests.get(owner[0]) == owner[0], "production default is Android USB adapter");
      backend =
          new QemuUsbBackend(
              inventory,
              disk,
              config.getInt("backend_port"),
              config.getString("backend_bus"),
              (UsbIpServer) server.get(owner[0]));
      main(() -> {
        try {
          check(!expired.get() && !repository.isBusy(), "adapter precedes repository admission");
          requests.set(owner[0], backend);
        } catch (IllegalAccessException error) {
          throw new AssertionError(error);
        }
      });
      check(session == 0 && !expired.get(), "service owns native session; bounded adapter barrier");
    } finally {
      release.countDown();
    }
  }

  private void verifySavedPairingAndRepeat(
      JSONObject profile, String computerId, UsbInventory inventory, UsbDevice disk)
      throws Exception {
    check(repository.snapshot().activeId.equals(computerId), "same saved computer after pairing");
    check(UsbIpService.running && repository.isBusy(), "service owns repository through ejection");
    visible("Остановить мост", true);
    if (manualFallback) {
      visible("Завершить передачу?", false);
      // The owned guest has already ordinary-unmounted and verified readback.
      // This is the user's explicit confirmation, never a synthetic Safe reply.
      visible("Извлечён · завершить", true);
    }
    await(() -> !UsbIpService.running && !repository.isBusy(), "service released repository", 15);
    byte[] editor =
        repository.edit(computerId, repository.snapshot().revision).get(15, TimeUnit.SECONDS);
    try {
      JSONObject saved =
          new JSONObject(new String(editor, StandardCharsets.UTF_8)).getJSONObject("connection");
      JSONObject credentials = saved.getJSONObject("credentials");
      check(
          !credentials.optString("token", "").isEmpty(), "controller durably saved pairing token");
      check(
          credentials.isNull("pair_code") && credentials.isNull("pair_nonce"),
          "one-time pairing material retired");
      check(
          saved
              .getJSONObject("profile")
              .getString("server_pin")
              .equals(profile.getString("server_pin")),
          "exact TLS identity retained");
    } finally {
      Arrays.fill(editor, (byte) 0);
    }
    connected = false;
    visible("Запустить мост", true);
    await(() -> connected, "second actual service uses saved token", 60);
    main(() -> {
      try {
        UsbIpService current = service();
        Field adapter = UsbIpService.class.getDeclaredField("usbRequests");
        adapter.setAccessible(true);
        check(
            current != null && adapter.get(current) == current,
            "new service uses unmodified default USB adapter");
      } catch (Exception error) {
        throw new AssertionError(error);
      }
    });
    check(repository.snapshot().activeId.equals(computerId), "repeat keeps saved destination");
    UsbInventory.Item currentDisk = inventory.item(disk.getDeviceId());
    check(
        !currentDisk.selected && !currentDisk.approved && !currentDisk.inUse,
        "repeat never reuses completed storage consent");
    if (remoteRevocation) {
      revoking = true;
      phase("REVOKE_CONNECTED");
      host("revoked");
      await(this::revocationRefused, "receiver revocation ends actual service", 75);
      visible(
          "Не удалось подключиться: сопряжение отозвано; получите новый код на компьютере", false);
      screenshot("revoked");
      stoppedQuietly();
      long priorEpoch = repository.snapshot().epoch;
      connected = false;
      visible("Запустить мост", true);
      await(
          () -> repository.snapshot().epoch > priorEpoch && revocationRefused(),
          "explicit retry with revoked token refused",
          60);
      check(!connected, "revoked token never establishes another connection");
      check(
          repository.snapshot().activeId.equals(computerId),
          "revocation does not delete or switch profile");
      stoppedQuietly();
      screenshot("revoked-retry");
      phase("REVOKED_AND_RETRY_REFUSED");
    } else {
      visible("Остановить мост", true);
      await(() -> !UsbIpService.running && !repository.isBusy(), "repeat service stopped", 15);
    }
    visible("Запустить мост", false);
    screenshot("service-stopped");
    phase("SERVICE_REPEAT");
  }

  private void mainScreen() throws Exception {
    test.startActivitySync(new Intent().setClassName(APP, "org.cgutman.usbip.config.UsbIpConfig")
        .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_CLEAR_TOP));
    test.waitForIdleSync();
  }

  private void profileDialog(String action) throws Exception {
    visible("Изменить компьютер", true);
    if (action.equals("switch")) visible("Другой компьютер", true);
    else {
      visible("Доступ и настройки: Тестовый Linux", true);
      visible("Забыть на этом устройстве", true);
    }
    visible(action.equals("switch") ? "Сменить компьютер?" : "Остановить и забыть компьютер?", false);
  }

  private void cancelStorageProfileActions(String computerId, UsbInventory inventory, UsbDevice disk)
      throws Exception {
    for (String action : new String[] {"switch", "forget"}) {
      profileDialog(action);
      visible("Извлечены · продолжить", false);
      visible("Сначала безопасно извлеките накопители на компьютере. Затем мост остановит передачу USB."
          + (action.equals("forget")
              ? " Данные входа и сохранённое доверие будут удалены с этого устройства." : ""), false);
      screenshot("storage-" + action + "-warning");
      visible("Отмена", true);
      check(repository.snapshot().activeId.equals(computerId)
          && repository.snapshot().computers.size() == 2 && UsbIpService.running && repository.isBusy(),
          "cancel " + action + " retains active authenticated profile");
      UsbInventory.Item item = inventory.item(disk.getDeviceId());
      check(item.selected && item.approved && item.inUse && item.canEject && backend.attached.get() == 1,
          "cancel " + action + " preserves exact active storage import and consent");
      mainScreen();
      visible("Остановить мост", false);
    }
    phase("PROFILE_STORAGE_CANCELLED");
  }

  private void profileAfterSafe(JSONObject profile, String computerId, UsbInventory inventory, UsbDevice disk)
      throws Exception {
    check(!inventory.item(disk.getDeviceId()).selected
        && !inventory.item(disk.getDeviceId()).approved, "receiver Safe already retired storage consent");
    profileDialog(profileAction);
    visible("Передача выбранных USB-устройств на компьютер будет остановлена."
        + (profileAction.equals("forget")
            ? " Данные входа и сохранённое доверие будут удалены с этого устройства." : ""), false);
    visible("Продолжить", true);
    await(() -> !UsbIpService.running && !repository.isBusy()
            && !computerId.equals(repository.snapshot().activeId), "profile action completes after teardown", 20);
    UsbInventory.Item item = inventory.item(disk.getDeviceId());
    check(!item.selected && !item.approved && !item.inUse && backend.attached.get() == 1,
        "new profile cannot inherit old storage consent or import");
    check(!context.getSharedPreferences("bridge", 0).contains("computer_resume"), "no automatic destination restart");
    if (profileAction.equals("forget")) {
      check(repository.snapshot().activeId == null && repository.snapshot().computers.size() == 1
          && repository.snapshot().computers.get(0).id.equals(neighborId),
          "forget removes exact old profile without selecting neighbor");
      try {
        byte[] stale = repository.edit(computerId, repository.snapshot().revision).get(15, TimeUnit.SECONDS);
        Arrays.fill(stale, (byte) 0);
        throw new AssertionError("forgotten credentials remain editable");
      } catch (ExecutionException expected) { check(true, "forgotten profile cannot expose credentials"); }
      mainScreen(); screenshot("storage-profile-forgotten");
      phase("PROFILE_FORGOTTEN");
    } else {
      check(repository.snapshot().activeId.equals(neighborId) && repository.snapshot().computers.size() == 2,
          "safe switch selects only explicit destination");
      screenshot("storage-profile-switched");
      visible("Тестовый Linux", true);
      await(() -> computerId.equals(repository.snapshot().activeId), "explicit switch back", 10);
      mainScreen(); connected = false;
      visible("Запустить мост", true);
      await(() -> connected, "switch back reconnects with saved paired credentials", 60);
      check(!inventory.item(disk.getDeviceId()).selected && !inventory.item(disk.getDeviceId()).approved,
          "switch back still requires fresh storage selection and consent");
      verifySavedPairingAndRepeat(profile, computerId, inventory, disk);
      phase("PROFILE_SWITCHED");
    }
  }

  private boolean revocationRefused() {
    ConnectionObservation.State state =
        ConnectionObservation.forProfile(repository.snapshot().connectionKey());
    return !UsbIpService.running
        && !repository.isBusy()
        && "pairing_required".equals(state.failure)
        && !state.active;
  }

  private void stoppedQuietly() throws Exception {
    long until = SystemClock.elapsedRealtime() + 5000;
    while (SystemClock.elapsedRealtime() < until) {
      poll();
      if (!revocationRefused()) throw new AssertionError("revoked service silently restarted");
      Thread.sleep(40);
    }
    check(true, "revoked service remains stopped for whole quiet interval");
  }

  private void await(BooleanSupplier condition, String label, int seconds) throws Exception {
    long end = SystemClock.elapsedRealtime() + seconds * 1000L;
    while (SystemClock.elapsedRealtime() < end) {
      poll();
      if (condition.getAsBoolean()) {
        check(true, label);
        return;
      }
      Thread.sleep(40);
    }
    throw new AssertionError(label);
  }

  private void host(String step) throws Exception {
    File file = new File(directory, marker + "-" + step + ".go");
    await(file::isFile, "owned host phase " + step, 300);
    check(file.length() == 0 && file.delete(), "consume exact phase marker");
  }

  private void activeSoak(UsbInventory inventory, UsbDevice disk, int seconds) throws Exception {
    check(foregroundService && seconds >= 60 && seconds <= 28800,
        "bounded active foreground-service soak");
    File done = new File(directory, marker + "-soaked.go");
    File uiRequest = new File(directory, marker + "-soakui.go");
    File uiAck = new File(directory, marker + "-soakui.ack");
    check(!done.exists(), "no stale active-soak completion marker");
    long start = SystemClock.elapsedRealtime(), nextSample = 0;
    int previous = -1;
    int uiChecks = 0;
    boolean announced = false;
    while (true) {
      poll();
      long elapsed = SystemClock.elapsedRealtime() - start;
      ConnectionObservation.State state =
          ConnectionObservation.forProfile(repository.snapshot().connectionKey());
      UsbInventory.Item item = inventory.item(disk.getDeviceId());
      check(UsbIpService.running && repository.isBusy() && state.active
          && state.phase.equals("connected"),
          "active soak retains connected service and repository owner");
      // receiverVerified denotes SSH installer/stack preflight, not TLS
      // authentication. The host independently checks the actual receiver.
      check(!state.receiverVerified, "TLS does not invent SSH preparation evidence");
      check(item != null && item.selected && item.approved && item.inUse && item.canEject,
          "active soak retains exact storage selection and import");
      check(backend.attached.get() == 1, "same USB import throughout active soak");
      check(elapsed <= (seconds + 180L) * 1000, "bounded host soak observer deadline");
      if (uiRequest.exists()) {
        check(uiRequest.length() == 0 && uiRequest.delete() && !uiAck.exists(),
            "one owned UI observation request");
        visible("Подключено к компьютеру", false);
        visible("Защищённое подключение", false);
        visible("Остановить мост", false);
        screenshot("active-soak-" + uiChecks++);
        File staging = new File(directory, marker + "-soakui.part");
        try (OutputStream out = new FileOutputStream(staging)) { out.write('1'); }
        check(staging.renameTo(uiAck), "atomic UI observation acknowledgement");
      }
      if (elapsed >= nextSample) {
        int completed = backend.completed.get();
        check(previous < 0 || completed > previous, "actual USB completions keep progressing");
        previous = completed;
        Debug.MemoryInfo memory = new Debug.MemoryInfo();
        Debug.getMemoryInfo(memory);
        Map<String, String> gc = Debug.getRuntimeStats();
        Runtime runtime = Runtime.getRuntime();
        JSONObject sample = new JSONObject()
            .put("elapsed_ms", elapsed).put("pid", android.os.Process.myPid())
            .put("cpu_ms", android.os.Process.getElapsedCpuTime())
            .put("pss_kib", memory.getTotalPss())
            .put("java_heap_pss_kib", memory.dalvikPss)
            .put("native_heap_pss_kib", memory.nativePss)
            .put("other_pss_kib", memory.otherPss)
            .put("java_used_bytes", runtime.totalMemory() - runtime.freeMemory())
            .put("native_allocated_bytes", Debug.getNativeHeapAllocatedSize())
            .put("gc_count", gc.get("art.gc.gc-count"))
            .put("gc_bytes_allocated", gc.get("art.gc.bytes-allocated"))
            .put("gc_bytes_freed", gc.get("art.gc.bytes-freed"))
            .put("ui_checks", uiChecks).put("receiver_preflight_verified", state.receiverVerified)
            .put("submitted", backend.submitted.get()).put("completed", completed)
            .put("imports", backend.attached.get());
        check(memory.getTotalPss() < 256 * 1024, "bounded active Android process PSS");
        File staging = new File(directory, "active-soak-live.part");
        try (OutputStream out = new FileOutputStream(staging)) {
          out.write(sample.toString().getBytes(StandardCharsets.UTF_8));
        }
        check(staging.renameTo(new File(directory, "active-soak-live.json")),
            "atomic bounded soak observation");
        nextSample = elapsed + 10000;
        if (!announced) {
          phase("SOAK_READY");
          announced = true;
        }
      }
      if (done.exists()) {
        check(elapsed >= seconds * 1000L, "full requested active soak duration");
        check(done.length() == 0 && done.delete(), "consume owned soak completion marker");
        phase("SOAK_DONE");
        return;
      }
      Thread.sleep(1000);
    }
  }

  private void outcome(UsbInventory inventory, UsbDevice disk, int expected, String label)
      throws Exception {
    long start = SystemClock.elapsedRealtime(), until = start + 240000;
    int previous = -99;
    while (SystemClock.elapsedRealtime() < until) {
      poll();
      UsbInventory.Item item = inventory.item(disk.getDeviceId());
      int state = item.ejectionState;
      if (state != previous) {
        outcomes.put(
            new JSONObject()
                .put("operation", label)
                .put("state", state)
                .put("elapsed_ms", SystemClock.elapsedRealtime() - start)
                .put("selected", item.selected)
                .put("approved", item.approved)
                .put("can_request", item.canEject));
        try (OutputStream file =
            new FileOutputStream(new File(directory, "os-ejection-states.json"))) {
          file.write(outcomes.toString().getBytes(StandardCharsets.UTF_8));
        }
        previous = state;
        if (state == 7) {
          check(
              item.selected && item.approved && !item.canEject,
              "uncertain result keeps exact consent/selection without replay permission");
          // Inspect the veto; do not call requestEjection here. A real late
          // Busy may arrive between observations and allow a new operation.
          // Duplicate admission itself is covered by deterministic model tests.
          visible("Извлечение не подтверждено · проверьте накопитель на компьютере", false);
          screenshot("uncertain-" + label);
          phase("UNCERTAIN_" + label);
        }
      }
      if (state == expected) {
        check(true, "actual OS result " + label);
        return;
      }
      if (state != 0 && state != 1 && state != 7) {
        screenshot("unexpected-" + label);
        throw new AssertionError(
            "unexpected actual OS state " + state + " while awaiting " + label);
      }
      Thread.sleep(40);
    }
    screenshot("deadline-" + label);
    throw new AssertionError("OS outcome deadline " + label + ", last state=" + previous);
  }

  private AccessibilityNodeInfo find(AccessibilityNodeInfo root, String text) {
    if (root == null) return null;
    if (text.equalsIgnoreCase(root.getText() == null ? "" : root.getText().toString())
        || text.contentEquals(root.getContentDescription() == null ? "" : root.getContentDescription()))
      return AccessibilityNodeInfo.obtain(root);
    for (int i = 0; i < root.getChildCount(); i++) {
      AccessibilityNodeInfo child = root.getChild(i), found = find(child, text);
      if (child != null) child.recycle();
      if (found != null) return found;
    }
    return null;
  }

  private void visible(String text, boolean click) throws Exception {
    long until = SystemClock.elapsedRealtime() + 12000;
    while (SystemClock.elapsedRealtime() < until) {
      poll();
      AccessibilityNodeInfo root = test.getUiAutomation().getRootInActiveWindow(),
          node = find(root, text);
      if (root != null) root.recycle();
      if (node != null) {
        try {
          node.performAction(
              AccessibilityNodeInfo.AccessibilityAction.ACTION_SHOW_ON_SCREEN.getId());
          if (node.isVisibleToUser() && (!click || node.isEnabled())) {
            check(!click || node.performAction(AccessibilityNodeInfo.ACTION_CLICK), "UI: " + text);
            return;
          }
        } finally {
          node.recycle();
        }
      }
      Thread.sleep(80);
    }
    screenshot("missing-action");
    throw new AssertionError("missing UI: " + text);
  }

  private void screenshot(String name) throws Exception {
    Thread.sleep(800);
    File output = new File(directory, "os-ejection-proof");
    check(output.isDirectory() || output.mkdir(), "screenshot folder");
    android.graphics.Bitmap bitmap = test.getUiAutomation().takeScreenshot();
    check(bitmap != null, "rendered screenshot");
    try (OutputStream stream = new FileOutputStream(new File(output, name + ".png"))) {
      check(
          bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, stream),
          "screenshot saved");
    } finally {
      bitmap.recycle();
    }
  }

  private void localNetworkDiagnosis() throws Exception {
    ConnectivityManager manager = context.getSystemService(ConnectivityManager.class);
    Network active = manager.getActiveNetwork();
    NetworkCapabilities capabilities = manager.getNetworkCapabilities(active);
    check(capabilities != null && capabilities.hasTransport(NetworkCapabilities.TRANSPORT_WIFI),
        "actual default network is Wi-Fi");
    for (Network network : manager.getAllNetworks()) {
      NetworkCapabilities current = manager.getNetworkCapabilities(network);
      check(current == null || !current.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED),
          "no Android network has validated Internet");
    }
    LinkProperties link = manager.getLinkProperties(active);
    check(link != null && link.getLinkAddresses().stream().anyMatch(address ->
        address.getAddress() instanceof java.net.Inet4Address
            && address.getAddress().isSiteLocalAddress()), "actual emulator LAN address");
    Activity diagnosis = test.startActivitySync(new Intent()
        .setClassName(APP, "org.cgutman.usbip.config.ConnectionDiagnostics")
        .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
    try {
      visible("Сетевое подключение есть. Это ещё не подтверждает доступность компьютера.", false);
      visible("Защищённое соединение с выбранным компьютером установлено.", false);
      // Direct TLS authenticates the computer but does not run SSH environment preflight.
      visible("Состояние приёмника сейчас не подтверждено.", false);
      visible("Компьютер запросил выбранное USB-устройство. Его готовность в рабочей программе проверяется на компьютере.", false);
      screenshot("local-only-diagnosis");
    } finally {
      main(diagnosis::finish);
      test.waitForIdleSync();
    }
    check(UsbIpService.running && repository.isBusy(), "diagnosis preserves active local service");
    phase("LOCAL_NETWORK_DIAGNOSIS");
  }

  int run() throws Exception {
    try (InputStream input =
        new ParcelFileDescriptor.AutoCloseInputStream(
            test.getUiAutomation().executeShellCommand("getprop ro.kernel.qemu"))) {
      byte[] bytes = new byte[64];
      int n = input.read(bytes);
      check(
          n > 0 && new String(bytes, 0, n, StandardCharsets.UTF_8).trim().equals("1"),
          "emulator only");
    }
    directory = context.getExternalFilesDir(null);
    File configFile = new File(directory, "os-ejection-fixture.json");
    check(configFile.length() > 0 && configFile.length() < 16384, "bounded private config");
    byte[] raw = new byte[(int) configFile.length()];
    try (DataInputStream input = new DataInputStream(new FileInputStream(configFile))) {
      input.readFully(raw);
      check(input.read() == -1, "exact config");
    }
    JSONObject config = new JSONObject(new String(raw, StandardCharsets.UTF_8));
    foregroundService = config.optBoolean("foreground_service", false);
    localOnlyNetwork = config.optBoolean("local_only_network", false);
    check(!localOnlyNetwork || foregroundService, "local network requires actual service");
    remoteRevocation = config.optBoolean("remote_revocation", false);
    manualFallback = config.optBoolean("manual_fallback", false);
    profileAction = config.optString("profile_action", "");
    check(profileAction.isEmpty() || ((profileAction.equals("switch") || profileAction.equals("forget"))
        && foregroundService && !remoteRevocation && !manualFallback
        && config.optInt("active_soak_seconds", 0) == 0), "bounded storage/profile scenario");
    check(!remoteRevocation || foregroundService, "revocation requires actual service");
    check(!manualFallback || (foregroundService && !remoteRevocation), "manual fallback scope");
    Arrays.fill(raw, (byte) 0);
    check(config.getString("scope").equals("owned-emulator-QEMU-storage"), "scope");
    marker = config.getString("marker");
    check(marker.matches("[a-f0-9]{32}"), "marker identity");
    JSONObject profile = config.getJSONObject("profile");
    check(
        profile.getString("transport").equals("direct")
            && profile.getJSONObject("endpoint").getString("host").equals("10.0.2.2"),
        "owned TLS target");
    ComputerRepository repo = ComputerRepository.get(context);
    repository = repo;
    repo.ready().get(30, TimeUnit.SECONDS);
    repo.reset().get(30, TimeUnit.SECONDS);
    if (!profileAction.isEmpty()) {
      byte[] command = new JSONObject().put("op", "add").put("revision", repo.snapshot().revision)
          .put("name", "Другой компьютер").put("activate", true)
          .put("connection", new JSONObject().put("kind", "legacy").put("host", "192.168.10.2").put("mode", 1))
          .toString().getBytes(StandardCharsets.UTF_8);
      try { repo.change(command).get(30, TimeUnit.SECONDS); }
      finally { Arrays.fill(command, (byte) 0); }
      neighborId = repo.snapshot().activeId;
    }
    repo.change(
            new JSONObject()
                .put("op", "add")
                .put("revision", repo.snapshot().revision)
                .put("name", "Тестовый Linux")
                .put("activate", true)
                .put(
                    "connection",
                    foregroundService
                        ? new JSONObject()
                            .put("kind", "secure")
                            .put("profile", profile)
                            .put("credentials", config.getJSONObject("secret"))
                        : new JSONObject()
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
    UsbInventory[] box = new UsbInventory[1];
    main(() -> {
      box[0] = new UsbInventory(fixture, fixture);
      box[0].bindComputers(repo);
    });
    UsbInventory inventory = box[0];
    field.set(app, inventory);
    Activity screen = null;
    String computerId = repo.snapshot().activeId;
    byte[] proof = new byte[32];
    new java.security.SecureRandom().nextBytes(proof);
    try {
      context
          .getSharedPreferences("bridge", 0)
          .edit()
          .putBoolean("welcome_seen", true)
          .putBoolean("notifications_requested", true)
          .commit();
      screen =
          test.startActivitySync(
              new Intent()
                  .setClassName(APP, "org.cgutman.usbip.config.UsbIpConfig")
                  .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_CLEAR_TASK));
      UsbDevice disk = UsbFixture.device(191, "Виртуальная флешка", 8, false, 91);
      main(() -> {
        fixture.permissions.add(disk.getDeviceId());
        fixture.connect(disk, UsbFixture.descriptor(8, false));
        inventory.select(
            disk.getDeviceId(), inventory.item(disk.getDeviceId()).generation, true, true);
      });
      await(() -> inventory.item(disk.getDeviceId()).ready, "storage descriptor ready", 10);
      if (foregroundService) {
        startService(inventory, disk, config);
      } else {
        backend =
            new QemuUsbBackend(
                inventory, disk, config.getInt("backend_port"), config.getString("backend_bus"));
        int port = backend.server.startLocal(backend, proof);
        ReceiverAssets.ensure(context);
        byte[] profileBytes = profile.toString().getBytes(StandardCharsets.UTF_8);
        byte[] credentials =
            config.getJSONObject("secret").toString().getBytes(StandardCharsets.UTF_8);
        try {
          session =
              NativeConnection.start(
                  profileBytes,
                  credentials,
                  port,
                  proof,
                  new File(context.getNoBackupFilesDir(), "receivers").getAbsolutePath());
        } finally {
          Arrays.fill(profileBytes, (byte) 0);
          Arrays.fill(credentials, (byte) 0);
        }
        check(session > 0, "native TLS session");
      }
      await(
          () -> connected && inventory.item(disk.getDeviceId()).canEject,
          "real receiver supports exact import",
          120);
      visible("Извлечь на компьютере", false);
      if (localOnlyNetwork) localNetworkDiagnosis();
      phase("READY");
      host("held");
      if (!profileAction.isEmpty()) cancelStorageProfileActions(computerId, inventory, disk);
      int soakSeconds = config.optInt("active_soak_seconds", 0);
      if (soakSeconds != 0) activeSoak(inventory, disk, soakSeconds);
      visible("Извлечь на компьютере", true);
      if (manualFallback) {
        outcome(inventory, disk, 5, "MANUAL");
        visible("Извлеките накопитель средствами компьютера", false);
        screenshot("manual");
        phase("MANUAL");
        File verified = new File(directory, marker + "-manualverified.go");
        await(
            () -> {
              UsbInventory.Item item = inventory.item(disk.getDeviceId());
              if (item.ejectionState != 5 || !item.selected || !item.approved || !item.inUse)
                throw new AssertionError("manual fallback lost ownership before OS verification");
              return verified.isFile();
            },
            "manual OS unmount/readback preserves selected imported storage",
            300);
        check(verified.length() == 0 && verified.delete(), "consume manual verification marker");
        check(
            backend.submitted.get() > 10 && backend.completed.get() > 10,
            "manual path actual URBs");
        screenshot("manual-verified");
        phase("MANUAL_VERIFIED");
        verifySavedPairingAndRepeat(profile, computerId, inventory, disk);
        return checks;
      }
      outcome(inventory, disk, 3, "BUSY");
      check(
          inventory.item(disk.getDeviceId()).selected
              && inventory.item(disk.getDeviceId()).approved
              && inventory.item(disk.getDeviceId()).canEject,
          "Busy retains selected/consented storage with retry");
      visible("Закройте файлы на компьютере и повторите извлечение", false);
      screenshot("busy");
      phase("BUSY");
      host("released");
      visible("Извлечь на компьютере", true);
      outcome(inventory, disk, 2, "SAFE");
      check(
          !inventory.item(disk.getDeviceId()).selected
              && !inventory.item(disk.getDeviceId()).approved
              && !inventory.item(disk.getDeviceId()).canEject,
          "Safe clears only exact selection and consent");
      visible("Передача завершена · для повторной выберите снова", false);
      screenshot("safe");
      check(
          backend.submitted.get() > 10 && backend.completed.get() > 10,
          "actual storage URBs passed Java server");
      phase("SAFE");
      host("verified");
      poll();
      if (!profileAction.isEmpty()) profileAfterSafe(profile, computerId, inventory, disk);
      else if (foregroundService) verifySavedPairingAndRepeat(profile, computerId, inventory, disk);
      return checks;
    } finally {
      if (foregroundService) {
        context.stopService(new Intent(context, UsbIpService.class));
        long until = SystemClock.elapsedRealtime() + 15000;
        while ((UsbIpService.running || repo.isBusy()) && SystemClock.elapsedRealtime() < until)
          Thread.sleep(40);
        check(!UsbIpService.running && !repo.isBusy(), "owned service cleanup complete");
      }
      if (session != 0) {
        NativeConnection.stop(session);
        session = 0;
      }
      if (backend != null) backend.close();
      Arrays.fill(proof, (byte) 0);
      if (screen != null) {
        Activity owned = screen;
        main(owned::finish);
      }
      field.set(app, original);
      main(inventory::close);
      check(configFile.delete(), "private fixture removed");
    }
  }
}
