package org.cgutman.usbip.connection;

import android.content.ContentResolver;
import android.content.res.AssetFileDescriptor;
import android.net.Uri;
import android.os.CancellationSignal;
import java.io.InputStream;
import java.util.Arrays;
import java.util.concurrent.*;

/** Bounded Android file-provider adapter. The retained task never owns an Activity. */
final class KeyImport {
  private static final int LIMIT = 65536;

  private static ThreadFactory threads(String name) {
    return action -> {
      Thread thread = new Thread(action, name);
      thread.setDaemon(true);
      return thread;
    };
  }

  // A provider may ignore cancellation or block while opening/closing its file.
  // Never grow a queue or create unlimited workers for repeated retries.
  private static final ExecutorService READERS =
      new ThreadPoolExecutor(
          0, 2, 30, TimeUnit.SECONDS, new SynchronousQueue<>(), threads("usb-most-key-read"));
  private static final ExecutorService CLOSERS =
      new ThreadPoolExecutor(
          0, 2, 30, TimeUnit.SECONDS, new SynchronousQueue<>(), threads("usb-most-key-close"));
  private static final ScheduledThreadPoolExecutor TIMER = timer();

  private static ScheduledThreadPoolExecutor timer() {
    ScheduledThreadPoolExecutor timer =
        new ScheduledThreadPoolExecutor(1, threads("usb-most-key-timeout"));
    timer.setRemoveOnCancelPolicy(true);
    return timer;
  }

  final CompletableFuture<Void> done = new CompletableFuture<>();
  private final CancellationSignal cancellation = new CancellationSignal();
  private volatile ScheduledFuture<?> timeout;
  private volatile AssetFileDescriptor descriptor;
  private boolean finished, abandoned;
  private byte[] value;

  KeyImport(ContentResolver resolver, Uri uri) {
    timeout =
        TIMER.schedule(
            () -> {
              finish(null);
              cancelProvider();
            },
            10,
            TimeUnit.SECONDS);
    if (obsolete()) timeout.cancel(false);
    try {
      READERS.execute(() -> read(resolver, uri));
    } catch (RejectedExecutionException busy) {
      finish(null);
    }
  }

  private synchronized boolean obsolete() {
    return finished || abandoned;
  }

  private void read(ContentResolver resolver, Uri uri) {
    byte[] buffer = new byte[LIMIT + 1];
    try {
      if (obsolete()) return;
      try (AssetFileDescriptor opened = resolver.openAssetFileDescriptor(uri, "r", cancellation)) {
        if (opened == null) throw new java.io.IOException("key_unavailable");
        descriptor = opened;
        if (obsolete()) return;
        try (InputStream stream = opened.createInputStream()) {
          int used = 0;
          while (used <= LIMIT && !obsolete()) {
            int count = stream.read(buffer, used, buffer.length - used);
            if (count < 0) {
              if (used == 0) throw new java.io.IOException("empty_key");
              finish(Arrays.copyOf(buffer, used));
              return;
            }
            if (count == 0) throw new java.io.IOException("stalled_key");
            used += count;
          }
          finish(null);
        }
      }
    } catch (Exception unavailable) {
      finish(null); // Provider messages/paths never reach UI or logs.
    } finally {
      descriptor = null;
      Arrays.fill(buffer, (byte) 0);
    }
  }

  private void finish(byte[] result) {
    synchronized (this) {
      if (finished || abandoned) {
        if (result != null) Arrays.fill(result, (byte) 0);
        return;
      }
      finished = true;
      value = result;
    }
    ScheduledFuture<?> timer = timeout;
    if (timer != null) timer.cancel(false);
    done.complete(null);
  }

  synchronized byte[] take() {
    if (abandoned || !finished) return null;
    byte[] owned = value;
    value = null;
    return owned;
  }

  void abandon() {
    synchronized (this) {
      abandoned = true;
      if (value != null) Arrays.fill(value, (byte) 0);
      value = null;
    }
    ScheduledFuture<?> timer = timeout;
    if (timer != null) timer.cancel(false);
    done.complete(null);
    cancelProvider();
  }

  private void cancelProvider() {
    try {
      CLOSERS.execute(() -> {
        AssetFileDescriptor opened = descriptor;
        if (opened != null) {
          try {
            opened.close();
          } catch (Exception ignored) {
          }
        }
        try {
          cancellation.cancel();
        } catch (RuntimeException ignored) {
        }
      });
    } catch (RejectedExecutionException busy) {
      // Both cancellation workers are already blocked in providers. Reader and
      // descriptor counts remain bounded; a late read still cannot publish data.
    }
  }
}
