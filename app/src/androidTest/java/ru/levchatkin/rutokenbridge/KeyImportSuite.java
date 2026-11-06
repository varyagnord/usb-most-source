package ru.levchatkin.rutokenbridge;

import android.app.*;
import android.content.*;
import android.net.Uri;
import android.os.*;
import android.view.*;
import android.widget.*;
import java.io.*;
import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;
import java.util.*;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicReference;
import org.cgutman.usbip.connection.ComputerRepository;
import org.json.JSONObject;

/** Real resolver/file/pipe I/O and optimized Activity lifecycle, owned emulator only. */
final class KeyImportSuite {
  private static final String APP = "ru.levchatkin.rutokenbridge";
  private static final String SETUP = "org.cgutman.usbip.connection.ConnectionSetup";
  private final Instrumentation test;
  private final Context context;
  private final AtomicReference<Activity> resumed = new AtomicReference<>();
  private final List<File> files = new ArrayList<>();
  private final List<Activity> activities = new ArrayList<>();
  private int checks;

  KeyImportSuite(Instrumentation test) {
    this.test = test;
    context = test.getTargetContext();
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
    if (failure[0] != null) throw new AssertionError("key import UI", failure[0]);
  }

  private View find(View root, String text) {
    if (root instanceof TextView && ((TextView) root).getText().toString().equals(text))
      return root;
    if (root instanceof ViewGroup)
      for (int i = 0; i < ((ViewGroup) root).getChildCount(); i++) {
        View found = find(((ViewGroup) root).getChildAt(i), text);
        if (found != null) return found;
      }
    return null;
  }

  private boolean ready(Activity activity) {
    boolean[] ready = {false};
    main(() -> {
      View save = find(activity.getWindow().getDecorView(), "Сохранить компьютер");
      ready[0] = save != null && save.isEnabled();
    });
    return ready[0];
  }

  private void awaitReady(Activity activity) throws Exception {
    long until = SystemClock.elapsedRealtime() + 15000;
    while (!ready(activity) && SystemClock.elapsedRealtime() < until) Thread.sleep(50);
    check(ready(activity), "editor becomes responsive after key operation");
  }

  private boolean chosen(Activity activity) {
    boolean[] found = {false};
    main(() -> found[0] = find(activity.getWindow().getDecorView(), "SSH-ключ выбран") != null);
    return found[0];
  }

  private void click(Activity activity, String text) {
    main(() -> {
      View view = find(activity.getWindow().getDecorView(), text);
      check(view != null && view.isEnabled() && view.performClick(), "UI control: " + text);
    });
  }

  private Activity launch() throws Exception {
    Activity activity =
        test.startActivitySync(
            new Intent()
                .setClassName(APP, SETUP)
                .putExtra("initial_transport", 0)
                .putExtra("computer_revision", ComputerRepository.get(context).snapshot().revision)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
    activities.add(activity);
    resumed.set(activity);
    awaitReady(activity);
    click(activity, "Ключ и дополнительные параметры");
    return activity;
  }

  private void deliver(Activity activity, Uri uri) {
    long started = SystemClock.elapsedRealtime();
    main(() -> {
      try {
        Method result =
            activity
                .getClass()
                .getDeclaredMethod("onActivityResult", int.class, int.class, Intent.class);
        result.setAccessible(true);
        result.invoke(activity, 24, Activity.RESULT_OK, new Intent().setData(uri));
      } catch (Exception error) {
        throw new AssertionError(error);
      }
    });
    check(
        SystemClock.elapsedRealtime() - started < 1500,
        "file result returns without blocking UI on provider I/O");
  }

  private Uri file(byte[] contents) throws Exception {
    File file = File.createTempFile("key-import-fixture-", ".key", context.getCacheDir());
    files.add(file);
    try (FileOutputStream output = new FileOutputStream(file)) {
      output.write(contents);
    }
    return Uri.fromFile(file);
  }

  private static Uri pipeUri(ParcelFileDescriptor[] pipe) {
    return Uri.fromFile(new File("/proc/self/fd/" + pipe[0].getFd()));
  }

  private static void closePipe(ParcelFileDescriptor[] pipe) {
    for (ParcelFileDescriptor fd : pipe)
      try {
        fd.close();
      } catch (IOException ignored) {
      }
  }

  private static void writePipe(ParcelFileDescriptor[] pipe, byte[] value) throws Exception {
    try (OutputStream output = new ParcelFileDescriptor.AutoCloseOutputStream(pipe[1])) {
      output.write(value);
    }
  }

  private void set(Activity a, String field, String value) {
    main(() -> ((EditText) a.getWindow().getDecorView().findViewWithTag(field)).setText(value));
  }

  int run() throws Exception {
    try (InputStream input =
        new ParcelFileDescriptor.AutoCloseInputStream(
            test.getUiAutomation().executeShellCommand("getprop ro.kernel.qemu"))) {
      ByteArrayOutputStream out = new ByteArrayOutputStream();
      byte[] part = new byte[64];
      int count;
      while ((count = input.read(part)) != -1) out.write(part, 0, count);
      check(out.toString("UTF-8").trim().equals("1"), "owned emulator only");
    }
    byte[] key;
    try (InputStream input = test.getContext().getAssets().open("receiver-fixture.json")) {
      ByteArrayOutputStream output = new ByteArrayOutputStream();
      byte[] part = new byte[4096];
      int count;
      while ((count = input.read(part)) != -1) {
        if (output.size() + count > 131072) throw new IOException("fixture size");
        output.write(part, 0, count);
      }
      JSONObject fixture = new JSONObject(output.toString("UTF-8"));
      check(fixture.getString("scope").equals("isolated-QEMU-only"), "synthetic SSH key only");
      key = (fixture.getString("private_key") + "\n").getBytes(StandardCharsets.UTF_8);
    }
    ComputerRepository repo = ComputerRepository.get(context);
    repo.ready().get(30, TimeUnit.SECONDS);
    repo.reset().get(30, TimeUnit.SECONDS);
    Application app = (Application) context.getApplicationContext();
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
    main(() -> app.registerActivityLifecycleCallbacks(callbacks));
    try {
      Activity current = launch();
      deliver(current, file("old-key-fixture".getBytes(StandardCharsets.UTF_8)));
      awaitReady(current);
      check(chosen(current), "ordinary file URI imported");
      ParcelFileDescriptor[] pipe = ParcelFileDescriptor.createPipe();
      try {
        deliver(current, pipeUri(pipe));
        Thread.sleep(150);
        check(!ready(current), "waiting stream disables save without blocking main thread");
        Activity old = current;
        main(old::recreate);
        long until = SystemClock.elapsedRealtime() + 10000;
        while ((resumed.get() == old || resumed.get() == null)
            && SystemClock.elapsedRealtime() < until) Thread.sleep(50);
        current = resumed.get();
        check(
            current != null && current != old && current.getClass().getName().equals(SETUP),
            "form recreated during read");
        activities.add(current);
        check(!ready(current), "rotation retains pending import");
        writePipe(pipe, key);
        awaitReady(current);
        check(chosen(current), "late key delivered to current form once");
      } finally {
        closePipe(pipe);
      }
      for (byte[] invalid : new byte[][] {new byte[0], new byte[65537]}) {
        deliver(current, file(invalid));
        awaitReady(current);
        check(chosen(current), "failed import keeps previously selected key");
      }
      deliver(
          current,
          Uri.fromFile(new File(context.getCacheDir(), "missing-key-" + UUID.randomUUID())));
      awaitReady(current);
      check(chosen(current), "missing provider file does not erase prior key");
      set(current, "Адрес компьютера", "127.0.0.1");
      set(current, "Имя пользователя", "fixture");
      click(current, "Сохранить компьютер");
      long until = SystemClock.elapsedRealtime() + 15000;
      while (repo.snapshot().activeId == null && SystemClock.elapsedRealtime() < until)
        Thread.sleep(50);
      check(repo.snapshot().activeId != null, "imported key can be saved");
      byte[] saved =
          repo.edit(repo.snapshot().activeId, repo.snapshot().revision).get(30, TimeUnit.SECONDS);
      try {
        String actual =
            new JSONObject(new String(saved, StandardCharsets.UTF_8))
                .getJSONObject("connection")
                .getJSONObject("credentials")
                .getString("private_key");
        check(
            Arrays.equals(actual.getBytes(StandardCharsets.UTF_8), key),
            "exact rotated key persisted; failed imports never replaced it");
      } finally {
        Arrays.fill(saved, (byte) 0);
      }

      current = launch();
      pipe = ParcelFileDescriptor.createPipe();
      try {
        deliver(current, pipeUri(pipe));
        Thread.sleep(150);
        check(!ready(current), "cancellation fixture is still reading");
        click(current, "Назад");
        Activity fresh = launch();
        try {
          writePipe(pipe, key);
        } catch (IOException expectedClosed) {
        }
        Thread.sleep(300);
        check(ready(fresh) && !chosen(fresh), "cancelled import cannot populate a different form");
        click(fresh, "Назад");
      } finally {
        closePipe(pipe);
      }

      current = launch();
      pipe = ParcelFileDescriptor.createPipe();
      try {
        long start = SystemClock.elapsedRealtime();
        deliver(current, pipeUri(pipe));
        Thread.sleep(150);
        check(!ready(current), "timeout fixture has a live empty stream");
        awaitReady(current);
        long elapsed = SystemClock.elapsedRealtime() - start;
        check(
            elapsed >= 8500 && elapsed < 15000,
            "provider timeout bounded independently of read completion");
        check(!chosen(current), "timeout does not accept partial key");
      } finally {
        closePipe(pipe);
      }
      // An exact-limit file is accepted; rejection is strictly beyond the limit.
      // Retry after timeout also proves that a late cancelled read cannot win.
      byte[] limitKey = new byte[65536];
      Arrays.fill(limitKey, (byte) 'x');
      deliver(current, file(limitKey));
      awaitReady(current);
      check(chosen(current), "exact 64 KiB accepted after timeout");
      click(current, "Назад");
      List<ParcelFileDescriptor[]> cancelled = new ArrayList<>();
      try {
        for (int attempt = 0; attempt < 6; attempt++) {
          current = launch();
          ParcelFileDescriptor[] held = ParcelFileDescriptor.createPipe();
          cancelled.add(held);
          deliver(current, pipeUri(held));
          Thread.sleep(75);
          // The provider may interrupt read on close, or keep it blocked. Both
          // outcomes are valid; repeated cancellation must keep UI responsive.
          click(current, "Назад");
        }
        current = launch();
        check(ready(current) && !chosen(current), "repeated cancelled reads cannot leak keys");
      } finally {
        for (ParcelFileDescriptor[] held : cancelled) closePipe(held);
      }
      int readers = 0, closers = 0;
      for (Thread thread : Thread.getAllStackTraces().keySet())
        if (thread.isAlive()) {
          if (thread.getName().equals("usb-most-key-read")) readers++;
          if (thread.getName().equals("usb-most-key-close")) closers++;
        }
      check(readers <= 2 && closers <= 2, "provider workers remain bounded");
      check(repo.snapshot().computers.size() == 1, "cancel and timeout cannot create computers");
      return checks;
    } finally {
      Arrays.fill(key, (byte) 0);
      main(() -> {
        for (Activity activity : activities) if (!activity.isDestroyed()) activity.finish();
        app.unregisterActivityLifecycleCallbacks(callbacks);
      });
      for (File file : files) check(file.delete(), "only owned key fixture removed");
      repo.reset().get(30, TimeUnit.SECONDS);
    }
  }
}
