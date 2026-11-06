package org.cgutman.usbip.core;

import org.cgutman.usbip.server.protocol.dev.UsbIpSubmitUrb;

public interface UsbBackend {
  int submit(UsbIpSubmitUrb request, int type, int endpoint);

  int cancel(int id);

  /** The default usbfs policy is deliberately conservative. */
  default boolean isFatalCompletion(Completion completion) {
    return completion.disconnected || completion.status == -19 || completion.status == -5;
  }

  final class Completion {
    public final int id, status, actualLength;
    public final byte[] data;
    public final boolean disconnected;
    public final long wanEpoch, wanSequence;

    public Completion(int id, int status, int actualLength, byte[] data, boolean disconnected) {
      this(id, status, actualLength, data, disconnected, 0, 0);
    }

    public Completion(int id, int status, int actualLength, byte[] data, boolean disconnected, long epoch, long sequence) {
      this.wanEpoch = epoch;
      this.wanSequence = sequence;
      this.id = id;
      this.status = status;
      this.actualLength = actualLength;
      this.data = data;
      this.disconnected = disconnected;
    }
  }
}
