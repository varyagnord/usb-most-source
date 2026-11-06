package org.cgutman.usbip.server;

import java.net.Socket;

/** Metadata only, offered exclusively inside the authenticated local-proof endpoint. */
public interface ManagementAccess {
  byte[] capabilities();

  byte[] validateImport(byte[] request);

  byte[] reserve();

  /** Bind a reserved key to an already successful, exact Android USB lease. */
  boolean attached(Socket socket, byte[] key);

  void abandon(byte[] key);

  void released(byte[] key);

  byte[] synchronize(byte[] report);
}
