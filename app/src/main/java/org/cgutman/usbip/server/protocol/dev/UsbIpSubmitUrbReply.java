package org.cgutman.usbip.server.protocol.dev;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import org.cgutman.usbip.core.TransferLimits;

public class UsbIpSubmitUrbReply extends UsbIpDevicePacket {
  public int status;
  public int actualLength;
  public int startFrame;
  public int numberOfPackets;
  public int errorCount;

  public byte[] inData;

  public UsbIpSubmitUrbReply(int seqNum, int devId, int dir, int ep) {
    super(UsbIpDevicePacket.USBIP_RET_SUBMIT, seqNum, devId, dir, ep);
  }

  protected byte[] serializeInternal() {
    if (actualLength < 0
        || actualLength > TransferLimits.MAX_TRANSFER_BYTES
        || (inData != null && actualLength > inData.length))
      throw new IllegalArgumentException("Invalid reply length");
    int inDataLen = inData == null ? 0 : actualLength;
    ByteBuffer bb =
        ByteBuffer.allocate((UsbIpDevicePacket.USBIP_HEADER_SIZE - 20) + inDataLen)
            .order(ByteOrder.BIG_ENDIAN);

    bb.putInt(status);
    bb.putInt(actualLength);
    bb.putInt(startFrame);
    bb.putInt(numberOfPackets);
    bb.putInt(errorCount);

    bb.position(UsbIpDevicePacket.USBIP_HEADER_SIZE - 20);

    if (inDataLen != 0) {
      bb.put(inData, 0, inDataLen);
    }

    return bb.array();
  }

  @Override
  public String toString() {
    String sb =
        super.toString()
            + String.format(java.util.Locale.ROOT, "Status: 0x%x\n", status)
            + String.format(java.util.Locale.ROOT, "Actual length: %d\n", actualLength)
            + String.format(java.util.Locale.ROOT, "Start frame: %d\n", startFrame)
            + String.format(java.util.Locale.ROOT, "Number Of Packets: %d\n", numberOfPackets)
            + String.format(java.util.Locale.ROOT, "Error Count: %d\n", errorCount);
    return sb;
  }
}
