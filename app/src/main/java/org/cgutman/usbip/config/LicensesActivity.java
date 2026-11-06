package org.cgutman.usbip.config;

import android.app.*;
import android.content.*;
import android.net.Uri;
import android.os.Bundle;
import android.view.*;
import android.widget.*;
import java.io.*;
import java.nio.charset.StandardCharsets;
import org.cgutman.usbipserverforandroid.BuildConfig;
import org.json.*;

/** Offline notices for the exact dependencies packaged with this application. */
public final class LicensesActivity extends Activity {
  private JSONArray entries;

  @Override
  public void onCreate(Bundle state) {
    super.onCreate(state);
    setTitle("Лицензии и исходники");
    LinearLayout root = new LinearLayout(this);
    root.setOrientation(LinearLayout.VERTICAL);
    root.setBackgroundColor(0xFFFAFBFC);
    Button source = new Button(this);
    source.setText("Исходники этой версии");
    source.setAllCaps(false);
    source.setOnClickListener(v -> {
      Uri uri =
          Uri.parse(
              "https://github.com/varyagnord/usb-most-source/tree/v"
                  + BuildConfig.VERSION_NAME
                  + "-vc"
                  + BuildConfig.VERSION_CODE);
      try {
        startActivity(new Intent(Intent.ACTION_VIEW, uri));
      } catch (ActivityNotFoundException e) {
        new AlertDialog.Builder(this)
            .setMessage(uri.toString())
            .setPositiveButton("Понятно", null)
            .show();
      }
    });
    root.addView(source, new LinearLayout.LayoutParams(-1, -2));
    ListView list = new ListView(this);
    root.addView(list, new LinearLayout.LayoutParams(-1, 0, 1));
    setContentView(root);
    try {
      entries = new JSONArray(read("index.json", 512 * 1024));
      if (entries.length() > 512) throw new IOException("notice_count");
      String[] titles = new String[entries.length()];
      for (int i = 0; i < titles.length; i++) {
        JSONObject entry = entries.getJSONObject(i);
        titles[i] =
            entry.getString("name")
                + " "
                + entry.getString("version")
                + "\n"
                + entry.getString("license");
      }
      list.setAdapter(new ArrayAdapter<>(this, android.R.layout.simple_list_item_1, titles));
      list.setOnItemClickListener((parent, view, position, id) -> show(position));
    } catch (IOException | JSONException e) {
      failure();
    }
  }

  private String read(String name, int limit) throws IOException {
    try (InputStream input = getAssets().open("licenses/" + name);
        ByteArrayOutputStream bytes = new ByteArrayOutputStream()) {
      byte[] block = new byte[8192];
      int count;
      while ((count = input.read(block)) != -1) {
        if (bytes.size() + count > limit) throw new IOException("notice_size");
        bytes.write(block, 0, count);
      }
      return new String(bytes.toByteArray(), StandardCharsets.UTF_8);
    }
  }

  private void show(int position) {
    try {
      JSONObject entry = entries.getJSONObject(position);
      StringBuilder body = new StringBuilder(entry.getString("attribution"));
      JSONArray texts = entry.getJSONArray("texts");
      if (texts.length() > 32) throw new IOException("notice_count");
      for (int i = 0; i < texts.length(); i++) {
        String name = texts.getString(i);
        if (!name.matches("[a-f0-9]{64}\\.txt")) throw new IOException("notice_name");
        body.append("\n\n").append(read(name, 512 * 1024));
        if (body.length() > 1024 * 1024) throw new IOException("notice_size");
      }
      TextView text = new TextView(this);
      text.setText(body);
      text.setTextIsSelectable(true);
      text.setTextSize(14);
      text.setTextColor(0xFF172B3A);
      int pad = Math.round(20 * getResources().getDisplayMetrics().density);
      text.setPadding(pad, pad, pad, pad);
      ScrollView scroll = new ScrollView(this);
      scroll.addView(text);
      new AlertDialog.Builder(this)
          .setTitle(entry.getString("name"))
          .setView(scroll)
          .setPositiveButton("Закрыть", null)
          .show();
    } catch (IOException | JSONException e) {
      failure();
    }
  }

  private void failure() {
    new AlertDialog.Builder(this)
        .setMessage("Не удалось открыть текст лицензии.")
        .setPositiveButton("Понятно", null)
        .show();
  }
}
