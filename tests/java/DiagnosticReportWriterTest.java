package org.cgutman.usbip.config;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;
import java.util.Arrays;

/** Exercises the actual production adapter, including errors on resource close. */
public final class DiagnosticReportWriterTest {
  private static int checks;
  private static void check(boolean value, String name) {
    if (!value) throw new AssertionError(name);
    checks++;
  }
  private static final class Sink extends OutputStream {
    final ByteArrayOutputStream data = new ByteArrayOutputStream();
    final boolean writeFailure, closeFailure;
    int closes;
    Sink(boolean writeFailure, boolean closeFailure) {
      this.writeFailure = writeFailure; this.closeFailure = closeFailure;
    }
    @Override public void write(int value) throws IOException {
      if (writeFailure) throw new IOException("write_failure");
      data.write(value);
    }
    @Override public void close() throws IOException {
      closes++;
      if (closeFailure) throw new IOException("close_failure");
    }
  }
  public static void main(String[] args) throws Exception {
    String text = "USB Мост\nТочный UTF-8 отчёт — проверка\n";
    Sink good = new Sink(false, false);
    DiagnosticReportWriter.writeAndClose(good, text);
    check(Arrays.equals(good.data.toByteArray(), text.getBytes(StandardCharsets.UTF_8)), "exact UTF-8 and final newline");
    check(good.closes == 1, "successful sink closed exactly once before return");
    for (boolean close : new boolean[] {false, true}) {
      Sink failed = new Sink(true, close);
      try {
        DiagnosticReportWriter.writeAndClose(failed, text);
        throw new AssertionError("write failure returned success");
      } catch (IOException expected) {
        check(expected.getMessage().equals("write_failure"), "primary write error preserved");
        check(expected.getSuppressed().length == (close ? 1 : 0), "close error correctly suppressed after write failure");
        check(failed.closes == 1, "failed sink closed exactly once");
      }
    }
    Sink failedClose = new Sink(false, true);
    try {
      DiagnosticReportWriter.writeAndClose(failedClose, text);
      throw new AssertionError("close failure returned success");
    } catch (IOException expected) {
      check(expected.getMessage().equals("close_failure"), "close error propagates, never success");
      check(Arrays.equals(failedClose.data.toByteArray(), text.getBytes(StandardCharsets.UTF_8)), "even fully written bytes do not imply a successful close");
      check(failedClose.closes == 1, "close not retried");
    }
    try {
      DiagnosticReportWriter.writeAndClose(null, text);
      throw new AssertionError("missing provider output returned success");
    } catch (IOException expected) {
      check(expected.getMessage().equals("output_unavailable"), "missing output explicitly fails");
    }
    System.out.println("PASS actual production DiagnosticReportWriter: " + checks + " checks");
  }
}
