package org.cgutman.usbip.connection;

import android.hardware.usb.*;
import java.nio.charset.StandardCharsets;
import java.util.Arrays;
import org.cgutman.usbip.jni.NativeConnection;
import org.json.*;

/** Android descriptors in; bounded Rust recognition and policy out. No USB open or permissions. */
public final class UsbRules {
  private UsbRules() {}

  public static String query(JSONObject request) throws JSONException {
    byte[] input = request.toString().getBytes(StandardCharsets.UTF_8);
    byte[] output = null;
    try {
      output = NativeConnection.computers(new byte[0], input, false);
      return new String(output, StandardCharsets.UTF_8);
    } finally {
      Arrays.fill(input, (byte) 0);
      if (output != null) Arrays.fill(output, (byte) 0);
    }
  }

  /** Called on the inventory metadata worker; serial may require Binder permission checks. */
  public static String identity(UsbDevice device, String serial) throws JSONException {
    if (serial == null || serial.length() > 1024) return null;
    JSONArray layout = new JSONArray();
    layout
        .put(1)
        .put(device.getVersion())
        .put(device.getDeviceClass())
        .put(device.getDeviceSubclass())
        .put(device.getDeviceProtocol());
    for (int c = 0; c < device.getConfigurationCount(); c++) {
      UsbConfiguration configuration = device.getConfiguration(c);
      JSONArray interfaces = new JSONArray();
      for (int i = 0; i < configuration.getInterfaceCount(); i++) {
        UsbInterface item = configuration.getInterface(i);
        JSONArray endpoints = new JSONArray();
        for (int e = 0; e < item.getEndpointCount(); e++) {
          UsbEndpoint endpoint = item.getEndpoint(e);
          endpoints.put(
              new JSONArray()
                  .put(endpoint.getAddress())
                  .put(endpoint.getAttributes())
                  .put(endpoint.getMaxPacketSize())
                  .put(endpoint.getInterval()));
        }
        interfaces.put(
            new JSONArray()
                .put(item.getId())
                .put(item.getAlternateSetting())
                .put(item.getInterfaceClass())
                .put(item.getInterfaceSubclass())
                .put(item.getInterfaceProtocol())
                .put(endpoints));
        if (interfaces.toString().length() > 8192) return null;
      }
      layout.put(
          new JSONArray()
              .put(configuration.getId())
              .put(configuration.getMaxPower())
              .put(configuration.isSelfPowered())
              .put(configuration.isRemoteWakeup())
              .put(interfaces));
      if (layout.toString().length() > 8192) return null;
    }
    String result =
        query(
            new JSONObject()
                .put("op", "usb_identity")
                .put("vendor", device.getVendorId())
                .put("product", device.getProductId())
                .put("serial", serial)
                .put("layout", layout.toString()));
    return result.equals("null") ? null : result;
  }
}
