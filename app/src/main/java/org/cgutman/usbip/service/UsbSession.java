package org.cgutman.usbip.service;

import android.hardware.usb.*;
import android.os.ParcelFileDescriptor;
import android.os.SystemClock;
import java.net.Socket;
import java.util.concurrent.atomic.AtomicBoolean;
import org.cgutman.usbip.core.*;
import org.cgutman.usbip.jni.NativeUsb;
import org.cgutman.usbip.server.UsbDeviceInfo;
import org.cgutman.usbip.server.UsbIpServer;
import org.cgutman.usbip.server.protocol.dev.*;

final class UsbSession implements UsbBackend, AutoCloseable {
  final UsbDevice device;
  final long generation;
  final UsbDeviceInfo description;
  final UsbDeviceConnection connection;
  final Socket socket;
  final int deviceId;
  private UsbConfiguration configuration;
  private final java.util.Map<Integer, UsbInterface> activeInterfaces =
      new java.util.LinkedHashMap<>();
  private final java.util.Set<Integer> restoreInterfaces = new java.util.LinkedHashSet<>();
  private final UsbIpServer server;
  private final DiagnosticLog log;
  private final java.util.function.IntConsumer configurationChanged;
  private final String session = Long.toHexString(System.nanoTime());
  private final UsbTransport transport;
  private final DiagnosticController diagnostics;
  private final HidTrace trace;
  private final boolean wan;
  private long wanEpoch = 1;
  // Before API 29 fromSocket() borrows the Socket's descriptor. Keep the
  // wrapper alive until session teardown; closing/collecting it earlier
  // closes the live TCP connection on Android 8/9.
  private final ParcelFileDescriptor socketDescriptor;
  private final AtomicBoolean closed = new AtomicBoolean();
  private final TransferCoordinator coordinator;
  private final Thread reaper;

  UsbSession(
      UsbDevice device,
      long generation,
      UsbConfiguration initialConfiguration,
      UsbDeviceInfo description,
      UsbDeviceConnection connection,
      Socket socket,
      UsbIpServer server,
      DiagnosticLog log,
      java.util.function.IntConsumer configurationChanged,
      DiagnosticController diagnostics,
      HidTrace trace,
      boolean wan) {
    this.wan = wan;
    this.device = device;
    this.generation = generation;
    this.description = description;
    this.connection = connection;
    this.socket = socket;
    this.server = server;
    this.log = log;
    this.configurationChanged = configurationChanged;
    this.diagnostics = diagnostics;
    this.trace = trace;
    configuration = initialConfiguration;
    rememberDefaultInterfaces();
    restoreInterfaces.addAll(activeInterfaces.keySet());
    deviceId = ((device.getDeviceId() / 1000) << 16) | (device.getDeviceId() % 1000);
    socketDescriptor = ParcelFileDescriptor.fromSocket(socket);
    try {
      int result = NativeUsb.configureSocket(socketDescriptor.getFd());
      if (result != 0) throw new IllegalStateException("tcp_keepalive_" + result);
      transport =
          trace != null && trace.backend == 1
              ? new AndroidReferenceBackend(
                  new AndroidSdkPort(connection, this::endpoint), trace::sdkRetired)
              : new RustUsbTransport(connection.getFileDescriptor(), trace, wan);
    } catch (RuntimeException | LinkageError e) {
      closeSocketDescriptor();
      throw e;
    }
    if (trace != null) server.observeWrites(socket, trace::tcpWritten);
    coordinator =
        new TransferCoordinator(
            this,
            new TransferCoordinator.Sink() {
              public void send(UsbIpDevicePacket p) {
                server.send(socket, p.serialize());
              }

              public void fault(String reason) {
                event("fault " + reason);
                server.finishClient(socket);
              }

              public void event(String e) {
                log.event("session=" + session + " " + e);
              }
            });
    reaper =
        new Thread(
            () -> {
              try {
                while (!closed.get()) {
                  Completion c = transport.reap(100);
                  if (c != null && c.wanEpoch != 0) {
                    stream(c);
                    continue;
                  }
                  if (c != null) {
                    if (trace != null) trace.javaCompleted(c);
                    coordinator.completed(c, SystemClock.elapsedRealtime());
                    if (c.disconnected) break;
                  }
                  coordinator.tick(SystemClock.elapsedRealtime());
                }
              } catch (RuntimeException | LinkageError e) {
                log.event("session=" + session + " reaper_error " + e.getClass().getSimpleName());
                server.killClient(socket);
              }
            },
            "usb-reaper");
    reaper.start();
    log.event(
        "session="
            + session
            + " ready backend="
            + (trace != null && trace.backend == 1 ? "android_sdk" : "rust")
            + (trace == null ? "" : " diagnostic_scope=" + trace.number));
  }

  private synchronized void stream(Completion c) {
    try {
      if (!closed.get() && c.wanEpoch == wanEpoch)
        server.send(
            socket,
            WanControl.event(
                WanControl.PUSH,
                deviceId,
                (int) c.wanSequence,
                (int) c.wanEpoch,
                c.status,
                c.data));
    } finally {
      if (c.data != null) java.util.Arrays.fill(c.data, (byte) 0);
    }
  }

  synchronized void wanControl(WanControl c) {
    if (!wan || c.devId != deviceId || c.epoch != wanEpoch) {
      server.killClient(socket);
      return;
    }
    long[] result = transport.wanCommand(c.command == WanControl.START ? 5 : 2, c.value);
    if (result.length < 1 || result[0] != 0) server.killClient(socket);
  }

  private int barrier() {
    if (!wan) return 0;
    if (coordinator.pendingCount() != 0) return -16;
    long[] result = transport.wanCommand(3, 0);
    if (result.length < 2 || result[0] != 0) return result.length == 0 ? -5 : (int) result[0];
    wanEpoch = result[1];
    server.send(socket, WanControl.event(WanControl.BARRIER, deviceId, 0, (int) wanEpoch, 0, null));
    return 0;
  }

  private synchronized UsbEndpoint endpoint(int address) {
    for (UsbInterface f : activeInterfaces.values())
      for (int i = 0; i < f.getEndpointCount(); i++)
        if (f.getEndpoint(i).getAddress() == address) return f.getEndpoint(i);
    return null;
  }

  synchronized void submitRequest(UsbIpSubmitUrb r) {
    if (closed.get()) return;
    if (r.devId != deviceId) {
      server.killClient(socket);
      return;
    }
    if (wan && r.ep == 3 && r.direction == 1) {
      coordinator.immediate(r, -95);
      return;
    }
    if (r.ep == 0) {
      int type = r.setup[0] & 255, request = r.setup[1] & 255;
      int value = (r.setup[2] & 255) | ((r.setup[3] & 255) << 8);
      int index = (r.setup[4] & 255) | ((r.setup[5] & 255) << 8);
      if (type == 0 && request == 9) {
        int status = -22;
        if (index == 0 && r.transferBufferLength == 0) status = selectConfiguration(value);
        log.event("session=" + session + " set_configuration result=" + status);
        coordinator.immediate(r, status);
        if (status == -5) server.finishClient(socket);
        return;
      }
      if (type == 1 && request == 11) {
        int status = -22;
        if (r.transferBufferLength == 0) {
          for (int i = 0; i < configuration.getInterfaceCount(); i++) {
            UsbInterface candidate = configuration.getInterface(i);
            if (candidate.getId() != index || candidate.getAlternateSetting() != value) continue;
            // USB/IP bulk streams are not implemented. Never silently run a UAS alternate.
            if (candidate.getInterfaceClass() == 8 && candidate.getInterfaceProtocol() == 0x62) {
              status = -95;
              break;
            }
            status = barrier();
            if (status == 0)
              status =
                  coordinator.pendingCount() != 0
                      ? -16
                      : connection.setInterface(candidate) ? 0 : -5;
            if (status == 0) activeInterfaces.put(index, candidate);
            break;
          }
        }
        log.event("session=" + session + " set_interface result=" + status);
        coordinator.immediate(r, status);
        if (status == -5) server.finishClient(socket);
        return;
      }
      coordinator.submit(r, 2, r.direction == 1 ? 128 : 0, SystemClock.elapsedRealtime());
      return;
    }
    int address = r.ep | (r.direction == 1 ? 128 : 0);
    for (UsbInterface f : activeInterfaces.values()) {
      for (int j = 0; j < f.getEndpointCount(); j++) {
        UsbEndpoint ep = f.getEndpoint(j);
        if (ep.getAddress() != address) continue;
        int type =
            ep.getType() == UsbConstants.USB_ENDPOINT_XFER_BULK
                ? 3
                : ep.getType() == UsbConstants.USB_ENDPOINT_XFER_INT ? 1 : -1;
        if (type != -1) {
          coordinator.submit(r, type, address, SystemClock.elapsedRealtime());
          return;
        }
      }
    }
    coordinator.immediate(r, -22);
  }

  private void rememberDefaultInterfaces() {
    activeInterfaces.clear();
    for (int i = 0; i < configuration.getInterfaceCount(); i++) {
      UsbInterface f = configuration.getInterface(i);
      if (f.getAlternateSetting() == 0) activeInterfaces.put(f.getId(), f);
    }
  }

  private int selectConfiguration(int value) {
    UsbConfiguration next = null;
    for (int i = 0; i < device.getConfigurationCount(); i++)
      if (device.getConfiguration(i).getId() == value) next = device.getConfiguration(i);
    if (next == null) return -22;
    int paused = barrier();
    if (paused != 0) return paused;
    final UsbConfiguration target = next;
    int result =
        ConfigurationTransition.apply(
            value,
            target.getId(),
            coordinator.pendingCount(),
            new ConfigurationTransition.Operations() {
              public boolean releaseInterfaces() {
                boolean ok = true;
                for (UsbInterface f : activeInterfaces.values()) {
                  int result =
                      NativeUsb.releaseInterface(connection.getFileDescriptor(), f.getId(), false);
                  if (result != 0)
                    log.event(
                        "configuration_release_failed interface=" + f.getId() + " errno=" + result);
                  ok &= result == 0;
                }
                return ok;
              }

              public boolean selectConfiguration() {
                for (int i = 0; i < target.getInterfaceCount(); i++)
                  if (target.getInterface(i).getAlternateSetting() == 0)
                    restoreInterfaces.add(target.getInterface(i).getId());
                boolean selected = connection.setConfiguration(target);
                if (!selected) log.event("configuration_select_failed value=" + target.getId());
                return selected;
              }

              public boolean claimInterfaces() {
                configuration = target;
                rememberDefaultInterfaces();
                restoreInterfaces.addAll(activeInterfaces.keySet());
                java.util.List<UsbInterface> claimed = new java.util.ArrayList<>();
                for (UsbInterface f : activeInterfaces.values()) {
                  if (!connection.claimInterface(f, true)) {
                    for (UsbInterface old : claimed)
                      NativeUsb.releaseInterface(
                          connection.getFileDescriptor(), old.getId(), false);
                    return false;
                  }
                  claimed.add(f);
                }
                return true;
              }
            });
    if (result == 0) configurationChanged.accept(value);
    return result;
  }

  void unlinkRequest(UsbIpUnlinkUrb r) {
    if (r.devId != deviceId) {
      server.killClient(socket);
      return;
    }
    if (trace != null) trace.unlink(r.seqNum, r.seqNumToUnlink);
    coordinator.unlink(r, SystemClock.elapsedRealtime());
  }

  public int submit(UsbIpSubmitUrb r, int type, int endpoint) {
    if (trace != null) trace.submitted(r.seqNum, type, endpoint);
    return transport.submit(r, type, endpoint);
  }

  public int cancel(int id) {
    return transport.cancel(id);
  }

  public boolean isFatalCompletion(Completion completion) {
    return transport.isFatalCompletion(completion);
  }

  private boolean connectionReleased;

  @Override
  public void close() {
    closeAndRelease();
  }

  synchronized boolean closeAndRelease() {
    if (!closed.compareAndSet(false, true)) return connectionReleased;
    coordinator.close();
    boolean interrupted = false;
    // Reap waits at most 100 ms per poll. Join before native close drains cancellations.
    while (reaper.isAlive())
      try {
        reaper.join();
      } catch (InterruptedException e) {
        interrupted = true;
      }
    transport.close();
    if (transport.canReleaseConnection()) {
      // All backend calls retired before returning drivers/closing the Android handle.
      for (int id : restoreInterfaces)
        NativeUsb.releaseInterface(connection.getFileDescriptor(), id, true);
      connection.close();
      connectionReleased = true;
    } else {
      AndroidReferenceBackend.quarantineResource(connection);
      log.event("session=" + session + " sdk_quarantine_restart_required");
    }
    if (diagnostics != null) diagnostics.ended(trace);
    closeSocketDescriptor();
    log.event("session=" + session + " closed");
    if (interrupted) Thread.currentThread().interrupt();
    return connectionReleased;
  }

  private void closeSocketDescriptor() {
    try {
      socketDescriptor.close();
    } catch (java.io.IOException e) {
      log.event("tcp_descriptor_close_failed");
    }
  }
}
