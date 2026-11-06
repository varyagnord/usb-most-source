package org.cgutman.usbip.server.protocol.dev;

import java.io.IOException;
import java.io.InputStream;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import org.cgutman.usbip.utils.StreamUtils;

public class UsbIpUnlinkUrb extends UsbIpDevicePacket {
  public int seqNumToUnlink;

  public static final int WIRE_SIZE = 4;

  public UsbIpUnlinkUrb(byte[] header) {
    super(header);
  }

  public static UsbIpUnlinkUrb read(byte[] header, InputStream in) throws IOException {
    UsbIpUnlinkUrb msg = new UsbIpUnlinkUrb(header);

    byte[] continuationHeader = new byte[WIRE_SIZE];
    StreamUtils.readAll(in, continuationHeader);

    ByteBuffer bb = ByteBuffer.wrap(continuationHeader).order(ByteOrder.BIG_ENDIAN);
    msg.seqNumToUnlink = bb.getInt();

    byte[] padding = new byte[24];
    StreamUtils.readAll(in, padding);

    return msg;
  }

  @Override
  public String toString() {
    String sb =
        super.toString()
            + String.format(
                java.util.Locale.ROOT, "Sequence number to unlink: %d\n", seqNumToUnlink);
    return sb;
  }

  @Override
  protected byte[] serializeInternal() {
    throw new UnsupportedOperationException("Serializing not supported");
  }
}
