package org.cgutman.usbip.server.protocol.cli;

import java.io.IOException;
import java.io.InputStream;
import org.cgutman.usbip.utils.StreamUtils;

/** Fixed-size envelope. Rust validates all fields before any import or state mutation. */
public final class ManagedRequest extends CommonPacket {
  public static final short CAPS = (short) 0x80e1;
  public static final short IMPORT = (short) 0x80e2;
  public static final short SYNC = (short) 0x80e3;
  public final byte[] body;

  ManagedRequest(byte[] header, InputStream input) throws IOException {
    super(header);
    if (version != 0x111 || status != 0) throw new IOException("invalid_managed_header");
    body = new byte[code == IMPORT ? 52 : code == SYNC ? 40 : 0];
    StreamUtils.readAll(input, body);
  }

  @Override
  protected byte[] serializeInternal() {
    throw new UnsupportedOperationException("request_only");
  }
}
