package org.cgutman.usbip.jni;

/** Pure network, selection and USB descriptor policy is owned and tested by Rust. */
public final class NativePolicy {
  static {
    System.loadLibrary("usblib");
  }

  private NativePolicy() {}

  public static native boolean validHost(String host, int mode);

  public static native int capability(
      int deviceClass, int configurations, int interfaces, int flags);

  public static native int[] describe(byte[] rawDescriptors);

  /** 0: inventory pairs; 1: [id,selected]; 2: [all]; 3: snapshot. Header is all-mode. */
  public static native long selectionCreate();

  public static native void selectionClose(long model);

  public static native int[] selection(long model, int action, int[] values);

  public static native int speed(int fileDescriptor);

  public static native String connectionCommands(String tabletAddress, int mode, int[] deviceIds);
}
