package org.cgutman.usbip.service;

import android.app.Application;

/** Owns process-lifetime Android adapters without retaining an Activity context. */
public final class BridgeApplication extends Application {
  private UsbInventory inventory;
  private DiagnosticController diagnostics;

  synchronized DiagnosticController diagnostics() {
    if (diagnostics == null) diagnostics = new DiagnosticController(this);
    return diagnostics;
  }

  synchronized UsbInventory inventory() {
    if (inventory == null) {
      inventory = new UsbInventory(this);
      inventory.bindComputers(org.cgutman.usbip.connection.ComputerRepository.get(this));
    }
    return inventory;
  }
}
