package org.cgutman.usbip.core;

/** Exactly one asynchronous request owner/reaper for a physical USB connection. */
public interface UsbTransport extends UsbBackend, AutoCloseable {
  Completion reap(int timeoutMs);
  void close();
  default long[] wanCommand(int op, int value) { return new long[] {-95}; }
  /** False means the owner must retain the Android connection until process exit. */
  default boolean canReleaseConnection() { return true; }
}
