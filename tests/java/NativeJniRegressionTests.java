import java.io.*;
import java.net.*;
import java.nio.file.*;
import java.util.*;
import java.util.concurrent.*;
import org.cgutman.usbip.core.UsbBackend;
import org.cgutman.usbip.jni.NativeUsb;

/** Real JVM/JNI validation; no USB hardware or replacement native code. */
public final class NativeJniRegressionTests {
  private static int checks;

  private static void check(boolean ok, String message) {
    if (!ok) throw new AssertionError(message);
    checks++;
  }

  private static int fd(FileDescriptor descriptor) throws Exception {
    var field = FileDescriptor.class.getDeclaredField("fd");
    field.setAccessible(true);
    return field.getInt(descriptor);
  }

  public static void main(String[] args) throws Exception {
    check(NativeUsb.open(-1) == 0, "invalid open");
    for (long bad : new long[] {0, -1, Long.MAX_VALUE, 0x123456789L}) {
      check(NativeUsb.cancel(bad, 1) == -22, "invalid cancel handle");
      check(NativeUsb.submit(bad, 1, 3, 130, 0, null, 0, null) == -22, "invalid submit handle");
      check(NativeUsb.reap(bad, 0).disconnected, "invalid reap handle returns an error");
      NativeUsb.close(bad);
    }
    Path path = Files.createTempFile("usb-most-jni", ".tmp");
    try (RandomAccessFile f = new RandomAccessFile(path.toFile(), "rw")) {
      int descriptor = fd(f.getFD());
      for (int id : new int[] {-1, 256, Integer.MIN_VALUE, Integer.MAX_VALUE})
        for (boolean reconnect : new boolean[] {false, true})
          check(
              NativeUsb.releaseInterface(descriptor, id, reconnect) == -22,
              "invalid interface must not reach ioctl");
      for (int id : new int[] {0, 1, 255})
        for (boolean reconnect : new boolean[] {false, true})
          check(
              NativeUsb.releaseInterface(descriptor, id, reconnect) == -25,
              "valid interface on ordinary fd returns ENOTTY");
      check(NativeUsb.releaseInterface(-1, 0, false) == -22, "invalid release fd");
      long[] handles = new long[4];
      for (int i = 0; i < 4; i++) {
        handles[i] = NativeUsb.open(descriptor);
        check(handles[i] > 0, "open handle");
      }
      check(NativeUsb.open(descriptor) == 0, "session limit");
      for (long h : handles) NativeUsb.close(h);
      for (int iteration = 0; iteration < 100; iteration++) {
        long h = NativeUsb.open(descriptor);
        check(h > 0, "open after close");
        check(h > handles[3], "handles never reused");
        UsbBackend.Completion c = NativeUsb.reap(h, 0);
        check(
            c.id == 0 && c.status < 0 && c.actualLength == 0 && c.data == null && c.disconnected,
            "native Java constructor values");
        for (int size : new int[] {-1, 1048577, Integer.MAX_VALUE})
          check(NativeUsb.submit(h, 1, 3, 130, 0, null, size, null) == -22, "length rejection");
        for (int size : new int[] {0, 1, 65556, 131072, 131073, 520192, 524288, 1048576})
          check(
              NativeUsb.submit(h, 1, 3, 130, 0, null, size, null) == -25,
              "valid length reaches ioctl");
        check(NativeUsb.submit(h, 1, 3, 1, 0, new byte[1], 2, null) == -22, "payload mismatch");
        check(NativeUsb.submit(h, 1, 2, 0, 0, null, 0, new byte[7]) == -22, "setup mismatch");
        check(NativeUsb.submit(h, 1, 3, 1, 0, null, 1, null) == -22, "missing OUT payload");
        check(NativeUsb.cancel(h, 1) == -2, "missing request");
        ExecutorService threads = Executors.newFixedThreadPool(3);
        try {
          Future<?> a =
              threads.submit(() -> {
                for (int i = 0; i < 20; i++) NativeUsb.reap(h, 0);
              });
          Future<?> b =
              threads.submit(() -> {
                for (int i = 0; i < 20; i++) NativeUsb.cancel(h, 1);
              });
          Future<?> close = threads.submit(() -> NativeUsb.close(h));
          a.get();
          b.get();
          close.get();
        } finally {
          threads.shutdownNow();
        }
        NativeUsb.close(h);
        check(NativeUsb.cancel(h, 1) == -22, "stale closed handle");
      }
      f.write(7);
      check(f.length() == 1, "Java fd remains owned by Java");
    } finally {
      Files.delete(path);
    }
    check(NativeUsb.configureSocket(-1) < 0, "invalid TCP fd");
    System.out.println(
        "PASS real JVM JNI: " + checks + " checks, invalid/stale handles and concurrent close");
  }
}
