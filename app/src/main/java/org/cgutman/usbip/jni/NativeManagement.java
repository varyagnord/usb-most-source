package org.cgutman.usbip.jni;

/** Bounded Rust intent registry; Android still owns authorization and USB leases. */
public final class NativeManagement {
  static {
    System.loadLibrary("usblib");
  }
  private NativeManagement() {}

  public static final int CAPS = 0;
  public static final int VALIDATE_IMPORT = 1;
  public static final int RESERVE = 2;
  public static final int RELEASED = 3;
  public static final int WITHDRAW = 4;
  public static final int REQUEST = 5;
  public static final int POLL = 6;
  public static final int SNAPSHOT = 7;
  public static final int ACK_SAFE = 8;
  public static final int TIMEOUT = 9;
  public static final int CAN_REQUEST = 10;

  /** Fresh random 16-byte app-service nonce; never an address or saved profile ID. */
  public static native long create(byte[] nonce);

  public static native void close(long model);

  /** Null rejects invalid/stale/unsupported metadata; successful mutations return []. */
  public static native byte[] execute(long model, int action, byte[] input);
}
