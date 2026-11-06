package org.cgutman.usbip.connection;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.*;
import android.os.Bundle;
import android.text.InputType;
import android.util.Base64;
import android.view.*;
import android.widget.*;
import java.io.*;
import java.nio.charset.StandardCharsets;
import java.util.Arrays;
import java.util.concurrent.CompletableFuture;
import org.cgutman.usbip.jni.NativePolicy;
import org.cgutman.usbip.service.UsbIpService;
import org.cgutman.usbipserverforandroid.R;
import org.json.JSONObject;

/** One computer profile. USB ownership/selection stays in the main screen. */
public final class ConnectionSetup extends Activity {
  private LinearLayout fields;
  private EditText name, host, port, user, password, sudoPassword, keyPassphrase, code;
  private RadioGroup network;
  private RadioButton vpn, lan;
  private TextView loadStatus;
  private ComputerRepository repository;
  private String computerId, originalProfile = "{}";
  private volatile long expectedRevision = -1;
  private byte[] original;
  private boolean retainedAway;
  private boolean metadataOnlyLoad, reviewAfterSave;
  private CompletableFuture<ComputerRepository.Snapshot> saving;
  private TextView keyLabel;
  private Spinner transport;
  private byte[] importedKey;
  private KeyImport importing;
  private android.net.Uri pendingKeyUri;
  private boolean loading;
  private boolean advanced;
  private Button advancedButton, saveButton;
  private ComputerDiscovery discovery;
  private AlertDialog discoveryDialog;
  private static final int KEY_REQUEST = 24;
  private static final int INK = 0xFF172B3A, ACCENT = 0xFF225FA5;

  @Override
  public void onCreate(Bundle state) {
    super.onCreate(state);
    setTitle("Компьютер");
    repository = ComputerRepository.get(this);
    computerId = getIntent().getStringExtra("computer_id");
    expectedRevision =
        state == null
            ? getIntent().getLongExtra("computer_revision", -1)
            : state.getLong("form_revision", -1);
    if (getActionBar() != null) getActionBar().hide();
    getWindow().setStatusBarColor(0xFFFAFBFC);
    getWindow().setNavigationBarColor(0xFFFAFBFC);
    getWindow()
        .getDecorView()
        .setSystemUiVisibility(
            View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR | View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR);
    getWindow().addFlags(WindowManager.LayoutParams.FLAG_SECURE);
    ScrollView scroll = new ScrollView(this);
    scroll.setFillViewport(true);
    scroll.setBackgroundColor(0xFFFAFBFC);
    fields =
        new LinearLayout(this) {
          @Override
          protected void onMeasure(int widthSpec, int heightSpec) {
            int width = Math.min(View.MeasureSpec.getSize(widthSpec), dp(640));
            int inset = width < dp(360) ? dp(16) : dp(24);
            setPadding(inset, dp(16), inset, dp(24));
            super.onMeasure(
                View.MeasureSpec.makeMeasureSpec(width, View.MeasureSpec.EXACTLY), heightSpec);
          }
        };
    fields.setOrientation(LinearLayout.VERTICAL);
    fields.setPadding(dp(20), dp(16), dp(20), dp(24));
    FrameLayout centered = new FrameLayout(this);
    centered.addView(
        fields, new FrameLayout.LayoutParams(-1, -2, Gravity.TOP | Gravity.CENTER_HORIZONTAL));
    scroll.addView(centered);
    setContentView(scroll);
    button("Назад").setOnClickListener(v -> finish());
    TextView heading = label("Подключение компьютера", 26);
    heading.setTypeface(android.graphics.Typeface.DEFAULT, android.graphics.Typeface.BOLD);
    androidx.core.view.ViewCompat.setAccessibilityHeading(heading, true);
    name = input("Название компьютера · необязательно", false, R.id.computer_edit_name);
    if (computerId != null)
      label("Сохранение подключения сбросит правила USB этого компьютера.", 14);
    loadStatus = label("", 14);
    label("Способ подключения", 18);
    transport = new Spinner(this);
    transport.setAdapter(
        new ArrayAdapter<String>(
            this,
            android.R.layout.simple_spinner_dropdown_item,
            new String[] {"SSH", "QR или код", "IP-адрес"}) {
          private View wrap(View view) {
            TextView text = (TextView) view;
            text.setSingleLine(false);
            text.setMaxLines(4);
            text.setEllipsize(null);
            text.setTextSize(16);
            text.setMinHeight(dp(48));
            return text;
          }

          public View getView(int position, View recycled, ViewGroup parent) {
            return wrap(super.getView(position, recycled, parent));
          }

          public View getDropDownView(int position, View recycled, ViewGroup parent) {
            return wrap(super.getDropDownView(position, recycled, parent));
          }
        });
    fields.addView(transport, match());
    host = input("Адрес компьютера", false, R.id.peer_input);
    network = new RadioGroup(this);
    network.setTag("legacy-hint");
    vpn = new RadioButton(this);
    vpn.setText("Tailscale");
    vpn.setId(R.id.network_vpn);
    lan = new RadioButton(this);
    lan.setText("Локальная сеть");
    lan.setId(R.id.network_lan);
    for (RadioButton choice : new RadioButton[] {vpn, lan}) {
      choice.setTextSize(16);
      choice.setTextColor(INK);
      choice.setMinHeight(dp(48));
      choice.setButtonTintList(android.content.res.ColorStateList.valueOf(ACCENT));
      network.addView(choice, match());
    }
    vpn.setChecked(true);
    fields.addView(network, match());
    port = input("Порт SSH", false);
    port.setInputType(InputType.TYPE_CLASS_NUMBER);
    port.setText("22");
    user = input("Имя пользователя", false);
    password = input("Пароль SSH", true);
    advancedButton = button("Ключ и дополнительные параметры");
    advancedButton.setTag("ssh-advanced-toggle");
    advancedButton.setOnClickListener(v -> {
      advanced = !advanced;
      showFields();
    });
    sudoPassword = input("Пароль администратора · если отличается", true);
    keyPassphrase = input("Пароль ключа · если задан", true);
    keyLabel = label("Можно использовать пароль или приватный SSH-ключ", 13);
    Button key = button("Выбрать SSH-ключ");
    key.setOnClickListener(v -> {
      Intent intent =
          new Intent(Intent.ACTION_OPEN_DOCUMENT)
              .setType("*/*")
              .addCategory(Intent.CATEGORY_OPENABLE);
      startActivityForResult(intent, KEY_REQUEST);
    });
    key.setTag("ssh-key-button");
    Button clear = button("Использовать пароль вместо ключа");
    clear.setTag("ssh-key-button");
    clear.setOnClickListener(v -> {
      wipeKey();
      keyPassphrase.getText().clear();
      keyLabel.setText("Вход по паролю");
    });
    TextView pairHint =
        label(
            "На компьютере откройте USB Мост и создайте код подключения. Сканируйте его или"
                + " вставьте вручную. Компьютер должен быть доступен по сети.",
            14);
    pairHint.setTag("pair-scan");
    Button find = button("Найти компьютер в локальной сети");
    find.setTag("pair-scan");
    find.setOnClickListener(v -> discover());
    Button scan = button("Сканировать QR-код");
    scan.setTag("pair-scan");
    scan.setOnClickListener(v -> startActivityForResult(new Intent(this, PairScanner.class), 25));
    code = input("Код подключения с компьютера", false);
    code.setSaveEnabled(false);
    code.setSingleLine(false);
    code.setMinLines(3);
    code.setMaxLines(8);
    TextView hint =
        label(
            "При первом входе приложение проверит компьютер и подготовит приём USB. Для установки"
                + " нужны права администратора. Данные входа защищены на этом устройстве.",
            14);
    hint.setTag("ssh-hint");
    TextView legacyHint =
        label(
            "На компьютере нужен готовый USB/IP-клиент. В локальной сети USB/IP передаёт данные без"
                + " шифрования.",
            14);
    legacyHint.setTag("legacy-hint");
    saveButton = button("Сохранить компьютер");
    saveButton.setOnClickListener(v -> save());
    code.addTextChangedListener(new android.text.TextWatcher() {
      public void beforeTextChanged(CharSequence value, int start, int count, int after) {}
      public void onTextChanged(CharSequence value, int start, int before, int count) {
        showFields();
      }
      public void afterTextChanged(android.text.Editable value) {}
    });
    Object retained = getLastNonConfigurationInstance();
    if (retained instanceof Draft) {
      Draft draft = (Draft) retained;
      boolean pendingLoad = draft.loading && draft.saving == null && draft.importing == null;
      restore(draft);
      if (reviewAfterSave) reviewSavedResult();
      else if (saving != null) {
        setLoading(true);
        watchSave(saving);
      } else if (importing != null) {
        setLoading(true);
        watchImport(importing);
      } else if (pendingLoad) load(metadataOnlyLoad);
      else setLoading(false);
    } else if (state != null && state.getBoolean("form_saving", false)) {
      reviewAfterSave = true;
      reviewSavedResult();
    } else {
      int selected = getIntent().getIntExtra("initial_transport", 1);
      transport.setSelection(selected >= 0 && selected <= 2 ? selected : 1);
      if (state != null) {
        name.setText(state.getString("form_name", ""));
        host.setText(state.getString("form_host", ""));
        port.setText(state.getString("form_port", "22"));
        user.setText(state.getString("form_user", ""));
        selected = state.getInt("form_mode", 1);
        transport.setSelection(selected >= 0 && selected <= 2 ? selected : 1);
        lan.setChecked(state.getInt("form_network", 0) == 1);
        vpn.setChecked(state.getInt("form_network", 0) != 1);
        advanced = state.getBoolean("form_advanced", false);
        keyLabel.setText("Введите пароль или выберите SSH-ключ заново");
      }
      load(state != null);
    }
    transport.setOnItemSelectedListener(
        new AdapterView.OnItemSelectedListener() {
          public void onItemSelected(AdapterView<?> p, View v, int position, long id) {
            showFields();
          }

          public void onNothingSelected(AdapterView<?> p) {}
        });
    showFields();
  }

  private void reviewSavedResult() {
    setLoading(true);
    repository
        .ready()
        .whenComplete((snapshot, failure) ->
            runOnUiThread(() -> {
              if (isDestroyed() || isFinishing() || retainedAway) return;
              Toast.makeText(
                      this,
                      "Проверьте результат сохранения в списке компьютеров",
                      Toast.LENGTH_LONG)
                  .show();
              setResult(RESULT_OK, new Intent().putExtra("review_computers", true));
              finish();
            }));
  }

  private void discover() {
    if (discovery != null) discovery.close();
    ArrayAdapter<ComputerDiscovery.Computer> items =
        new ArrayAdapter<>(this, android.R.layout.simple_list_item_1);
    AlertDialog dialog =
        new AlertDialog.Builder(this)
            .setTitle("Поиск компьютеров…")
            .setAdapter(
                items,
                (d, which) -> {
                  ComputerDiscovery.Computer selected = items.getItem(which);
                  if (selected == null) return;
                  // Discovery is only a hint. The actual pin/claim comes from the QR code.
                  Toast.makeText(
                          this,
                          "Сканируйте код USB Мост на выбранном компьютере",
                          Toast.LENGTH_LONG)
                      .show();
                  startActivityForResult(new Intent(this, PairScanner.class), 25);
                })
            .setNegativeButton("Закрыть", null)
            .create();
    ComputerDiscovery browse =
        new ComputerDiscovery(
            this,
            (computers, available) -> {
              if (isFinishing() || isDestroyed() || discoveryDialog != dialog) return;
              items.clear();
              items.addAll(computers);
              dialog.setTitle(
                  !available
                      ? (computers.isEmpty()
                          ? "Компьютеры не найдены — используйте QR-код"
                          : "Найденные компьютеры")
                      : computers.isEmpty() ? "Поиск компьютеров…" : "Компьютеры рядом");
            });
    discovery = browse;
    discoveryDialog = dialog;
    dialog.setOnDismissListener(d -> {
      browse.close();
      if (discovery == browse) {
        discovery = null;
        discoveryDialog = null;
      }
    });
    dialog.show();
    browse.start();
  }

  private void load(boolean restoredProcess) {
    metadataOnlyLoad = restoredProcess;
    setLoading(true);
    repository
        .ready()
        .whenComplete((snapshot, failure) -> {
          if (failure != null) {
            loadFailure();
            return;
          }
          if (expectedRevision < 0) expectedRevision = snapshot.revision;
          if (computerId == null) {
            runOnUiThread(() -> {
              if (!isDestroyed() && !retainedAway) setLoading(false);
            });
            return;
          }
          ComputerRepository.Computer selected = null;
          for (ComputerRepository.Computer item : snapshot.computers)
            if (item.id.equals(computerId)) selected = item;
          if (selected == null || snapshot.revision != expectedRevision) {
            loadFailure();
            return;
          }
          final ComputerRepository.Computer target = selected;
          if (restoredProcess) {
            runOnUiThread(() -> {
              if (isDestroyed() || retainedAway) return;
              originalProfile = target.profile.isEmpty() ? "{}" : target.profile;
              setLoading(false);
              showFields();
            });
            return;
          }
          repository
              .edit(computerId, expectedRevision)
              .whenComplete((data, error) ->
                  runOnUiThread(() -> {
                    if (error != null) {
                      loadFailure();
                      return;
                    }
                    if (isDestroyed() || retainedAway) {
                      Arrays.fill(data, (byte) 0);
                      return;
                    }
                    try {
                      JSONObject entry = new JSONObject(new String(data, StandardCharsets.UTF_8));
                      name.setText(entry.getString("name"));
                      JSONObject connection = entry.getJSONObject("connection");
                      if (connection.getString("kind").equals("legacy")) {
                        transport.setSelection(2);
                        host.setText(connection.getString("host"));
                        lan.setChecked(connection.getInt("mode") == 1);
                        vpn.setChecked(connection.getInt("mode") != 1);
                      } else {
                        JSONObject profile = connection.getJSONObject("profile");
                        originalProfile = profile.toString();
                        boolean ssh = profile.getString("transport").equals("ssh");
                        transport.setSelection(ssh ? 0 : 1);
                        JSONObject endpoint = profile.getJSONObject("endpoint");
                        host.setText(endpoint.getString("host"));
                        port.setText(ssh ? Integer.toString(endpoint.getInt("port")) : "22");
                        user.setText(profile.optString("user", ""));
                        JSONObject secret = connection.getJSONObject("credentials");
                        password.setText(optional(secret, "password"));
                        sudoPassword.setText(optional(secret, "sudo_password"));
                        keyPassphrase.setText(optional(secret, "passphrase"));
                        code.setText(optional(secret, "pair_code"));
                        if (!secret.isNull("private_key")) {
                          importedKey =
                              secret.getString("private_key").getBytes(StandardCharsets.UTF_8);
                          keyLabel.setText("Сохранённый SSH-ключ");
                        }
                        if (!ssh && !secret.isNull("token"))
                          code.setHint("Компьютер уже сопряжён. Для замены вставьте новый код");
                      }
                      original = data;
                      setLoading(false);
                      showFields();
                    } catch (Exception invalid) {
                      loadFailure();
                    } finally {
                      if (original != data) Arrays.fill(data, (byte) 0);
                    }
                  }));
        });
  }

  private static String optional(JSONObject value, String key) {
    return value.isNull(key) ? "" : value.optString(key, "");
  }

  private void loadFailure() {
    runOnUiThread(() -> {
      if (isDestroyed() || retainedAway) return;
      loadStatus.setText(
          "Не удалось открыть данные. Вернитесь в список компьютеров и повторите попытку.");
      loadStatus.setVisibility(View.VISIBLE);
      saveButton.setEnabled(false);
    });
  }

  private void setLoading(boolean value) {
    loading = value;
    loadStatus.setText(
        importing != null ? "Чтение SSH-ключа…" : saving == null ? "Загрузка…" : "Сохранение…");
    loadStatus.setVisibility(value ? View.VISIBLE : View.GONE);
    for (int i = 1; i < fields.getChildCount(); i++) enable(fields.getChildAt(i), !value);
    if (!value
        && pendingKeyUri != null
        && importing == null
        && saving == null
        && !isDestroyed()
        && !isFinishing()
        && !retainedAway) startImport();
  }

  private void enable(View view, boolean value) {
    if (view instanceof EditText || view instanceof Spinner || view instanceof Button)
      view.setEnabled(value);
    if (view instanceof ViewGroup)
      for (int i = 0; i < ((ViewGroup) view).getChildCount(); i++)
        enable(((ViewGroup) view).getChildAt(i), value);
  }

  private boolean editingPairedDirect() {
    if (computerId == null || original == null || code.length() != 0) return false;
    try {
      JSONObject connection = new JSONObject(new String(original, StandardCharsets.UTF_8))
          .getJSONObject("connection");
      return connection.getString("kind").equals("secure")
          && connection.getJSONObject("profile").getString("transport").equals("direct")
          && !connection.getJSONObject("credentials").isNull("token");
    } catch (Exception invalid) {
      return false;
    }
  }

  private void showFields() {
    int mode = transport.getSelectedItemPosition();
    boolean ssh = mode == 0;
    for (View v : new View[] {host, port, user, password, sudoPassword, keyPassphrase, keyLabel})
      v.setVisibility(ssh ? View.VISIBLE : View.GONE);
    for (int i = 0; i < fields.getChildCount(); i++) {
      View v = fields.getChildAt(i);
      if ("legacy-hint".equals(v.getTag())) v.setVisibility(mode == 2 ? View.VISIBLE : View.GONE);
      if ("pair-scan".equals(v.getTag())) v.setVisibility(mode == 1 ? View.VISIBLE : View.GONE);
      if ("ssh-key-button".equals(v.getTag()) || "ssh-hint".equals(v.getTag()))
        v.setVisibility(ssh ? View.VISIBLE : View.GONE);
    }
    advancedButton.setVisibility(ssh ? View.VISIBLE : View.GONE);
    advancedButton.setText(
        advanced ? "Скрыть дополнительные параметры" : "Ключ и дополнительные параметры");
    for (View v : new View[] {sudoPassword, keyPassphrase, keyLabel})
      v.setVisibility(ssh && advanced ? View.VISIBLE : View.GONE);
    for (int i = 0; i < fields.getChildCount(); i++) {
      View v = fields.getChildAt(i);
      if ("ssh-key-button".equals(v.getTag()))
        v.setVisibility(ssh && advanced ? View.VISIBLE : View.GONE);
    }
    host.setVisibility(mode == 1 && !editingPairedDirect() ? View.GONE : View.VISIBLE);
    saveButton.setText("Сохранить компьютер");
    code.setVisibility(mode == 1 ? View.VISIBLE : View.GONE);
    for (int i = 0; i < fields.getChildCount(); i++) {
      View v = fields.getChildAt(i);
      if (v.getTag() instanceof EditText) v.setVisibility(((EditText) v.getTag()).getVisibility());
    }
  }

  private void save() {
    if (loading || saving != null) return;
    if (UsbIpService.running || repository.isBusy()) {
      Toast.makeText(this, "Сначала остановите мост", Toast.LENGTH_LONG).show();
      return;
    }
    try {
      int mode = transport.getSelectedItemPosition();
      JSONObject connection;
      JSONObject old = new JSONObject(originalProfile);
      JSONObject oldSecret =
          original == null
              ? new JSONObject()
              : new JSONObject(new String(original, StandardCharsets.UTF_8))
                  .getJSONObject("connection")
                  .optJSONObject("credentials");
      if (oldSecret == null) oldSecret = new JSONObject();
      String title = name.getText().toString().trim();
      if (mode == 2) {
        String address = host.getText().toString().trim();
        int networkMode = lan.isChecked() ? 1 : 0;
        if (!NativePolicy.validHost(address, networkMode)) {
          host.setError(
              networkMode == 0
                  ? "Укажите IPv4 компьютера в Tailscale"
                  : "Укажите локальный IPv4: 192.168…, 10… или 172.16–31…");
          return;
        }
        connection =
            new JSONObject().put("kind", "legacy").put("host", address).put("mode", networkMode);
        if (title.isEmpty()) title = address;
      } else {
        JSONObject profile = new JSONObject(), secret = new JSONObject();
        if (mode == 0) {
          String address = host.getText().toString().trim(),
              login = user.getText().toString().trim();
          int number = Integer.parseInt(port.getText().toString().trim());
          boolean same =
              old.optString("transport").equals("ssh")
                  && old.optJSONObject("endpoint") != null
                  && old.getJSONObject("endpoint").optString("host").equals(address)
                  && old.getJSONObject("endpoint").optInt("port", 0) == number;
          profile
              .put("transport", "ssh")
              .put("endpoint", new JSONObject().put("host", address).put("port", number))
              .put("user", login)
              // A saved computer keeps its identity when its network address changes.
              .put("host_key", old.optString("transport").equals("ssh")
                  ? old.opt("host_key") : JSONObject.NULL)
              .put("reverse_port", same ? old.optInt("reverse_port", 0) : 0)
              .put("prepare", !same || old.optBoolean("prepare", true));
          if (importedKey != null) {
            secret.put("private_key", new String(importedKey, StandardCharsets.UTF_8));
            if (keyPassphrase.length() > 0)
              secret.put("passphrase", keyPassphrase.getText().toString());
          } else secret.put("password", password.getText().toString());
          if (sudoPassword.length() > 0)
            secret.put("sudo_password", sudoPassword.getText().toString());
          String client = optional(oldSecret, "client_id");
          secret.put(
              "client_id",
              client.isEmpty() ? java.util.UUID.randomUUID().toString().replace("-", "") : client);
        } else {
          String value = code.getText().toString().trim();
          if (value.isEmpty()) {
            if (!old.optString("transport").equals("direct")
                || original == null
                || oldSecret.isNull("token")) throw new IOException("code");
            String address = host.getText().toString().trim();
            if (address.isEmpty()) {
              host.setError("Укажите адрес компьютера");
              return;
            }
            host.setError(null);
            profile = old;
            profile.getJSONObject("endpoint").put("host", address);
            secret = oldSecret;
          } else {
            if (!value.startsWith("usbmost:1:") || value.length() > 2048)
              throw new IOException("code");
            byte[] decoded =
                Base64.decode(
                    value.substring(10), Base64.URL_SAFE | Base64.NO_WRAP | Base64.NO_PADDING);
            try {
              JSONObject pair = new JSONObject(new String(decoded, StandardCharsets.UTF_8));
              profile
                  .put("transport", "direct")
                  .put("endpoint", pair.getJSONObject("endpoint"))
                  .put("server_pin", pair.getString("server_pin"));
              byte[] nonce = new byte[16];
              new java.security.SecureRandom().nextBytes(nonce);
              secret
                  .put("pair_code", value)
                  .put(
                      "pair_nonce",
                      Base64.encodeToString(
                          nonce, Base64.URL_SAFE | Base64.NO_WRAP | Base64.NO_PADDING));
              Arrays.fill(nonce, (byte) 0);
            } finally {
              Arrays.fill(decoded, (byte) 0);
            }
          }
        }
        if (title.isEmpty()) {
          title = profile.getJSONObject("endpoint").getString("host");
          if (title.codePointCount(0, title.length()) > 64)
            title = title.substring(0, title.offsetByCodePoints(0, 64));
        }
        connection =
            new JSONObject()
                .put("kind", "secure")
                .put("profile", profile)
                .put("credentials", secret);
      }
      JSONObject command =
          new JSONObject()
              .put("op", computerId == null ? "add" : "replace")
              .put("revision", expectedRevision)
              .put("name", title)
              .put("connection", connection);
      if (computerId == null) command.put("activate", true);
      else command.put("id", computerId);
      // After process death, only the non-secret per-computer provisioning ID may
      // be recovered on explicit Save; never restore a password/key/token to a draft.
      if (mode == 0 && computerId != null && original == null) {
        saving =
            repository
                .edit(computerId, expectedRevision)
                .thenCompose(previous -> {
                  try {
                    JSONObject prior =
                        new JSONObject(new String(previous, StandardCharsets.UTF_8))
                            .getJSONObject("connection");
                    JSONObject credentials = prior.optJSONObject("credentials");
                    String client = credentials == null ? "" : optional(credentials, "client_id");
                    if (!client.isEmpty())
                      command
                          .getJSONObject("connection")
                          .getJSONObject("credentials")
                          .put("client_id", client);
                    return submit(command);
                  } catch (Exception invalid) {
                    CompletableFuture<ComputerRepository.Snapshot> failed =
                        new CompletableFuture<>();
                    failed.completeExceptionally(invalid);
                    return failed;
                  } finally {
                    Arrays.fill(previous, (byte) 0);
                  }
                });
      } else saving = submit(command);
      setLoading(true);
      watchSave(saving);
    } catch (Exception invalid) {
      saveFailure();
    }
  }

  private CompletableFuture<ComputerRepository.Snapshot> submit(JSONObject command) {
    byte[] encoded = command.toString().getBytes(StandardCharsets.UTF_8);
    try {
      return repository.change(encoded);
    } finally {
      Arrays.fill(encoded, (byte) 0);
    }
  }

  private void watchSave(CompletableFuture<ComputerRepository.Snapshot> operation) {
    operation.whenComplete((snapshot, failure) ->
        runOnUiThread(() -> {
          if (isDestroyed() || isFinishing() || retainedAway) return;
          saving = null;
          if (failure != null) {
            setLoading(false);
            saveFailure();
            return;
          }
          setResult(RESULT_OK);
          finish();
        }));
  }

  private void saveFailure() {
    new AlertDialog.Builder(this)
        .setMessage(
            "Не удалось сохранить. Проверьте поля и данные входа. Если список компьютеров"
                + " изменился, откройте настройку заново.")
        .setPositiveButton("Понятно", null)
        .show();
  }

  @Override
  protected void onActivityResult(int request, int result, Intent data) {
    super.onActivityResult(request, result, data);
    if (request == 25) {
      String scanned = data == null ? null : data.getStringExtra("pair_code");
      if (result == RESULT_OK && scanned != null && scanned.length() <= 2048) {
        code.setText(scanned);
        transport.setSelection(1);
        showFields();
      }
      return;
    }
    if (request != KEY_REQUEST || result != RESULT_OK || data == null || data.getData() == null)
      return;
    if (saving != null || importing != null) return;
    pendingKeyUri = data.getData();
    if (!loading) startImport();
  }

  private void startImport() {
    android.net.Uri uri = pendingKeyUri;
    pendingKeyUri = null;
    importing = new KeyImport(getApplicationContext().getContentResolver(), uri);
    setLoading(true);
    watchImport(importing);
  }

  private void watchImport(KeyImport operation) {
    operation.done.whenComplete((ignored, failure) ->
        runOnUiThread(() -> {
          if (isDestroyed() || isFinishing() || retainedAway || importing != operation) return;
          importing = null;
          byte[] key = operation.take();
          if (key != null) {
            wipeKey();
            importedKey = key;
            keyLabel.setText("SSH-ключ выбран");
          } else
            Toast.makeText(
                    this,
                    "Ключ не прочитан: файл недоступен, пуст или больше 64 КБ. Попробуйте файл в"
                        + " памяти устройства.",
                    Toast.LENGTH_LONG)
                .show();
          setLoading(false);
        }));
  }

  /** Secrets survive configuration changes in memory, never in a Bundle/file. */
  private static final class Draft {
    String name, host, port, user, id, profile;
    long revision;
    int networkMode;
    boolean loading, metadataOnlyLoad, reviewAfterSave;
    CompletableFuture<ComputerRepository.Snapshot> saving;
    KeyImport importing;
    android.net.Uri pendingKeyUri;
    int mode;
    boolean advanced;
    byte[] password, sudo, passphrase, code, key, original;

    void clear() {
      for (byte[] value : new byte[][] {password, sudo, passphrase, code, key, original})
        if (value != null) Arrays.fill(value, (byte) 0);
    }
  }

  @Override
  public Object onRetainNonConfigurationInstance() {
    retainedAway = true;
    Draft draft = new Draft();
    draft.name = name.getText().toString();
    draft.id = computerId;
    draft.profile = originalProfile;
    draft.revision = expectedRevision;
    draft.networkMode = lan.isChecked() ? 1 : 0;
    draft.loading = loading;
    draft.metadataOnlyLoad = metadataOnlyLoad;
    draft.reviewAfterSave = reviewAfterSave;
    draft.saving = saving;
    draft.importing = importing;
    importing = null;
    draft.pendingKeyUri = pendingKeyUri;
    pendingKeyUri = null;
    draft.original = original;
    original = null;
    draft.host = host.getText().toString();
    draft.port = port.getText().toString();
    draft.user = user.getText().toString();
    draft.mode = transport.getSelectedItemPosition();
    draft.advanced = advanced;
    draft.password = password.getText().toString().getBytes(StandardCharsets.UTF_8);
    draft.sudo = sudoPassword.getText().toString().getBytes(StandardCharsets.UTF_8);
    draft.passphrase = keyPassphrase.getText().toString().getBytes(StandardCharsets.UTF_8);
    draft.code = code.getText().toString().getBytes(StandardCharsets.UTF_8);
    draft.key = importedKey;
    importedKey = null; // transfer ownership; old Activity onDestroy must not wipe it
    return draft;
  }

  private void restore(Draft draft) {
    try {
      name.setText(draft.name);
      computerId = draft.id;
      originalProfile = draft.profile;
      expectedRevision = draft.revision;
      loading = draft.loading;
      metadataOnlyLoad = draft.metadataOnlyLoad;
      reviewAfterSave = draft.reviewAfterSave;
      saving = draft.saving;
      importing = draft.importing;
      draft.importing = null;
      pendingKeyUri = draft.pendingKeyUri;
      draft.pendingKeyUri = null;
      original = draft.original;
      draft.original = null;
      lan.setChecked(draft.networkMode == 1);
      vpn.setChecked(draft.networkMode != 1);
      host.setText(draft.host);
      port.setText(draft.port);
      user.setText(draft.user);
      transport.setSelection(draft.mode);
      advanced = draft.advanced;
      password.setText(new String(draft.password, StandardCharsets.UTF_8));
      sudoPassword.setText(new String(draft.sudo, StandardCharsets.UTF_8));
      keyPassphrase.setText(new String(draft.passphrase, StandardCharsets.UTF_8));
      code.setText(new String(draft.code, StandardCharsets.UTF_8));
      importedKey = draft.key;
      draft.key = null;
      if (importedKey != null) keyLabel.setText("SSH-ключ выбран");
    } finally {
      draft.clear();
    }
  }

  @Override
  protected void onSaveInstanceState(Bundle state) {
    state.putString("form_name", name.getText().toString());
    state.putLong("form_revision", expectedRevision);
    state.putInt("form_network", lan.isChecked() ? 1 : 0);
    state.putBoolean("form_saving", saving != null || reviewAfterSave);
    state.putInt("form_mode", transport.getSelectedItemPosition());
    state.putString("form_host", host.getText().toString());
    state.putString("form_port", port.getText().toString());
    state.putString("form_user", user.getText().toString());
    state.putBoolean("form_advanced", advanced);
    super.onSaveInstanceState(state);
  }

  private void wipeKey() {
    if (importedKey != null) {
      Arrays.fill(importedKey, (byte) 0);
      importedKey = null;
    }
  }

  @Override
  protected void onPause() {
    if (discoveryDialog != null) discoveryDialog.dismiss();
    if (discovery != null) {
      discovery.close();
      discovery = null;
    }
    super.onPause();
  }

  @Override
  protected void onDestroy() {
    pendingKeyUri = null;
    if (importing != null) {
      importing.abandon();
      importing = null;
    }
    if (original != null) {
      Arrays.fill(original, (byte) 0);
      original = null;
    }
    wipeKey();
    password.getText().clear();
    sudoPassword.getText().clear();
    keyPassphrase.getText().clear();
    code.getText().clear();
    super.onDestroy();
  }

  private EditText input(String hint, boolean hidden) {
    return input(hint, hidden, View.generateViewId());
  }

  private EditText input(String hint, boolean hidden, int id) {
    EditText view = new EditText(this);
    view.setId(id);
    view.setTag(hint);
    TextView title = label(hint, 14);
    title.setLabelFor(view.getId());
    title.setTag(view);
    view.setTextColor(INK);
    view.setTextSize(16);
    view.setSingleLine(true);
    view.setImeOptions(android.view.inputmethod.EditorInfo.IME_FLAG_NO_EXTRACT_UI);
    view.setMinHeight(dp(56));
    view.setPadding(dp(12), dp(12), dp(12), dp(12));
    view.setInputType(
        InputType.TYPE_CLASS_TEXT
            | (hidden
                ? InputType.TYPE_TEXT_VARIATION_PASSWORD
                : InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD));
    view.setImportantForAutofill(View.IMPORTANT_FOR_AUTOFILL_NO);
    view.setSaveEnabled(!hidden);
    view.setFilters(
        new android.text.InputFilter[] {
          new android.text.InputFilter.LengthFilter(hidden ? 4096 : 2048)
        });
    fields.addView(view, match());
    return view;
  }

  private TextView label(String text, int size) {
    TextView view = new TextView(this);
    view.setText(text);
    view.setTextSize(size);
    view.setTextColor(INK);
    view.setPadding(0, dp(12), 0, dp(8));
    fields.addView(view, match());
    return view;
  }

  private Button button(String text) {
    Button view =
        org.cgutman.usbip.config.BridgeUi.button(this, text, text.equals("Сохранить компьютер"));
    fields.addView(view, match());
    return view;
  }

  private LinearLayout.LayoutParams match() {
    return new LinearLayout.LayoutParams(
        ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT);
  }

  private int dp(int value) {
    return Math.round(value * getResources().getDisplayMetrics().density);
  }
}
