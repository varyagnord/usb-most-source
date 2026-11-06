package org.cgutman.usbip.service;

import android.hardware.usb.*;
import java.nio.ByteBuffer;
import java.util.IdentityHashMap;
import java.util.concurrent.TimeoutException;
import java.util.function.IntFunction;
import org.cgutman.usbip.core.AndroidReferenceBackend;

/** This connection is exclusively SDK-owned: never run NativeUsb.reap on its fd. */
final class AndroidSdkPort implements AndroidReferenceBackend.Port {
  private final UsbDeviceConnection connection;
  private final IntFunction<UsbEndpoint> endpoints;
  private final IdentityHashMap<UsbRequest, SdkToken> tokens = new IdentityHashMap<>();
  AndroidSdkPort(UsbDeviceConnection connection, IntFunction<UsbEndpoint> endpoints) {
    this.connection = connection;
    this.endpoints = endpoints;
  }
  private final class SdkToken implements AndroidReferenceBackend.Token {
    final UsbRequest request;
    SdkToken(UsbRequest request) { this.request = request; }
    public boolean queue(ByteBuffer bytes) { return request.queue(bytes); }
    public boolean cancel() { return request.cancel(); }
    public void close() {
      synchronized (tokens) { tokens.remove(request); }
      request.close();
    }
  }
  public AndroidReferenceBackend.Token create(int endpoint) {
    UsbEndpoint ep = endpoints.apply(endpoint);
    if (ep == null || ep.getType() != UsbConstants.USB_ENDPOINT_XFER_INT) return null;
    UsbRequest request = new UsbRequest();
    if (!request.initialize(connection, ep)) { request.close(); return null; }
    SdkToken token = new SdkToken(request);
    synchronized (tokens) { tokens.put(request, token); }
    return token;
  }
  public AndroidReferenceBackend.Token waitRequest(int timeout) throws TimeoutException {
    UsbRequest request = connection.requestWait(timeout);
    synchronized (tokens) { return request == null ? null : tokens.get(request); }
  }
  public int control(int type, int request, int value, int index, byte[] data, int timeout) {
    return connection.controlTransfer(type, request, value, index, data, data.length, timeout);
  }
}
