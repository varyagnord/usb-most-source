package org.cgutman.usbip.service;

import android.content.Context;
import java.io.*;
import java.nio.charset.StandardCharsets;
import java.text.SimpleDateFormat;
import java.util.*;

/** Bounded metadata-only log; never accepts USB payloads. */
public final class DiagnosticLog {
  private static final Object LOCK = new Object();
  private static final int MAX_BYTES = 256 * 1024;
  private final File file;

  public DiagnosticLog(Context context) {
    file = new File(context.getFilesDir(), "usbip.log");
  }

  public void event(String message) {
    String stamp =
        new SimpleDateFormat("yyyy-MM-dd'T'HH:mm:ss.SSSXXX", Locale.ROOT).format(new Date());
    String bounded = message.substring(0, Math.min(512, message.length())).replace('\n', ' ');
    byte[] line = (stamp + " " + bounded + "\n").getBytes(StandardCharsets.UTF_8);
    synchronized (LOCK) {
      try {
        if (file.length() + line.length > MAX_BYTES) {
          File old = new File(file.getParentFile(), "usbip.previous.log");
          if (old.exists() && !old.delete()) return;
          if (!file.renameTo(old)) return; // Never grow an unrotatable file indefinitely.
        }
        try (FileOutputStream out = new FileOutputStream(file, true)) {
          out.write(line);
        }
      } catch (IOException ignored) {
        /* Logging must not fail a USB transaction. */
      }
    }
  }

  public static void export(Context context, OutputStream out) throws IOException {
    ByteArrayOutputStream snapshot = new ByteArrayOutputStream();
    synchronized (LOCK) {
      for (String name : new String[] {"usbip.previous.log", "usbip.log"}) {
        File f = new File(context.getFilesDir(), name);
        if (!f.exists()) continue;
        try (InputStream in = new FileInputStream(f)) {
          byte[] b = new byte[8192];
          int n;
          while ((n = in.read(b)) != -1) snapshot.write(b, 0, n);
        }
      }
    }
    // A slow document provider cannot hold the logger lock or stall USB requests.
    snapshot.writeTo(out);
  }

  public static String snapshotTail(Context context, int limit) {
    if (limit < 0 || limit > 32768) throw new IllegalArgumentException("log limit");
    synchronized (LOCK) {
      try {
        File current = new File(context.getFilesDir(), "usbip.log");
        File previous = new File(context.getFilesDir(), "usbip.previous.log");
        int currentBytes = (int) Math.min(limit, current.length());
        int previousBytes = (int) Math.min(limit - currentBytes, previous.length());
        ByteArrayOutputStream out = new ByteArrayOutputStream(currentBytes + previousBytes);
        for (Object[] item : new Object[][] {{previous, previousBytes}, {current, currentBytes}}) {
          File file = (File) item[0];
          int count = (Integer) item[1];
          if (count == 0 || !file.exists()) continue;
          try (RandomAccessFile input = new RandomAccessFile(file, "r")) {
            input.seek(Math.max(0, input.length() - count));
            byte[] bytes = new byte[count];
            input.readFully(bytes);
            out.write(bytes);
          }
        }
        return new String(out.toByteArray(), StandardCharsets.UTF_8);
      } catch (IOException error) { return "journal_unavailable"; }
    }
  }
}
