package org.cgutman.usbip.server;

import java.net.Socket;
import java.util.List;
import org.cgutman.usbip.server.protocol.dev.UsbIpSubmitUrb;
import org.cgutman.usbip.server.protocol.dev.UsbIpUnlinkUrb;
import org.cgutman.usbip.server.protocol.dev.WanControl;

public interface UsbRequestHandler {
  public List<UsbDeviceInfo> getDevices();

  public UsbDeviceInfo getDeviceByBusId(String busId);

  /** Description pinned to this session, never a fresh lookup by a reusable USB bus ID. */
  public UsbDeviceInfo getAttachedDevice(Socket socket);

  public boolean attachToDevice(Socket s, String busId);
  default ManagementAccess management() { return null; }
  default boolean supportsWan(String busId) { return false; }
  default boolean attachToDevice(Socket s, String busId, boolean wan) { return !wan && attachToDevice(s, busId); }
  default void wanControl(Socket s, WanControl control) { throw new IllegalStateException("wan_not_negotiated"); }

  public void detachFromDevice(Socket s, String busId);

  public void submitUrbRequest(Socket s, UsbIpSubmitUrb msg);

  public void abortUrbRequest(Socket s, UsbIpUnlinkUrb msg);

  public void cleanupSocket(Socket s);
}
