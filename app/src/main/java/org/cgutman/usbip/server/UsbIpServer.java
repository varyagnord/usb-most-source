package org.cgutman.usbip.server;

import java.io.IOException;
import java.io.PushbackInputStream;
import java.net.*;
import java.util.Arrays;
import java.util.Map;
import java.util.concurrent.*;
import java.util.concurrent.atomic.AtomicBoolean;
import org.cgutman.usbip.server.protocol.ProtoDefs;
import org.cgutman.usbip.server.protocol.cli.*;
import org.cgutman.usbip.server.protocol.dev.*;

/** Bounded connections/writers, exact framing and independent receive/cancel path. */
public final class UsbIpServer {
  public static final int PORT = 3240;
  public static final int MAX_CLIENTS = 8; // Four imports plus discovery/control connections.
  private final ConcurrentHashMap<Socket, Client> clients = new ConcurrentHashMap<>();
  private final Object lifecycle = new Object();
  private final ScheduledThreadPoolExecutor timer = new ScheduledThreadPoolExecutor(1);
  private volatile ServerSocket listener;
  private volatile boolean stopped;
  private UsbRequestHandler handler;
  private String allowedHost;
  private byte[] localProof;
  private ManagementAccess management;
  private final java.util.function.Consumer<String> log;
  private final DiagnosticAccess diagnostics;

  public UsbIpServer(java.util.function.Consumer<String> log) {
    this(log, null);
  }

  public UsbIpServer(java.util.function.Consumer<String> log, DiagnosticAccess diagnostics) {
    this.log = log;
    this.diagnostics = diagnostics;
    timer.setRemoveOnCancelPolicy(true);
  }

  private final class Client {
    final Socket socket;
    final BlockingQueue<byte[]> writes = new ArrayBlockingQueue<>(4096);
    final AtomicBoolean ended = new AtomicBoolean();
    volatile boolean finish;
    volatile boolean wan;
    final java.util.concurrent.atomic.AtomicLong bytesQueued =
        new java.util.concurrent.atomic.AtomicLong();
    volatile java.util.function.Consumer<byte[]> writeObserver;
    final Thread writer;

    Client(Socket s) {
      socket = s;
      writer =
          new Thread(
              () -> {
                try {
                  while (!ended.get()) {
                    byte[] bytes = writes.poll(200, TimeUnit.MILLISECONDS);
                    if (bytes == null) {
                      if (finish) break;
                      else continue;
                    }
                    ScheduledFuture<?> timeout =
                        timer.schedule(() -> killClient(socket), 10, TimeUnit.SECONDS);
                    try {
                      socket.getOutputStream().write(bytes);
                      java.util.function.Consumer<byte[]> observer = writeObserver;
                      if (observer != null) {
                        try {
                          observer.accept(bytes);
                        } catch (RuntimeException error) {
                          log.accept("diagnostic_observer_failed");
                        }
                      }
                    } finally {
                      timeout.cancel(false);
                      bytesQueued.addAndGet(-bytes.length);
                      Arrays.fill(bytes, (byte) 0);
                    }
                    if (finish && writes.isEmpty()) break;
                  }
                } catch (IOException | InterruptedException e) {
                  Thread.currentThread().interrupt();
                } finally {
                  killClient(socket);
                }
              },
              "usbip-writer");
    }

    synchronized void enqueue(byte[] b) {
      long count = bytesQueued.addAndGet(b.length);
      if (ended.get()
          || count > 16L * 1024 * 1024
          || (!wan && writes.size() >= 128)
          || !writes.offer(b)) {
        bytesQueued.addAndGet(-b.length);
        Arrays.fill(b, (byte) 0);
        killClient(socket);
      }
    }
  }

  public void send(Socket s, byte[] bytes) {
    Client c = clients.get(s);
    if (c != null) c.enqueue(bytes);
  }

  public void observeWrites(Socket socket, java.util.function.Consumer<byte[]> observer) {
    Client client = clients.get(socket);
    if (client != null) client.writeObserver = observer;
  }

  public void finishClient(Socket s) {
    Client c = clients.get(s);
    if (c != null) {
      c.finish = true;
      timer.schedule(() -> killClient(s), 2, TimeUnit.SECONDS);
    }
  }

  public void killClient(Socket s) {
    Client c = clients.get(s);
    if (c != null)
      synchronized (c) {
        if (!c.ended.compareAndSet(false, true)) return;
        try {
          s.close();
        } catch (IOException ignored) {
        }
        c.writer.interrupt();
        byte[] b;
        while ((b = c.writes.poll()) != null) {
          c.bytesQueued.addAndGet(-b.length);
          Arrays.fill(b, (byte) 0);
        }
      }
  }

  public void start(UsbRequestHandler h, String allowedHost) throws IOException {
    startInternal(h, allowedHost, null);
  }

  /** An additional private endpoint for embedded SSH/TLS; never a LAN allowlist bypass. */
  public int startLocal(UsbRequestHandler handler, byte[] proof) throws IOException {
    if (proof == null || proof.length != 32) throw new IOException("invalid_local_proof");
    startInternal(handler, "127.0.0.1", proof.clone());
    return listener.getLocalPort();
  }

  private void startInternal(UsbRequestHandler h, String allowedHost, byte[] proof)
      throws IOException {
    this.localProof = proof;
    this.management = proof == null ? null : h.management();
    this.handler = h;
    this.allowedHost = allowedHost;
    listener = new ServerSocket();
    listener.setReuseAddress(true);
    listener.bind(
        proof == null ? new InetSocketAddress(PORT) : new InetSocketAddress("127.0.0.1", 0),
        MAX_CLIENTS);
    new Thread(
            () -> {
              while (!stopped) {
                try {
                  Socket s = listener.accept();
                  synchronized (lifecycle) {
                    if (stopped
                        || !s.getInetAddress().getHostAddress().equals(this.allowedHost)
                        || clients.values().stream().filter(client -> !client.ended.get()).count()
                            >= MAX_CLIENTS) {
                      s.close();
                      continue;
                    }
                    s.setTcpNoDelay(true);
                    s.setKeepAlive(true);
                    s.setSoTimeout(30_000);
                    Client c = new Client(s);
                    clients.put(s, c);
                    c.writer.start();
                    new Thread(() -> serve(c), "usbip-receiver").start();
                  }
                } catch (IOException e) {
                  if (!stopped) log.accept("listener_error");
                }
              }
            },
            "usbip-listener")
        .start();
  }

  private void serve(Client c) {
    Socket s = c.socket;
    byte[] managedKey = null;
    boolean managedBound = false;
    try {
      if (localProof != null) {
        byte[] header = new byte[40];
        try {
          s.setSoTimeout(5_000);
          new java.io.DataInputStream(s.getInputStream()).readFully(header);
          byte[] magic = new byte[] {'U', 'M', 'L', 'O', 'C', 'A', 'L', '1'};
          if (!java.util.Arrays.equals(magic, java.util.Arrays.copyOf(header, 8))
              || !java.security.MessageDigest.isEqual(
                  localProof, java.util.Arrays.copyOfRange(header, 8, 40)))
            throw new IOException("invalid_local_proof");
          s.getOutputStream().write(1);
          s.setSoTimeout(30_000);
        } finally {
          java.util.Arrays.fill(header, (byte) 0);
        }
      }
      CommonPacket p =
          CommonPacket.read(s.getInputStream(), diagnostics != null, management != null);
      if (p == null || p.status != 0) throw new IOException("invalid_handshake");
      if (p instanceof ManagedRequest && p.code != ManagedRequest.IMPORT) {
        ManagedRequest request = (ManagedRequest) p;
        byte[] body =
            request.code == ManagedRequest.CAPS
                ? management.capabilities()
                : management.synchronize(request.body);
        int size = request.code == ManagedRequest.CAPS ? 32 : 40;
        if (body == null || body.length != size) throw new IOException("invalid_managed_metadata");
        send(
            s,
            java.nio.ByteBuffer.allocate(8 + size)
                .putShort((short) 0x111)
                .putShort((short) (p.code & 0x7fff))
                .putInt(0)
                .put(body)
                .array());
        finishClient(s);
        while (!s.isClosed() && !stopped) Thread.sleep(20);
        return;
      }
      if (p instanceof DiagnosticRequest) {
        DiagnosticRequest request = (DiagnosticRequest) p;
        boolean success = true;
        if (request.code == DiagnosticRequest.BACKEND)
          success = diagnostics.selectBackend(request.mode);
        if (request.code == DiagnosticRequest.CLEAR) success = diagnostics.clear();
        byte[] body =
            request.code == DiagnosticRequest.SELFTEST
                ? diagnostics.selftest()
                : diagnostics.snapshot();
        if (body == null || body.length > DiagnosticAccess.MAX_BYTES)
          throw new IOException("diagnostic_size");
        java.nio.ByteBuffer reply = java.nio.ByteBuffer.allocate(12 + body.length);
        reply
            .putShort(p.version)
            .putShort((short) (p.code & 0x7fff))
            .putInt(success ? 0 : 1)
            .putInt(body.length)
            .put(body);
        send(s, reply.array());
        finishClient(s);
        while (!s.isClosed() && !stopped) Thread.sleep(20);
        return;
      }
      if (p.code == WanImportRequest.CAPS) {
        send(
            s,
            java.nio.ByteBuffer.allocate(24)
                .putShort((short) 0x111)
                .putShort((short) 0xdf)
                .putInt(0)
                .putInt(0x57414e31)
                .putInt(2048)
                .putInt(32)
                .putInt(1)
                .array());
        finishClient(s);
        while (!s.isClosed() && !stopped) Thread.sleep(20);
        return;
      }
      if (p.code == ProtoDefs.OP_REQ_DEVLIST) {
        DevListReply out = new DevListReply(p.version);
        out.devInfoList = handler.getDevices();
        send(s, out.serialize());
        finishClient(s);
        while (!s.isClosed() && !stopped) Thread.sleep(20);
        return;
      }
      boolean managed = p instanceof ManagedRequest;
      String busid;
      boolean wan;
      if (managed) {
        byte[] validated = management.validateImport(((ManagedRequest) p).body);
        if (validated == null || validated.length != 52)
          throw new IOException("invalid_managed_import");
        int end = 0;
        while (end < 32 && validated[end] != 0) end++;
        busid = new String(validated, 0, end, java.nio.charset.StandardCharsets.US_ASCII);
        wan = java.nio.ByteBuffer.wrap(validated).getInt(32) == 1;
      } else {
        if (!(p instanceof ImportDeviceRequest)) throw new IOException("invalid_import");
        busid = ((ImportDeviceRequest) p).busid;
        wan = p instanceof WanImportRequest;
      }
      ImportDeviceReply reply = new ImportDeviceReply(p.version);
      if (managed) reply.code = (short) 0xe2;
      else if (wan) reply.code = (short) 0xe0;
      c.wan = wan;
      boolean supported = !wan || handler.supportsWan(busid);
      if (managed && supported) managedKey = management.reserve();
      boolean attached =
          supported
              && (!managed || managedKey != null && managedKey.length == 24)
              && handler.attachToDevice(s, busid, wan);
      if (attached) reply.devInfo = handler.getAttachedDevice(s);
      if (managed && attached && reply.devInfo != null) {
        managedBound = management.attached(s, managedKey);
        if (!managedBound) reply.devInfo = null;
      }
      reply.status =
          !supported ? 95 : attached && reply.devInfo != null ? ProtoDefs.ST_OK : ProtoDefs.ST_NA;
      byte[] handshake = reply.serialize();
      if (managed && reply.status == 0) {
        handshake = java.util.Arrays.copyOf(handshake, handshake.length + 24);
        System.arraycopy(managedKey, 0, handshake, handshake.length - 24, 24);
      }
      if (wan && reply.status == 0) {
        handshake = java.util.Arrays.copyOf(handshake, handshake.length + 16);
        java.nio.ByteBuffer.wrap(handshake, handshake.length - 16, 16)
            .putInt(0x57414e31)
            .putInt(2048)
            .putInt(32)
            .putInt(1);
      }
      send(s, handshake);
      if (reply.status != ProtoDefs.ST_OK) {
        finishClient(s);
        while (!s.isClosed() && !stopped) Thread.sleep(20);
        return;
      }
      s.setSoTimeout(0); // Idle interrupt reads are normal, never reconnect just for silence.
      PushbackInputStream input = new PushbackInputStream(s.getInputStream(), 1);
      while (!s.isClosed()) {
        int first = input.read(); // Unlimited idle; a partially received frame is bounded.
        if (first < 0) break;
        input.unread(first);
        ScheduledFuture<?> frameTimeout = timer.schedule(() -> killClient(s), 15, TimeUnit.SECONDS);
        UsbIpDevicePacket request;
        try {
          request = UsbIpDevicePacket.read(input, wan);
        } finally {
          frameTimeout.cancel(false);
        }
        if (request instanceof WanControl) handler.wanControl(s, (WanControl) request);
        else if (request instanceof UsbIpSubmitUrb)
          handler.submitUrbRequest(s, (UsbIpSubmitUrb) request);
        else if (request instanceof UsbIpUnlinkUrb)
          handler.abortUrbRequest(s, (UsbIpUnlinkUrb) request);
        else throw new IOException("invalid_command");
      }
    } catch (IOException | InterruptedException | RuntimeException e) {
      log.accept("connection_end " + e.getClass().getSimpleName());
    } finally {
      killClient(s);
      try {
        handler.cleanupSocket(s);
      } finally {
        try {
          if (managedKey != null) {
            if (managedBound) management.released(managedKey);
            else management.abandon(managedKey);
          }
        } finally {
          clients.remove(s);
        }
      }
    }
  }

  public void stop() {
    // Registration and shutdown share a gate: accept() may return after stop
    // has begun, but that socket must never escape the shutdown snapshot.
    synchronized (lifecycle) {
      stopped = true;
      if (localProof != null) Arrays.fill(localProof, (byte) 0);
      try {
        if (listener != null) listener.close();
      } catch (IOException ignored) {
      }
      for (Map.Entry<Socket, Client> e : clients.entrySet()) killClient(e.getKey());
      timer.shutdownNow();
    }
  }
}
