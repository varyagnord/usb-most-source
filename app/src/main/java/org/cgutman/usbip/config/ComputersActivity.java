package org.cgutman.usbip.config;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.*;
import android.content.res.ColorStateList;
import android.graphics.Typeface;
import android.os.*;
import android.view.*;
import android.widget.*;
import java.nio.charset.StandardCharsets;
import java.util.Arrays;
import java.util.concurrent.CompletableFuture;
import org.cgutman.usbip.connection.ComputerRepository;
import org.cgutman.usbip.connection.ConnectionSetup;
import org.cgutman.usbip.service.UsbInventory;
import org.cgutman.usbip.service.UsbIpService;
import org.cgutman.usbipserverforandroid.R;
import org.json.JSONObject;

/** Public identity/selection UI. Credentials never enter this Activity. */
public final class ComputersActivity extends Activity {
  private static final int INK = 0xFF172B3A,
      MUTED = 0xFF586875,
      ACCENT = 0xFF225FA5,
      PAPER = 0xFFFAFBFC;
  private ComputerRepository repository;
  private LinearLayout body;
  private ScrollView scroll;
  private boolean pending;
  private final Handler main = new Handler(Looper.getMainLooper());
  private final Runnable changed = this::render;

  @Override
  public void onCreate(Bundle state) {
    super.onCreate(state);
    if (getActionBar() != null) getActionBar().hide();
    setTitle("Компьютеры и доступ");
    getWindow().setStatusBarColor(PAPER);
    getWindow().setNavigationBarColor(PAPER);
    getWindow()
        .getDecorView()
        .setSystemUiVisibility(
            View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR | View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR);
    repository = ComputerRepository.get(this);
    scroll = new ScrollView(this);
    scroll.setBackgroundColor(PAPER);
    scroll.setFillViewport(true);
    FrameLayout centered = new FrameLayout(this);
    body =
        new LinearLayout(this) {
          @Override
          protected void onMeasure(int widthSpec, int heightSpec) {
            int width = Math.min(View.MeasureSpec.getSize(widthSpec), dp(640));
            setPadding(
                width < dp(360) ? dp(16) : dp(24),
                dp(16),
                width < dp(360) ? dp(16) : dp(24),
                dp(24));
            super.onMeasure(
                View.MeasureSpec.makeMeasureSpec(width, View.MeasureSpec.EXACTLY), heightSpec);
          }
        };
    body.setOrientation(LinearLayout.VERTICAL);
    centered.addView(
        body, new FrameLayout.LayoutParams(-1, -2, Gravity.TOP | Gravity.CENTER_HORIZONTAL));
    scroll.addView(centered);
    setContentView(scroll);
    render();
  }

  @Override
  protected void onStart() {
    super.onStart();
    repository.addListener(changed);
    render();
  }

  @Override
  protected void onStop() {
    repository.removeListener(changed);
    super.onStop();
  }

  @Override
  protected void onDestroy() {
    main.removeCallbacksAndMessages(null);
    super.onDestroy();
  }

  @Override
  protected void onActivityResult(int request, int result, Intent data) {
    super.onActivityResult(request, result, data);
    if (request == 1 && result == RESULT_OK) setResult(RESULT_OK);
    render();
  }

  private void render() {
    if (isDestroyed() || isFinishing()) return;
    int y = scroll.getScrollY();
    body.removeAllViews();
    button(body, "Назад", false, this::finish);
    TextView heading = text(body, "Компьютеры и доступ", 26, INK);
    heading.setTypeface(Typeface.DEFAULT, Typeface.BOLD);
    androidx.core.view.ViewCompat.setAccessibilityHeading(heading, true);
    ComputerRepository.Snapshot snapshot = repository.snapshot();
    if (snapshot.status == ComputerRepository.Status.LOADING) {
      text(body, "Загрузка…", 16, MUTED);
      return;
    }
    if (snapshot.status == ComputerRepository.Status.FAILED) {
      text(body, "Сохранённые данные недоступны", 20, INK);
      text(
          body,
          "Повторите попытку. Если данные не восстановятся, можно удалить сохранённые подключения и"
              + " настроить их заново.",
          16,
          MUTED);
      button(body, "Повторить", true, () -> run(repository.reload()));
      button(
          body,
          "Удалить сохранённые подключения",
          false,
          () ->
              new AlertDialog.Builder(this)
                  .setTitle("Удалить все подключения?")
                  .setMessage(
                      "Сохранённые компьютеры, данные входа и доверие будут удалены с этого"
                          + " устройства. На компьютерах настройки доступа останутся прежними.")
                  .setNegativeButton("Отмена", null)
                  .setPositiveButton("Удалить", (d, w) -> run(repository.reset()))
                  .show());
      return;
    }
    if (pending) text(body, "Завершение подключения…", 16, MUTED);
    if (snapshot.computers.isEmpty())
      text(body, "Добавьте компьютер, чтобы передавать ему USB-устройства.", 17, MUTED);
    for (ComputerRepository.Computer computer : snapshot.computers) {
      LinearLayout row = new LinearLayout(this);
      row.setOrientation(LinearLayout.HORIZONTAL);
      row.setGravity(Gravity.TOP);
      body.addView(row, new LinearLayout.LayoutParams(-1, -2));
      RadioButton select = new RadioButton(this);
      select.setText(computer.name);
      select.setTextSize(18);
      select.setTextColor(INK);
      select.setMinHeight(dp(52));
      select.setButtonTintList(ColorStateList.valueOf(ACCENT));
      select.setChecked(computer.id.equals(snapshot.activeId));
      select.setEnabled(!pending);
      select.setTag("computer:" + computer.id);
      row.addView(select, new LinearLayout.LayoutParams(0, -2, 1));
      select.setOnClickListener(v -> {
        if (computer.id.equals(repository.snapshot().activeId)) return;
        if (UsbIpService.running || repository.isBusy())
          confirmStop(
              "Сменить компьютер?",
              computer,
              snapshot,
              () -> mutate("activate", computer, snapshot));
        else mutate("activate", computer, snapshot);
      });
      Button more = new Button(this);
      more.setText("⋮");
      more.setTextSize(24);
      more.setTextColor(ACCENT);
      more.setContentDescription("Доступ и настройки: " + computer.name);
      more.setPadding(0, 0, 0, 0);
      more.setBackgroundTintList(ColorStateList.valueOf(PAPER));
      more.setMinHeight(dp(48));
      more.setEnabled(!pending);
      row.addView(more, new LinearLayout.LayoutParams(dp(48), -2));
      more.setOnClickListener(v -> details(computer, snapshot));
      text(body, detail(computer), 14, MUTED).setPadding(dp(32), 0, 0, dp(12));
      View divider = new View(this);
      divider.setBackgroundColor(0xFFE7EDF2);
      body.addView(divider, new LinearLayout.LayoutParams(-1, dp(1)));
    }
    Button add =
        button(
            body,
            "Добавить компьютер",
            true,
            () -> {
              if (UsbIpService.running || repository.isBusy()) {
                Toast.makeText(this, "Сначала остановите мост на главном экране", Toast.LENGTH_LONG)
                    .show();
                return;
              }
              startActivityForResult(
                  new Intent(this, ConnectionSetup.class)
                      .putExtra("computer_revision", repository.snapshot().revision)
                      .putExtra("initial_transport", 1),
                  1);
            });
    add.setId(R.id.add_computer);
    add.setEnabled(!pending);
    scroll.post(() -> scroll.scrollTo(0, y));
  }

  public static String detail(ComputerRepository.Computer computer) {
    if (computer.kind.equals("legacy"))
      return computer.host
          + " · "
          + (computer.mode == 0 ? "USB/IP через Tailscale" : "USB/IP в локальной сети");
    try {
      JSONObject profile = new JSONObject(computer.profile),
          endpoint = profile.getJSONObject("endpoint");
      String host = endpoint.getString("host");
      if (host.contains(":")) host = "[" + host + "]";
      return host
          + ":"
          + endpoint.getInt("port")
          + " · "
          + (profile.getString("transport").equals("ssh") ? "SSH" : "USB Мост");
    } catch (Exception invalid) {
      return "Параметры недоступны";
    }
  }

  private void usbRules(
      ComputerRepository.Computer computer,
      ComputerRepository.Snapshot before,
      org.json.JSONArray rules) {
    String[] labels = new String[rules.length()];
    try {
      for (int i = 0; i < rules.length(); i++) {
        JSONObject rule = rules.getJSONObject(i), identity = rule.getJSONObject("identity");
        String name = rule.getString("name");
        if (name.isEmpty())
          name =
              String.format(
                  java.util.Locale.ROOT,
                  "USB %04x:%04x",
                  identity.getInt("vendor"),
                  identity.getInt("product"));
        labels[i] = name + (rule.getBoolean("automatic") ? " · автовыбор" : " · ручной выбор");
      }
      new AlertDialog.Builder(this)
          .setTitle("Правила USB · " + computer.name)
          .setItems(
              labels,
              (dialog, index) ->
                  new AlertDialog.Builder(this)
                      .setTitle("Забыть правило?")
                      .setMessage(
                          "Сохранённое имя и автовыбор будут удалены. Уже выбранное устройство"
                              + " останется в текущем сеансе.")
                      .setNegativeButton("Отмена", null)
                      .setPositiveButton(
                          "Забыть",
                          (d, which) -> {
                            byte[] command = null;
                            try {
                              command =
                                  new JSONObject()
                                      .put("op", "forget_usb_rule")
                                      .put("id", computer.id)
                                      .put("revision", before.revision)
                                      .put(
                                          "identity",
                                          rules.getJSONObject(index).getJSONObject("identity"))
                                      .toString()
                                      .getBytes(java.nio.charset.StandardCharsets.UTF_8);
                              run(repository.change(command));
                            } catch (Exception failure) {
                              Toast.makeText(
                                      this, "Откройте список правил заново", Toast.LENGTH_LONG)
                                  .show();
                            } finally {
                              if (command != null) java.util.Arrays.fill(command, (byte) 0);
                            }
                          })
                      .show())
          .setNegativeButton("Закрыть", null)
          .show();
    } catch (Exception failure) {
      Toast.makeText(this, "Не удалось открыть правила", Toast.LENGTH_LONG).show();
    }
  }

  private void details(ComputerRepository.Computer computer, ComputerRepository.Snapshot before) {
    LinearLayout content = column();
    ScrollView page = new ScrollView(this);
    page.addView(content);
    text(content, computer.name, 22, INK).setTypeface(Typeface.DEFAULT, Typeface.BOLD);
    text(content, detail(computer), 16, MUTED);
    String pin = null, transport = "legacy";
    try {
      if (computer.kind.equals("secure")) {
        JSONObject profile = new JSONObject(computer.profile);
        transport = profile.getString("transport");
        String field = transport.equals("ssh") ? "host_key" : "server_pin";
        if (!profile.isNull(field)) pin = profile.getString(field);
        if (transport.equals("ssh"))
          text(
              content,
              "Пользователь: "
                  + android.text.BidiFormatter.getInstance().unicodeWrap(profile.getString("user")),
              16,
              INK);
      }
    } catch (Exception ignored) {
    }
    if (pin != null) {
      text(
          content,
          transport.equals("ssh") ? "Сохранённый отпечаток SSH" : "Сохранённый отпечаток USB Мост",
          14,
          MUTED);
      String shown =
          transport.equals("ssh") ? "SHA256:" + pin.replace('-', '+').replace('_', '/') : pin;
      TextView identity = text(content, shown, 14, INK);
      identity.setTypeface(Typeface.MONOSPACE);
      identity.setTextIsSelectable(true);
      button(
          content,
          "Копировать отпечаток",
          false,
          () -> {
            getSystemService(ClipboardManager.class)
                .setPrimaryClip(ClipData.newPlainText("Отпечаток компьютера", shown));
            Toast.makeText(this, "Отпечаток скопирован", Toast.LENGTH_SHORT).show();
          });
    } else
      text(
          content,
          transport.equals("ssh")
              ? "Ключ компьютера ещё не подтверждён"
              : "Доступ ограничен сохранённым IP-адресом",
          14,
          MUTED);
    text(content, "Удаление здесь не меняет настройки доступа на компьютере.", 14, MUTED);
    AlertDialog dialog =
        new AlertDialog.Builder(this).setView(page).setNegativeButton("Закрыть", null).create();
    try {
      org.json.JSONArray rules = new org.json.JSONArray(computer.usbRules);
      if (rules.length() != 0)
        button(
            content,
            "Правила USB · " + rules.length(),
            false,
            () -> {
              dialog.dismiss();
              usbRules(computer, before, rules);
            });
    } catch (org.json.JSONException ignored) {
    }
    button(
        content,
        "Переименовать",
        false,
        () -> {
          dialog.dismiss();
          rename(computer, before);
        });
    button(
        content,
        "Изменить подключение",
        false,
        () -> {
          dialog.dismiss();
          Runnable edit =
              () -> {
                if (!current(computer, before)) return;
                startActivityForResult(
                    new Intent(this, ConnectionSetup.class)
                        .putExtra("computer_id", computer.id)
                        .putExtra("computer_revision", repository.snapshot().revision),
                    1);
              };
          if (UsbIpService.running || repository.isBusy())
            confirmStop("Остановить мост для настройки?", computer, before, edit);
          else edit.run();
        });
    button(
        content,
        "Забыть на этом устройстве",
        false,
        () -> {
          dialog.dismiss();
          boolean active =
              computer.id.equals(before.activeId) && (UsbIpService.running || repository.isBusy());
          if (active)
            confirmStop(
                "Остановить и забыть компьютер?",
                computer,
                before,
                () -> mutate("forget", computer, before));
          else
            new AlertDialog.Builder(this)
                .setTitle("Забыть компьютер?")
                .setMessage(
                    "Данные входа и сохранённое доверие к «"
                        + computer.name
                        + "» будут удалены с этого устройства.")
                .setNegativeButton("Отмена", null)
                .setPositiveButton("Забыть", (d, w) -> mutate("forget", computer, before))
                .show();
        });
    dialog.show();
  }

  private void rename(ComputerRepository.Computer computer, ComputerRepository.Snapshot before) {
    EditText name = new EditText(this);
    name.setText(computer.name);
    name.setSingleLine(true);
    name.setSelectAllOnFocus(true);
    LinearLayout content = column();
    content.addView(name, new LinearLayout.LayoutParams(-1, -2));
    AlertDialog dialog =
        new AlertDialog.Builder(this)
            .setTitle("Название компьютера")
            .setView(content)
            .setNegativeButton("Отмена", null)
            .setPositiveButton("Сохранить", null)
            .create();
    dialog.setOnShowListener(d ->
        dialog
            .getButton(AlertDialog.BUTTON_POSITIVE)
            .setOnClickListener(v -> {
              try {
                JSONObject command =
                    new JSONObject()
                        .put("op", "rename")
                        .put("revision", before.revision)
                        .put("id", computer.id)
                        .put("name", name.getText().toString().trim());
                dialog.getButton(AlertDialog.BUTTON_POSITIVE).setEnabled(false);
                submit(command)
                    .whenComplete((snapshot, error) ->
                        main.post(() -> {
                          if (isDestroyed() || isFinishing()) return;
                          if (error == null) {
                            dialog.dismiss();
                            setResult(RESULT_OK);
                            render();
                          } else {
                            name.setError(
                                "Проверьте название. Если данные изменились, откройте настройку"
                                    + " заново.");
                            dialog.getButton(AlertDialog.BUTTON_POSITIVE).setEnabled(true);
                          }
                        }));
              } catch (Exception invalid) {
                changedMessage();
              }
            }));
    dialog.show();
  }

  private boolean current(
      ComputerRepository.Computer computer, ComputerRepository.Snapshot before) {
    ComputerRepository.Snapshot now = repository.snapshot();
    if (now.status == ComputerRepository.Status.READY
        && java.util.Objects.equals(before.activeId, now.activeId))
      for (ComputerRepository.Computer item : now.computers)
        if (item.id.equals(computer.id) && item.generation == computer.generation) return true;
    changedMessage();
    return false;
  }

  private void mutate(
      String operation, ComputerRepository.Computer computer, ComputerRepository.Snapshot before) {
    if (!current(computer, before)) {
      render();
      return;
    }
    try {
      run(
          submit(
              new JSONObject()
                  .put("op", operation)
                  .put("revision", repository.snapshot().revision)
                  .put("id", computer.id)));
    } catch (Exception invalid) {
      changedMessage();
    }
  }

  private boolean storageSelected() {
    for (UsbInventory.Item item : UsbInventory.get(this).snapshot())
      if (item.storage && (item.selected || item.inUse)) return true;
    return false;
  }

  private void confirmStop(
      String title,
      ComputerRepository.Computer computer,
      ComputerRepository.Snapshot before,
      Runnable after) {
    boolean storage = storageSelected();
    String message =
        storage
            ? "Сначала безопасно извлеките накопители на компьютере. Затем мост остановит передачу"
                + " USB."
            : "Передача выбранных USB-устройств на компьютер будет остановлена.";
    if (title.contains("забыть"))
      message += " Данные входа и сохранённое доверие будут удалены с этого устройства.";
    new AlertDialog.Builder(this)
        .setTitle(title)
        .setMessage(message)
        .setNegativeButton("Отмена", (d, w) -> render())
        .setOnCancelListener(d -> render())
        .setPositiveButton(
            storage ? "Извлечены · продолжить" : "Продолжить",
            (d, w) -> {
              if (!current(computer, before)) {
                render();
                return;
              }
              if (!storage && storageSelected()) {
                confirmStop(title, computer, before, after);
                return;
              }
              pending = true;
              render();
              stopService(new Intent(this, UsbIpService.class));
              waitStopped(SystemClock.elapsedRealtime() + 15000, after);
            })
        .show();
  }

  private void waitStopped(long deadline, Runnable after) {
    if (isDestroyed() || isFinishing()) return;
    if (!UsbIpService.running && !repository.isBusy()) {
      pending = false;
      after.run();
      render();
      return;
    }
    if (SystemClock.elapsedRealtime() >= deadline) {
      pending = false;
      Toast.makeText(
              this,
              "Мост ещё завершает работу. Проверьте его состояние на главном экране.",
              Toast.LENGTH_LONG)
          .show();
      render();
      return;
    }
    main.postDelayed(() -> waitStopped(deadline, after), 100);
  }

  private CompletableFuture<ComputerRepository.Snapshot> submit(JSONObject command) {
    byte[] encoded = command.toString().getBytes(StandardCharsets.UTF_8);
    try {
      return repository.change(encoded);
    } finally {
      Arrays.fill(encoded, (byte) 0);
    }
  }

  private void run(CompletableFuture<ComputerRepository.Snapshot> task) {
    pending = true;
    render();
    task.whenComplete((snapshot, error) ->
        main.post(() -> {
          if (isDestroyed() || isFinishing()) return;
          pending = false;
          if (error != null) changedMessage();
          else setResult(RESULT_OK);
          render();
        }));
  }

  private void changedMessage() {
    Toast.makeText(
            this,
            "Данные изменились или недоступны. Обновите список и повторите действие.",
            Toast.LENGTH_LONG)
        .show();
  }

  private LinearLayout column() {
    LinearLayout result = new LinearLayout(this);
    result.setOrientation(LinearLayout.VERTICAL);
    result.setPadding(dp(24), dp(16), dp(24), dp(16));
    return result;
  }

  private TextView text(LinearLayout parent, String value, int size, int color) {
    TextView view = new TextView(this);
    view.setText(value);
    view.setTextSize(size);
    view.setTextColor(color);
    view.setPadding(0, dp(8), 0, dp(8));
    parent.addView(view, new LinearLayout.LayoutParams(-1, -2));
    return view;
  }

  private Button button(LinearLayout parent, String title, boolean primary, Runnable action) {
    Button view = new Button(this);
    view.setText(title);
    view.setTextSize(16);
    view.setAllCaps(false);
    view.setMinHeight(dp(48));
    view.setTextColor(primary ? 0xFFFFFFFF : ACCENT);
    view.setBackgroundTintList(ColorStateList.valueOf(primary ? ACCENT : PAPER));
    LinearLayout.LayoutParams lp = new LinearLayout.LayoutParams(-1, -2);
    lp.topMargin = dp(8);
    parent.addView(view, lp);
    view.setOnClickListener(v -> action.run());
    view.setEnabled(!pending);
    return view;
  }

  private int dp(int value) {
    return Math.round(value * getResources().getDisplayMetrics().density);
  }
}
