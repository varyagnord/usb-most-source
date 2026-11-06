package ru.levchatkin.rutokenbridge;

import android.hardware.usb.UsbDevice;
import java.io.*;
import java.net.*;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.*;
import java.util.concurrent.*;
import java.util.concurrent.atomic.AtomicInteger;
import org.cgutman.usbip.core.ManagedImports;
import org.cgutman.usbip.jni.NativeManagement;
import org.cgutman.usbip.server.*;
import org.cgutman.usbip.server.protocol.*;
import org.cgutman.usbip.server.protocol.dev.*;
import org.cgutman.usbip.service.UsbInventory;

/**
 * Test APK only. Replaces usbfs with an explicitly owned QEMU usbip_host disk. Production Java
 * framing, Rust management and inventory leases remain real. Only Linux's zeroed reply metadata is
 * adapted; payloads/statuses are unchanged.
 */
final class QemuUsbBackend implements UsbRequestHandler, AutoCloseable {
  final UsbIpServer server;
  final AtomicInteger submitted = new AtomicInteger(), completed = new AtomicInteger();
  final AtomicInteger attached = new AtomicInteger();
  private final UsbInventory inventory;
  private final UsbDevice device;
  private final long generation;
  private final int backendPort;
  private final String backendBus;
  private final UsbDeviceInfo info;
  private final byte[] upstreamDescriptor;
  private final ManagedImports management;
  private final Map<Socket, Channel> channels = new ConcurrentHashMap<>();
  volatile Throwable failure;

  QemuUsbBackend(UsbInventory inventory, UsbDevice device, int port, String bus) throws Exception {
    this(inventory, device, port, bus, new UsbIpServer(event -> {}));
  }

  QemuUsbBackend(UsbInventory inventory, UsbDevice device, int port, String bus, UsbIpServer server)
      throws Exception {
    this.server = server;
    if (port < 1024 || port > 65535 || !bus.matches("[0-9]+-[0-9.]+"))
      throw new IllegalArgumentException("owned backend endpoint");
    this.inventory = inventory;
    this.device = device;
    generation = inventory.item(device.getDeviceId()).generation;
    backendPort = port;
    backendBus = bus;
    try (Socket socket = connect()) {
      socket
          .getOutputStream()
          .write(
              ByteBuffer.allocate(8)
                  .putShort((short) 0x111)
                  .putShort((short) 0x8005)
                  .putInt(0)
                  .array());
      DataInputStream input = new DataInputStream(socket.getInputStream());
      byte[] header = new byte[12];
      input.readFully(header);
      ByteBuffer h = ByteBuffer.wrap(header);
      if (h.getShort() != 0x111 || h.getShort() != 5 || h.getInt() != 0 || h.getInt() != 1)
        throw new IOException("exactly one owned export required");
      upstreamDescriptor = new byte[312];
      input.readFully(upstreamDescriptor);
      ByteBuffer d = ByteBuffer.wrap(upstreamDescriptor);
      if (!wireString(upstreamDescriptor, 256, 32).equals(bus)) throw new IOException("wrong disk");
      info = new UsbDeviceInfo();
      info.dev = new UsbIpDevice();
      info.dev.path = "/owned-qemu-usb/1-8";
      info.dev.busid = "1-8";
      d.position(288);
      info.dev.busnum = d.getInt();
      info.dev.devnum = d.getInt();
      info.dev.speed = d.getInt();
      info.dev.idVendor = d.getShort();
      info.dev.idProduct = d.getShort();
      info.dev.bcdDevice = d.getShort();
      info.dev.bDeviceClass = d.get();
      info.dev.bDeviceSubClass = d.get();
      info.dev.bDeviceProtocol = d.get();
      info.dev.bConfigurationValue = d.get();
      info.dev.bNumConfigurations = d.get();
      info.dev.bNumInterfaces = d.get();
      if ((info.dev.idVendor & 65535) != 0x46f4
          || info.dev.idProduct != 1
          || info.dev.bNumInterfaces < 0
          || info.dev.bNumInterfaces > 1)
        throw new IOException(
            "synthetic QEMU disk identity "
                + (info.dev.idVendor & 65535)
                + ":"
                + (info.dev.idProduct & 65535)
                + " interfaces="
                + info.dev.bNumInterfaces
                + " raw="
                + Arrays.toString(Arrays.copyOfRange(upstreamDescriptor, 300, 312)));
      // usbip_host resets the owned source to configuration 0 before the
      // receiving kernel enumerates it. DEVLIST then legitimately has no
      // interfaces. Preserve that metadata rather than fabricating config 1.
      info.interfaces = new UsbIpInterface[info.dev.bNumInterfaces];
      for (int i = 0; i < info.interfaces.length; i++) {
        byte[] iface = new byte[4];
        input.readFully(iface);
        if (iface[0] != 8) throw new IOException("storage only");
        UsbIpInterface it = new UsbIpInterface();
        it.bInterfaceClass = iface[0];
        it.bInterfaceSubClass = iface[1];
        it.bInterfaceProtocol = iface[2];
        info.interfaces[i] = it;
      }
    }
    byte[] nonce = new byte[16];
    new java.security.SecureRandom().nextBytes(nonce);
    long handle = NativeManagement.create(nonce);
    if (handle <= 0) throw new IOException("native management allocation");
    management =
        new ManagedImports(
            new ManagedImports.Model() {
              public byte[] execute(int action, byte[] input) {
                return NativeManagement.execute(handle, action, input);
              }

              public void close() {
                NativeManagement.close(handle);
              }
            },
            (socket, request) ->
                channels.containsKey(socket)
                    ? inventory.bindEjection(device.getDeviceId(), generation, socket, request)
                    : null);
  }

  private static String wireString(byte[] bytes, int offset, int length) {
    int n = 0;
    while (n < length && bytes[offset + n] != 0) n++;
    return new String(bytes, offset, n, StandardCharsets.US_ASCII);
  }

  private Socket connect() throws IOException {
    Socket socket = new Socket();
    try {
      socket.connect(new InetSocketAddress("10.0.2.2", backendPort), 5000);
      socket.setSoTimeout(10000);
      socket.setTcpNoDelay(true);
      return socket;
    } catch (IOException error) {
      socket.close();
      throw error;
    }
  }

  public List<UsbDeviceInfo> getDevices() {
    UsbInventory.Item item = inventory.item(device.getDeviceId());
    return item != null && item.generation == generation && item.selected && item.ready
        ? Collections.singletonList(info)
        : Collections.emptyList();
  }

  public UsbDeviceInfo getDeviceByBusId(String id) {
    return id.equals("1-8") && !getDevices().isEmpty() ? info : null;
  }

  public UsbDeviceInfo getAttachedDevice(Socket socket) {
    return channels.containsKey(socket) ? info : null;
  }

  public ManagementAccess management() {
    return management;
  }

  public synchronized boolean attachToDevice(Socket socket, String id) {
    if (getDeviceByBusId(id) == null
        || !channels.isEmpty()
        || !inventory.acquire(device.getDeviceId(), generation, socket)) return false;
    try {
      Channel channel = new Channel(socket);
      channels.put(socket, channel);
      attached.incrementAndGet();
      inventory.state(device.getDeviceId(), generation, socket, "Подключено к компьютеру");
      channel.reader.start();
      return true;
    } catch (Exception error) {
      failure = error;
      inventory.released(device.getDeviceId(), generation, socket);
      return false;
    }
  }

  public void detachFromDevice(Socket socket, String id) {
    server.killClient(socket);
  }

  public void submitUrbRequest(Socket socket, UsbIpSubmitUrb request) {
    ByteBuffer b = ByteBuffer.allocate(48);
    b.putInt(1)
        .putInt(request.seqNum)
        .putInt(request.devId)
        .putInt(request.direction)
        .putInt(request.ep)
        .putInt(request.transferFlags)
        .putInt(request.transferBufferLength)
        .putInt(request.startFrame)
        .putInt(request.numberOfPackets)
        .putInt(request.interval)
        .put(request.setup);
    send(socket, b.array(), request.outData);
  }

  public void abortUrbRequest(Socket socket, UsbIpUnlinkUrb request) {
    ByteBuffer b = ByteBuffer.allocate(48);
    b.putInt(2)
        .putInt(request.seqNum)
        .putInt(request.devId)
        .putInt(request.direction)
        .putInt(request.ep)
        .putInt(request.seqNumToUnlink);
    send(socket, b.array(), null);
  }

  private void send(Socket socket, byte[] header, byte[] data) {
    Channel channel = channels.get(socket);
    try {
      if (channel == null) throw new IOException("missing owned backend");
      synchronized (channel) {
        int seq = ByteBuffer.wrap(header).getInt(4);
        if (channel.pending.size() >= 128 || channel.pending.putIfAbsent(seq, header) != null)
          throw new IOException("bounded exact sequence map");
        channel.output.write(header);
        if (data != null) channel.output.write(data);
        submitted.incrementAndGet();
      }
    } catch (IOException error) {
      failure = error;
      server.killClient(socket);
    }
  }

  public void cleanupSocket(Socket socket) {
    Channel channel = channels.remove(socket);
    if (channel != null) channel.close();
    inventory.released(device.getDeviceId(), generation, socket);
  }

  public void close() {
    server.stop();
    for (Socket socket : new ArrayList<>(channels.keySet())) cleanupSocket(socket);
    management.close();
  }

  private final class Channel implements AutoCloseable {
    final Socket owner, upstream;
    final OutputStream output;
    final Map<Integer, byte[]> pending = new ConcurrentHashMap<>();
    final Thread reader;
    volatile boolean closing;

    Channel(Socket owner) throws IOException {
      this.owner = owner;
      upstream = connect();
      try {
        output = upstream.getOutputStream();
        byte[] importRequest = new byte[40];
        ByteBuffer.wrap(importRequest).putShort((short) 0x111).putShort((short) 0x8003).putInt(0);
        byte[] bus = backendBus.getBytes(StandardCharsets.US_ASCII);
        System.arraycopy(bus, 0, importRequest, 8, bus.length);
        output.write(importRequest);
        DataInputStream in = new DataInputStream(upstream.getInputStream());
        byte[] header = new byte[8];
        in.readFully(header);
        ByteBuffer h = ByteBuffer.wrap(header);
        if (h.getShort() != 0x111 || h.getShort() != 3 || h.getInt() != 0)
          throw new IOException("backend import");
        byte[] descriptor = new byte[312];
        in.readFully(descriptor);
        if (!Arrays.equals(descriptor, upstreamDescriptor))
          throw new IOException("backend changed");
        upstream.setSoTimeout(0);
        reader = new Thread(() -> read(in), "owned-qemu-replies");
        reader.setDaemon(true);
      } catch (IOException error) {
        upstream.close();
        throw error;
      }
    }

    private void read(DataInputStream input) {
      try {
        while (!closing) {
          byte[] header = new byte[48];
          input.readFully(header);
          ByteBuffer h = ByteBuffer.wrap(header);
          int op = h.getInt(), seq = h.getInt();
          byte[] request = pending.remove(seq);
          if (request == null
              || op != ByteBuffer.wrap(request).getInt() + 2
              || h.getInt() != 0
              || h.getInt() != 0
              || h.getInt() != 0) throw new IOException("backend completion identity");
          int size = op == 3 ? h.getInt(24) : 0;
          if (size < 0 || size > ByteBuffer.wrap(request).getInt(24) || size > 1048576)
            throw new IOException("backend completion length");
          if (op == 4 && h.getInt(20) == -104) pending.remove(ByteBuffer.wrap(request).getInt(20));
          System.arraycopy(request, 8, header, 8, 12);
          int body = op == 3 && ByteBuffer.wrap(request).getInt(12) == 1 ? size : 0;
          byte[] reply = Arrays.copyOf(header, 48 + body);
          input.readFully(reply, 48, body);
          server.send(owner, reply);
          completed.incrementAndGet();
        }
      } catch (IOException error) {
        if (!closing && !owner.isClosed()) {
          failure = error;
          server.killClient(owner);
        }
      }
    }

    public void close() {
      closing = true;
      try {
        upstream.close();
      } catch (IOException ignored) {
      }
      if (Thread.currentThread() != reader) {
        try {
          reader.join(3000);
        } catch (InterruptedException e) {
          Thread.currentThread().interrupt();
        }
      }
      pending.clear();
    }
  }
}
