package org.cgutman.usbip.connection;

import android.content.Context;
import android.util.AtomicFile;
import java.io.*;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.util.Arrays;
import java.util.zip.GZIPInputStream;
import org.json.*;

/** Only fixed product artifact names; no paths/URLs from the target or a QR code. */
public final class ReceiverAssets {
  private ReceiverAssets() {}

  public static synchronized void ensure(Context context) throws Exception {
    byte[] metadata;
    try (InputStream input = context.getAssets().open("receivers/manifest.json")) {
      metadata = read(input, 8192);
    }
    JSONObject manifest = new JSONObject(new String(metadata, StandardCharsets.UTF_8));
    JSONArray entries = manifest.getJSONArray("artifacts");
    if (entries.length() > 4) throw new IOException("receiver_manifest");
    File root = new File(context.getNoBackupFilesDir(), "receivers");
    if (!root.isDirectory() && !root.mkdir()) throw new IOException("receiver_directory");
    for (int i = 0; i < entries.length(); i++) {
      JSONObject entry = entries.getJSONObject(i);
      String name = entry.getString("name");
      if (!name.matches("usb-most-(linux|windows)-(x86_64|aarch64)(\\.exe)?"))
        throw new IOException("receiver_name");
      int size = entry.getInt("bytes");
      if (size < 1 || size > 20 * 1024 * 1024) throw new IOException("receiver_size");
      String hash = entry.getString("sha256");
      if (!hash.matches("[a-f0-9]{64}")) throw new IOException("receiver_hash");
      File destination = new File(root, name);
      if (destination.isFile() && destination.length() == size) {
        try (InputStream input = new FileInputStream(destination)) {
          if (digest(input).equals(hash)) continue;
        }
      }
      AtomicFile target = new AtomicFile(destination);
      FileOutputStream output = null;
      try (InputStream input =
          new GZIPInputStream(context.getAssets().open("receivers/" + name + ".gz.bin"))) {
        output = target.startWrite();
        MessageDigest digest = MessageDigest.getInstance("SHA-256");
        byte[] block = new byte[16384];
        int n, total = 0;
        while ((n = input.read(block)) != -1) {
          total += n;
          if (total > size) throw new IOException("receiver_size");
          digest.update(block, 0, n);
          output.write(block, 0, n);
        }
        Arrays.fill(block, (byte) 0);
        if (total != size || !hex(digest.digest()).equals(hash))
          throw new IOException("receiver_hash");
        target.finishWrite(output);
        output = null;
      } catch (Exception error) {
        if (output != null) target.failWrite(output);
        throw error;
      }
    }
    AtomicFile target = new AtomicFile(new File(root, "manifest.json"));
    FileOutputStream output = null;
    try {
      output = target.startWrite();
      output.write(metadata);
      target.finishWrite(output);
    } catch (Exception error) {
      if (output != null) target.failWrite(output);
      throw error;
    }
  }

  private static byte[] read(InputStream input, int maximum) throws IOException {
    ByteArrayOutputStream bytes = new ByteArrayOutputStream();
    byte[] block = new byte[4096];
    int n;
    while ((n = input.read(block)) != -1) {
      if (bytes.size() + n > maximum) throw new IOException("manifest_size");
      bytes.write(block, 0, n);
    }
    return bytes.toByteArray();
  }

  private static String digest(InputStream input) throws Exception {
    MessageDigest digest = MessageDigest.getInstance("SHA-256");
    byte[] block = new byte[16384];
    int n;
    while ((n = input.read(block)) != -1) digest.update(block, 0, n);
    return hex(digest.digest());
  }

  private static String hex(byte[] bytes) {
    StringBuilder out = new StringBuilder();
    for (byte b : bytes) out.append(String.format(java.util.Locale.ROOT, "%02x", b & 255));
    return out.toString();
  }
}
