package org.cgutman.usbip.config;

import android.app.AlertDialog;
import android.content.*;
import android.net.*;
import android.os.*;
import android.provider.Settings;
import android.view.*;
import android.widget.*;
import androidx.activity.ComponentActivity;
import androidx.activity.result.ActivityResultLauncher;
import androidx.activity.result.contract.ActivityResultContracts;
import java.nio.charset.StandardCharsets;
import org.cgutman.usbip.connection.ComputerRepository;
import org.cgutman.usbip.connection.ConnectionObservation;
import org.cgutman.usbip.jni.NativeConnection;
import org.cgutman.usbip.service.*;
import org.cgutman.usbipserverforandroid.BuildConfig;
import org.cgutman.usbipserverforandroid.R;
import org.json.*;

/** Read-only Android observations; Rust chooses explanations and next actions. */
public final class ConnectionDiagnostics extends ComponentActivity {
  private static final int INK = 0xFF172B3A, MUTED = 0xFF586875;
  private LinearLayout content;
  private String reportText, pendingReport;
  private ComputerRepository computers;
  private final Runnable changed = this::render;
  private final ActivityResultLauncher<String> export =
      registerForActivityResult(
          new ActivityResultContracts.CreateDocument("text/plain"),
          uri -> {
            final String text = pendingReport;
            pendingReport = null;
            if (uri == null) return;
            if (text == null) {
              toast("Повторите сохранение отчёта");
              return;
            }
            new Thread(
                    () -> {
                      boolean success = false;
                      try {
                        DiagnosticReportWriter.writeAndClose(
                            getContentResolver().openOutputStream(uri), text);
                        success = true;
                      } catch (Exception ignored) {
                      }
                      final boolean saved = success;
                      runOnUiThread(
                          () -> toast(saved ? "Отчёт сохранён" : "Не удалось сохранить отчёт"));
                    },
                    "usb-most-report")
                .start();
          });

  @Override
  public void onCreate(Bundle state) {
    super.onCreate(state);
    computers = ComputerRepository.get(this);
    if (getActionBar() != null) getActionBar().hide();
    if (state != null) {
      String value = state.getString("pending_report");
      if (value != null && value.length() <= 32768) pendingReport = value;
    }
    getWindow().setStatusBarColor(0xFFFAFBFC);
    getWindow().setNavigationBarColor(0xFFFAFBFC);
    getWindow()
        .getDecorView()
        .setSystemUiVisibility(
            View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR | View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR);
    ScrollView scroll = new ScrollView(this);
    scroll.setFillViewport(true);
    scroll.setBackgroundColor(0xFFFAFBFC);
    content = new LinearLayout(this);
    content.setOrientation(LinearLayout.VERTICAL);
    int pad = Math.round(20 * getResources().getDisplayMetrics().density);
    content.setPadding(pad, pad, pad, pad);
    scroll.addView(content, new ScrollView.LayoutParams(-1, -2));
    setContentView(scroll);
  }

  @Override
  protected void onStart() {
    super.onStart();
    computers.addListener(changed);
  }

  @Override
  protected void onStop() {
    computers.removeListener(changed);
    super.onStop();
  }

  @Override
  protected void onResume() {
    super.onResume();
    render();
  }

  @Override
  public void onSaveInstanceState(Bundle state) {
    super.onSaveInstanceState(state);
    if (pendingReport != null) state.putString("pending_report", pendingReport);
  }

  private JSONObject observe() throws Exception {
    UsbInventory inventory = UsbInventory.get(this);
    int total = 0, selected = 0, ready = 0, inUse = 0, permission = 0, consent = 0, unsupported = 0;
    for (UsbInventory.Item item : inventory.snapshot()) {
      total++;
      if (item.capability != 0) unsupported++;
      if (item.inUse) inUse++;
      if (!item.selected) continue;
      selected++;
      boolean granted = inventory.permissionGranted(item.device.getDeviceId(), item.generation);
      if (!granted) permission++;
      if (item.storage && !item.approved) consent++;
      if (item.ready && granted) ready++;
    }
    ComputerRepository.Snapshot snapshot = computers.snapshot();
    ComputerRepository.Computer computer = snapshot.active();
    boolean secure = computer != null && computer.kind.equals("secure");
    ConnectionObservation.State observation =
        ConnectionObservation.forProfile(snapshot.connectionKey());
    String transport = computer == null ? "none" : "legacy";
    if (secure) {
      try {
        transport = new JSONObject(computer.profile).getString("transport");
      } catch (JSONException ignored) {
        transport = "none";
      }
    }
    boolean configured = snapshot.status == ComputerRepository.Status.READY && computer != null;
    boolean running = UsbIpService.running;
    JSONObject usb =
        new JSONObject()
            .put("total", total)
            .put("selected", selected)
            .put("ready", ready)
            .put("in_use", inUse)
            .put("permission_missing", permission)
            .put("consent_missing", consent)
            .put("unsupported", unsupported);
    return new JSONObject()
        .put("schema", 1)
        .put("configured", configured)
        .put("transport", transport)
        .put("running", running)
        .put("network", network())
        .put("phase", secure && running && observation.active ? observation.phase : "idle")
        .put(
            "receiver_verified",
            secure && running && observation.active && observation.receiverVerified)
        .put("failure", observation.failure == null ? JSONObject.NULL : observation.failure)
        .put("usb", usb);
  }

  private Object network() {
    try {
      ConnectivityManager manager = getSystemService(ConnectivityManager.class);
      if (manager == null) return JSONObject.NULL;
      for (Network network : manager.getAllNetworks()) {
        LinkProperties link = manager.getLinkProperties(network);
        if (link == null) continue;
        for (LinkAddress address : link.getLinkAddresses()) {
          if (!address.getAddress().isLoopbackAddress()
              && !address.getAddress().isAnyLocalAddress()) return true;
        }
      }
      return false;
    } catch (RuntimeException unavailable) {
      return JSONObject.NULL;
    }
  }

  private void render() {
    if (isDestroyed() || isFinishing()) return;
    content.removeAllViews();
    button("Назад", false, 0, this::finish);
    heading("Проверка подключения", 24);
    label("Проверяется текущее состояние. Устройства и настройки не изменяются.", 14, MUTED);
    try {
      String encoded =
          NativeConnection.diagnose(observe().toString().getBytes(StandardCharsets.UTF_8));
      if (encoded == null) throw new JSONException("invalid_observations");
      JSONObject report = new JSONObject(encoded);
      TextView summary = heading(copy(report.getString("summary")), 18);
      summary.setId(R.id.diagnosis_summary);
      summary.setAccessibilityLiveRegion(View.ACCESSIBILITY_LIVE_REGION_POLITE);
      String action = report.getString("action");
      if (!action.equals("none"))
        button(actionLabel(action), true, R.id.diagnosis_action, () -> action(action));
      StringBuilder text =
          new StringBuilder(
              "USB Мост · "
                  + BuildConfig.VERSION_NAME
                  + "\nAndroid API "
                  + Build.VERSION.SDK_INT
                  + "\n\n");
      text.append(copy(report.getString("summary"))).append("\n\n");
      JSONArray checks = report.getJSONArray("checks");
      for (int i = 0; i < checks.length(); i++) {
        JSONObject check = checks.getJSONObject(i);
        String level = level(check.getString("level"));
        String area = area(check.getString("area"));
        heading(area + " · " + level, 15);
        String detail = copy(check.getString("message"));
        label(detail, 14, MUTED);
        text.append(area).append(" · ").append(level).append('\n').append(detail).append("\n\n");
      }
      JSONObject usb = report.getJSONObject("observations").getJSONObject("usb");
      text.append("USB: найдено ")
          .append(usb.getInt("total"))
          .append(", выбрано ")
          .append(usb.getInt("selected"))
          .append(", передаётся ")
          .append(usb.getInt("in_use"))
          .append("\n\n");
      text.append(
          "Отчёт не содержит адресов, имён компьютеров, паролей, ключей доступа и содержимого"
              + " USB.\n");
      reportText = text.toString();
      button("Проверить снова", false, R.id.diagnosis_refresh, this::render);
      button("Сохранить отчёт", false, R.id.diagnosis_export, this::preview);
    } catch (Exception unavailable) {
      reportText = null;
      label(
          "Не удалось получить состояние. Вернитесь на главный экран и повторите проверку.",
          16,
          INK);
      button("Повторить", true, R.id.diagnosis_refresh, this::render);
    }
  }

  private void action(String action) {
    if (action.equals("network")) {
      try {
        startActivity(new Intent(Settings.ACTION_WIRELESS_SETTINGS));
      } catch (ActivityNotFoundException unavailable) {
        toast("Откройте настройки сети Android");
      }
      return;
    }
    setResult(RESULT_OK, new Intent().putExtra("diagnostic_action", action));
    finish();
  }

  private void preview() {
    final String text = reportText;
    if (text == null) return;
    TextView view = new TextView(this);
    view.setText(text);
    view.setTextColor(INK);
    view.setTextSize(14);
    view.setTextIsSelectable(true);
    int pad = Math.round(20 * getResources().getDisplayMetrics().density);
    view.setPadding(pad, pad, pad, pad);
    ScrollView scroll = new ScrollView(this);
    scroll.addView(view);
    new AlertDialog.Builder(this)
        .setTitle("Отчёт для поддержки")
        .setView(scroll)
        .setNegativeButton("Отмена", null)
        .setPositiveButton(
            "Сохранить",
            (dialog, which) -> {
              pendingReport = text;
              export.launch("usb-most-check.txt");
            })
        .show();
  }

  private TextView label(String value, int size, int color) {
    TextView view = new TextView(this);
    view.setText(value);
    view.setTextSize(size);
    view.setTextColor(color);
    int spacing = Math.round(8 * getResources().getDisplayMetrics().density);
    view.setPadding(0, spacing, 0, spacing);
    content.addView(view, new LinearLayout.LayoutParams(-1, -2));
    return view;
  }

  private TextView heading(String value, int size) {
    TextView view = label(value, size, INK);
    view.setTypeface(null, android.graphics.Typeface.BOLD);
    if (Build.VERSION.SDK_INT >= 28) view.setAccessibilityHeading(true);
    return view;
  }

  private void button(String title, boolean primary, int id, Runnable action) {
    Button view = BridgeUi.button(this, title, primary);
    if (id != 0) view.setId(id);
    view.setOnClickListener(v -> action.run());
    content.addView(view, new LinearLayout.LayoutParams(-1, -2));
  }

  private void toast(String value) {
    Toast.makeText(this, value, Toast.LENGTH_LONG).show();
  }

  private static String actionLabel(String action) {
    switch (action) {
      case "setup":
        return "Настроить компьютер";
      case "usb":
        return "Выбрать USB-устройства";
      case "network":
        return "Настройки сети";
      case "help":
        return "Что проверить на компьютере";
      default:
        return "На главный экран";
    }
  }

  private static String area(String value) {
    switch (value) {
      case "configuration":
        return "Настройка";
      case "usb":
        return "USB-устройства";
      case "network":
        return "Сеть";
      case "computer":
        return "Компьютер";
      case "receiver":
        return "Приёмник";
      default:
        return "Передача USB";
    }
  }

  private static String level(String value) {
    switch (value) {
      case "good":
        return "Проверено";
      case "attention":
        return "Нужно действие";
      case "progress":
        return "Выполняется";
      default:
        return "Не проверено";
    }
  }

  private static String copy(String code) {
    switch (code) {
      case "computer_saved":
        return "Компьютер сохранён. Его доступность проверяется при подключении.";
      case "choose_computer":
        return "Сначала выберите компьютер для подключения.";
      case "usb_absent":
        return "Подключите USB-устройство к телефону или планшету.";
      case "select_usb":
        return "Выберите устройства, которые нужно передать компьютеру.";
      case "usb_unsupported":
        return "Подключённые устройства пока не поддерживаются. Ограничение указано рядом с каждым"
                   + " устройством.";
      case "usb_permission":
        return "Для выбранного USB нет разрешения Android. Выберите его на главном экране и"
                   + " разрешите доступ.";
      case "storage_consent":
        return "Сначала безопасно извлеките накопитель в Android и подтвердите передачу"
                   + " компьютеру.";
      case "usb_not_ready":
        return "Не все выбранные устройства готовы. Проверьте их состояние на главном экране.";
      case "usb_ready":
        return "Выбранные USB-устройства доступны приложению.";
      case "network_present":
        return "Сетевое подключение есть. Это ещё не подтверждает доступность компьютера.";
      case "network_absent":
        return "Сетевое подключение не обнаружено. Подключитесь к Wi-Fi или мобильной сети.";
      case "network_unknown":
        return "Android не предоставил сведения о сети. Доступность компьютера пока не"
                   + " подтверждена.";
      case "identity_confirmation":
        return "Подтвердите ключ компьютера на главном экране после сверки с компьютером.";
      case "computer_authenticated":
        return "Защищённое соединение с выбранным компьютером установлено.";
      case "computer_connecting":
        return "Подключение ещё выполняется. Обновите проверку через несколько секунд.";
      case "computer_reconnecting":
        return "Связь восстанавливается автоматически. Проверьте, что компьютер включён и доступен"
                   + " в сети.";
      case "computer_unchecked":
        return "Соединение с компьютером сейчас не подтверждено.";
      case "receiver_verified":
        return "Приёмник и стек USB/IP проверены при подключении. Драйвер конкретного устройства"
                   + " проверяется на компьютере.";
      case "receiver_preparing":
        return "Выполняется подготовка приёмника на компьютере.";
      case "receiver_unchecked":
        return "Состояние приёмника сейчас не подтверждено.";
      case "usb_transferring":
        return "Компьютер запросил выбранное USB-устройство. Его готовность в рабочей программе"
                   + " проверяется на компьютере.";
      case "usb_not_imported":
        return "Компьютер пока не запросил USB. Само сетевое соединение ещё не означает передачу"
                   + " устройства.";
      case "bridge_stopped":
        return "Мост остановлен. Запустите его на главном экране, чтобы проверить связь с"
                   + " компьютером.";
      case "waiting_for_import":
        return "Мост ожидает запроса USB от компьютера. Проверьте приёмник на компьютере.";
      case "identity_not_confirmed":
        return "Ключ компьютера не подтверждён или изменился. Сверьте его на компьютере перед новым"
                   + " подключением.";
      case "login_failed":
        return "Компьютер отклонил данные входа. Проверьте учётную запись и пароль или SSH-ключ.";
      case "private_key_invalid":
        return "Не удалось прочитать SSH-ключ. Проверьте формат ключа и пароль к нему.";
      case "configuration_invalid":
        return "Сохранённые параметры подключения недоступны. Настройте компьютер заново.";
      case "ssh_forwarding_denied":
        return "SSH-сервер не разрешил обратный туннель. Администратору нужно проверить разрешения"
                   + " SSH.";
      case "linux_usb_driver_unavailable":
        return "В текущем ядре Linux недоступен драйвер USB/IP (vhci_hcd). Нужен совместимый пакет"
                   + " модуля для этого ядра; отключение SELinux не решает эту причину.";
      case "linux_systemd_required":
        return "Для установки приёмника Linux нужна система со службой systemd.";
      case "administrator_required":
        return "Для первоначальной установки в Windows нужна учётная запись с правами"
                   + " администратора.";
      case "preparation_failed":
        return "Подготовка компьютера не завершилась. Причина может быть в разрешениях или"
                   + " компонентах системы; проверьте установщик на компьютере.";
      case "receiver_artifact_unavailable":
        return "Приёмник для этого компьютера отсутствует или не прошёл проверку. Проверьте версию"
                   + " приложения и поддерживаемую архитектуру.";
      case "computer_restart_required":
        return "Для завершения установки требуется перезагрузка компьютера.";
      case "computer_busy":
        return "Приёмник занят другим подключением. Завершите его на компьютере перед повторной"
                   + " попыткой.";
      case "pair_again":
        return "Нужно повторное сопряжение. Создайте новый код на компьютере и настройте"
                   + " подключение.";
      case "protected_storage_failed":
        return "Не удалось открыть или сохранить защищённые данные. Настройте подключение заново;"
                   + " пароли не попадут в отчёт.";
      case "computer_unreachable":
        return "Ответ компьютера не получен. Проверьте адрес, сеть и работу приёмника. По одному"
                   + " тайм-ауту нельзя определить точную причину.";
      case "receiver_health_failed":
        return "Не удалось подтвердить готовность приёмника. Проверьте его состояние и версию на"
                   + " компьютере.";
      case "protocol_incompatible":
        return "Ответ приёмника не соответствует протоколу. Проверьте совместимость версий"
                   + " приложения и приёмника.";
      default:
        return "Подключение не удалось завершить. Повторите его с главного экрана и сохраните"
                   + " отчёт, если ошибка повторяется.";
    }
  }
}
