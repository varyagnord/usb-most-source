package ru.levchatkin.rutokenbridge;

import android.app.*;
import android.content.*;
import android.graphics.*;
import android.os.*;
import android.view.*;
import android.widget.*;
import java.io.*;
import java.util.*;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicReference;
import org.cgutman.usbip.connection.ComputerRepository;
import org.json.JSONObject;

/** Real optimized Activity navigation, lifecycle and visible layout on owned emulators. */
final class NavigationSuite {
  private static final String APP = "ru.levchatkin.rutokenbridge";
  private static final String MAIN = "org.cgutman.usbip.config.UsbIpConfig";
  private final Instrumentation test;
  private final Context context;
  private final AtomicReference<Activity> resumed = new AtomicReference<>();
  private int checks;
  private final List<String> scenarios = new ArrayList<>();

  NavigationSuite(Instrumentation test) {
    this.test = test;
    context = test.getTargetContext();
  }

  private void check(boolean ok, String message) {
    if (!ok) throw new AssertionError(message);
    checks++;
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
    if (failure[0] != null) throw new AssertionError("navigation UI", failure[0]);
    test.waitForIdleSync();
  }

  private View find(View v, Class<?> type, String text) {
    if (!v.isShown()) return null;
    if (type.isInstance(v)
        && (text == null
            || v instanceof TextView && ((TextView) v).getText().toString().equals(text))) return v;
    if (v instanceof ViewGroup)
      for (int i = 0; i < ((ViewGroup) v).getChildCount(); i++) {
        View found = find(((ViewGroup) v).getChildAt(i), type, text);
        if (found != null) return found;
      }
    return null;
  }

  private View visible(Class<?> type, String text) {
    View[] found = {null};
    main(() -> {
      Activity a = resumed.get();
      if (a != null) found[0] = find(a.getWindow().getDecorView(), type, text);
    });
    return found[0];
  }

  private View await(Class<?> type, String text) throws Exception {
    for (int i = 0; i < 60; i++) {
      View found = visible(type, text);
      if (found != null) return found;
      Thread.sleep(100);
    }
    throw new AssertionError("Visible control missing: " + text);
  }

  private void click(String text) throws Exception {
    View v = await(Button.class, text);
    main(() -> check(v.isShown() && v.isEnabled() && v.performClick(), "visible click: " + text));
    Thread.sleep(100);
  }

  private EditText input(String tag) throws Exception {
    await(EditText.class, null);
    View[] found = {null};
    main(() -> found[0] = resumed.get().getWindow().getDecorView().findViewWithTag(tag));
    check(found[0] instanceof EditText, "tagged input: " + tag);
    return (EditText) found[0];
  }

  private android.view.accessibility.AccessibilityNodeInfo accessible(
      android.view.accessibility.AccessibilityNodeInfo root, String text) {
    if (root == null) return null;
    if (root.isClickable()
        && ((root.getText() != null && root.getText().toString().equalsIgnoreCase(text))
            || (root.getContentDescription() != null
                && root.getContentDescription().toString().equals(text)))) return root;
    for (int i = 0; i < root.getChildCount(); i++) {
      android.view.accessibility.AccessibilityNodeInfo found = accessible(root.getChild(i), text);
      if (found != null) return found;
    }
    return null;
  }

  private void dialogClick(String title) throws Exception {
    for (int i = 0; i < 60; i++) {
      android.view.accessibility.AccessibilityNodeInfo root =
          test.getUiAutomation().getRootInActiveWindow();
      android.view.accessibility.AccessibilityNodeInfo found = accessible(root, title);
      if (found != null) {
        check(
            found.performAction(android.view.accessibility.AccessibilityNodeInfo.ACTION_CLICK),
            "accessible click: " + title);
        Thread.sleep(200);
        return;
      }
      Thread.sleep(100);
    }
    throw new AssertionError("dialog control missing: " + title);
  }

  private void multipleComputers(ComputerRepository repo) throws Exception {
    String first = repo.snapshot().activeId;
    click("Изменить компьютер");
    click("Добавить компьютер");
    Spinner transport = (Spinner) await(Spinner.class, null);
    main(() -> transport.setSelection(2));
    RadioButton lan = (RadioButton) await(RadioButton.class, "Локальная сеть");
    EditText name = input("Название компьютера · необязательно"),
        address = input("Адрес компьютера");
    main(() -> {
      lan.performClick();
      name.setText("Второй компьютер");
      address.setText("192.168.12.35");
    });
    click("Сохранить компьютер");
    await(Button.class, "Добавить компьютер");
    check(
        repo.snapshot().computers.size() == 2 && !repo.snapshot().activeId.equals(first),
        "adding selects only new computer");
    String second = repo.snapshot().activeId;
    screenshot("17-two-computers");
    dialogClick("Доступ и настройки: Второй компьютер");
    dialogClick("Переименовать");
    android.view.accessibility.AccessibilityNodeInfo root =
        test.getUiAutomation().getRootInActiveWindow();
    List<android.view.accessibility.AccessibilityNodeInfo> fields =
        root.findAccessibilityNodeInfosByText("Второй компьютер");
    boolean renamed = false;
    for (android.view.accessibility.AccessibilityNodeInfo field : fields)
      if (field.isEditable()) {
        Bundle value = new Bundle();
        value.putCharSequence(
            android.view.accessibility.AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE,
            "Рабочий ПК");
        renamed =
            field.performAction(
                android.view.accessibility.AccessibilityNodeInfo.ACTION_SET_TEXT, value);
        break;
      }
    check(renamed, "rename field reachable through Android accessibility");
    dialogClick("Сохранить");
    await(RadioButton.class, "Рабочий ПК");
    check(repo.snapshot().activeId.equals(second), "rename preserves selected identity");
    back();
    if (Build.VERSION.SDK_INT >= 33)
      shell("pm grant " + APP + " android.permission.POST_NOTIFICATIONS");
    click("Запустить мост");
    for (int i = 0; i < 100 && !repo.isBusy(); i++) Thread.sleep(50);
    check(repo.isBusy(), "real foreground service reserves selected computer");
    click("Изменить компьютер");
    RadioButton select = (RadioButton) await(RadioButton.class, "192.168.12.34");
    main(select::performClick);
    dialogClick("Отмена");
    check(
        repo.snapshot().activeId.equals(second) && repo.isBusy(),
        "cancel switch leaves current service intact");
    select = (RadioButton) await(RadioButton.class, "192.168.12.34");
    RadioButton target = select;
    main(target::performClick);
    dialogClick("Продолжить");
    for (int i = 0; i < 150 && (!first.equals(repo.snapshot().activeId) || repo.isBusy()); i++)
      Thread.sleep(100);
    check(
        first.equals(repo.snapshot().activeId) && !repo.isBusy(),
        "switch waits for actual service teardown before activation");
    check(
        !context.getSharedPreferences("bridge", 0).contains("computer_resume"),
        "switch does not auto-start selected computer");
    dialogClick("Доступ и настройки: Рабочий ПК");
    dialogClick("Забыть на этом устройстве");
    dialogClick("Отмена");
    check(repo.snapshot().computers.size() == 2, "cancel forget retains both computers");
    dialogClick("Доступ и настройки: Рабочий ПК");
    dialogClick("Забыть на этом устройстве");
    dialogClick("Забыть");
    for (int i = 0; i < 60 && repo.snapshot().computers.size() != 1; i++) Thread.sleep(100);
    check(
        repo.snapshot().computers.size() == 1 && first.equals(repo.snapshot().activeId),
        "forget inactive leaves active credentials intact");
    back();
    await(Button.class, "Запустить мост");
  }

  private void editSelected(ComputerRepository repo) throws Exception {
    click("Изменить компьютер");
    await(Button.class, "Добавить компьютер");
    dialogClick("Доступ и настройки: " + repo.snapshot().active().name);
    dialogClick("Изменить подключение");
    await(Spinner.class, null);
  }

  private void back() throws Exception {
    test.sendKeyDownUpSync(KeyEvent.KEYCODE_BACK);
    test.waitForIdleSync();
    Thread.sleep(150);
  }

  private Activity launch() throws Exception {
    Activity current = resumed.get();
    if (current != null) main(current::finish);
    Activity a =
        test.startActivitySync(
            new Intent().setClassName(APP, MAIN).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
    resumed.set(a);
    test.waitForIdleSync();
    Thread.sleep(250);
    return a;
  }

  private void recreate() throws Exception {
    Activity old = resumed.get();
    main(old::recreate);
    for (int i = 0; i < 60 && (resumed.get() == null || resumed.get() == old); i++)
      Thread.sleep(100);
    check(resumed.get() != null && resumed.get() != old, "Activity recreated");
    test.waitForIdleSync();
    Thread.sleep(250);
  }

  private void geometry(View v, int width) {
    if (!v.isShown()) return;
    if (v instanceof TextView && ((TextView) v).length() > 0) {
      TextView t = (TextView) v;
      check(t.getWidth() > 0 && t.getWidth() <= width,
          "text fits window: " + t.getText() + " (view=" + t.getWidth() + ", window=" + width + ")");
      android.text.Layout layout = t.getLayout();
      check(layout != null, "text laid out");
      if (!(t instanceof EditText)) {
        for (int i = 0; i < layout.getLineCount(); i++)
          check(layout.getEllipsisCount(i) == 0, "no ellipsis");
        check(
            t.getHeight()
                >= layout.getHeight() + t.getCompoundPaddingTop() + t.getCompoundPaddingBottom(),
            "text fits height: " + t.getText());
      }
      if (t instanceof Button)
        check(
            t.getHeight() >= Math.round(48 * context.getResources().getDisplayMetrics().density),
            "48dp target");
    }
    if (v instanceof ViewGroup)
      for (int i = 0; i < ((ViewGroup) v).getChildCount(); i++)
        geometry(((ViewGroup) v).getChildAt(i), width);
  }

  private boolean layoutPending(View view) {
    if (!view.isShown()) return false;
    if (view.isLayoutRequested()
        || view instanceof TextView && ((TextView) view).length() > 0 && view.getWidth() == 0)
      return true;
    if (view instanceof ViewGroup)
      for (int i = 0; i < ((ViewGroup) view).getChildCount(); i++)
        if (layoutPending(((ViewGroup) view).getChildAt(i))) return true;
    return false;
  }

  private void screenshot(String name) throws Exception {
    boolean ready = false;
    for (int i = 0; i < 60; i++) {
      boolean[] measured = {false};
      main(() -> {
        Activity a = resumed.get();
        View root = a == null ? null : a.getWindow().getDecorView();
        measured[0] = root != null && root.getWidth() > 0 && !layoutPending(root);
      });
      if (measured[0]) { ready = true; break; }
      Thread.sleep(50);
    }
    check(ready, "layout completed before geometry capture: " + name);
    main(() -> {
      Activity a = resumed.get();
      View root = a.getWindow().getDecorView();
      geometry(root, root.getWidth());
      ScrollView scroll = (ScrollView) find(root, ScrollView.class, null);
      if (scroll != null) scroll.scrollTo(0, 0);
      File folder = new File(context.getExternalFilesDir(null), "navigation-proof");
      check(folder.isDirectory() || folder.mkdirs(), "owned screenshots folder");
      Bitmap bitmap =
          Bitmap.createBitmap(root.getWidth(), root.getHeight(), Bitmap.Config.ARGB_8888);
      try (OutputStream out = new FileOutputStream(new File(folder, name + ".png"))) {
        root.draw(new Canvas(bitmap));
        check(bitmap.compress(Bitmap.CompressFormat.PNG, 100, out), "screen saved");
      } catch (IOException e) {
        throw new AssertionError(e);
      } finally {
        bitmap.recycle();
      }
    });
    scenarios.add(name);
  }

  private String shell(String command) throws Exception {
    try (InputStream in =
            new ParcelFileDescriptor.AutoCloseInputStream(
                test.getUiAutomation().executeShellCommand(command));
        ByteArrayOutputStream out = new ByteArrayOutputStream()) {
      byte[] part = new byte[4096];
      int n;
      while ((n = in.read(part)) != -1) out.write(part, 0, n);
      return out.toString("UTF-8").trim();
    }
  }

  private void diagnosticUsb() throws Exception {
    check(shell("getprop ro.kernel.qemu").equals("1"), "USB observations fixture is emulator-only");
    String hidden = shell("settings get global hidden_api_policy");
    Object application = context.getApplicationContext();
    java.lang.reflect.Field field = application.getClass().getDeclaredField("inventory");
    field.setAccessible(true);
    org.cgutman.usbip.service.UsbInventory original =
        (org.cgutman.usbip.service.UsbInventory) field.get(application);
    org.cgutman.usbip.service.UsbInventory[] fixtureInventory =
        new org.cgutman.usbip.service.UsbInventory[1];
    UsbFixture fixture = new UsbFixture(context);
    fixture.autoGrant = false;
    try {
      if (Build.VERSION.SDK_INT >= 28) shell("settings put global hidden_api_policy 1");
      main(() -> {
        fixtureInventory[0] = new org.cgutman.usbip.service.UsbInventory(fixture, fixture);
        fixtureInventory[0].bindComputers(
            org.cgutman.usbip.connection.ComputerRepository.get(context));
      });
      field.set(application, fixtureInventory[0]);
      org.cgutman.usbip.service.UsbInventory inventory = fixtureInventory[0];
      android.hardware.usb.UsbDevice device =
          UsbFixture.device(119, "Private USB fixture name", 3, false);
      main(() -> {
        fixture.connect(device, UsbFixture.descriptor(3, false));
        inventory.select(
            device.getDeviceId(), inventory.item(device.getDeviceId()).generation, true, false);
      });
      check(fixture.requests.size() == 1, "one explicit selection permission request");
      main(() -> fixture.reply(fixture.requests.get(0), false));
      Activity activity =
          test.startActivitySync(
              new Intent()
                  .setClassName(APP, "org.cgutman.usbip.config.ConnectionDiagnostics")
                  .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
      resumed.set(activity);
      await(
          TextView.class,
          "Для выбранного USB нет разрешения Android. Выберите его на главном экране и разрешите"
              + " доступ.");
      long revision = inventory.revision();
      click("Проверить снова");
      recreate();
      check(
          fixture.requests.size() == 1 && inventory.revision() == revision,
          "read-only diagnosis and rotation never re-request permission or mutate USB");
      screenshot("13-diagnosis-usb-denied");
      main(() -> fixture.disconnect(device));
      click("Проверить снова");
      await(TextView.class, "Подключите USB-устройство к телефону или планшету.");
      check(
          fixture.requests.size() == 1 && inventory.snapshot().isEmpty(),
          "detached USB no longer shown and no late permission requested");
      screenshot("14-diagnosis-detached");
      Activity current = resumed.get();
      if (current != null) main(current::finish);
    } finally {
      field.set(application, original);
      if (fixtureInventory[0] != null) main(fixtureInventory[0]::close);
      if (Build.VERSION.SDK_INT >= 28) {
        if (hidden.equals("null")) shell("settings delete global hidden_api_policy");
        else shell("settings put global hidden_api_policy " + Integer.parseInt(hidden));
      }
    }
  }

  int run() throws Exception {
    SharedPreferences prefs = context.getSharedPreferences("bridge", 0);
    check(shell("getprop ro.kernel.qemu").equals("1"), "only disposable emulator catalog");
    ComputerRepository repo = ComputerRepository.get(context);
    repo.ready().get(30, TimeUnit.SECONDS);
    repo.reset().get(30, TimeUnit.SECONDS);
    Map<String, ?> before = new HashMap<>(prefs.getAll());
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
    try {
      check(prefs.edit().clear().commit(), "clean first launch fixture");
      launch();
      await(Button.class, "Позже");
      check(
          visible(CheckBox.class, "Все и новые устройства") == null,
          "onboarding is separate from main");
      screenshot("01-welcome");
      recreate();
      await(Button.class, "Позже");
      click("Настроить подключение");
      screenshot("02-choose");
      recreate();
      await(Button.class, "По QR-коду");
      back();
      await(Button.class, "Позже");
      click("Позже");
      await(CheckBox.class, "Все и новые устройства");
      check(
          visible(EditText.class, null) == null
              && visible(Button.class, "Сохранить журнал") == null,
          "main has no setup fields or diagnostics");
      screenshot("03-home-unconfigured");
      check(prefs.getBoolean("welcome_seen", false), "skip persisted");
      launch();
      check(visible(Button.class, "Позже") == null, "skip survives relaunch");
      click("Настроить подключение");
      click("По QR-коду");
      Spinner mode = (Spinner) await(Spinner.class, null);
      check(mode.getSelectedItemPosition() == 1, "QR choice opens QR immediately");
      screenshot("04-qr-form");
      recreate();
      check(
          ((Spinner) await(Spinner.class, null)).getSelectedItemPosition() == 1,
          "QR form survives recreation");
      click("Назад");
      await(Button.class, "По QR-коду");
      click("Через SSH");
      mode = (Spinner) await(Spinner.class, null);
      check(mode.getSelectedItemPosition() == 0, "SSH choice opens SSH immediately");
      check(visible(Button.class, "Выбрать SSH-ключ") == null, "SSH advanced collapsed initially");
      screenshot("05-ssh-form");
      click("Ключ и дополнительные параметры");
      await(Button.class, "Выбрать SSH-ключ");
      screenshot("06-ssh-advanced");
      recreate();
      await(Button.class, "Выбрать SSH-ключ");
      click("Назад");
      click("По IP · готовый USB/IP");
      screenshot("07-legacy");
      RadioButton lan = (RadioButton) await(RadioButton.class, "Локальная сеть");
      EditText address = input("Адрес компьютера");
      main(() -> {
        lan.performClick();
        address.setText("8.8.8.8");
      });
      click("Сохранить компьютер");
      check(
          address.getError() != null && repo.snapshot().computers.isEmpty(),
          "invalid address stays in form, no profile committed");
      main(() -> {
        address.setError(null);
        address.setText("192.168.12.34");
      });
      recreate();
      EditText restoredAddress = input("Адрес компьютера");
      check(
          restoredAddress.getText().toString().equals("192.168.12.34"),
          "draft survives recreation");
      click("Сохранить компьютер");
      await(Button.class, "Запустить мост");
      check(repo.snapshot().active().host.equals("192.168.12.34"), "valid endpoint committed");
      screenshot("08-home-configured");
      multipleComputers(repo);
      click("Настройки");
      screenshot("09-settings");
      recreate();
      await(Button.class, "Сохранить журнал");
      Map<String, ?> savedBeforeDiagnosis = new HashMap<>(prefs.getAll());
      click("Проверить подключение");
      await(TextView.class, "Проверка подключения");
      await(TextView.class, "Подключите USB-устройство к телефону или планшету.");
      check(
          visible(TextView.class, "Компьютер · Проверено") == null,
          "saved address does not claim a checked computer");
      screenshot("11-diagnosis");
      recreate();
      await(Button.class, "Проверить снова");
      click("Проверить снова");
      check(
          prefs.getAll().equals(savedBeforeDiagnosis),
          "diagnosis cannot mutate saved configuration");
      click("Сохранить отчёт");
      android.view.accessibility.AccessibilityNodeInfo reportWindow = null;
      for (int i = 0; i < 30; i++) {
        reportWindow = test.getUiAutomation().getRootInActiveWindow();
        if (reportWindow != null
            && !reportWindow.findAccessibilityNodeInfosByText("Отчёт для поддержки").isEmpty())
          break;
        if (reportWindow != null) reportWindow.recycle();
        reportWindow = null;
        Thread.sleep(100);
      }
      check(reportWindow != null, "redacted report preview shown before export");
      check(
          reportWindow.findAccessibilityNodeInfosByText("192.168.12.34").isEmpty(),
          "saved computer address absent from report");
      check(
          !reportWindow.findAccessibilityNodeInfosByText("USB: найдено").isEmpty(),
          "report contains bounded device counts");
      reportWindow.recycle();
      back();
      await(Button.class, "Проверить снова");
      click("Выбрать USB-устройства");
      await(Button.class, "Запустить мост");
      click("Настройки");
      back();
      await(Button.class, "Запустить мост");
      editSelected(repo);
      EditText draft = input("Адрес компьютера");
      main(() -> draft.setText("192.168.99.99"));
      click("Назад");
      await(Button.class, "Добавить компьютер");
      click("Назад");
      await(Button.class, "Запустить мост");
      check(
          repo.snapshot().active().host.equals("192.168.12.34"),
          "cancelled edit cannot replace working endpoint");
      launch();
      await(Button.class, "Запустить мост");
      check(
          visible(TextView.class, "192.168.12.34") != null,
          "summary uses committed profile, not draft");
      click("Настройки");
      click("Как начать");
      await(Button.class, "Позже");
      back();
      await(Button.class, "Сохранить журнал");
      back();
      await(Button.class, "Запустить мост");
      // Upgrade migration is derived from saved configuration, not a mandatory new flag.
      check(prefs.edit().remove("welcome_seen").commit(), "old app fixture");
      launch();
      await(Button.class, "Запустить мост");
      check(visible(Button.class, "Позже") == null, "configured existing user bypasses onboarding");
      String direct =
          "{\"transport\":\"direct\",\"endpoint\":{\"host\":\"127.0.0.1\",\"port\":41600},\"server_pin\":\"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\"}";
      repo.change(
              new JSONObject()
                  .put("op", "replace")
                  .put("revision", repo.snapshot().revision)
                  .put("id", repo.snapshot().activeId)
                  .put("name", "Защищённый компьютер")
                  .put(
                      "connection",
                      new JSONObject()
                          .put("kind", "secure")
                          .put("profile", new JSONObject(direct))
                          .put(
                              "credentials",
                              new JSONObject()
                                  .put("token", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")))
                  .toString()
                  .getBytes(java.nio.charset.StandardCharsets.UTF_8))
          .get(30, TimeUnit.SECONDS);
      launch();
      await(Button.class, "Запустить мост");
      click("Изменить компьютер");
      screenshot("15-computers");
      click("Добавить компьютер");
      Spinner directToSsh = (Spinner) await(Spinner.class, null);
      main(() -> directToSsh.setSelection(0));
      Thread.sleep(200);
      check(directToSsh.getSelectedItemPosition() == 0, "direct to SSH selects SSH");
      View[] portField = {null};
      main(() ->
          portField[0] = resumed.get().getWindow().getDecorView().findViewWithTag("Порт SSH"));
      check(
          portField[0] instanceof EditText
              && ((EditText) portField[0]).getText().toString().equals("22"),
          "new SSH uses port22 rather than saved TLS port");
      main(() -> directToSsh.setSelection(2));
      click("Назад");
      await(Button.class, "Добавить компьютер");
      click("Назад");
      await(Button.class, "Запустить мост");
      check(
          repo.snapshot().computers.size() == 1
              && repo.snapshot().active().kind.equals("secure")
              && new JSONObject(repo.snapshot().active().profile)
                  .getString("transport")
                  .equals("direct"),
          "cancel legacy cannot add a computer or downgrade secure connection");
      click("Настройки");
      click("Инструкция для компьютера");
      android.view.accessibility.AccessibilityNodeInfo helpWindow =
          test.getUiAutomation().getRootInActiveWindow();
      check(helpWindow != null, "computer help dialog available");
      for (String prerequisite : new String[] {"systemd", "vhci_hcd", "UDisks2", "udisks2"})
        check(
            !helpWindow.findAccessibilityNodeInfosByText(prerequisite).isEmpty(),
            "computer help explains Linux prerequisite: " + prerequisite);
      android.view.accessibility.AccessibilityNodeInfo dismiss = accessible(helpWindow, "Понятно");
      check(dismiss != null && dismiss.isVisibleToUser(), "help dismiss remains visible");
      Bitmap helpImage = test.getUiAutomation().takeScreenshot();
      check(helpImage != null, "actual help window screenshot");
      try (OutputStream out =
          new FileOutputStream(
              new File(context.getExternalFilesDir(null), "navigation-proof/18-computer-help.png"))) {
        check(helpImage.compress(Bitmap.CompressFormat.PNG, 100, out), "help screenshot saved");
      } finally {
        helpImage.recycle();
        helpWindow.recycle();
      }
      dialogClick("Понятно");
      click("Назад");
      scenarios.add("18-computer-help");
      java.io.File catalog = new File(context.getNoBackupFilesDir(), "computers.enc");
      byte[] committed = java.nio.file.Files.readAllBytes(catalog.toPath());
      byte[] corrupt = committed.clone();
      corrupt[corrupt.length - 1] ^= 1;
      java.nio.file.Files.write(catalog.toPath(), corrupt);
      try {
        repo.reload().get(30, TimeUnit.SECONDS);
        throw new AssertionError("tampered catalog accepted");
      } catch (java.util.concurrent.ExecutionException expected) {
        checks++;
      }
      launch();
      await(TextView.class, "Сохранённые данные недоступны");
      check(visible(Button.class, "Запустить мост") == null, "failed store cannot offer start");
      screenshot("10-profile-recovery");
      click("Настроить подключение");
      await(Button.class, "Удалить сохранённые подключения");
      screenshot("16-store-recovery");
      dialogClick("Удалить сохранённые подключения");
      dialogClick("Отмена");
      check(
          repo.snapshot().status == ComputerRepository.Status.FAILED,
          "cancel reset preserves failed store");
      java.nio.file.Files.write(catalog.toPath(), committed);
      click("Повторить");
      await(Button.class, "Добавить компьютер");
      check(
          repo.snapshot().active().kind.equals("secure"),
          "recovery retains exact saved connection");
      back();
      java.nio.file.Files.write(catalog.toPath(), corrupt);
      try {
        repo.reload().get(30, TimeUnit.SECONDS);
        throw new AssertionError("tampered catalog accepted before explicit deletion");
      } catch (java.util.concurrent.ExecutionException expected) {
        checks++;
      }
      launch();
      await(TextView.class, "Сохранённые данные недоступны");
      click("Настроить подключение");
      await(Button.class, "Удалить сохранённые подключения");
      dialogClick("Удалить сохранённые подключения");
      dialogClick("Удалить");
      await(Button.class, "Добавить компьютер");
      check(
          repo.snapshot().status == ComputerRepository.Status.READY
              && repo.snapshot().computers.isEmpty()
              && repo.snapshot().activeId == null,
          "confirmed recovery deletion creates ready empty catalog without selected computer");
      repo.reload().get(30, TimeUnit.SECONDS);
      check(repo.snapshot().computers.isEmpty(), "confirmed recovery deletion survives reload");
      try {
        repo.resume().get(30, TimeUnit.SECONDS);
        throw new AssertionError("deleted connection resumed after recovery reset");
      } catch (java.util.concurrent.ExecutionException expected) {
        checks++;
      }
      screenshot("19-store-reset-confirmed");
      back();
      launch();
      click("Настройки");
      click("Проверить подключение");
      await(TextView.class, "Сначала выберите компьютер для подключения.");
      screenshot("12-diagnosis-setup");
      click("Настроить компьютер");
      await(Button.class, "По QR-коду");
      back();
      diagnosticUsb();
      org.json.JSONObject result =
          new org.json.JSONObject()
              .put("status", "passed")
              .put("checks", checks)
              .put("screens", new org.json.JSONArray(scenarios));
      try (Writer out =
          new FileWriter(
              new File(context.getExternalFilesDir(null), "navigation-proof/result.json"))) {
        out.write(result.toString(2));
      }
    } finally {
      Activity a = resumed.get();
      if (a != null) main(a::finish);
      main(() -> application.unregisterActivityLifecycleCallbacks(callbacks));
      SharedPreferences.Editor edit = prefs.edit().clear();
      for (Map.Entry<String, ?> e : before.entrySet()) {
        Object v = e.getValue();
        if (v instanceof String) edit.putString(e.getKey(), (String) v);
        else if (v instanceof Boolean) edit.putBoolean(e.getKey(), (Boolean) v);
        else if (v instanceof Integer) edit.putInt(e.getKey(), (Integer) v);
        else if (v instanceof Long) edit.putLong(e.getKey(), (Long) v);
        else if (v instanceof Float) edit.putFloat(e.getKey(), (Float) v);
      }
      check(edit.commit(), "prior preferences restored");
    }
    return checks;
  }
}
