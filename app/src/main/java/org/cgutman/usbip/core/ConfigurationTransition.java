package org.cgutman.usbip.core;

/** USB configuration changes must be executed, and never race outstanding URBs. */
public final class ConfigurationTransition {
  public interface Operations {
    boolean releaseInterfaces();

    boolean selectConfiguration();

    boolean claimInterfaces();
  }

  private ConfigurationTransition() {}

  public static int apply(int requested, int supported, int pending, Operations operations) {
    if (requested != supported) return -22;
    if (pending != 0) return -16;
    if (!operations.releaseInterfaces()) return -5;
    if (!operations.selectConfiguration()) return -5;
    return operations.claimInterfaces() ? 0 : -5;
  }
}
