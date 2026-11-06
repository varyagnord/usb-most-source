package org.cgutman.usbip.jni;

import org.cgutman.usbip.core.UsbBackend;

public final class NativeUsb {
  public static native long[] wanNetworkTest(int rtt, int jitter, int reports);
  public static native UsbBackend.Completion wanSelfTest(int rtt, int jitter, int lossPerMille, int reports);
  public static native long[] wanCommand(long handle, int op, int value);
  public static native int configureSocket(int fd);

  /** Release without rebinding the Android driver until session teardown. Caller keeps fd alive. */
  public static native int releaseInterface(int fd, int interfaceId, boolean reconnect);

  static {
    System.loadLibrary("usblib");
  }

  private NativeUsb() {}

  public static native long open(int fd);

  public static native int submit(
      long handle,
      int id,
      int type,
      int endpoint,
      int flags,
      byte[] data,
      int length,
      byte[] setup);

  public static native int cancel(long handle, int id);

  public static native UsbBackend.Completion reap(long handle, int timeoutMs);

  public static native void close(long handle);

  /** Available only in a diagnostic native build, never a production control API. */
  public static native int enableHidDiagnostics(long handle);

  /** Transient internal records; digests must be compared and discarded, never exported. */
  public static native long[] takeHidDiagnostics(long handle);

  /** Synthetic, at most 32 bytes; no device I/O. Diagnostic builds only. */
  public static native UsbBackend.Completion diagnosticRoundTrip(byte[] data);
}
