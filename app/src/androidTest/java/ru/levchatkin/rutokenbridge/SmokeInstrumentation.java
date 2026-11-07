package ru.levchatkin.rutokenbridge;

import android.app.*;
import android.content.*;
import android.content.pm.ActivityInfo;
import android.hardware.usb.UsbDevice;
import android.os.*;
import android.view.*;
import android.view.accessibility.AccessibilityNodeInfo;
import android.widget.*;
import java.io.*;
import java.lang.reflect.*;
import java.net.*;
import java.nio.*;
import java.nio.charset.StandardCharsets;
import java.util.*;
import java.util.concurrent.*;
import org.cgutman.usbip.connection.ComputerRepository;
import org.cgutman.usbip.service.UsbInventory;
import org.json.JSONObject;

/** Exercises the actual optimized release without keeping app code for tests. */
public final class SmokeInstrumentation extends Instrumentation {
  private static final String APP = "ru.levchatkin.rutokenbridge";
  private static final String ACTIVITY = "org.cgutman.usbip.config.UsbIpConfig";
  private static final String SERVICE = "org.cgutman.usbip.service.UsbIpService";
  private int checks;
  private boolean layoutOnly;
  private boolean navigationOnly;
  private boolean connectionOnly;
  private boolean pairingOnly;
  private boolean cameraDenied;
  private boolean cameraQr;
  private boolean connectionLayout;
  private boolean licensesOnly;
  private boolean privacyOnly;
  private boolean transportOnly;
  private boolean keyImportOnly;
  private boolean usbRulesOnly;
  private boolean usbRulesResume;
  private boolean managedEjectionOnly;
  private boolean osEjectionOnly;
  private boolean osRebootResume;

  boolean osRebootResume() { return osRebootResume; }
  private boolean diagnosticsOnly;
  private String migrationStage;
  private String liveProfilesStage;
  private boolean accessOnly;
  private boolean accessResume;
  private boolean accessEndpoints;
  private boolean accessEndpointsResume;
  private boolean quickActionsOnly;
  private boolean quickActionsResume;
  private boolean quickActionsUi;
  private boolean quickActionsKeyguard;
  private String computerStoreScenario;
  private String usbTransition;
  private boolean retainConnectionFixture;
  private boolean diagnosticBuild;
  private Context context;
  private volatile Activity activity;
  private Application.ActivityLifecycleCallbacks activityCallbacks;
  private String localAddress;
  private final StringBuilder progress = new StringBuilder();

  private synchronized void check(boolean ok, String label) {
    if (!ok) throw new AssertionError(label);
    checks++;
  }

  private void phase(String name) {
    progress.append("PASS ").append(name).append('\n');
    Bundle b = new Bundle();
    b.putString("stream", "PASS " + name + "\n");
    sendStatus(0, b);
  }

  @Override
  public void onCreate(Bundle args) {
    super.onCreate(args);
    navigationOnly = args != null && "navigation".equals(args.getString("suite"));
    layoutOnly = args != null && "layout".equals(args.getString("suite"));
    connectionOnly = args != null && "connection".equals(args.getString("suite"));
    retainConnectionFixture =
        connectionOnly && "true".equals(args.getString("retain-owned-fixture"));
    cameraDenied = args != null && "camera-denied".equals(args.getString("suite"));
    cameraQr = args != null && "camera-qr".equals(args.getString("suite"));
    connectionLayout = args != null && "connection-layout".equals(args.getString("suite"));
    transportOnly = args != null && "transport".equals(args.getString("suite"));
    keyImportOnly = args != null && "key-import".equals(args.getString("suite"));
    usbRulesOnly = args != null && "usb-rules".equals(args.getString("suite"));
    usbTransition = args == null ? "rotate" : args.getString("usb-transition", "rotate");
    managedEjectionOnly = args != null && "managed-ejection".equals(args.getString("suite"));
    osEjectionOnly = args != null && "os-ejection".equals(args.getString("suite"));
    osRebootResume = osEjectionOnly && "true".equals(args.getString("reboot-resume"));
    diagnosticsOnly = args != null && "diagnostics".equals(args.getString("suite"));
    migrationStage = args != null && "migration-connection".equals(args.getString("suite"))
        ? args.getString("stage", "connect") : null;
    liveProfilesStage = args != null && "live-profiles".equals(args.getString("suite"))
        ? args.getString("stage", "first") : null;
    accessOnly = args != null && "access".equals(args.getString("suite"));
    accessResume = args != null && "access-resume".equals(args.getString("suite"));
    accessEndpoints = args != null && "access-endpoints".equals(args.getString("suite"));
    accessEndpointsResume = args != null && "access-endpoints-resume".equals(args.getString("suite"));
    usbRulesResume = args != null && "usb-rules-resume".equals(args.getString("suite"));
    quickActionsOnly = args != null && "quick-actions".equals(args.getString("suite"));
    quickActionsResume = args != null && "quick-actions-resume".equals(args.getString("suite"));
    quickActionsUi = args != null && "quick-actions-ui".equals(args.getString("suite"));
    quickActionsKeyguard = args != null && "quick-actions-keyguard".equals(args.getString("suite"));
    computerStoreScenario =
        args != null && "computer-store".equals(args.getString("suite"))
            ? args.getString("scenario", "secure")
            : null;
    licensesOnly = args != null && "licenses".equals(args.getString("suite"));
    privacyOnly = args != null && "privacy".equals(args.getString("suite"));
    pairingOnly =
        cameraQr || cameraDenied || args != null && "pairing".equals(args.getString("suite"));
    diagnosticBuild = args != null && "true".equals(args.getString("diagnostic"));
    start();
  }

  private void onMain(Runnable action) {
    Throwable[] failure = new Throwable[1];
    super.runOnMainSync(() -> {
      try {
        action.run();
      } catch (Throwable error) {
        failure[0] = error;
      }
    });
    if (failure[0] != null) throw new AssertionError("UI assertion/action failed", failure[0]);
  }

  private String heldWakeLocks() throws Exception {
    String power = shell("dumpsys power");
    int begin = power.indexOf("Wake Locks:"), end = power.indexOf("Suspend Blockers:");
    check(begin >= 0 && end > begin, "power dump has current wake lock section");
    return power.substring(begin, end);
  }

  private Intent service() {
    return new Intent()
        .setClassName(APP, SERVICE)
        .putExtra("computer_revision", ComputerRepository.get(context).snapshot().revision);
  }

  private Intent screen() {
    return new Intent().setClassName(APP, ACTIVITY).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
  }

  private void resumeScreen() {
    onMain(() -> context.startActivity(screen().addFlags(Intent.FLAG_ACTIVITY_REORDER_TO_FRONT)));
  }

  private String shell(String command) throws Exception {
    try (ParcelFileDescriptor fd = getUiAutomation().executeShellCommand(command);
        InputStream in = new ParcelFileDescriptor.AutoCloseInputStream(fd)) {
      ByteArrayOutputStream out = new ByteArrayOutputStream();
      byte[] b = new byte[4096];
      int n;
      while ((n = in.read(b)) != -1) out.write(b, 0, n);
      return out.toString("UTF-8");
    }
  }

  private View find(View root, Class<?> type, String text) {
    if (type.isInstance(root)
        && (text == null
            || (root instanceof TextView && ((TextView) root).getText().toString().contains(text))))
      return root;
    if (root instanceof ViewGroup) {
      ViewGroup group = (ViewGroup) root;
      for (int i = 0; i < group.getChildCount(); i++) {
        View result = find(group.getChildAt(i), type, text);
        if (result != null) return result;
      }
    }
    return null;
  }

  private View view(Class<?> type, String text) {
    View[] out = new View[1];
    onMain(() -> {
      View root = activity.getWindow().getDecorView();
      if (type == EditText.class && text == null) out[0] = root.findViewWithTag("Адрес компьютера");
      if (out[0] == null) out[0] = find(root, type, text);
    });
    return out[0];
  }

  private void click(String text) {
    String title = text.equals("Сохранить адрес") ? "Сохранить компьютер" : text;
    View control = null;
    for (int i = 0; i < 100; i++) {
      control = view(Button.class, title);
      if (control != null && control.isShown() && control.isEnabled()) break;
      SystemClock.sleep(50);
    }
    final View v = control;
    check(v != null, "button exists: " + title);
    onMain(() ->
        check(
            v.isShown() && v.isEnabled() && v.performClick(), "visible button clicked: " + title));
    waitForIdleSync();
  }

  private void waitText(String text) throws Exception {
    for (int i = 0; i < 100; i++) {
      if (view(TextView.class, text) != null) {
        checks++;
        return;
      }
      Thread.sleep(100);
    }
    throw new AssertionError("UI did not show: " + text);
  }

  private void stop() throws Exception {
    onMain(() -> context.stopService(service()));
    for (int i = 0; i < 100; i++) {
      try (Socket ignored = connect()) {
        Thread.sleep(50);
      } catch (ConnectException e) {
        if (!ComputerRepository.get(context).isBusy()) {
          Thread.sleep(100);
          return;
        }
        Thread.sleep(50);
      }
    }
    throw new AssertionError("listener or computer ownership remains after stop");
  }

  private void start(String host) throws Exception {
    final String peer = host.equals("127.0.0.1") ? localAddress : host;
    configure(peer, host.equals("127.0.0.1") ? 1 : 0);
    onMain(() -> context.startForegroundService(service()));
    // UI refresh runs every 500 ms and may still show the previous instance's
    // status during a fast restart. Await the actual listener, not stale text.
    boolean listening = false;
    for (int attempt = 0; attempt < 100 && !listening; attempt++) {
      try (Socket probe = connect()) {
        listening = true;
      } catch (ConnectException notStartedYet) {
        Thread.sleep(50);
      }
    }
    check(listening, "new service listener is ready");
    waitText("Готово.");
  }

  private void configure(String host, int mode) throws Exception {
    ComputerRepository repo = ComputerRepository.get(context);
    repo.ready().get(30, TimeUnit.SECONDS);
    ComputerRepository.Snapshot before = repo.snapshot();
    if (before.active() != null
        && before.active().kind.equals("legacy")
        && before.active().host.equals(host)
        && before.active().mode == mode) return;
    JSONObject command =
        new JSONObject()
            .put("op", before.activeId == null ? "add" : "replace")
            .put("revision", before.revision)
            .put("name", host)
            .put(
                "connection",
                new JSONObject().put("kind", "legacy").put("host", host).put("mode", mode));
    if (before.activeId == null) command.put("activate", true);
    else command.put("id", before.activeId);
    repo.change(command.toString().getBytes(StandardCharsets.UTF_8)).get(30, TimeUnit.SECONDS);
  }

  private Socket connect() throws Exception {
    Socket s = new Socket();
    try {
      s.connect(new InetSocketAddress(localAddress, 3240), 2000);
      s.setSoTimeout(4000);
      return s;
    } catch (Exception e) {
      s.close();
      throw e;
    }
  }

  private byte[] header(int op, int status) {
    return ByteBuffer.allocate(8)
        .putShort((short) 0x111)
        .putShort((short) op)
        .putInt(status)
        .array();
  }

  private void closed(Socket s, String label) throws Exception {
    try {
      check(s.getInputStream().read() == -1, label);
    } catch (SocketException reset) {
      checks++;
    }
  }

  private void devlist(boolean fragmented) throws Exception {
    try (Socket s = connect()) {
      byte[] h = header(0x8005, 0);
      if (fragmented)
        for (byte b : h) {
          s.getOutputStream().write(b);
          Thread.sleep(5);
        }
      else s.getOutputStream().write(h);
      byte[] answer = new byte[12];
      new DataInputStream(s.getInputStream()).readFully(answer);
      check(
          Arrays.equals(
              answer,
              ByteBuffer.allocate(12)
                  .putShort((short) 0x111)
                  .putShort((short) 5)
                  .putInt(0)
                  .putInt(0)
                  .array()),
          "empty DEVLIST exact response");
      closed(s, "DEVLIST EOF");
    }
  }

  private org.json.JSONObject diagnosticQuery(int op, Integer mode) throws Exception {
    try (Socket socket = connect()) {
      socket.setSoTimeout(op == 0x80f3 ? 120000 : 15000);
      socket.getOutputStream().write(header(op, 0));
      if (mode != null) socket.getOutputStream().write(ByteBuffer.allocate(4).putInt(mode).array());
      DataInputStream in = new DataInputStream(socket.getInputStream());
      check(
          in.readUnsignedShort() == 0x111 && in.readUnsignedShort() == (op & 0x7fff),
          "diagnostic reply framing");
      check(in.readInt() == 0, "diagnostic operation succeeded");
      int length = in.readInt();
      check(length > 0 && length <= 256 * 1024, "diagnostic body bounded");
      byte[] bytes = new byte[length];
      in.readFully(bytes);
      return new org.json.JSONObject(new String(bytes, java.nio.charset.StandardCharsets.UTF_8));
    }
  }

  private void diagnostics() throws Exception {
    stop();
    start("127.0.0.1");
    if (!diagnosticBuild) {
      for (int op = 0x80f0; op <= 0x80f3; op++)
        try (Socket socket = connect()) {
          socket.getOutputStream().write(header(op, 0));
          closed(socket, "production has no diagnostic command " + op);
        }
    } else {
      org.json.JSONObject state = diagnosticQuery(0x80f0, null);
      check(
          state.getBoolean("diagnostic_build") && state.getInt("schema") == 1,
          "explicit diagnostic build");
      check(
          state.getInt("active_hid") == 0 && state.getJSONArray("sessions").length() == 0,
          "non-Logitech fixtures never enable HID tracing");
      check(
          diagnosticQuery(0x80f1, 1).getInt("selected_backend") == 1,
          "SDK mode selected without USB I/O");
      check(
          diagnosticQuery(0x80f1, 0).getInt("selected_backend") == 0,
          "Rust mode restored without USB I/O");
      org.json.JSONObject result = diagnosticQuery(0x80f3, null);
      check(
          result.getBoolean("success") && result.getInt("checks") >= 937,
          "real ART JNI, SDK cancellation and failed-control continuation selftest");
      check(
          diagnosticQuery(0x80f2, null).getJSONArray("sessions").length() == 0,
          "bounded diagnostic history clear");
      try (Socket socket = connect()) {
        socket.getOutputStream().write(header(0x80f1, 0));
        socket.getOutputStream().write(ByteBuffer.allocate(4).putInt(2).array());
        closed(socket, "invalid backend rejected");
      }
      stop();
      start("100.101.101.101");
      try (Socket socket = connect()) {
        socket.getOutputStream().write(header(0x80f0, 0));
        closed(socket, "diagnostics denies unconfigured source address");
      }
    }
    stop();
    phase("diagnostic build gating, peer isolation, backend selection, real ART selftest");
  }

  private void wanNegotiation() throws Exception {
    stop();
    start("127.0.0.1");
    try (Socket socket = connect()) {
      socket.getOutputStream().write(header(0x80df, 0));
      byte[] raw = exact(socket.getInputStream(), 24);
      ByteBuffer b = ByteBuffer.wrap(raw);
      check(
          b.getShort() == 0x111 && b.getShort() == 0xdf && b.getInt() == 0,
          "WAN capability header");
      check(
          b.getInt() == 0x57414e31 && b.getInt() == 2048 && b.getInt() == 32 && b.getInt() == 1,
          "WAN bounded profile negotiation");
    }
    for (int profile : new int[] {1, 2})
      try (Socket socket = connect()) {
        ByteBuffer b = ByteBuffer.allocate(44).put(header(0x80e0, 0));
        byte[] name = new byte[32];
        byte[] text = "999-999".getBytes(java.nio.charset.StandardCharsets.US_ASCII);
        System.arraycopy(text, 0, name, 0, text.length);
        b.put(name).putInt(profile);
        socket.getOutputStream().write(b.array());
        if (profile == 1) {
          ByteBuffer reply = ByteBuffer.wrap(exact(socket.getInputStream(), 8));
          check(reply.getInt(4) == 95, "unknown USB cannot enable WAN");
        } else closed(socket, "invalid WAN profile rejected");
      }
    stop();
    phase("WAN capability framing, profile validation, ordinary USB isolation");
  }

  private int fixtureDescriptors() {
    int count = 0;
    for (String fd : new File("/proc/self/fd").list()) {
      try {
        String target = android.system.Os.readlink("/proc/self/fd/" + fd);
        if (target.endsWith("/jni-non-usb") || target.endsWith("/jni-non-usb (deleted)")) count++;
      } catch (android.system.ErrnoException closedConcurrently) {
        // Android's renderer/binder threads may close unrelated descriptors.
      }
    }
    return count;
  }

  private void jni() throws Exception {
    Class<?> nativeUsb =
        Class.forName("org.cgutman.usbip.jni.NativeUsb", true, context.getClassLoader());
    Method open = nativeUsb.getMethod("open", int.class),
        close = nativeUsb.getMethod("close", long.class);
    Method reap = nativeUsb.getMethod("reap", long.class, int.class);
    Method cancel = nativeUsb.getMethod("cancel", long.class, int.class);
    Method configure = nativeUsb.getMethod("configureSocket", int.class);
    Method release = nativeUsb.getMethod("releaseInterface", int.class, int.class, boolean.class);
    Method submit =
        nativeUsb.getMethod(
            "submit",
            long.class,
            int.class,
            int.class,
            int.class,
            int.class,
            byte[].class,
            int.class,
            byte[].class);
    check((Long) open.invoke(null, -1) == 0, "JNI rejects invalid descriptor");
    check((Integer) configure.invoke(null, -1) < 0, "JNI socket error");
    check((Integer) cancel.invoke(null, 0L, 1) == -22, "JNI cancel null handle");
    check(
        (Integer) submit.invoke(null, 0L, 1, 3, 0x81, 0, null, 0, null) == -22,
        "JNI submit null handle");
    close.invoke(null, 0L);
    for (long invalid : new long[] {-1L, Long.MAX_VALUE, 0x123456789L}) {
      check((Integer) cancel.invoke(null, invalid, 1) == -22, "JNI rejects arbitrary handle");
      check(
          (Integer) submit.invoke(null, invalid, 1, 3, 0x81, 0, null, 0, null) == -22,
          "JNI submit rejects arbitrary handle");
      check(reap.invoke(null, invalid, 0) != null, "JNI invalid handle yields completion error");
      close.invoke(null, invalid);
    }
    check(fixtureDescriptors() == 0, "no stale JNI fixture descriptors");
    File fake = new File(context.getCacheDir(), "jni-non-usb");
    try (RandomAccessFile file = new RandomAccessFile(fake, "rw");
        ParcelFileDescriptor fd = ParcelFileDescriptor.dup(file.getFD())) {
      for (boolean reconnect : new boolean[] {false, true}) {
        check((Integer) release.invoke(null, -1, 0, reconnect) == -22, "release invalid fd");
        for (int id : new int[] {-1, 256})
          check(
              (Integer) release.invoke(null, fd.getFd(), id, reconnect) == -22,
              "release invalid interface");
        check(
            (Integer) release.invoke(null, fd.getFd(), 1, reconnect) < 0,
            "release ordinary file fails through real JNI/kernel");
      }
      check(fixtureDescriptors() == 2, "interface ioctl does not retain or close caller fd");
      for (int i = 0; i < 32; i++) {
        long handle = (Long) open.invoke(null, fd.getFd());
        check(handle != 0, "JNI duplicates fd");
        check(fixtureDescriptors() == 3, "native owns one duplicated descriptor");
        try {
          // A regular file yields a kernel/SELinux error, exercising real JNI construction.
          Object completion = reap.invoke(null, handle, 0);
          check(
              completion != null
                  && completion
                      .getClass()
                      .getName()
                      .equals("org.cgutman.usbip.core.UsbBackend$Completion"),
              "native constructs retained Completion");
          check((Integer) cancel.invoke(null, handle, 123) == -2, "unknown native request");
          for (int length : new int[] {-1, 1048577, Integer.MAX_VALUE})
            check(
                (Integer) submit.invoke(null, handle, 1, 3, 0x81, 0, null, length, null) == -22,
                "native length bound");
          check(
              (Integer) submit.invoke(null, handle, 1, 3, 1, 0, new byte[1], 2, null) == -22,
              "native payload length mismatch");
          check(
              (Integer) submit.invoke(null, handle, 1, 2, 0, 0, null, 0, new byte[7]) == -22,
              "native setup size");
          for (int length : new int[] {0, 65556, 131072, 131073, 520192, 524288, 1048576}) {
            int rc = (Integer) submit.invoke(null, handle, 1, 3, 0x81, 0, null, length, null);
            check(
                rc == -25 || rc == -1 || rc == -13,
                "valid length " + length + " reaches kernel/SELinux, rc=" + rc);
          }
        } finally {
          close.invoke(null, handle);
        }
        close.invoke(null, handle);
        check((Integer) cancel.invoke(null, handle, 123) == -22, "JNI rejects stale handle");
        check(fixtureDescriptors() == 2, "native descriptor closed exactly once");
      }
    } finally {
      fake.delete();
    }
    check(fixtureDescriptors() == 0, "JNI fd ownership/no leak");
    try (ServerSocket server = new ServerSocket(0, 1, InetAddress.getByName("127.0.0.1"));
        Socket client = new Socket("127.0.0.1", server.getLocalPort());
        Socket peer = server.accept()) {
      try (ParcelFileDescriptor fd = ParcelFileDescriptor.fromSocket(client)) {
        check((Integer) configure.invoke(null, fd.getFd()) == 0, "TCP socket tuning");
        // Model a live USB session: the wrapper must stay owned until teardown.
        for (int i = 0; i < 8; i++) {
          System.gc();
          System.runFinalization();
          client.getOutputStream().write(i);
          peer.setSoTimeout(2000);
          check(peer.getInputStream().read() == i, "TCP remains usable for session lifetime");
        }
      }
      if (Build.VERSION.SDK_INT >= 29) {
        client.getOutputStream().write(7);
        check(peer.getInputStream().read() == 7, "modern Android closes only the duplicate");
      } else {
        closed(peer, "legacy Android closes shared fd at session teardown");
      }
    }
    phase("JNI symbols, native-to-Java constructor, bounds, 65556 bytes, descriptor lifecycle");
  }

  private void connectionJni() throws Exception {
    Class<?> api = context.getClassLoader().loadClass("org.cgutman.usbip.jni.NativeConnection");
    Method start =
        api.getMethod("start", byte[].class, byte[].class, int.class, byte[].class, String.class);
    Method poll = api.getMethod("poll", long.class), stop = api.getMethod("stop", long.class);
    Method approve = api.getMethod("approve", long.class, String.class, boolean.class);
    Method saved = api.getMethod("saved", long.class, long.class, boolean.class);
    byte[] secret =
        "{\"password\":\"synthetic-fixture\"}".getBytes(java.nio.charset.StandardCharsets.UTF_8);
    String profile =
        "{\"transport\":\"ssh\",\"endpoint\":{\"host\":\"127.0.0.1\",\"port\":9},\"user\":\"fixture\",\"host_key\":\"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\",\"reverse_port\":0,\"prepare\":false}";
    check(
        (Long)
                start.invoke(
                    null,
                    "{}".getBytes(java.nio.charset.StandardCharsets.UTF_8),
                    secret,
                    1,
                    new byte[32],
                    "/unused-fixture")
            == 0,
        "connection JNI rejects malformed profile");
    long handle =
        (Long)
            start.invoke(
                null,
                profile.getBytes(java.nio.charset.StandardCharsets.UTF_8),
                secret,
                1,
                new byte[32],
                "/unused-fixture");
    check(handle > 0, "connection JNI starts asynchronous work on ART");
    try {
      boolean failed = false;
      long deadline = SystemClock.elapsedRealtime() + 5000;
      while (SystemClock.elapsedRealtime() < deadline) {
        String event = (String) poll.invoke(null, handle);
        if (event != null && event.contains("\"code\":\"reconnecting\"")) {
          failed = true;
          break;
        }
        Thread.sleep(10);
      }
      check(failed, "connection JNI retries unreachable SSH without USB");
      check(
          !(Boolean) approve.invoke(null, handle, "stale-pin", true),
          "connection JNI rejects stale approval");
    } finally {
      stop.invoke(null, handle);
      Arrays.fill(secret, (byte) 0);
    }
    stop.invoke(null, handle);
    check(
        poll.invoke(null, handle) == null, "connection JNI stop is idempotent and revokes handle");
    check(
        !(Boolean) saved.invoke(null, handle, 1L, true),
        "connection JNI rejects stale save acknowledgement");
    phase("connection JNI ART runtime, bounds, cancellation and stale trust (no USB)");
  }

  private void legacyPage() throws Exception {
    homePage();
    ComputerRepository repo = ComputerRepository.get(context);
    repo.ready().get(30, TimeUnit.SECONDS);
    Intent editor =
        new Intent()
            .setClassName(APP, "org.cgutman.usbip.connection.ConnectionSetup")
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            .putExtra("computer_revision", repo.snapshot().revision)
            .putExtra("initial_transport", 2);
    if (repo.snapshot().activeId != null) editor.putExtra("computer_id", repo.snapshot().activeId);
    activity = startActivitySync(editor);
    for (int i = 0; i < 100; i++) {
      View save = view(Button.class, "Сохранить компьютер");
      if (save != null && save.isEnabled()) return;
      Thread.sleep(50);
    }
    throw new AssertionError("bound legacy editor did not load");
  }

  private void homePage() throws Exception {
    for (int i = 0; i < 12; i++) {
      View all = view(CheckBox.class, "Все и новые");
      if (all != null && all.isShown()) {
        checks++;
        return;
      }
      onMain(() -> clickVisibleBack(activity.getWindow().getDecorView()));
      waitForIdleSync();
      Thread.sleep(100);
    }
    throw new AssertionError("main screen not visible after navigation");
  }

  private boolean clickVisibleBack(View v) {
    if (!v.isShown()) return false;
    if (v instanceof Button && ((Button) v).getText().toString().equals("Назад"))
      return v.performClick();
    if (v instanceof ViewGroup)
      for (int i = 0; i < ((ViewGroup) v).getChildCount(); i++)
        if (clickVisibleBack(((ViewGroup) v).getChildAt(i))) return true;
    return false;
  }

  private void validation() throws Exception {
    legacyPage();
    EditText input = (EditText) view(EditText.class, null);
    check(input != null, "server input exists");
    int notificationDenials = 0;
    for (String invalid :
        new String[] {
          "",
          " ",
          "127.0.0.1",
          "100.63.255.255",
          "100.128.0.0",
          "100.64.0.256",
          "100.64.0.-1",
          "100.64.0",
          "100.64.0.1.2",
          "abc",
          "100.64.0.+1",
          "100.64.0.1\n99",
          "100.64.0.999999999999999999999",
          "١٠٠.٦٤.٠.١"
        }) {
      onMain(() -> {
        input.setError(null);
        input.setText(invalid);
      });
      click("Сохранить адрес");
      boolean[] rejected = new boolean[1];
      for (int attempt = 0; attempt < 100 && !rejected[0]; attempt++) {
        onMain(() -> rejected[0] = input.getError() != null);
        if (!rejected[0] && Build.VERSION.SDK_INT >= 33) {
          AccessibilityNodeInfo root = getUiAutomation().getRootInActiveWindow();
          if (root != null) {
            for (String id :
                new String[] {
                  "permission_deny_button", "permission_deny_and_dont_ask_again_button"
                }) {
              for (AccessibilityNodeInfo button :
                  root.findAccessibilityNodeInfosByViewId(
                      "com.android.permissioncontroller:id/" + id)) {
                if (button.performAction(AccessibilityNodeInfo.ACTION_CLICK)) notificationDenials++;
              }
            }
          }
        }
        if (!rejected[0]) Thread.sleep(100);
      }
      check(rejected[0], "invalid host rejected after permission callback: " + invalid);
    }
    onMain(() -> {
      input.setError(null);
      input.setText(" 100.64.0.1 ");
    });
    click("Сохранить адрес");
    click("Запустить");
    if (Build.VERSION.SDK_INT >= 33) {
      for (int attempt = 0; attempt < 100 && notificationDenials == 0; attempt++) {
        AccessibilityNodeInfo root = getUiAutomation().getRootInActiveWindow();
        if (root != null)
          for (String id :
              new String[] {"permission_deny_button", "permission_deny_and_dont_ask_again_button"})
            for (AccessibilityNodeInfo button :
                root.findAccessibilityNodeInfosByViewId(
                    "com.android.permissioncontroller:id/" + id))
              if (button.performAction(AccessibilityNodeInfo.ACTION_CLICK)) notificationDenials++;
        if (notificationDenials == 0) Thread.sleep(100);
      }
      check(notificationDenials > 0, "notification denial dialog exercised");
      check(
          context.checkSelfPermission("android.permission.POST_NOTIFICATIONS") != 0,
          "notification permission remains denied");
    }
    waitText("Готово.");
    check(
        ComputerRepository.get(context).snapshot().active().host.equals("100.64.0.1"),
        "valid host trimmed and persisted");
    check(ComputerRepository.get(context).isBusy(), "active destination reserved while running");
    click("Настройки");
    click("Компьютеры и доступ");
    click("Добавить компьютер");
    check(
        activity.getClass().getName().equals("org.cgutman.usbip.config.ComputersActivity"),
        "running bridge blocks adding and activating another computer");
    homePage();
    try (Socket s = connect()) {
      closed(s, "non-allowlisted source rejected");
    }
    click("Остановить");
    waitText("Служба остановлена");
    check(!ComputerRepository.get(context).isBusy(), "computer released after stop");
    phase("UI validation, start/stop, notification permission path, peer allowlist");
  }

  private void networkAndSelectionUi() throws Exception {
    legacyPage();
    RadioButton lan = (RadioButton) view(RadioButton.class, "Локальная сеть");
    RadioButton vpn = (RadioButton) view(RadioButton.class, "Tailscale");
    check(lan != null && vpn != null, "network modes visible");
    EditText input = (EditText) view(EditText.class, null);
    onMain(() -> lan.performClick());
    waitText("USB/IP передаёт данные без");
    for (String bad :
        new String[] {
          "100.83.192.84",
          "8.8.8.8",
          "127.0.0.1",
          "192.168.0.256",
          "172.32.0.1",
          "10.00.0.1",
          "host.local"
        }) {
      onMain(() -> {
        input.setError(null);
        input.setText(bad);
      });
      click("Сохранить адрес");
      onMain(() -> check(input.getError() != null, "LAN invalid address rejected: " + bad));
    }
    onMain(() -> input.setText(localAddress));
    click("Сохранить адрес");
    click("Запустить");
    waitText("Готово.");
    devlist(false);
    check(ComputerRepository.get(context).isBusy(), "transport edits reserved during sharing");
    click("Остановить");
    waitText("Служба остановлена");
    legacyPage();
    RadioButton editVpn = (RadioButton) view(RadioButton.class, "Tailscale");
    RadioButton editLan = (RadioButton) view(RadioButton.class, "Локальная сеть");
    EditText edited = (EditText) view(EditText.class, null);
    onMain(() -> editVpn.performClick());
    onMain(() ->
        check(
            edited.getText().toString().equals(localAddress),
            "changing draft transport does not replace saved address"));
    onMain(() -> editLan.performClick());
    onMain(() ->
        check(
            edited.getText().toString().equals(localAddress),
            "draft address preserved across transport choice"));
    homePage();
    CheckBox all = (CheckBox) view(CheckBox.class, "Все и новые");
    check(all != null, "all and future device selection visible");
    onMain(() -> all.performClick());
    Thread.sleep(600);
    onMain(() -> check(all.isChecked(), "all mode works with no attached USB"));
    onMain(() -> all.performClick());
    Thread.sleep(600);
    onMain(() -> check(!all.isChecked(), "all mode can be cleared"));
    waitText("Пока ничего не подключено");
    try {
      configure("8.8.8.8", 1);
      throw new AssertionError("invalid persisted peer accepted");
    } catch (ExecutionException expected) {
      checks++;
    }
    onMain(() -> context.startForegroundService(service().putExtra("computer_revision", -1L)));
    waitText("Не удалось запустить");
    try (Socket ignored = connect()) {
      throw new AssertionError("invalid persisted peer opened listener");
    } catch (ConnectException expected) {
      checks++;
    }
    onMain(() ->
        check(
            activity.getActionBar() == null || !activity.getActionBar().isShowing(),
            "no duplicate ActionBar title"));
    phase(
        "LAN/VPN validation, actual LAN peer traffic, drafts, selection, persisted-policy"
            + " enforcement");
  }

  private String helpCommands() throws Exception {
    for (int attempt = 0; attempt < 30; attempt++) {
      AccessibilityNodeInfo root = getUiAutomation().getRootInActiveWindow();
      if (root != null) {
        List<AccessibilityNodeInfo> nodes =
            root.findAccessibilityNodeInfosByViewId(APP + ":id/connection_commands");
        if (!nodes.isEmpty() && nodes.get(0).getText() != null)
          return nodes.get(0).getText().toString();
      }
      Thread.sleep(100);
    }
    throw new AssertionError("Connection commands not visible");
  }

  private String attachCommand(UsbDevice device) {
    int id = device.getDeviceId();
    return " -b " + (id / 1000) + "-" + (id % 1000);
  }

  private void copyHelp(String expected) throws Exception {
    systemClick("Копировать команды");
    homePage();
    onMain(() -> {
      ClipboardManager clipboard =
          (ClipboardManager) context.getSystemService(Context.CLIPBOARD_SERVICE);
      check(
          clipboard != null && clipboard.hasPrimaryClip(),
          "connection commands copied to real clipboard");
      check(
          clipboard.getPrimaryClip().getItemAt(0).getText().toString().equals(expected),
          "clipboard matches displayed commands exactly");
    });
  }

  private void systemClick(String text) throws Exception {
    for (int i = 0; i < 50; i++) {
      AccessibilityNodeInfo root = getUiAutomation().getRootInActiveWindow();
      if (root != null)
        for (AccessibilityNodeInfo node : root.findAccessibilityNodeInfosByText(text))
          if (node.isClickable() && node.performAction(AccessibilityNodeInfo.ACTION_CLICK)) {
            checks++;
            Thread.sleep(200);
            return;
          }
      Thread.sleep(100);
    }
    throw new AssertionError("Dialog action missing: " + text);
  }

  private void chooseDevice(String name) throws Exception {
    CheckBox choice = (CheckBox) view(CheckBox.class, name);
    check(choice != null, "USB row exists: " + name);
    onMain(() -> choice.performClick());
  }

  private void waitChoice(String name, boolean selected) throws Exception {
    for (int attempt = 0; attempt < 100; attempt++) {
      boolean[] matches = {false};
      onMain(() -> {
        View found = find(activity.getWindow().getDecorView(), CheckBox.class, name);
        matches[0] = found instanceof CheckBox && ((CheckBox) found).isChecked() == selected;
      });
      if (matches[0]) {
        checks++;
        return;
      }
      Thread.sleep(50);
    }
    throw new AssertionError("USB selection UI did not refresh: " + name);
  }

  private void ready(UsbInventory inventory, int count) throws Exception {
    for (int attempt = 0; attempt < 100; attempt++) {
      int ready = 0;
      for (UsbInventory.Item item : inventory.snapshot()) if (item.selected && item.ready) ready++;
      if (ready == count) {
        checks++;
        Thread.sleep(600);
        return;
      }
      Thread.sleep(100);
    }
    throw new AssertionError("Ready USB count did not become " + count);
  }

  private byte[] exact(InputStream input, int length) throws Exception {
    byte[] bytes = new byte[length];
    new DataInputStream(input).readFully(bytes);
    return bytes;
  }

  private void devlistCount(int count) throws Exception {
    try (Socket socket = connect()) {
      socket.getOutputStream().write(header(0x8005, 0));
      byte[] response = exact(socket.getInputStream(), 12);
      check(
          ByteBuffer.wrap(response).getInt(8) == count,
          "selected inventory reaches DEVLIST: " + count);
      check(
          exact(socket.getInputStream(), count * 316).length == count * 316,
          "selected descriptor payload exact");
    }
  }

  private void layoutTree(View root, int left, int right) {
    if (root.getVisibility() != View.VISIBLE || root.getWidth() == 0) return;
    int[] position = new int[2];
    root.getLocationOnScreen(position);
    check(
        position[0] >= left && position[0] + root.getWidth() <= right,
        "view fits actual parent width: " + root.getClass().getSimpleName());
    if (root instanceof TextView) {
      TextView text = (TextView) root;
      android.text.Layout layout = text.getLayout();
      if (!(text instanceof EditText) && layout != null && layout.getLineCount() > 0) {
        int available =
            text.getWidth() - text.getCompoundPaddingLeft() - text.getCompoundPaddingRight();
        for (int line = 0; line < layout.getLineCount(); line++) {
          check(
              layout.getLineMax(line) <= available + 1,
              "text stays within field: " + text.getText());
          check(layout.getEllipsisCount(line) == 0, "text is not ellipsized: " + text.getText());
        }
        check(
            layout.getLineEnd(layout.getLineCount() - 1) == layout.getText().length(),
            "entire label laid out: " + text.getText());
        check(
            layout.getHeight()
                <= text.getHeight()
                    - text.getCompoundPaddingTop()
                    - text.getCompoundPaddingBottom(),
            "label height is not clipped: " + text.getText());
        if (text instanceof RadioButton)
          for (int line = 0; line + 1 < layout.getLineCount(); line++) {
            int end = layout.getLineEnd(line);
            CharSequence label = layout.getText();
            check(
                Character.isWhitespace(label.charAt(end - 1))
                    || end < label.length() && Character.isWhitespace(label.charAt(end)),
                "network mode only wraps between words");
          }
      }
      if (text instanceof EditText && text.getWidth() >= dp(288))
        check(
            text.getPaint().measureText(text.getText().toString())
                <= text.getWidth() - text.getCompoundPaddingLeft() - text.getCompoundPaddingRight(),
            "full IPv4 fits at supported phone width with enlarged font");
      if (text instanceof Button) check(text.getHeight() >= dp(48), "minimum touch height");
    }
    if (root instanceof ViewGroup) {
      ViewGroup group = (ViewGroup) root;
      for (int i = 0; i < group.getChildCount(); i++) layoutTree(group.getChildAt(i), left, right);
    }
  }

  private int dp(int value) {
    return Math.round(value * context.getResources().getDisplayMetrics().density);
  }

  private void layoutSnapshot(View root, String filename) {
    android.graphics.Bitmap bitmap =
        android.graphics.Bitmap.createBitmap(
            root.getWidth(), root.getHeight(), android.graphics.Bitmap.Config.ARGB_8888);
    try {
      root.draw(new android.graphics.Canvas(bitmap));
      File directory = new File(context.getExternalFilesDir(null), "layout-proof");
      check(directory.isDirectory() || directory.mkdirs(), "layout screenshot directory");
      try (FileOutputStream out = new FileOutputStream(new File(directory, filename))) {
        check(
            bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, out),
            "real layout screenshot saved");
      }
    } catch (IOException error) {
      throw new AssertionError(error);
    } finally {
      bitmap.recycle();
    }
  }

  private boolean scrollDialog(AccessibilityNodeInfo node) {
    if (node == null) return false;
    if ("android.widget.ScrollView".contentEquals(node.getClassName()) && node.isScrollable())
      return node.performAction(AccessibilityNodeInfo.ACTION_SCROLL_FORWARD);
    for (int i = 0; i < node.getChildCount(); i++) if (scrollDialog(node.getChild(i))) return true;
    return false;
  }

  private void dialogAction(String text, boolean activate) throws Exception {
    for (int attempt = 0; attempt < 20; attempt++) {
      AccessibilityNodeInfo root = getUiAutomation().getRootInActiveWindow();
      if (root != null) {
        for (AccessibilityNodeInfo node : root.findAccessibilityNodeInfosByText(text)) {
          android.graphics.Rect bounds = new android.graphics.Rect();
          node.getBoundsInScreen(bounds);
          if (node.isClickable() && node.isVisibleToUser() && bounds.height() >= dp(48)) {
            check(
                bounds.left >= 0
                    && bounds.right <= context.getResources().getDisplayMetrics().widthPixels,
                "dialog action fits viewport: " + text);
            if (activate)
              check(
                  node.performAction(AccessibilityNodeInfo.ACTION_CLICK),
                  "dialog action activated: " + text);
            Thread.sleep(150);
            return;
          }
        }
        scrollDialog(root);
      }
      Thread.sleep(150);
    }
    throw new AssertionError("Dialog action unreachable: " + text);
  }

  private void dialogSnapshot(String name) throws Exception {
    android.graphics.Bitmap image = getUiAutomation().takeScreenshot();
    check(image != null, "dialog screenshot captured");
    try (FileOutputStream out =
        new FileOutputStream(
            new File(
                context.getExternalFilesDir(null),
                "layout-proof/"
                    + name
                    + "-font"
                    + context.getResources().getConfiguration().fontScale
                    + ".png"))) {
      check(
          image.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, out),
          "dialog screenshot saved");
    } finally {
      image.recycle();
    }
  }

  private void layoutProof() throws Exception {
    check(shell("getprop ro.kernel.qemu").trim().equals("1"), "layout suite is emulator only");
    File screenshots = new File(context.getExternalFilesDir(null), "layout-proof");
    File[] previous = screenshots.listFiles();
    if (previous != null)
      for (File file : previous)
        if (file.isFile() && file.getName().endsWith(".png"))
          check(file.delete(), "old test screenshot removed");
    String hidden = shell("settings get global hidden_api_policy").trim();
    if (Build.VERSION.SDK_INT >= 28) shell("settings put global hidden_api_policy 1");
    Object application = context.getApplicationContext();
    Field field = application.getClass().getDeclaredField("inventory");
    field.setAccessible(true);
    UsbInventory original = (UsbInventory) field.get(application);
    UsbFixture fixture = new UsbFixture(context);
    UsbInventory[] replacement = new UsbInventory[1];
    try {
      stop();
      configure("192.168.123.123", 1);
      onMain(() -> activity.finish());
      waitForIdleSync();
      onMain(() -> {
        replacement[0] = new UsbInventory(fixture, fixture);
        replacement[0].bindComputers(ComputerRepository.get(context));
      });
      field.set(application, replacement[0]);
      context.getSharedPreferences("bridge", 0).edit().putBoolean("welcome_seen", true).commit();
      activity = startActivitySync(screen());
      UsbInventory inventory = replacement[0];
      UsbDevice key = UsbFixture.device(101, "USB-ключ", 11, false);
      UsbDevice mouse = UsbFixture.device(102, "Беспроводная мышь с USB-приёмником", 3, false);
      UsbDevice disk = UsbFixture.device(103, "Тестовый накопитель 64 ГБ", 8, false);
      onMain(() -> {
        fixture.connect(key, UsbFixture.descriptor(11, false));
        fixture.connect(mouse, UsbFixture.descriptor(3, false));
        fixture.connect(disk, UsbFixture.descriptor(8, false));
        inventory.select(
            key.getDeviceId(), inventory.item(key.getDeviceId()).generation, true, false);
        inventory.select(
            mouse.getDeviceId(), inventory.item(mouse.getDeviceId()).generation, true, false);
      });
      ready(inventory, 2);
      waitText("Беспроводная мышь");
      int surfaceId = context.getResources().getIdentifier("bridge_surface", "id", APP);
      View surface = activity.findViewById(surfaceId);
      check(surface != null, "responsive surface exists");
      String scale = Float.toString(context.getResources().getConfiguration().fontScale);
      for (int width : new int[] {280, 320, 360, 600}) {
        if (dp(width) > context.getResources().getDisplayMetrics().widthPixels) continue;
        onMain(() -> {
          ViewGroup.LayoutParams params = surface.getLayoutParams();
          params.width = dp(width);
          surface.setLayoutParams(params);
        });
        waitForIdleSync();
        onMain(() -> {
          check(surface.getWidth() == dp(width), "surface resized without recreating Activity");
          check(
              ComputerRepository.get(context).snapshot().active().host.equals("192.168.123.123"),
              "saved IP survives live resize");
          check(
              inventory.snapshot().size() == 3
                  && inventory.isSelected(key.getDeviceId())
                  && inventory.isSelected(mouse.getDeviceId())
                  && !inventory.isSelected(disk.getDeviceId()),
              "USB selection survives live resize");
          check(
              ((android.graphics.drawable.ColorDrawable) surface.getBackground()).getColor()
                  == 0xFFFAFBFC,
              "light surface also under system dark mode");
          int[] origin = new int[2];
          surface.getLocationOnScreen(origin);
          layoutTree(surface, origin[0], origin[0] + surface.getWidth());
          ScrollView scroll = (ScrollView) find(surface, ScrollView.class, null);
          scroll.scrollTo(0, 0);
          layoutSnapshot(surface, "width" + width + "-font" + scale + "-top.png");
          scroll.fullScroll(View.FOCUS_DOWN);
        });
        waitForIdleSync();
        onMain(() -> layoutSnapshot(surface, "width" + width + "-font" + scale + "-usb.png"));
      }
      chooseDevice("Тестовый накопитель");
      dialogAction("Извлечён · передать", false);
      dialogSnapshot("storage-consent");
      dialogAction("Отмена", true);
      check(
          !inventory.item(disk.getDeviceId()).approved, "cancel storage consent remains effective");
      chooseDevice("Тестовый накопитель");
      dialogAction("Извлечён · передать", true);
      ready(inventory, 3);
      Object owner = new Object();
      long generation = inventory.item(disk.getDeviceId()).generation;
      CheckBox diskChoice = (CheckBox) view(CheckBox.class, "Тестовый накопитель");
      onMain(() -> {
        check(inventory.acquire(disk.getDeviceId(), generation, owner), "synthetic storage lease");
        // No render can occur between lease acquisition and this click.
        diskChoice.performClick();
      });
      dialogAction("Извлечён · завершить", false);
      dialogSnapshot("storage-return");
      dialogAction("Отмена", true);
      check(inventory.isSelected(disk.getDeviceId()), "cancel return preserves lease selection");
      onMain(() -> inventory.released(disk.getDeviceId(), generation, owner));
      homePage();
      click("Настройки");
      click("Инструкция для компьютера");
      dialogAction("Копировать команды", false);
      dialogSnapshot("connection-help");
      dialogAction("Закрыть", true);
      phase(
          "responsive geometry: actual parent widths 280/320/360/600, full labels, USB fixtures,"
              + " live resize");
    } finally {
      onMain(() -> activity.finish());
      field.set(application, original);
      if (replacement[0] != null) onMain(() -> replacement[0].close());
      if (Build.VERSION.SDK_INT >= 28) {
        if (hidden.equals("null") || hidden.isEmpty())
          shell("settings delete global hidden_api_policy");
        else shell("settings put global hidden_api_policy " + hidden);
      }
    }
  }

  private void deviceUiAndInventory() throws Exception {
    String hidden = shell("settings get global hidden_api_policy").trim();
    if (Build.VERSION.SDK_INT >= 28) shell("settings put global hidden_api_policy 1");
    Object application = context.getApplicationContext();
    Field field = application.getClass().getDeclaredField("inventory");
    field.setAccessible(true);
    UsbInventory original = (UsbInventory) field.get(application);
    UsbFixture fixture = new UsbFixture(context);
    UsbInventory[] replacement = new UsbInventory[1];
    try {
      stop();
      configure(localAddress, 1);
      onMain(() -> activity.finish());
      waitForIdleSync();
      onMain(() -> {
        replacement[0] = new UsbInventory(fixture, fixture);
        replacement[0].bindComputers(ComputerRepository.get(context));
      });
      field.set(application, replacement[0]);
      UsbInventory inventory = replacement[0];
      activity = startActivitySync(screen());
      android.accessibilityservice.AccessibilityServiceInfo accessibility =
          getUiAutomation().getServiceInfo();
      accessibility.flags |=
          android.accessibilityservice.AccessibilityServiceInfo.FLAG_REPORT_VIEW_IDS;
      getUiAutomation().setServiceInfo(accessibility);
      waitText("Пока ничего не подключено");
      UsbDevice key = UsbFixture.device(101, "Test Key", 11, false);
      UsbDevice mouse = UsbFixture.device(102, "Test Mouse", 3, false);
      UsbDevice disk = UsbFixture.device(103, "Test Flash", 8, false);
      ByteArrayOutputStream fixtureBytes = new ByteArrayOutputStream();
      try (InputStream inputFixture =
          getContext().getAssets().open("rutoken-lite-descriptors.bin")) {
        byte[] block = new byte[256];
        int read;
        while ((read = inputFixture.read(block)) != -1) fixtureBytes.write(block, 0, read);
      }
      byte[] realTokenDescriptors = fixtureBytes.toByteArray();
      check(
          realTokenDescriptors.length == 111 && realTokenDescriptors[16] == 0,
          "serial-free physical descriptor regression asset");
      onMain(() -> {
        fixture.connect(key, realTokenDescriptors);
        fixture.connect(mouse, UsbFixture.descriptor(3, false));
        fixture.connect(disk, UsbFixture.descriptor(8, false));
      });
      waitText("Test Flash");
      check(inventory.snapshot().size() == 3, "three devices hotplug into real Activity");
      homePage();
      click("Настройки");
      click("Инструкция для компьютера");
      String emptyCommands = helpCommands();
      check(
          emptyCommands.contains("usbip list -r ") && !emptyCommands.contains("usbip attach"),
          "help does not share unselected or unapproved devices");
      copyHelp(emptyCommands);
      CheckBox all = (CheckBox) view(CheckBox.class, "Все и новые");
      onMain(() -> all.performClick());
      systemClick("Отмена");
      check(!inventory.all(), "storage confirmation cancel preserves selection");
      onMain(() -> all.performClick());
      UsbDevice lateDisk = UsbFixture.device(110, "Late Flash", 8, false);
      onMain(() -> fixture.connect(lateDisk, UsbFixture.descriptor(8, false)));
      systemClick("Извлечён");
      ready(inventory, 3);
      check(
          !inventory.isSelected(lateDisk.getDeviceId()),
          "consent does not include a disk attached after dialog opened");
      onMain(() -> fixture.disconnect(lateDisk));
      check(inventory.all(), "all mode selects approved existing devices");
      start("127.0.0.1");
      devlistCount(3);
      android.graphics.Bitmap capture = getUiAutomation().takeScreenshot();
      try (FileOutputStream out =
          new FileOutputStream(
              new File(context.getExternalFilesDir(null), "usb-selection-e2e.png"))) {
        check(
            capture.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, out),
            "device list screenshot saved");
      } finally {
        capture.recycle();
      }
      chooseDevice("Test Mouse");
      ready(inventory, 2);
      devlistCount(2);
      check(!inventory.all(), "partial selection exits all mode");
      chooseDevice("Test Key");
      ready(inventory, 1);
      devlistCount(1);
      chooseDevice("Test Mouse");
      ready(inventory, 2);
      long oldGeneration = inventory.item(disk.getDeviceId()).generation;
      onMain(() -> fixture.disconnect(disk));
      ready(inventory, 1);
      devlistCount(1);
      UsbDevice replug = UsbFixture.device(103, "Test Flash", 8, false);
      onMain(() -> fixture.connect(replug, UsbFixture.descriptor(8, false)));
      Thread.sleep(600);
      onMain(() -> inventory.select(replug.getDeviceId(), oldGeneration, true, true));
      check(
          !inventory.isSelected(replug.getDeviceId()),
          "reused ID not shared after detach in partial mode");
      onMain(() ->
          check(
              !inventory.state(replug.getDeviceId(), oldGeneration, new Object(), "late status"),
              "detached generation cannot overwrite replacement status"));
      final CheckBox selectAll = (CheckBox) view(CheckBox.class, "Все и новые");
      onMain(() -> selectAll.performClick());
      systemClick("Извлечён");
      ready(inventory, 3);
      devlistCount(3);
      UsbDevice extraMouse = UsbFixture.device(104, "New Mouse", 3, false);
      onMain(() -> fixture.connect(extraMouse, UsbFixture.descriptor(3, false)));
      ready(inventory, 4);
      devlistCount(4);
      UsbDevice extraDisk = UsbFixture.device(105, "New Flash", 8, false);
      onMain(() -> fixture.connect(extraDisk, UsbFixture.descriptor(8, false)));
      Thread.sleep(600);
      check(
          !inventory.isSelected(extraDisk.getDeviceId()),
          "new storage waits for eject confirmation in all mode");
      devlistCount(4);
      chooseDevice("New Flash");
      systemClick("Извлечён");
      ready(inventory, 5);
      devlistCount(5);
      homePage();
      click("Настройки");
      click("Инструкция для компьютера");
      String allCommands = helpCommands();
      for (UsbInventory.Item selected : inventory.snapshot())
        check(
            allCommands.contains(attachCommand(selected.device)),
            "help includes each approved waiting USB");
      // Change inventory while dialog is open. First copy refreshes, never copies stale bus IDs.
      onMain(() ->
          inventory.select(
              mouse.getDeviceId(), inventory.item(mouse.getDeviceId()).generation, false, false));
      systemClick("Копировать команды");
      homePage();
      String refreshedCommands = helpCommands();
      check(
          !refreshedCommands.contains(attachCommand(mouse)),
          "stale help refresh removes deselected USB");
      copyHelp(refreshedCommands);
      // Inventory changed directly while the help dialog was open. Await the
      // Activity's periodic render before toggling, rather than click stale state.
      waitChoice("Test Mouse", false);
      chooseDevice("Test Mouse");
      ready(inventory, 5);
      int[] leasedIds = {
        key.getDeviceId(), mouse.getDeviceId(), replug.getDeviceId(), extraMouse.getDeviceId()
      };
      Object[] owners = {new Object(), new Object(), new Object(), new Object()};
      for (int i = 0; i < leasedIds.length; i++) {
        final int n = i;
        onMain(() ->
            check(
                inventory.acquire(leasedIds[n], inventory.item(leasedIds[n]).generation, owners[n]),
                "cross-service USB lease acquired"));
      }
      homePage();
      click("Настройки");
      click("Инструкция для компьютера");
      String waitingCommands = helpCommands();
      check(
          waitingCommands.contains(attachCommand(extraDisk)),
          "help includes waiting unleased disk");
      for (int id : leasedIds)
        check(
            !waitingCommands.contains(" -b " + (id / 1000) + "-" + (id % 1000)),
            "help excludes devices already imported on PC");
      copyHelp(waitingCommands);
      long keyGeneration = inventory.item(key.getDeviceId()).generation;
      String leasedState = inventory.item(key.getDeviceId()).state;
      onMain(() -> {
        check(
            !inventory.state(key.getDeviceId(), keyGeneration, new Object(), "wrong owner"),
            "foreign owner cannot overwrite status");
        check(
            !inventory.state(key.getDeviceId(), keyGeneration, null, "null owner"),
            "null owner cannot overwrite status");
        check(
            !inventory.state(key.getDeviceId(), keyGeneration - 1, owners[0], "stale generation"),
            "stale generation cannot overwrite status");
        inventory.released(key.getDeviceId(), keyGeneration, null);
        check(inventory.item(key.getDeviceId()).inUse, "null release preserves live lease");
        check(
            inventory.item(key.getDeviceId()).state.equals(leasedState),
            "rejected callbacks leave status intact");
        check(
            inventory.state(key.getDeviceId(), keyGeneration, owners[0], "Подключено к компьютеру"),
            "current lease owner updates status");
      });
      click("Остановить");
      systemClick("Отмена");
      devlistCount(5);
      chooseDevice("Test Flash");
      systemClick("Отмена");
      check(
          inventory.isSelected(replug.getDeviceId()),
          "cancel PC eject prompt preserves selected storage");
      onMain(() ->
          check(
              !inventory.acquire(
                  extraDisk.getDeviceId(),
                  inventory.item(extraDisk.getDeviceId()).generation,
                  new Object()),
              "global four-device lease bound"));
      onMain(() ->
          inventory.released(
              key.getDeviceId(), inventory.item(key.getDeviceId()).generation, new Object()));
      check(
          !inventory.acquire(
              key.getDeviceId(), inventory.item(key.getDeviceId()).generation, new Object()),
          "wrong owner cannot release lease");
      for (int i = 0; i < leasedIds.length; i++) {
        final int n = i;
        onMain(() ->
            inventory.released(leasedIds[n], inventory.item(leasedIds[n]).generation, owners[n]));
      }
      ready(inventory, 4);
      Object failedOwner = new Object();
      long mouseGeneration = inventory.item(mouse.getDeviceId()).generation;
      onMain(() -> {
        check(
            inventory.acquire(mouse.getDeviceId(), mouseGeneration, failedOwner),
            "failed-import lease acquired");
        inventory.released(mouse.getDeviceId(), mouseGeneration, failedOwner, 4);
        check(
            inventory.item(mouse.getDeviceId()).notice == 4,
            "claim failure remains visible after lease release");
      });
      waitText("Интерфейс USB занят");
      chooseDevice("Test Mouse");
      chooseDevice("Test Mouse");
      ready(inventory, 4);
      check(
          inventory.item(mouse.getDeviceId()).notice == 0, "explicit retry clears failure notice");
      check(
          !inventory.item(replug.getDeviceId()).approved,
          "closing a disk lease revokes old eject consent");
      check(
          inventory.item(replug.getDeviceId()).notice == 1,
          "disk release retains visible handoff explanation");
      waitText("Передача завершена");
      chooseDevice("Test Flash");
      systemClick("Извлечён");
      ready(inventory, 5);
      check(inventory.item(replug.getDeviceId()).notice == 0, "new consent clears old notice");
      chooseDevice("New Flash");
      ready(inventory, 4);
      check(!inventory.item(extraDisk.getDeviceId()).approved, "deselect revokes disk consent");
      chooseDevice("New Flash");
      systemClick("Извлечён");
      ready(inventory, 5);
      onMain(() -> selectAll.performClick());
      ready(inventory, 5);
      UsbDevice hub = UsbFixture.device(106, "Test Hub", 9, false),
          iso = UsbFixture.device(107, "Test ISO", 1, true);
      onMain(() -> {
        fixture.connect(hub, UsbFixture.descriptor(9, false));
        fixture.connect(iso, UsbFixture.descriptor(1, true));
      });
      Thread.sleep(700);
      check(
          !inventory.isSelected(hub.getDeviceId()) && !inventory.isSelected(iso.getDeviceId()),
          "unsupported hub and ISO excluded from all mode");
      CheckBox hubChoice = (CheckBox) view(CheckBox.class, "Test Hub");
      onMain(() -> check(!hubChoice.isEnabled(), "unsupported device row disabled"));
      devlistCount(5);
      onMain(() -> selectAll.performClick());
      ready(inventory, 0);
      devlistCount(0);
      fixture.autoGrant = false;
      UsbDevice denied = UsbFixture.device(108, "Denied Mouse", 3, false);
      onMain(() -> fixture.connect(denied, UsbFixture.descriptor(3, false)));
      Thread.sleep(600);
      int before = fixture.requests.size();
      chooseDevice("Denied Mouse");
      Thread.sleep(300);
      check(fixture.requests.size() == before + 1, "one permission request per selection");
      UsbFixture.Request request = fixture.requests.get(fixture.requests.size() - 1);
      onMain(() -> fixture.reply(request, false));
      Thread.sleep(600);
      check(!inventory.authorized(denied.getDeviceId()), "permission denial blocks export");
      devlistCount(0);
      check(fixture.requests.size() == before + 1, "denial does not create a dialog loop");
      chooseDevice("Denied Mouse");
      chooseDevice("Denied Mouse");
      Thread.sleep(300);
      UsbFixture.Request retry = fixture.requests.get(fixture.requests.size() - 1);
      onMain(() -> fixture.reply(retry, true));
      ready(inventory, 1);
      devlistCount(1);
      UsbDevice pending = UsbFixture.device(109, "Pending Mouse", 3, false);
      onMain(() -> fixture.connect(pending, UsbFixture.descriptor(3, false)));
      Thread.sleep(600);
      chooseDevice("Pending Mouse");
      Thread.sleep(300);
      UsbFixture.Request stale = fixture.requests.get(fixture.requests.size() - 1);
      onMain(() -> fixture.disconnect(pending));
      UsbDevice reused = UsbFixture.device(109, "Replacement Mouse", 3, false);
      onMain(() -> {
        fixture.connect(reused, UsbFixture.descriptor(3, false));
        fixture.reply(stale, true);
      });
      Thread.sleep(600);
      check(
          !inventory.isSelected(reused.getDeviceId())
              && !inventory.authorized(reused.getDeviceId()),
          "stale permission callback cannot authorize reused USB ID");
      devlistCount(1);
      chooseDevice("Test Flash");
      systemClick("Извлечён");
      ready(inventory, 2);
      stop();
      check(
          !inventory.item(replug.getDeviceId()).approved
              && !inventory.isSelected(replug.getDeviceId()),
          "stopping service revokes disk handoff consent");
      // The service must retain its computer until an already admitted USB open
      // returns, even after TCP has been stopped and the service record disappears.
      start("127.0.0.1");
      ready(inventory, 1); // Denied Mouse was explicitly granted on retry above.
      fixture.openEntered = new CountDownLatch(1);
      fixture.openRelease = new CountDownLatch(1);
      try (Socket importing = connect()) {
        importing.getOutputStream().write(header(0x8003, 0));
        byte[] bus = new byte[32];
        byte[] id =
            ((denied.getDeviceId() / 1000) + "-" + (denied.getDeviceId() % 1000))
                .getBytes(StandardCharsets.US_ASCII);
        System.arraycopy(id, 0, bus, 0, id.length);
        importing.getOutputStream().write(bus);
        check(
            fixture.openEntered.await(5, TimeUnit.SECONDS),
            "real USB/IP import reached blocked Android open");
        String activeBefore = ComputerRepository.get(context).snapshot().activeId;
        onMain(() -> context.stopService(service()));
        waitForIdleSync();
        check(
            ComputerRepository.get(context).isBusy(),
            "stopping service retains computer during admitted open");
        try {
          configure("192.168.12.99", 1);
          throw new AssertionError("computer changed during pending USB open");
        } catch (ExecutionException expected) {
          checks++;
        }
        check(
            activeBefore.equals(ComputerRepository.get(context).snapshot().activeId),
            "failed switch preserves original identity");
      } finally {
        fixture.openRelease.countDown();
      }
      stop();
      check(
          !fixture.openGateTimedOut && !ComputerRepository.get(context).isBusy(),
          "USB open returned before ownership was released");
      fixture.openEntered = null;
      fixture.openRelease = null;
      configure(localAddress, 1);
      phase("actual service stop blocks computer replacement until in-flight USB open returns");
      phase(
          "real Activity + injected USB adapter: key/mouse/storage, hotplug, all/partial, eject"
              + " confirmation, permissions and stale callbacks");
    } finally {
      try {
        stop();
      } finally {
        onMain(() -> activity.finish());
        field.set(application, original);
        if (replacement[0] != null) onMain(() -> replacement[0].close());
        if (Build.VERSION.SDK_INT >= 28) {
          if (hidden.equals("null") || hidden.isEmpty())
            shell("settings delete global hidden_api_policy");
          else shell("settings put global hidden_api_policy " + hidden);
        }
        activity = startActivitySync(screen());
      }
    }
  }

  private void protocol() throws Exception {
    start("127.0.0.1");
    devlist(false);
    devlist(true);
    for (byte[] invalid : new byte[][] {header(0x8005, 1), header(0xffff, 0), header(0, 0)}) {
      try (Socket s = connect()) {
        s.getOutputStream().write(invalid);
        closed(s, "bad handshake closed");
      }
      devlist(false);
    }
    for (int length = 0; length < 8; length++) {
      try (Socket s = connect()) {
        s.getOutputStream().write(header(0x8005, 0), 0, length);
        s.shutdownOutput();
        closed(s, "truncated handshake");
      }
    }
    for (int length : new int[] {0, 1, 31}) {
      try (Socket s = connect()) {
        s.getOutputStream().write(header(0x8003, 0));
        s.getOutputStream().write(new byte[length]);
        s.shutdownOutput();
        closed(s, "truncated import");
      }
    }
    try (Socket s = connect()) {
      s.getOutputStream().write(header(0x8003, 0));
      s.getOutputStream().write(new byte[32]);
      byte[] reply = new byte[8];
      new DataInputStream(s.getInputStream()).readFully(reply);
      check(
          ByteBuffer.wrap(reply).getShort(2) == 3 && ByteBuffer.wrap(reply).getInt(4) != 0,
          "missing USB import reports failure");
      closed(s, "failed import closes");
    }
    ExecutorService pool = Executors.newFixedThreadPool(3);
    try {
      List<Future<?>> pending = new ArrayList<>();
      for (int t = 0; t < 3; t++)
        pending.add(
            pool.submit(() -> {
              for (int i = 0; i < 10; i++)
                try {
                  devlist(false);
                } catch (Exception e) {
                  throw new RuntimeException(e);
                }
            }));
      for (Future<?> f : pending) f.get(20, TimeUnit.SECONDS);
    } finally {
      pool.shutdownNow();
    }
    Thread.sleep(300);
    List<Socket> held = new ArrayList<>();
    try {
      for (int i = 0; i < 8; i++) {
        held.add(connect());
        Thread.sleep(100);
      }
      try (Socket excess = connect()) {
        closed(excess, "ninth active connection rejected");
      }
    } finally {
      for (Socket s : held) s.close();
    }
    Thread.sleep(300);
    devlist(false);
    try (Socket slow = connect()) {
      slow.setSoTimeout(35000);
      slow.getOutputStream().write(1);
      long start = SystemClock.elapsedRealtime();
      closed(slow, "partial handshake timeout");
      check(SystemClock.elapsedRealtime() - start >= 25000, "timeout not premature");
    }
    devlist(false);
    java.util.Set<Long> previousReaders = new java.util.HashSet<>();
    for (Thread thread : Thread.getAllStackTraces().keySet())
      if (thread.getName().equals("usbip-receiver")) previousReaders.add(thread.getId());
    try (Socket pending = connect()) {
      pending.getOutputStream().write(1);
      // TCP connect may finish in the emulator's NAT backlog before accept().
      // This case checks an admitted pending read; the separate host fixture
      // deterministically covers stop racing accept/registration under its gate.
      boolean admitted = false;
      long admissionDeadline = SystemClock.elapsedRealtime() + 5000;
      while (!admitted && SystemClock.elapsedRealtime() < admissionDeadline) {
        for (Thread thread : Thread.getAllStackTraces().keySet())
          if (thread.getName().equals("usbip-receiver")
              && !previousReaders.contains(thread.getId())
              && thread.isAlive()) admitted = true;
        if (!admitted) Thread.sleep(10);
      }
      check(admitted, "pending read admitted before service stop");
      stop();
      closed(pending, "stop closes pending read");
    }
    for (int i = 0; i < 20; i++) {
      start("127.0.0.1");
      onMain(() -> context.startForegroundService(service()));
      devlist(false);
      stop();
    }
    phase("USB/IP framing, malformed/partial requests, concurrency cap, timeout, restart");
  }

  private void lifecycle() throws Exception {
    start("127.0.0.1");
    onMain(() -> context.startForegroundService(service()));
    devlist(false);
    ActivityMonitor monitor = addMonitor(ACTIVITY, null, false);
    try {
      int opposite =
          activity.getResources().getConfiguration().orientation
                  == android.content.res.Configuration.ORIENTATION_LANDSCAPE
              ? ActivityInfo.SCREEN_ORIENTATION_PORTRAIT
              : ActivityInfo.SCREEN_ORIENTATION_LANDSCAPE;
      onMain(() -> activity.setRequestedOrientation(opposite));
      Activity replacement = monitor.waitForActivityWithTimeout(10000);
      check(replacement != null, "Activity recreated on rotation");
      activity = replacement;
      waitText("Готово.");
      devlist(false);
    } finally {
      removeMonitor(monitor);
    }
    shell("input keyevent KEYCODE_HOME");
    Thread.sleep(500);
    devlist(false);
    shell("input keyevent KEYCODE_SLEEP");
    Thread.sleep(500);
    devlist(false);
    shell("input keyevent KEYCODE_WAKEUP");
    shell("wm dismiss-keyguard");
    resumeScreen();
    waitText("Готово.");
    devlist(false);
    check(heldWakeLocks().contains("RutokenBridge:service"), "foreground service holds wake lock");
    stop();
    waitText("Служба остановлена");
    check(!heldWakeLocks().contains("RutokenBridge:service"), "wake lock released");
    try (ServerSocket occupied = new ServerSocket(3240)) {
      onMain(() -> context.startForegroundService(service()));
      waitText("Не удалось открыть порт");
    }
    start("127.0.0.1");
    devlist(false);
    stop();
    phase("rotation, background, screen off/on, repeated start, locks, occupied-port recovery");
  }

  private void logs() throws Exception {
    File log = new File(context.getFilesDir(), "usbip.log");
    // Seed near the cap: the next real service event must rotate rather than grow.
    try (FileOutputStream out = new FileOutputStream(log)) {
      out.write(new byte[256 * 1024 - 8]);
    }
    start("127.0.0.1");
    stop();
    check(log.length() > 0 && log.length() <= 256 * 1024, "current log bounded");
    check(
        new File(context.getFilesDir(), "usbip.previous.log").length() == 256 * 1024 - 8,
        "real event rotates log");
    // Remove only synthetic fixture; exported output must remain useful text.
    check(new File(context.getFilesDir(), "usbip.previous.log").delete(), "fixture removed");
    homePage();
    click("Настройки");
    click("Сохранить журнал");
    Thread.sleep(700);
    boolean documentUi = false;
    for (int i = 0; i < 50 && !documentUi; i++) {
      AccessibilityNodeInfo root = getUiAutomation().getRootInActiveWindow();
      documentUi =
          root != null
              && root.getPackageName() != null
              && root.getPackageName().toString().contains("documentsui");
      if (!documentUi) Thread.sleep(100);
    }
    check(documentUi, "system document picker opened");
    getUiAutomation()
        .performGlobalAction(android.accessibilityservice.AccessibilityService.GLOBAL_ACTION_BACK);
    Thread.sleep(500);
    resumeScreen();
    waitText("Сохранить журнал");
    phase("log rotation and document export cancellation (file export checked by host UI runner)");
  }

  @Override
  public void onStart() {
    Bundle result = new Bundle();
    int code = Activity.RESULT_CANCELED;
    context = getTargetContext();
    try {
      java.util.Enumeration<NetworkInterface> interfaces = NetworkInterface.getNetworkInterfaces();
      while (interfaces.hasMoreElements()) {
        java.util.Enumeration<InetAddress> ips = interfaces.nextElement().getInetAddresses();
        while (ips.hasMoreElements()) {
          InetAddress ip = ips.nextElement();
          if (ip instanceof Inet4Address && ip.isSiteLocalAddress() && !ip.isLoopbackAddress())
            localAddress = ip.getHostAddress();
        }
      }
      if (localAddress == null) throw new AssertionError("Emulator private IPv4 required");
    } catch (SocketException error) {
      throw new AssertionError(error);
    }
    try {
      check(Build.VERSION.SDK_INT >= 26, "supported API");
      if (computerStoreScenario != null) {
        checks += new ComputerStoreSuite(this).run(computerStoreScenario);
      } else if (migrationStage != null) {
        // Seeding must precede the first Activity/repository access in a fresh process.
        checks += new DiagnosticsSuite(this).runMigration(migrationStage);
      } else if (liveProfilesStage != null) {
        checks += new DiagnosticsSuite(this).runLiveProfiles(liveProfilesStage);
      } else {
        activityCallbacks =
            new Application.ActivityLifecycleCallbacks() {
              public void onActivityResumed(Activity a) {
                if (a.getPackageName().equals(APP)) activity = a;
              }

              public void onActivityCreated(Activity a, Bundle b) {}

              public void onActivityStarted(Activity a) {}

              public void onActivityPaused(Activity a) {}

              public void onActivityStopped(Activity a) {}

              public void onActivitySaveInstanceState(Activity a, Bundle b) {}

              public void onActivityDestroyed(Activity a) {}
            };
        ((Application) context.getApplicationContext())
            .registerActivityLifecycleCallbacks(activityCallbacks);
        activity = startActivitySync(screen());
        check(activity != null, "release Activity launches");
        stop();
        if (navigationOnly) {
          onMain(() -> activity.finish());
          checks += new NavigationSuite(this).run();
        } else if (quickActionsKeyguard) {
          onMain(() -> activity.finish());
          checks += new QuickActionsSuite(this).keyguardChecks();
        } else if (quickActionsUi) {
          onMain(() -> activity.finish());
          checks += new QuickActionsSuite(this).interfaceChecks();
        } else if (quickActionsResume) {
          onMain(() -> activity.finish());
          checks += new QuickActionsSuite(this).resume();
        } else if (quickActionsOnly) {
          onMain(() -> activity.finish());
          checks += new QuickActionsSuite(this).run();
        } else if (usbRulesResume) {
          onMain(() -> activity.finish());
          checks += new UsbRulesSuite(this).resume();
        } else if (accessOnly || accessResume || accessEndpoints || accessEndpointsResume) {
          onMain(() -> activity.finish());
          AccessSuite suite = new AccessSuite(this);
          checks += accessEndpoints || accessEndpointsResume
              ? suite.endpoints(accessEndpointsResume) : accessResume ? suite.resume() : suite.run();
        } else if (diagnosticsOnly) {
          onMain(() -> activity.finish());
          checks += new DiagnosticsSuite(this).run();
        } else if (osEjectionOnly) {
          onMain(() -> activity.finish());
          checks += new OsEjectionSuite(this).run();
        } else if (managedEjectionOnly) {
          onMain(() -> activity.finish());
          checks += new ManagedEjectionSuite(this).run();
        } else if (usbRulesOnly) {
          onMain(() -> activity.finish());
          checks += new UsbRulesSuite(this).run(usbTransition);
        } else if (keyImportOnly) {
          checks += new KeyImportSuite(this).run();
        } else if (transportOnly) {
          checks += new TransportSuite(this).run();
        } else if (privacyOnly) {
          checks += new PrivacySuite(this).run(activity);
        } else if (licensesOnly) {
          checks += new LicensesSuite(this).run();
        } else if (connectionLayout) {
          checks += new ConnectionLayoutSuite(this).run();
        } else if (pairingOnly) {
          checks += new PairingSuite(this).run(cameraDenied, cameraQr);
        } else if (connectionOnly) {
          connectionJni();
          checks += new ConnectionSuite(this, activity).run(retainConnectionFixture);
        } else if (layoutOnly) {
          layoutProof();
        } else {
          check(
              shell("getprop ro.kernel.qemu").trim().equals("1"),
              "general fixture owns emulator only");
          ComputerRepository.get(context).ready().get(30, TimeUnit.SECONDS);
          ComputerRepository.get(context).reset().get(30, TimeUnit.SECONDS);
          jni();
          connectionJni();
          View later = view(Button.class, "Позже");
          if (later != null && later.isShown()) click("Позже");
          validation();
          networkAndSelectionUi();
          deviceUiAndInventory();
          protocol();
          diagnostics();
          wanNegotiation();
          lifecycle();
          logs();
        }
      }
      result.putString(
          "stream",
          "PASS optimized release API " + Build.VERSION.SDK_INT + ": " + checks + " checks\n");
      code = Activity.RESULT_OK;
    } catch (Throwable error) {
      StringWriter trace = new StringWriter();
      error.printStackTrace(new PrintWriter(trace));
      result.putString("stream", progress + "FAIL " + trace);
    } finally {
      try {
        // Even stop() queries the repository; a v1 seed must remain untouched
        // until the next process runs the real migration.
        if (!"seed".equals(migrationStage)) stop();
      } catch (Exception ignored) {
      }
      if (activityCallbacks != null)
        ((Application) context.getApplicationContext())
            .unregisterActivityLifecycleCallbacks(activityCallbacks);
      shellQuiet("input keyevent KEYCODE_WAKEUP");
    }
    finish(code, result);
  }

  private void shellQuiet(String command) {
    try {
      shell(command);
    } catch (Exception ignored) {
    }
  }
}
