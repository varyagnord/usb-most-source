package org.cgutman.usbip.service;

import android.os.SystemClock;
import org.cgutman.usbip.core.*;
import org.cgutman.usbip.jni.NativeUsb;
import org.cgutman.usbip.server.protocol.dev.UsbIpSubmitUrb;

final class RustUsbTransport implements UsbTransport {
  private final long handle;
  private final HidTrace trace;

  RustUsbTransport(int fd, HidTrace trace) { this(fd, trace, false); }

  RustUsbTransport(int fd, HidTrace trace, boolean wan) {
    this.trace = trace;
    handle = NativeUsb.open(fd);
    if (handle == 0) throw new IllegalStateException("usbfs_open_failed");
    try {
      if (wan && NativeUsb.wanCommand(handle, 1, 1)[0] != 0) throw new IllegalStateException("wan_enable_failed");
    } catch (RuntimeException | LinkageError error) { NativeUsb.close(handle); throw error; }
    if (trace != null) {
      try {
        trace.nativeClock(SystemClock.elapsedRealtime());
        if (NativeUsb.enableHidDiagnostics(handle) != 0) throw new IllegalStateException("native_diagnostic_enable_failed");
      } catch (RuntimeException | LinkageError error) {
        NativeUsb.close(handle);
        throw error;
      }
    }
  }
  public int submit(UsbIpSubmitUrb r, int type, int endpoint) {
    return NativeUsb.submit(handle, r.seqNum, type, endpoint, r.transferFlags,
        r.outData, r.transferBufferLength, type == 2 ? r.setup : null);
  }
  public int cancel(int id) { return NativeUsb.cancel(handle, id); }
  public Completion reap(int timeout) {
    Completion c = NativeUsb.reap(handle, timeout);
    if (trace != null && c != null) trace.nativeBatch(NativeUsb.takeHidDiagnostics(handle), c);
    return c;
  }
  public long[] wanCommand(int op, int value) { return NativeUsb.wanCommand(handle, op, value); }
  public void close() { NativeUsb.close(handle); }
}
