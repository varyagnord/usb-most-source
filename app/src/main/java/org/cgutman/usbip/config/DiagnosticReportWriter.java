package org.cgutman.usbip.config;

import java.io.IOException;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;

/** Android stream adapter: acknowledge success only after writing AND closing. */
final class DiagnosticReportWriter {
  private DiagnosticReportWriter() {}

  static void writeAndClose(OutputStream stream, String report) throws IOException {
    try (OutputStream owned = stream) {
      if (owned == null) throw new IOException("output_unavailable");
      owned.write(report.getBytes(StandardCharsets.UTF_8));
    }
  }
}
