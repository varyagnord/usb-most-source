package org.cgutman.usbip.server.protocol.cli;

import java.io.IOException;
import java.io.InputStream;
import java.nio.ByteBuffer;
import org.cgutman.usbip.utils.StreamUtils;

/** Private diagnostic extension. No USB payload, device commands or file paths. */
public final class DiagnosticRequest extends CommonPacket {
  public static final short GET = (short) 0x80f0, BACKEND = (short) 0x80f1,
      CLEAR = (short) 0x80f2, SELFTEST = (short) 0x80f3;
  public int mode;
  public DiagnosticRequest(byte[] header, InputStream in) throws IOException {
    super(header);
    if (version != 0x111 || status != 0) throw new IOException("diagnostic_header");
    if (code == BACKEND) {
      byte[] bytes = new byte[4];
      StreamUtils.readAll(in, bytes);
      mode = ByteBuffer.wrap(bytes).getInt();
      if (mode != 0 && mode != 1) throw new IOException("diagnostic_backend");
    }
  }
  protected byte[] serializeInternal() { throw new UnsupportedOperationException(); }
}
