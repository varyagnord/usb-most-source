package org.cgutman.usbip.jni;

/** Bounded JNI surface; owns no USB handles. */
public final class NativeConnection {
  static {
    System.loadLibrary("usblib");
  }

  private NativeConnection() {}

  public static native long start(
      byte[] profile, byte[] secret, int port, byte[] proof, String artifacts);

  public static native String poll(long id);

  public static native boolean approve(long id, String pin, boolean accept);

  public static native boolean saved(long id, long operation, boolean success);

  public static native void stop(long id);

  public static native String decodeQr(byte[] grayscale, int width, int height);

  /** Read-only explanation of current observations; no credentials or USB data. */
  public static native String diagnose(byte[] observations);

  /** Settings-only transaction/query. Caller must wipe both arguments and result.
   * A mutation returns a candidate, never writes disk or acknowledges durability.
   * Invalid input throws IllegalArgumentException with a fixed, non-secret code. */
  public static native byte[] computers(byte[] snapshot, byte[] request, boolean running);
}
