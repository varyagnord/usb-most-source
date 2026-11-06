package org.cgutman.usbip.server;

/** Present only in an explicitly diagnostic build; the normal USB/IP server has none. */
public interface DiagnosticAccess {
  int MAX_BYTES = 256 * 1024;
  byte[] snapshot();
  boolean selectBackend(int mode);
  boolean clear();
  byte[] selftest();
}
