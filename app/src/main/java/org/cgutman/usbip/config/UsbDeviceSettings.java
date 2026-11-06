package org.cgutman.usbip.config;

import android.app.*;
import android.text.InputFilter;
import android.view.*;
import android.widget.*;
import java.nio.charset.StandardCharsets;
import java.util.Arrays;
import org.cgutman.usbip.connection.ComputerRepository;
import org.cgutman.usbip.connection.UsbRules;
import org.cgutman.usbip.service.UsbInventory;
import org.json.*;

/** Optional per-device settings; the home list keeps selection and live status. */
final class UsbDeviceSettings {
  private UsbDeviceSettings() {}

  private static boolean sameIdentity(JSONObject left, JSONObject right) throws JSONException {
    return left.getInt("vendor") == right.getInt("vendor")
        && left.getInt("product") == right.getInt("product")
        && left.getString("serial_hash").equals(right.getString("serial_hash"))
        && left.getString("layout_hash").equals(right.getString("layout_hash"));
  }

  static AlertDialog show(Activity activity, UsbInventory inventory, UsbInventory.Item item) {
    ComputerRepository repository = ComputerRepository.get(activity);
    ComputerRepository.Snapshot snapshot = repository.snapshot();
    ComputerRepository.Computer computer = snapshot.active();
    String scope = computer == null ? "" : computer.id + ":" + computer.usbEpoch;
    boolean persistent =
        computer != null
            && item.identity != null
            && item.capability == 0
            && !item.ruleReason.equals("ambiguous")
            && !item.ruleReason.equals("unavailable")
            && inventory.permissionGranted(item.device.getDeviceId(), item.generation);
    JSONObject previous = null;
    try {
      if (computer != null && item.identity != null) {
        JSONArray rules = new JSONArray(computer.usbRules);
        for (int i = 0; i < rules.length(); i++)
          if (sameIdentity(
              rules.getJSONObject(i).getJSONObject("identity"), new JSONObject(item.identity)))
            previous = rules.getJSONObject(i);
      }
    } catch (JSONException invalid) {
      persistent = false;
    }
    final boolean saveRule = persistent;
    final JSONObject oldRule = previous;
    Runnable clearAttachmentName =
        () -> inventory.nameAttachment(item.device.getDeviceId(), item.generation, scope, "");
    int pad = Math.round(20 * activity.getResources().getDisplayMetrics().density);
    LinearLayout content = new LinearLayout(activity);
    content.setOrientation(LinearLayout.VERTICAL);
    content.setPadding(pad, pad / 2, pad, pad / 2);
    TextView heading = new TextView(activity);
    heading.setText("Устройство");
    heading.setTextSize(22);
    heading.setTextColor(0xFF172B3A);
    heading.setTypeface(android.graphics.Typeface.DEFAULT, android.graphics.Typeface.BOLD);
    heading.setPadding(0, pad / 2, 0, pad);
    androidx.core.view.ViewCompat.setAccessibilityHeading(heading, true);
    content.addView(heading);
    TextView label = new TextView(activity);
    label.setText("Имя устройства");
    label.setTextColor(0xFF536675);
    content.addView(label);
    EditText name = new EditText(activity);
    name.setId(View.generateViewId());
    label.setLabelFor(name.getId());
    name.setSingleLine(true);
    name.setText(oldRule == null ? item.name : oldRule.optString("name", ""));
    name.setHint("Название из USB");
    name.setFilters(new InputFilter[] {new InputFilter.LengthFilter(64)});
    content.addView(name, new LinearLayout.LayoutParams(-1, -2));
    CheckBox automatic = new CheckBox(activity);
    automatic.setText("Выбирать при подключении");
    automatic.setTextSize(16);
    automatic.setTextColor(0xFF172B3A);
    automatic.setMinHeight(Math.round(48 * activity.getResources().getDisplayMetrics().density));
    automatic.setChecked(
        saveRule && !item.storage && oldRule != null && oldRule.optBoolean("automatic"));
    automatic.setVisibility(saveRule && !item.storage ? View.VISIBLE : View.GONE);
    content.addView(automatic, new LinearLayout.LayoutParams(-1, -2));
    TextView hint = new TextView(activity);
    hint.setTextSize(14);
    hint.setTextColor(0xFF536675);
    hint.setText(
        !saveRule
            ? "Имя действует до отключения устройства или смены компьютера. Чтобы сохранить"
                + " правило, нужен выбранный компьютер, доступ USB и однозначный серийный номер."
            : item.storage
                ? "Имя сохраняется для «"
                    + computer.name
                    + "». Передача накопителя каждый раз требует подтверждения извлечения."
                : "Для «"
                    + computer.name
                    + "». Мост запускается отдельно. Ручной выбор действует до переподключения USB"
                    + " или смены компьютера.");
    content.addView(hint, new LinearLayout.LayoutParams(-1, -2));
    Button positive = action(activity, content, "Сохранить", true);
    Button cancel = action(activity, content, "Отмена", false);
    Button neutral = oldRule == null ? null : action(activity, content, "Забыть правило", false);
    Button[] controls = {positive, cancel, neutral};
    ScrollView scroll = new ScrollView(activity);
    scroll.addView(content);
    AlertDialog dialog = new AlertDialog.Builder(activity).setView(scroll).create();
    cancel.setOnClickListener(view -> dialog.dismiss());
    dialog.setOnShowListener(ignored -> {
      positive.setOnClickListener(view -> {
        try {
          String title = name.getText().toString().trim();
          if (!UsbRules.query(new JSONObject().put("op", "usb_name").put("name", title))
              .equals("true")) {
            name.setError("До 64 символов, без служебных символов");
            return;
          }
          UsbInventory.Item current = inventory.item(item.device.getDeviceId());
          if (current == null
              || current.generation != item.generation
              || !repository.snapshot().connectionKey().equals(snapshot.connectionKey())
              || (saveRule
                  && (!item.identity.equals(current.identity)
                      || current.ruleReason.equals("ambiguous")
                      || current.ruleReason.equals("unavailable")
                      || !inventory.permissionGranted(
                          item.device.getDeviceId(), item.generation)))) {
            hint.setText("Устройство или компьютер изменились. Откройте настройки заново.");
            return;
          }
          if (!saveRule) {
            if (inventory.nameAttachment(item.device.getDeviceId(), item.generation, scope, title))
              dialog.dismiss();
            else hint.setText("Компьютер изменился. Откройте настройки заново.");
          } else {
            JSONObject command =
                new JSONObject()
                    .put("op", "set_usb_rule")
                    .put("revision", snapshot.revision)
                    .put("id", computer.id)
                    .put(
                        "rule",
                        new JSONObject()
                            .put("identity", new JSONObject(item.identity))
                            .put("name", title)
                            .put("automatic", !item.storage && automatic.isChecked())
                            .put("storage", item.storage));
            save(activity, repository, dialog, hint, command, clearAttachmentName, controls);
          }
        } catch (Exception invalid) {
          hint.setText("Не удалось сохранить. Откройте настройки заново.");
        }
      });
      if (oldRule != null)
        neutral.setOnClickListener(view -> {
          try {
            save(
                activity,
                repository,
                dialog,
                hint,
                new JSONObject()
                    .put("op", "forget_usb_rule")
                    .put("revision", snapshot.revision)
                    .put("id", computer.id)
                    .put("identity", oldRule.getJSONObject("identity")),
                clearAttachmentName,
                controls);
          } catch (Exception invalid) {
            hint.setText("Не удалось забыть правило. Откройте настройки заново.");
          }
        });
    });
    dialog.show();
    // Apply after decor inflation (PhoneWindow otherwise restores the theme's
    // animation). An exiting editor must not retain focus across recreation.
    dialog.getWindow().setWindowAnimations(0);
    dialog.getWindow().setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE);
    return dialog;
  }

  private static Button action(
      Activity activity, LinearLayout parent, String text, boolean primary) {
    Button button = new Button(activity);
    button.setText(text);
    button.setAllCaps(false);
    button.setTextSize(16);
    button.setMinHeight(Math.round(48 * activity.getResources().getDisplayMetrics().density));
    button.setTextColor(primary ? 0xFFFFFFFF : 0xFF225FA5);
    button.setBackgroundTintList(
        android.content.res.ColorStateList.valueOf(primary ? 0xFF225FA5 : 0x00000000));
    parent.addView(button, new LinearLayout.LayoutParams(-1, -2));
    return button;
  }

  private static void save(
      Activity activity,
      ComputerRepository repository,
      AlertDialog dialog,
      TextView hint,
      JSONObject command,
      Runnable applied,
      Button[] controls) {
    for (Button button : controls)
      if (button != null) {
        button.setEnabled(false);
        button.setAlpha(0.5f);
      }
    hint.setText("Сохранение…");
    dialog.setCancelable(false);
    byte[] bytes = command.toString().getBytes(StandardCharsets.UTF_8);
    try {
      repository
          .change(bytes)
          .whenComplete((saved, failure) ->
              activity.runOnUiThread(() -> {
                if (failure == null) applied.run();
                if (activity.isDestroyed() || activity.isFinishing() || !dialog.isShowing()) return;
                if (failure == null) dialog.dismiss();
                else {
                  hint.setText(
                      "Данные изменились или недоступны. Закройте окно и повторите попытку.");
                  controls[1].setEnabled(true);
                  controls[1].setAlpha(1f);
                  dialog.setCancelable(true);
                }
              }));
    } finally {
      Arrays.fill(bytes, (byte) 0);
    }
  }
}
