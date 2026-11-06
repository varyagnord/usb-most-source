package org.cgutman.usbip.config;

import android.Manifest;
import android.app.AlertDialog;
import android.content.*;
import android.content.pm.PackageManager;
import android.content.res.ColorStateList;
import android.graphics.Typeface;
import android.graphics.drawable.*;
import android.os.*;
import android.view.*;
import android.widget.*;
import androidx.activity.ComponentActivity;
import androidx.activity.result.ActivityResultLauncher;
import androidx.activity.result.contract.ActivityResultContracts;
import androidx.core.content.ContextCompat;
import java.io.OutputStream;
import java.net.*;
import java.util.*;
import org.cgutman.usbip.connection.ComputerRepository;
import org.cgutman.usbip.jni.NativePolicy;
import org.cgutman.usbip.service.*;
import org.cgutman.usbipserverforandroid.BuildConfig;
import org.cgutman.usbipserverforandroid.R;

/** Android presentation only. Rust validates peers and owns device selection. */
public final class UsbIpConfig extends ComponentActivity {
  private static final int INK = 0xFF172B3A,
      MUTED = 0xFF586875,
      ACCENT = 0xFF225FA5,
      PAPER = 0xFFFAFBFC,
      WARNING = 0xFF945315;
  private static final int HOME = 0, WELCOME = 1, CHOOSE = 2, SETTINGS = 3;
  private final ArrayList<Integer> backStack = new ArrayList<>();
  private final ScrollView[] pages = new ScrollView[4];
  private FrameLayout pageHost;
  private int page;
  private Button changeComputer;
  private ComputerRepository computers;
  private boolean initialRoutePending;
  private long notificationRevision = -1;
  private QuickActions.Binding notificationBinding;
  private boolean quickResumed;
  private Button diagnoseInline;
  private final ActivityResultLauncher<Intent> connectionCheck =
      registerForActivityResult(
          new ActivityResultContracts.StartActivityForResult(),
          result -> {
            if (result.getResultCode() != RESULT_OK || result.getData() == null) return;
            String action = result.getData().getStringExtra("diagnostic_action");
            if ("setup".equals(action)) chooseComputer();
            else if ("help".equals(action)) connectionHelp();
            else showPage(HOME, true);
          });
  private TextView computerName, computerDetail, settingsComputer;
  private androidx.activity.OnBackPressedCallback back;
  private final ActivityResultLauncher<Intent> setup =
      registerForActivityResult(
          new ActivityResultContracts.StartActivityForResult(),
          result -> {
            if (result.getResultCode() == RESULT_OK) {
              completeSetup();
              if (result.getData() != null
                  && result.getData().getBooleanExtra("review_computers", false)) manageComputers();
            }
          });
  private final Handler handler = new Handler(Looper.getMainLooper());
  private final Map<Integer, LinearLayout> rows = new LinkedHashMap<>();
  private TextView status, selectionHint;
  private Button toggle;
  private CheckBox all;
  private LinearLayout devices;
  private UsbInventory inventory;
  private boolean binding;
  private long seen = -1;
  private org.cgutman.usbip.connection.ConnectionController.TrustRequest showingTrust;
  private AlertDialog trustDialog;
  private AlertDialog deviceDialog;
  private AlertDialog confirmationDialog;
  private final Runnable refresh =
      new Runnable() {
        public void run() {
          render();
          handler.postDelayed(this, 500);
        }
      };
  private final ActivityResultLauncher<String> notifications =
      registerForActivityResult(
          new ActivityResultContracts.RequestPermission(),
          granted -> {
            long revision = notificationRevision;
            notificationRevision = -1;
            QuickActions.Binding binding = notificationBinding;
            notificationBinding = null;
            if (revision >= 0 && QuickActions.current(this, binding) && !QuickActions.locked(this))
              startBridge(revision);
          });
  private final ActivityResultLauncher<String> export =
      registerForActivityResult(
          new ActivityResultContracts.CreateDocument("text/plain"),
          uri -> {
            if (uri == null) return;
            new Thread(
                    () -> {
                      String message;
                      try (OutputStream out = getContentResolver().openOutputStream(uri)) {
                        if (out == null) throw new java.io.IOException("no_output");
                        DiagnosticLog.export(this, out);
                        message = "Журнал сохранён";
                      } catch (Exception e) {
                        message = "Не удалось сохранить журнал";
                      }
                      final String result = message;
                      runOnUiThread(() -> Toast.makeText(this, result, Toast.LENGTH_LONG).show());
                    },
                    "log-export")
                .start();
          });

  @Override
  public void onCreate(Bundle saved) {
    super.onCreate(saved);
    if (getActionBar() != null) getActionBar().hide();
    inventory = UsbInventory.get(this);
    SharedPreferences prefs = getSharedPreferences("bridge", MODE_PRIVATE);
    computers = ComputerRepository.get(this);
    initialRoutePending = saved == null || saved.getBoolean("initial_route_pending", false);
    getWindow().setStatusBarColor(PAPER);
    getWindow().setNavigationBarColor(PAPER);
    getWindow()
        .getDecorView()
        .setSystemUiVisibility(
            View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR | View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR);
    pageHost = new FrameLayout(this);
    pageHost.setBackgroundColor(PAPER);
    pageHost.setId(R.id.bridge_surface);
    setContentView(pageHost);
    LinearLayout body = page(HOME, "USB Мост", false);
    Button settings = button("Настройки", false);
    settings.setId(R.id.open_settings);
    TextView heading = (TextView) body.getChildAt(0);
    body.removeView(heading);
    heading.setGravity(Gravity.CENTER_VERTICAL);
    LinearLayout toolbar = new AdaptiveActions();
    toolbar.setGravity(Gravity.CENTER_VERTICAL);
    body.addView(toolbar, 0, match());
    toolbar.addView(heading, match());
    toolbar.addView(settings, match());
    settings.setOnClickListener(v -> showPage(SETTINGS, true));
    LinearLayout connection = section(body);
    computerName = text(connection, "", 18, INK);
    computerName.setId(R.id.computer_name);
    computerName.setTypeface(Typeface.DEFAULT, Typeface.BOLD);
    computerDetail = text(connection, "", 14, MUTED);
    status = text(connection, UsbIpService.status, 16, MUTED);
    status.setId(R.id.bridge_status);
    status.setAccessibilityLiveRegion(View.ACCESSIBILITY_LIVE_REGION_POLITE);
    diagnoseInline = button("Проверить подключение", false);
    diagnoseInline.setId(R.id.connection_error_help);
    diagnoseInline.setVisibility(View.GONE);
    diagnoseInline.setOnClickListener(
        v -> connectionCheck.launch(new Intent(this, ConnectionDiagnostics.class)));
    connection.addView(diagnoseInline, match());
    changeComputer = button("Изменить компьютер", false);
    changeComputer.setId(R.id.change_computer);
    changeComputer.setOnClickListener(v -> manageComputers());

    toggle = button("Запустить мост", true);
    toggle.setId(R.id.bridge_toggle);
    connection.addView(toggle, spaced());
    toggle.setOnClickListener(v -> toggleBridge());
    connection.addView(changeComputer, match());
    LinearLayout settingsBody = page(SETTINGS, "Настройки", true);
    settingsComputer = text(settingsBody, "", 16, INK);
    Button computer = button("Компьютеры и доступ", false);
    computer.setId(R.id.manage_computers);
    settingsBody.addView(computer, spaced());
    computer.setOnClickListener(v -> manageComputers());
    Button diagnose = button("Проверить подключение", false);
    diagnose.setId(R.id.check_connection);
    settingsBody.addView(diagnose, spaced());
    diagnose.setOnClickListener(
        v -> connectionCheck.launch(new Intent(this, ConnectionDiagnostics.class)));
    Button help = button("Инструкция для компьютера", false);
    help.setId(R.id.connection_help);
    settingsBody.addView(help, spaced());
    help.setOnClickListener(v -> connectionHelp());
    Button guide = button("Как начать", false);
    settingsBody.addView(guide, spaced());
    guide.setOnClickListener(v -> showPage(WELCOME, true));
    Button tile = button("Плитка быстрого запуска", false);
    tile.setId(R.id.add_quick_tile);
    settingsBody.addView(tile, spaced());
    tile.setOnClickListener(v -> addQuickTile());
    Button privacy = button(getString(R.string.privacy_title), false);
    privacy.setId(R.id.privacy_notice);
    settingsBody.addView(privacy, spaced());
    privacy.setOnClickListener(v -> startActivity(new Intent(this, PrivacyActivity.class)));
    LinearLayout usb = section(body);
    text(usb, "USB-устройства", 16, INK).setTypeface(Typeface.DEFAULT, Typeface.BOLD);
    all = new CheckBox(this);
    all.setId(R.id.select_all);
    all.setText("Все и новые устройства");
    all.setTextColor(INK);
    all.setTextSize(16);
    all.setMinHeight(dp(52));
    all.setButtonTintList(ColorStateList.valueOf(ACCENT));
    usb.addView(all, match());
    all.setOnCheckedChangeListener((button, checked) -> {
      if (binding) return;
      if (!checked && hasInUseStorage())
        confirmReturn(
            () -> inventory.selectAll(false, java.util.Collections.emptyList()),
            () -> renderDevices(true));
      else if (checked && hasUnapprovedStorage()) {
        java.util.List<UsbInventory.Item> offered = inventory.snapshot();
        confirmStorage(() -> inventory.selectAll(true, offered), () -> renderDevices(true));
      } else inventory.selectAll(checked, java.util.Collections.emptyList());
      renderDevices(true);
    });
    selectionHint = text(usb, "", 13, MUTED);
    devices = column();
    devices.setId(R.id.usb_device_list);
    usb.addView(devices, match());
    LinearLayout footer = new AdaptiveActions();
    footer.setGravity(Gravity.CENTER);
    settingsBody.addView(footer, spaced());
    Button logs = button("Сохранить журнал", false);
    logs.setId(R.id.export_log);
    footer.addView(logs, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1));
    logs.setOnClickListener(v -> export.launch("usb-most-log.txt"));
    Button about = button("О приложении", false);
    about.setId(R.id.about_bridge);
    footer.addView(about, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1));
    about.setOnClickListener(v ->
        new AlertDialog.Builder(this)
            .setTitle("USB Мост · " + BuildConfig.VERSION_NAME)
            .setMessage(
                "При первом SSH-подключении USB Мост подготовит приёмник на компьютере. Для"
                    + " установки нужны права администратора. Драйвер самого USB-устройства"
                    + " устанавливается отдельно, если этого требует его производитель.\n\n"
                    + "Поддерживаются control, bulk и interrupt. ISO и USB bulk streams пока"
                    + " недоступны.\n\n"
                    + "На основе USB/IP Server for Android — Cameron Gutman. GPLv3. Журнал не"
                    + " содержит PIN и содержимого USB-запросов.")
            .setNeutralButton(
                "Лицензии",
                (dialog, which) -> startActivity(new Intent(this, LicensesActivity.class)))
            .setPositiveButton("Понятно", null)
            .show());
    buildWelcome();
    buildChooser();
    back =
        new androidx.activity.OnBackPressedCallback(false) {
          @Override
          public void handleOnBackPressed() {
            goBack();
          }
        };
    getOnBackPressedDispatcher().addCallback(this, back);
    page = HOME;
    if (saved != null) {
      ArrayList<Integer> stack = saved.getIntegerArrayList("screens");
      if (stack != null)
        for (Integer screen : stack)
          if (screen != null && screen >= HOME && screen <= SETTINGS && backStack.size() < 8)
            backStack.add(screen);
      int screen = saved.getInt("screen", HOME);
      showPage(screen >= HOME && screen <= SETTINGS ? screen : HOME, false);
    } else {
      showPage(HOME, false);
    }
    if (saved != null)
      for (int i = 0; i < pages.length; i++) {
        final ScrollView scroll = pages[i];
        final int y = saved.getInt("scroll_" + i, 0);
        scroll.post(() -> scroll.scrollTo(0, y));
      }
    render();
  }

  private LinearLayout page(int index, String title, boolean up) {
    ScrollView scroll = new ScrollView(this);
    scroll.setId(View.generateViewId());
    scroll.setSaveEnabled(false); // page/scroll state is saved explicitly, never hidden form state
    scroll.setFillViewport(true);
    scroll.setClipToPadding(false);
    FrameLayout centered = new FrameLayout(this);
    LinearLayout body = new ContentColumn();
    centered.addView(
        body, new FrameLayout.LayoutParams(-1, -2, Gravity.TOP | Gravity.CENTER_HORIZONTAL));
    scroll.addView(centered);
    pages[index] = scroll;
    pageHost.addView(scroll);
    if (up) {
      Button upButton = button("Назад", false);
      body.addView(upButton, match());
      upButton.setOnClickListener(v -> goBack());
    }
    TextView heading = text(body, title, 28, INK);
    heading.setTypeface(Typeface.DEFAULT, Typeface.BOLD);
    androidx.core.view.ViewCompat.setAccessibilityHeading(heading, true);
    return body;
  }

  private void buildWelcome() {
    LinearLayout body = page(WELCOME, "USB на ПК", true);
    text(
        body,
        "Передавайте USB-устройства с телефона или планшета на компьютер с Windows или Linux.",
        17,
        MUTED);
    text(body, "1. Подключите компьютер", 20, INK);
    text(
        body,
        "По QR-коду USB Мост или через SSH. Компьютер должен быть доступен по сети.",
        15,
        MUTED);
    text(body, "2. Выберите USB-устройства", 20, INK);
    text(
        body,
        "Подключите их к телефону или планшету, затем разрешите доступ к нужным устройствам.",
        15,
        MUTED);
    text(body, "3. Запустите мост", 20, INK);
    text(body, "Состояние подключения и выбранные устройства всегда на главном экране.", 15, MUTED);
    Button start = button("Настроить подключение", true);
    body.addView(start, spaced());
    start.setOnClickListener(v -> chooseComputer());
    Button skip = button("Позже", false);
    body.addView(skip, match());
    skip.setOnClickListener(v -> {
      getSharedPreferences("bridge", MODE_PRIVATE).edit().putBoolean("welcome_seen", true).apply();
      home();
    });
  }

  private void buildChooser() {
    LinearLayout body = page(CHOOSE, "Подключить компьютер", true);
    text(body, "Выберите способ настройки", 17, MUTED);
    Button qr = button("По QR-коду", true);
    qr.setId(R.id.setup_qr);
    body.addView(qr, spaced());
    text(
        body,
        "Откройте USB Мост на компьютере и создайте код подключения. Приёмник должен быть"
            + " установлен.",
        14,
        MUTED);
    qr.setOnClickListener(v -> openSetup(1));
    Button ssh = button("Через SSH", false);
    ssh.setId(R.id.setup_ssh);
    body.addView(ssh, spaced());
    text(
        body,
        "Введите адрес и данные входа. Приложение подготовит приёмник при первом соединении. SSH"
            + " должен быть доступен; для установки нужны права администратора.",
        14,
        MUTED);
    ssh.setOnClickListener(v -> openSetup(0));
    Button ip = button("По IP · готовый USB/IP", false);
    ip.setId(R.id.setup_legacy);
    body.addView(ip, spaced());
    text(body, "Для ручной настройки через локальную сеть или Tailscale.", 14, MUTED);
    ip.setOnClickListener(v -> openSetup(2));
  }

  private boolean canEdit() {
    if (computers.snapshot().status != ComputerRepository.Status.READY) {
      manageComputers();
      return false;
    }
    if (!UsbIpService.running && !computers.isBusy()) return true;
    Toast.makeText(this, "Остановите мост, чтобы изменить компьютер", Toast.LENGTH_LONG).show();
    return false;
  }

  private void chooseComputer() {
    initialRoutePending = false;
    if (computers.snapshot().status != ComputerRepository.Status.READY
        || !computers.snapshot().computers.isEmpty()) manageComputers();
    else if (canEdit()) showPage(CHOOSE, true);
  }

  private void manageComputers() {
    initialRoutePending = false;
    setup.launch(new Intent(this, ComputersActivity.class));
  }

  private void openSetup(int transport) {
    if (canEdit())
      setup.launch(
          new Intent(this, org.cgutman.usbip.connection.ConnectionSetup.class)
              .putExtra("initial_transport", transport)
              .putExtra("computer_revision", computers.snapshot().revision));
  }

  private void showPage(int next, boolean push) {
    if (push) initialRoutePending = false;
    android.view.inputmethod.InputMethodManager keyboard =
        (android.view.inputmethod.InputMethodManager) getSystemService(INPUT_METHOD_SERVICE);
    if (keyboard != null) keyboard.hideSoftInputFromWindow(pageHost.getWindowToken(), 0);
    if (push && next != page && backStack.size() < 8) backStack.add(page);
    page = next;
    for (int i = 0; i < pages.length; i++)
      pages[i].setVisibility(i == page ? View.VISIBLE : View.GONE);
    if (back != null) back.setEnabled(page != HOME);
    setTitle(page == HOME ? "USB Мост" : page == SETTINGS ? "Настройки" : "Подключение компьютера");
  }

  private void home() {
    backStack.clear();
    showPage(HOME, false);
    render();
  }

  private void goBack() {
    int previous = backStack.isEmpty() ? HOME : backStack.remove(backStack.size() - 1);
    showPage(previous, false);
  }

  private void completeSetup() {
    getSharedPreferences("bridge", MODE_PRIVATE).edit().putBoolean("welcome_seen", true).apply();
    home();
  }

  private boolean configured() {
    return ConnectionSummary.read(this).configured;
  }

  private void addQuickTile() {
    if (Build.VERSION.SDK_INT >= 33) {
      android.app.StatusBarManager manager = getSystemService(android.app.StatusBarManager.class);
      if (manager != null) {
        try {
          manager.requestAddTileService(
              new ComponentName(this, BridgeTileService.class),
              "USB Мост",
              android.graphics.drawable.Icon.createWithResource(this, R.drawable.ic_bridge_tile),
              getMainExecutor(),
              result -> {
                if (isDestroyed() || isFinishing()) return;
                if (result == android.app.StatusBarManager.TILE_ADD_REQUEST_RESULT_TILE_ADDED
                    || result
                        == android.app.StatusBarManager.TILE_ADD_REQUEST_RESULT_TILE_ALREADY_ADDED)
                  Toast.makeText(this, "Плитка «USB Мост» доступна в шторке", Toast.LENGTH_LONG)
                      .show();
                else if (result
                    != android.app.StatusBarManager.TILE_ADD_REQUEST_RESULT_TILE_NOT_ADDED)
                  quickTileHelp();
              });
          return;
        } catch (RuntimeException unavailable) {
          // Some Android distributions provide no tile-add prompt.
        }
      }
    }
    quickTileHelp();
  }

  private void quickTileHelp() {
    if (confirmationDialog != null) confirmationDialog.dismiss();
    confirmationDialog =
        new AlertDialog.Builder(this)
            .setTitle("Плитка USB Мост")
            .setMessage(
                "Откройте шторку быстрых настроек, нажмите «Изменить» и добавьте «USB Мост». Плитка"
                    + " включает и выключает мост для выбранного компьютера.")
            .setPositiveButton("Понятно", null)
            .show();
  }

  private void toggleBridge() {
    if (UsbIpService.running) requestStop(QuickActions.capture(this));
    else if (!configured()) chooseComputer();
    else requestStart(QuickActions.capture(this));
  }

  private void requestStop(QuickActions.Binding expected) {
    if (expected.action != QuickActions.STOP || !QuickActions.current(this, expected)) return;
    Runnable stop =
        () -> {
          if (QuickActions.current(this, expected) && !QuickActions.locked(this))
            stopService(new Intent(this, UsbIpService.class));
        };
    if (hasSelectedStorage()) confirmReturn(stop, () -> {});
    else stop.run();
  }

  private void requestStart(QuickActions.Binding expected) {
    ComputerRepository.Snapshot snapshot = computers.snapshot();
    if (expected.action != QuickActions.START || !QuickActions.current(this, expected, snapshot)) {
      render();
      return;
    }
    final long requestedRevision = snapshot.revision;
    if (Build.VERSION.SDK_INT >= 33
        && ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS)
            != PackageManager.PERMISSION_GRANTED
        && !getSharedPreferences("bridge", MODE_PRIVATE)
            .getBoolean("notifications_requested", false)) {
      getSharedPreferences("bridge", MODE_PRIVATE)
          .edit()
          .putBoolean("notifications_requested", true)
          .apply();
      notificationRevision = requestedRevision;
      notificationBinding = expected;
      notifications.launch(Manifest.permission.POST_NOTIFICATIONS);
    } else startBridge(requestedRevision);
  }

  private void startBridge(long revision) {
    ComputerRepository.Snapshot snapshot = computers.snapshot();
    if (snapshot.status != ComputerRepository.Status.READY
        || snapshot.revision != revision
        || snapshot.active() == null) {
      Toast.makeText(
              this,
              "Настройки изменились. Проверьте выбранный компьютер и запустите мост снова.",
              Toast.LENGTH_LONG)
          .show();
      render();
      return;
    }
    if (UsbIpService.running || computers.isBusy()) return;
    ContextCompat.startForegroundService(
        this,
        new Intent(this, UsbIpService.class)
            .putExtra(UsbIpService.EXTRA_COMPUTER_REVISION, revision));
  }

  private boolean hasSelectedStorage() {
    for (UsbInventory.Item item : inventory.snapshot())
      if (item.storage && (item.selected || item.inUse)) return true;
    return false;
  }

  private boolean hasUnapprovedStorage() {
    for (UsbInventory.Item i : inventory.snapshot())
      if (i.storage && !i.approved && i.capability == 0) return true;
    return false;
  }

  private boolean hasInUseStorage() {
    for (UsbInventory.Item item : inventory.snapshot()) if (item.storage && item.inUse) return true;
    return false;
  }

  private void confirmReturn(Runnable yes, Runnable no) {
    confirm(
        "Завершить передачу?",
        "Сначала безопасно извлеките накопитель на компьютере. Это защитит файлы от повреждения.",
        "Извлечён · завершить",
        yes,
        no);
  }

  private void confirmStorage(Runnable yes, Runnable no) {
    confirm(
        "Передать накопитель?",
        "Сначала безопасно извлеките накопитель в настройках Android. После работы извлеките его на"
            + " компьютере перед отключением моста.\n\n"
            + "Новые накопители также потребуют подтверждения.",
        "Извлечён · передать",
        yes,
        no);
  }

  private void confirm(String title, String message, String action, Runnable yes, Runnable no) {
    ScrollView scroll = new ScrollView(this);
    LinearLayout content = column();
    content.setPadding(dp(24), dp(8), dp(24), dp(12));
    scroll.addView(content);
    text(content, message, 16, INK);
    Button accept = button(action, true);
    content.addView(accept, spaced());
    Runnable reject =
        () -> {
          no.run();
          renderDevices(true);
        };
    AlertDialog dialog =
        new AlertDialog.Builder(this)
            .setTitle(title)
            .setView(scroll)
            .setNegativeButton("Отмена", (d, which) -> reject.run())
            .setOnCancelListener(d -> reject.run())
            .create();
    accept.setOnClickListener(v -> {
      dialog.dismiss();
      yes.run();
      renderDevices(true);
    });
    if (confirmationDialog != null) confirmationDialog.dismiss();
    confirmationDialog = dialog;
    dialog.show();
  }

  private void render() {
    ComputerRepository.Snapshot savedComputers = computers.snapshot();
    if (initialRoutePending && savedComputers.status != ComputerRepository.Status.LOADING) {
      initialRoutePending = false;
      if (savedComputers.status == ComputerRepository.Status.READY
          && savedComputers.computers.isEmpty()
          && !getSharedPreferences("bridge", MODE_PRIVATE).getBoolean("welcome_seen", false)
          && !UsbIpService.running) showPage(WELCOME, true);
    }
    org.cgutman.usbip.connection.ConnectionController.TrustRequest request =
        org.cgutman.usbip.connection.ConnectionController.trustRequest;
    if (request != null && request != showingTrust) {
      if (trustDialog != null) trustDialog.dismiss();
      showingTrust = request;
      String pin = request.pin;
      trustDialog =
          new AlertDialog.Builder(this)
              .setTitle("Доверять компьютеру?")
              .setMessage(
                  "При первом подключении сверьте отпечаток SSH-ключа компьютера:\n\nSHA256:"
                      + pin.replace('-', '+').replace('_', '/'))
              .setPositiveButton(
                  "Доверять",
                  (d, w) ->
                      org.cgutman.usbip.connection.ConnectionController.approve(request, true))
              .setNegativeButton(
                  "Отмена",
                  (d, w) ->
                      org.cgutman.usbip.connection.ConnectionController.approve(request, false))
              .setOnCancelListener(
                  d -> org.cgutman.usbip.connection.ConnectionController.approve(request, false))
              .show();
    } else if (request == null) {
      showingTrust = null;
      if (trustDialog != null) {
        trustDialog.dismiss();
        trustDialog = null;
      }
    }
    ConnectionSummary summary = ConnectionSummary.read(this);
    if (!summary.title.contentEquals(computerName.getText())) computerName.setText(summary.title);
    if (!summary.detail.contentEquals(computerDetail.getText()))
      computerDetail.setText(summary.detail);
    String setting = summary.title + "\n" + summary.detail;
    if (!setting.contentEquals(settingsComputer.getText())) settingsComputer.setText(setting);
    changeComputer.setVisibility(
        !savedComputers.computers.isEmpty()
                || savedComputers.status == ComputerRepository.Status.FAILED
            ? View.VISIBLE
            : View.GONE);
    boolean running = UsbIpService.running;
    String current = UsbIpService.status;
    if (!current.contentEquals(status.getText())) status.setText(current);
    status.setTextColor(current.startsWith("Не удалось") ? WARNING : running ? ACCENT : MUTED);
    diagnoseInline.setVisibility(current.startsWith("Не удалось") ? View.VISIBLE : View.GONE);
    String label =
        running
            ? "Остановить мост"
            : summary.configured ? "Запустить мост" : "Настроить подключение";
    if (!label.contentEquals(toggle.getText())) toggle.setText(label);
    toggle.setEnabled(
        running
            || (savedComputers.status != ComputerRepository.Status.LOADING && !computers.isBusy()));
    inventory.refresh();
    renderDevices(false);
  }

  private List<String> deviceIps(int networkMode) {
    Set<String> values = new TreeSet<>();
    try {
      Enumeration<NetworkInterface> interfaces = NetworkInterface.getNetworkInterfaces();
      while (interfaces.hasMoreElements()) {
        Enumeration<InetAddress> ips = interfaces.nextElement().getInetAddresses();
        while (ips.hasMoreElements()) {
          InetAddress ip = ips.nextElement();
          if (ip instanceof Inet4Address
              && NativePolicy.validHost(ip.getHostAddress(), networkMode))
            values.add(ip.getHostAddress());
        }
      }
    } catch (SocketException ignored) {
    }
    return new ArrayList<>(values);
  }

  private void connectionHelp() {
    if (org.cgutman.usbip.connection.ConnectionController.enabled(this)) {
      new AlertDialog.Builder(this)
          .setTitle("Выбранный компьютер")
          .setMessage(
              "При SSH-подключении приложение установит и настроит приёмник, если у учётной"
                  + " записи есть права администратора. В Linux заранее нужны systemd и модуль"
                  + " vhci_hcd для вашего ядра. Если их нет, приложение сообщит, что нужно"
                  + " подготовить на компьютере. Для подключения по коду сначала установите"
                  + " приёмник USB Мост на компьютере.\n\n"
                  + "Для кнопки «Извлечь на компьютере» в Linux нужна системная служба UDisks2."
                  + " Если её нет, администратор может установить пакет udisks2. Передача USB"
                  + " работает и без неё — завершайте работу с накопителем средствами компьютера.")
          .setPositiveButton("Понятно", null)
          .show();
      return;
    }
    final ComputerRepository.Snapshot computerSnapshot = computers.snapshot();
    final ComputerRepository.Computer activeComputer = computerSnapshot.active();
    if (activeComputer == null) {
      chooseComputer();
      return;
    }
    final int shownMode = activeComputer.mode;
    List<String> ips = deviceIps(shownMode);
    final long revision = inventory.revision();
    List<Integer> eligible = new ArrayList<>();
    for (UsbInventory.Item item : inventory.snapshot())
      if (item.selected
          && item.ready
          && !item.inUse
          && inventory.authorized(item.device.getDeviceId()))
        eligible.add(item.device.getDeviceId());
    int[] ids = new int[eligible.size()];
    for (int i = 0; i < ids.length; i++) ids[i] = eligible.get(i);
    ScrollView scroll = new ScrollView(this);
    LinearLayout content = column();
    content.setPadding(dp(24), dp(8), dp(24), dp(8));
    scroll.addView(content);
    TextView explanation =
        text(
            content,
            "На компьютере нужен USB/IP-клиент и драйвер USB-устройства. Компьютер подключается к"
                + " телефону или планшету. Сохранённый IP компьютера ограничивает доступ."
                + " Одновременно — до четырёх USB-устройств.\n\n"
                + "Linux: установите пакет USB/IP для вашего ядра, запустите мост и выполните"
                + " команды ниже. В Windows нужен совместимый USB/IP-клиент с подписанным"
                + " драйвером; эти команды — для Linux.",
            15,
            INK);
    TextView commands = text(content, "", 14, ACCENT);
    commands.setId(R.id.connection_commands);
    commands.setTypeface(Typeface.MONOSPACE);
    commands.setTextIsSelectable(true);
    Spinner address = new Spinner(this);
    address.setId(R.id.connection_address);
    if (ips.isEmpty()) {
      commands.setText(
          shownMode == 0
              ? "Подключите Tailscale на Android-устройстве: его IPv4 пока недоступен."
              : "Подключите телефон или планшет к локальной сети: её IPv4 пока недоступен.");
    } else {
      ArrayAdapter<String> adapter =
          new ArrayAdapter<>(this, android.R.layout.simple_spinner_item, ips);
      adapter.setDropDownViewResource(android.R.layout.simple_spinner_dropdown_item);
      address.setAdapter(adapter);
      content.addView(address, 1, match());
      commands.setText(NativePolicy.connectionCommands(ips.get(0), shownMode, ids));
      address.setOnItemSelectedListener(
          new AdapterView.OnItemSelectedListener() {
            public void onItemSelected(AdapterView<?> parent, View view, int position, long id) {
              commands.setText(NativePolicy.connectionCommands(ips.get(position), shownMode, ids));
            }

            public void onNothingSelected(AdapterView<?> parent) {}
          });
      text(
          content,
          ids.length == 0
              ? "Команда list проверит связь. Выберите USB-устройства; для накопителя сначала"
                  + " подтвердите передачу."
              : "Показаны выбранные устройства, ожидающие подключения ПК. Уже подключённые сюда не"
                  + " включены.",
          13,
          MUTED);
    }
    Button copy = button("Копировать команды", true);
    if (!ips.isEmpty()) content.addView(copy, spaced());
    AlertDialog dialog =
        new AlertDialog.Builder(this)
            .setTitle("Подключить компьютер")
            .setView(scroll)
            .setNegativeButton("Закрыть", null)
            .create();
    copy.setOnClickListener(v -> {
      if (!computers.snapshot().connectionKey().equals(computerSnapshot.connectionKey())
          || inventory.revision() != revision
          || !deviceIps(shownMode).equals(ips)) {
        dialog.dismiss();
        Toast.makeText(
                this, "Устройства или сеть изменились · команды обновлены", Toast.LENGTH_LONG)
            .show();
        connectionHelp();
        return;
      }
      ClipboardManager clipboard = (ClipboardManager) getSystemService(CLIPBOARD_SERVICE);
      if (clipboard != null)
        clipboard.setPrimaryClip(ClipData.newPlainText("USB Мост", commands.getText()));
      dialog.dismiss();
    });
    dialog.show();
  }

  private void renderDevices(boolean force) {
    if (!force && seen == inventory.revision()) return;
    seen = inventory.revision();
    List<UsbInventory.Item> items = inventory.snapshot();
    binding = true;
    all.setChecked(inventory.all());
    binding = false;
    int count = 0;
    for (UsbInventory.Item i : items) if (i.selected) count++;
    selectionHint.setText(
        items.isEmpty()
            ? "Подключите устройство через OTG или USB-хаб"
            : "Выбрано: "
                + count
                + " из "
                + items.size()
                + " · новые устройства появляются автоматически");
    Set<Integer> live = new HashSet<>();
    for (UsbInventory.Item item : items) live.add(item.device.getDeviceId());
    for (Integer id : new ArrayList<>(rows.keySet()))
      if (!live.contains(id)) {
        devices.removeView(rows.remove(id));
      }
    if (items.isEmpty()) {
      devices.removeAllViews();
      rows.clear();
      TextView empty = text(devices, "Пока ничего не подключено", 17, INK);
      empty.setPadding(0, dp(22), 0, dp(22));
      return;
    }
    if (rows.isEmpty()) devices.removeAllViews();
    for (UsbInventory.Item item : items) {
      int id = item.device.getDeviceId();
      LinearLayout row = rows.get(id);
      if (row == null) {
        row = column();
        row.setPadding(0, dp(8), 0, dp(12));
        devices.addView(row, match());
        rows.put(id, row);
        CheckBox choice = new CheckBox(this);
        choice.setTextSize(17);
        choice.setTextColor(INK);
        choice.setMinHeight(dp(48));
        choice.setButtonTintList(ColorStateList.valueOf(ACCENT));
        LinearLayout header = new LinearLayout(this);
        header.setOrientation(LinearLayout.HORIZONTAL);
        header.setGravity(Gravity.CENTER_VERTICAL);
        header.addView(choice, new LinearLayout.LayoutParams(0, -2, 1));
        Button options = new Button(this);
        options.setText("⋮");
        options.setTextSize(22);
        options.setMinWidth(0);
        options.setMinimumWidth(0);
        options.setMinHeight(dp(48));
        options.setMinimumHeight(dp(48));
        options.setPadding(0, 0, 0, 0);
        options.setBackgroundColor(android.graphics.Color.TRANSPARENT);
        header.addView(options, new LinearLayout.LayoutParams(dp(48), -2));
        row.addView(header, match());
        text(row, "", 12, MUTED);
        text(row, "", 13, ACCENT);
        Button eject = new Button(this);
        eject.setText("Извлечь на компьютере");
        eject.setAllCaps(false);
        eject.setTextColor(ACCENT);
        eject.setBackgroundColor(android.graphics.Color.TRANSPARENT);
        eject.setMinHeight(dp(48));
        row.addView(eject, match());
      }
      LinearLayout header = (LinearLayout) row.getChildAt(0);
      CheckBox choice = (CheckBox) header.getChildAt(0);
      Button options = (Button) header.getChildAt(1);
      options.setContentDescription("Настройки устройства: " + item.name);
      options.setOnClickListener(view -> {
        UsbInventory.Item current = inventory.item(id);
        if (current != null && current.generation == item.generation) {
          if (deviceDialog != null) deviceDialog.dismiss();
          deviceDialog = UsbDeviceSettings.show(this, inventory, current);
        }
      });
      choice.setOnCheckedChangeListener(null);
      choice.setText(item.name);
      choice.setTextColor(item.capability == 0 ? INK : MUTED);
      choice.setChecked(item.selected);
      choice.setEnabled(item.capability == 0 && item.ejectionState != 1);
      choice.setContentDescription(item.name + ", " + item.detail);
      ((TextView) row.getChildAt(1)).setText(item.detail);
      String state =
          item.ejectionState > 0
              ? ejectionState(item.ejectionState)
              : item.capability != 0
                  ? reason(item.capability)
                  : item.notice != 0
                      ? notice(item.notice)
                      : item.storage && !item.approved
                          ? "Сначала извлеките в Android"
                          : !item.selected ? "Не выбрано" : item.state;
      ((TextView) row.getChildAt(2)).setText(state);
      ((TextView) row.getChildAt(2))
          .setTextColor(
              item.ejectionState == 2
                  ? MUTED
                  : item.ejectionState >= 3
                          || item.capability != 0
                          || item.notice != 0
                          || item.storage && !item.approved
                          || item.selected && !item.ready
                      ? WARNING
                      : item.selected ? ACCENT : MUTED);
      Button eject = (Button) row.getChildAt(3);
      eject.setContentDescription("Извлечь на компьютере: " + item.name);
      eject.setVisibility(item.canEject || item.ejectionState == 1 ? View.VISIBLE : View.GONE);
      eject.setEnabled(item.canEject);
      eject.setAlpha(item.canEject ? 1f : 0.5f);
      eject.setOnClickListener(view -> {
        inventory.requestEjection(id, item.generation);
        renderDevices(true);
      });
      choice.setButtonTintList(ColorStateList.valueOf(item.capability == 0 ? ACCENT : 0xFF8B98A3));
      choice.setOnCheckedChangeListener((button, checked) -> {
        UsbInventory.Item current = inventory.item(id);
        if (current == null || current.generation != item.generation) {
          renderDevices(true);
          return;
        }
        if (!checked && current.canEject) {
          inventory.requestEjection(id, current.generation);
        } else if (!checked && current.storage && current.inUse)
          confirmReturn(
              () -> inventory.select(id, current.generation, false, false),
              () -> renderDevices(true));
        else if (checked && current.storage && !current.approved)
          confirmStorage(
              () -> inventory.select(id, current.generation, true, true),
              () -> renderDevices(true));
        else inventory.select(id, current.generation, checked, false);
        renderDevices(true);
      });
    }
  }

  private String ejectionState(int state) {
    switch (state) {
      case 1:
        return "Завершение работы с накопителем…";
      case 2:
        return "Передача завершена · для повторной выберите снова";
      case 3:
        return "Закройте файлы на компьютере и повторите извлечение";
      case 4:
        return "Компьютер не разрешил извлечение · проверьте права";
      case 5:
        return "Извлеките накопитель средствами компьютера";
      case 6:
        return "Компьютер больше не видит это подключение";
      default:
        return "Извлечение не подтверждено · проверьте накопитель на компьютере";
    }
  }

  private String notice(int code) {
    switch (code) {
      case 1:
        return "Передача завершена · для повторной выберите снова";
      case 2:
        return "Не удалось открыть USB · выберите повторно";
      case 3:
        return "Не удалось настроить USB · выберите повторно";
      case 4:
        return "Интерфейс USB занят · выберите повторно";
      default:
        return "Не удалось начать передачу · выберите повторно";
    }
  }

  private String reason(int code) {
    switch (code) {
      case 1:
        return "USB-хаб · выбирайте подключённые к нему устройства";
      case 2:
        return "ISO-передача пока не поддерживается";
      case 4:
        return "UAS / bulk streams пока не поддерживаются";
      default:
        return "Нет доступных USB-интерфейсов";
    }
  }

  private LinearLayout column() {
    LinearLayout view = new LinearLayout(this);
    view.setOrientation(LinearLayout.VERTICAL);
    return view;
  }

  private LinearLayout section(LinearLayout body) {
    View rule = new View(this);
    rule.setBackgroundColor(0xFFDDE4EA);
    LinearLayout.LayoutParams line =
        new LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, dp(1));
    line.topMargin = dp(24);
    line.bottomMargin = dp(16);
    body.addView(rule, line);
    LinearLayout view = column();
    body.addView(view, match());
    return view;
  }

  /** Measure within the actual parent window, including split-screen and live resizing. */
  private final class ContentColumn extends LinearLayout {
    ContentColumn() {
      super(UsbIpConfig.this);
      setOrientation(VERTICAL);
    }

    @Override
    protected void onMeasure(int widthSpec, int heightSpec) {
      int available =
          View.MeasureSpec.getMode(widthSpec) == View.MeasureSpec.UNSPECIFIED
              ? dp(640)
              : View.MeasureSpec.getSize(widthSpec);
      int width = Math.min(available, dp(640));
      int inset = width < dp(360) ? dp(16) : dp(24);
      setPadding(inset, dp(24), inset, dp(24));
      super.onMeasure(
          View.MeasureSpec.makeMeasureSpec(width, View.MeasureSpec.EXACTLY), heightSpec);
    }
  }

  private final class AdaptiveActions extends LinearLayout {
    AdaptiveActions() {
      super(UsbIpConfig.this);
    }

    @Override
    protected void onMeasure(int widthSpec, int heightSpec) {
      adaptRow(this, View.MeasureSpec.getSize(widthSpec));
      super.onMeasure(widthSpec, heightSpec);
    }
  }

  private void adaptRow(LinearLayout row, int width) {
    float widest = 0;
    for (int i = 0; i < row.getChildCount(); i++) {
      TextView child = (TextView) row.getChildAt(i);
      float label =
          child.getPaint().measureText(child.getText().toString())
              + child.getCompoundPaddingLeft()
              + child.getCompoundPaddingRight();
      widest = Math.max(widest, Math.max(child.getMinimumWidth(), label));
    }
    float required = widest * row.getChildCount() + dp(8) * Math.max(0, row.getChildCount() - 1);
    boolean stack = required > width - row.getPaddingLeft() - row.getPaddingRight();
    row.setOrientation(stack ? LinearLayout.VERTICAL : LinearLayout.HORIZONTAL);
    for (int i = 0; i < row.getChildCount(); i++) {
      View child = row.getChildAt(i);
      int desiredWidth = stack ? ViewGroup.LayoutParams.MATCH_PARENT : 0;
      float weight = stack ? 0 : 1;
      int top = stack && i != 0 ? dp(8) : 0;
      int start = !stack && i != 0 ? dp(8) : 0;
      LinearLayout.LayoutParams old = (LinearLayout.LayoutParams) child.getLayoutParams();
      if (old.width == desiredWidth
          && old.weight == weight
          && old.topMargin == top
          && old.getMarginStart() == start) continue;
      LinearLayout.LayoutParams next =
          row instanceof RadioGroup
              ? new RadioGroup.LayoutParams(
                  desiredWidth, ViewGroup.LayoutParams.WRAP_CONTENT, weight)
              : new LinearLayout.LayoutParams(
                  desiredWidth, ViewGroup.LayoutParams.WRAP_CONTENT, weight);
      next.topMargin = top;
      next.setMarginStart(start);
      child.setLayoutParams(next);
    }
  }

  private TextView text(LinearLayout parent, String value, int size, int color) {
    TextView view = new TextView(this);
    view.setText(value);
    view.setTextSize(size);
    view.setTextColor(color);
    view.setPadding(0, dp(5), 0, dp(7));
    parent.addView(view, match());
    return view;
  }

  private Button button(String value, boolean primary) {
    return BridgeUi.button(this, value, primary);
  }

  private GradientDrawable inputShape(int fill, int stroke, int weight) {
    GradientDrawable shape = shape(fill, 8);
    shape.setStroke(dp(weight), stroke);
    return shape;
  }

  private GradientDrawable shape(int color, int radius) {
    GradientDrawable shape = new GradientDrawable();
    shape.setColor(color);
    shape.setCornerRadius(dp(radius));
    return shape;
  }

  private LinearLayout.LayoutParams match() {
    return new LinearLayout.LayoutParams(
        ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT);
  }

  private LinearLayout.LayoutParams spaced() {
    LinearLayout.LayoutParams p = match();
    p.topMargin = dp(14);
    return p;
  }

  private int dp(int value) {
    return Math.round(value * getResources().getDisplayMetrics().density);
  }

  @Override
  protected void onSaveInstanceState(Bundle state) {
    state.putInt("screen", page);
    state.putIntegerArrayList("screens", new ArrayList<>(backStack));
    for (int i = 0; i < pages.length; i++) state.putInt("scroll_" + i, pages[i].getScrollY());
    state.putBoolean("initial_route_pending", initialRoutePending);
    super.onSaveInstanceState(state);
  }

  private void consumeQuickAction() {
    // Older Android can briefly resume us while the private entry is finishing,
    // then pause again to deliver its new Intent. A dialog created in that
    // transient resume would immediately be retired by onPause.
    if (!quickResumed || !hasWindowFocus()) return;
    QuickActions.Binding action = QuickActions.consume(this);
    if (action == null) return;
    if (configured()) home();
    if (action.action == QuickActions.START) requestStart(action);
    else if (action.action == QuickActions.STOP) requestStop(action);
  }

  @Override
  protected void onPostResume() {
    super.onPostResume();
    quickResumed = true;
    consumeQuickAction();
  }

  @Override
  public void onWindowFocusChanged(boolean hasFocus) {
    super.onWindowFocusChanged(hasFocus);
    if (hasFocus) consumeQuickAction();
  }

  @Override
  protected void onNewIntent(Intent intent) {
    super.onNewIntent(intent);
    // No extras from the exported launcher are commands. Only the in-memory
    // request accepted by the private Activity can reach this path.
    handler.post(this::consumeQuickAction);
  }

  @Override
  protected void onResume() {
    super.onResume();
    handler.removeCallbacks(refresh);
    handler.post(refresh);
  }

  @Override
  protected void onPause() {
    quickResumed = false;
    handler.removeCallbacks(refresh);
    // Retire this unsaved editor before the Activity token; returning from the
    // background must not resurrect an editor for an obsolete USB attachment.
    if (deviceDialog != null) {
      deviceDialog.dismiss();
      deviceDialog = null;
    }
    if (confirmationDialog != null) {
      confirmationDialog.dismiss();
      confirmationDialog = null;
    }
    super.onPause();
  }

  @Override
  protected void onDestroy() {
    handler.removeCallbacks(refresh);
    if (deviceDialog != null) deviceDialog.dismiss();
    if (trustDialog != null) trustDialog.dismiss();
    if (confirmationDialog != null) confirmationDialog.dismiss();
    super.onDestroy();
  }
}
