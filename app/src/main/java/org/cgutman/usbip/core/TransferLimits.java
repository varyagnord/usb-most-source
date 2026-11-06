package org.cgutman.usbip.core;

/** Bounded bulk buffers cover modern host USB3 requests as well as CCID. */
public final class TransferLimits {
  // Keep aligned with Rust engine::MAX_BYTES and protocol wire::MAX.
  // Rust separately limits retained request/completion memory to 8 MiB per engine.
  public static final int MAX_TRANSFER_BYTES = 1024 * 1024;

  private TransferLimits() {}
}
