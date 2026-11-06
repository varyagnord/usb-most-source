import java.io.*;
import java.net.*;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.*;
import java.util.concurrent.*;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.function.BooleanSupplier;
import org.cgutman.usbip.core.ManagedImports;
import org.cgutman.usbip.jni.NativeManagement;
import org.cgutman.usbip.server.*;
import org.cgutman.usbip.server.protocol.*;
import org.cgutman.usbip.server.protocol.cli.*;
import org.cgutman.usbip.server.protocol.dev.*;

/** Real Rust/JNI + authenticated loopback server, synthetic lease adapter, no USB/disk access. */
public final class ManagedServerRegressionTests {
  private static int checks;

  private static void check(boolean yes, String label) {
    checks++;
    if (!yes) throw new AssertionError(label);
  }

  private static byte[] header(int operation) {
    return ByteBuffer.allocate(8)
        .putShort((short) 0x111)
        .putShort((short) operation)
        .putInt(0)
        .array();
  }

  private static byte[] report(byte[] key, long sequence, int state) {
    return ByteBuffer.allocate(40).put(key).putLong(sequence).putInt(state).putInt(0).array();
  }

  private static byte[] read(Socket socket, int size) throws IOException {
    byte[] bytes = new byte[size];
    new DataInputStream(socket.getInputStream()).readFully(bytes);
    return bytes;
  }

  private static byte[] remaining(InputStream input) throws IOException {
    ByteArrayOutputStream output = new ByteArrayOutputStream();
    byte[] buffer = new byte[512];
    for (int count; (count = input.read(buffer)) >= 0; ) output.write(buffer, 0, count);
    return output.toByteArray();
  }

  private static void await(BooleanSupplier condition, String label) throws Exception {
    long deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(4);
    while (!condition.getAsBoolean() && System.nanoTime() < deadline) Thread.sleep(5);
    check(condition.getAsBoolean(), label);
  }

  private static final class Lease implements ManagedImports.Lease {
    volatile boolean current = true, can, applied;
    volatile int state = -1;
    volatile BooleanSupplier request;
    final Socket socket;
    final UsbIpServer server;

    Lease(Socket socket, UsbIpServer server) {
      this.socket = socket;
      this.server = server;
    }

    public boolean current() {
      return current;
    }

    public void state(int state, boolean can) {
      this.state = state;
      this.can = can;
    }

    public boolean applySafe() {
      if (!current) return false;
      applied = true;
      state = 2;
      can = false;
      if (server != null) server.killClient(socket);
      return true;
    }
  }

  private static final class Fixture implements AutoCloseable, UsbRequestHandler {
    final UsbIpServer server = new UsbIpServer(event -> {});
    final byte[] proof = new byte[32], nonce = new byte[16];
    final ConcurrentHashMap<Socket, Lease> active = new ConcurrentHashMap<>();
    final List<Lease> leases = new CopyOnWriteArrayList<>();
    final AtomicInteger claims = new AtomicInteger();
    final ManagedImports management;
    final int port;
    volatile boolean allowBind = true;

    Fixture(boolean secure, long deadline) throws Exception {
      new java.security.SecureRandom().nextBytes(proof);
      new java.security.SecureRandom().nextBytes(nonce);
      long model = NativeManagement.create(nonce);
      check(model > 0, "model allocated");
      management =
          new ManagedImports(
              new ManagedImports.Model() {
                public byte[] execute(int action, byte[] bytes) {
                  return NativeManagement.execute(model, action, bytes);
                }

                public void close() {
                  NativeManagement.close(model);
                }
              },
              (socket, request) -> {
                Lease lease = active.get(socket);
                if (!allowBind || lease == null) return null;
                lease.request = request;
                return lease;
              },
              deadline);
      if (secure) port = server.startLocal(this, proof);
      else {
        server.start(this, "127.0.0.1");
        port = 3240;
      }
    }

    public ManagementAccess management() {
      return management;
    }

    public List<UsbDeviceInfo> getDevices() {
      return Collections.emptyList();
    }

    public UsbDeviceInfo getDeviceByBusId(String id) {
      if (!id.equals("1-7")) return null;
      UsbDeviceInfo info = new UsbDeviceInfo();
      info.dev = new UsbIpDevice();
      info.dev.busid = id;
      info.dev.path = "/synthetic";
      info.dev.busnum = 1;
      info.dev.devnum = 7;
      info.dev.speed = 3;
      info.dev.idVendor = (short) 0xcafe;
      info.dev.idProduct = 2;
      info.interfaces = new UsbIpInterface[0];
      return info;
    }

    public UsbDeviceInfo getAttachedDevice(Socket socket) {
      return active.containsKey(socket) ? getDeviceByBusId("1-7") : null;
    }

    public boolean supportsWan(String id) {
      return id.equals("1-7");
    }

    public boolean attachToDevice(Socket socket, String id) {
      return attachToDevice(socket, id, false);
    }

    public boolean attachToDevice(Socket socket, String id, boolean wan) {
      if (getDeviceByBusId(id) == null) return false;
      claims.incrementAndGet();
      Lease lease = new Lease(socket, server);
      leases.add(lease);
      active.put(socket, lease);
      return true;
    }

    public void detachFromDevice(Socket socket, String id) {
      server.killClient(socket);
    }

    public void submitUrbRequest(Socket socket, UsbIpSubmitUrb request) {
      throw new AssertionError("metadata test must not send URBs");
    }

    public void abortUrbRequest(Socket socket, UsbIpUnlinkUrb request) {
      throw new AssertionError("metadata test must not send UNLINK");
    }

    public void cleanupSocket(Socket socket) {
      active.remove(socket);
    }

    Socket connect(boolean authenticate) throws Exception {
      Socket socket = new Socket("127.0.0.1", port);
      socket.setSoTimeout(3000);
      if (authenticate) {
        socket.getOutputStream().write("UMLOCAL1".getBytes(StandardCharsets.US_ASCII));
        socket.getOutputStream().write(proof);
        check(socket.getInputStream().read() == 1, "proof acknowledged");
      }
      return socket;
    }

    byte[] rpc(int operation, byte[] body) throws Exception {
      try (Socket socket = connect(true)) {
        socket.getOutputStream().write(header(operation));
        socket.getOutputStream().write(body);
        byte[] bytes = remaining(socket.getInputStream());
        if (bytes.length > 0)
          check(
              ByteBuffer.wrap(bytes).getInt(4) == 0
                  && (ByteBuffer.wrap(bytes).getShort(2) & 0xffff) == (operation & 0x7fff),
              "reply operation/status");
        return bytes;
      }
    }

    byte[] importRequest(String id, boolean wan) {
      byte[] bytes = new byte[52];
      byte[] busid = id.getBytes(StandardCharsets.US_ASCII);
      System.arraycopy(busid, 0, bytes, 0, busid.length);
      ByteBuffer.wrap(bytes).putInt(32, wan ? 1 : 0);
      System.arraycopy(nonce, 0, bytes, 36, 16);
      return bytes;
    }

    byte[] importDevice(Socket socket, boolean wan, int expected) throws Exception {
      socket.getOutputStream().write(header(0x80e2));
      socket.getOutputStream().write(importRequest("1-7", wan));
      byte[] prefix = read(socket, 8);
      check(
          ByteBuffer.wrap(prefix).getShort(2) == 0xe2
              && ByteBuffer.wrap(prefix).getInt(4) == expected,
          "managed IMPORT status");
      if (expected != 0) {
        check(socket.getInputStream().read() == -1, "failed import no suffix");
        return null;
      }
      byte[] descriptor = read(socket, 312);
      check(
          Arrays.equals(descriptor, getDeviceByBusId("1-7").dev.serialize()),
          "descriptor unchanged");
      byte[] key = read(socket, 24);
      check(Arrays.equals(Arrays.copyOf(key, 16), nonce), "key belongs to service");
      if (wan) {
        byte[] suffix = read(socket, 16);
        check(ByteBuffer.wrap(suffix).getInt() == 0x57414e31, "WAN suffix follows managed key");
      }
      return key;
    }

    public void close() {
      server.stop();
      management.close();
    }
  }

  private static void codec() throws Exception {
    for (int operation : new int[] {0x80e1, 0x80e2, 0x80e3}) {
      int size = operation == 0x80e2 ? 52 : operation == 0x80e3 ? 40 : 0;
      ByteArrayInputStream denied =
          new ByteArrayInputStream(ByteBuffer.allocate(8 + size).put(header(operation)).array());
      check(
          CommonPacket.read(denied, false) == null && denied.available() == size,
          "plain parser never reads management body");
      for (int n = 0; n < size; n++) {
        byte[] bytes = ByteBuffer.allocate(8 + n).put(header(operation)).array();
        try {
          CommonPacket.read(new ByteArrayInputStream(bytes), false, true);
          throw new AssertionError("truncated metadata accepted");
        } catch (IOException expected) {
          checks++;
        }
      }
      for (boolean status : new boolean[] {false, true}) {
        byte[] bytes = header(operation);
        if (status) bytes[7] = 1;
        else bytes[1] = 18;
        try {
          CommonPacket.read(new ByteArrayInputStream(bytes), false, true);
          throw new AssertionError("invalid managed header");
        } catch (IOException expected) {
          checks++;
        }
      }
    }
  }

  private static void wire() throws Exception {
    byte[] previous;
    try (Fixture f = new Fixture(true, 30000)) {
      try (Socket bad = f.connect(false)) {
        bad.getOutputStream().write(new byte[40]);
        check(bad.getInputStream().read() == -1, "bad proof rejects management");
      }
      byte[] caps = f.rpc(0x80e1, new byte[0]);
      check(
          caps.length == 40 && ByteBuffer.wrap(caps).getInt(8) == 0x554d4d31,
          "bounded capabilities");
      byte[] invalid = f.importRequest("1-7", false);
      invalid[36] ^= 1;
      check(
          f.rpc(0x80e2, invalid).length == 0 && f.claims.get() == 0,
          "wrong session cannot claim USB");
      try (Socket socket = f.connect(true)) {
        byte[] key = f.importDevice(socket, false, 0);
        previous = key;
        Lease lease = f.leases.get(0);
        check(!lease.request.getAsBoolean(), "not ready before receiver Idle");
        byte[] idle = report(key, 0, 0);
        byte[] wait = f.rpc(0x80e3, idle);
        check(ByteBuffer.wrap(wait).getInt(40) == 0 && lease.can, "ready after real sync");
        check(
            lease.request.getAsBoolean() && lease.state == 1, "explicit request starts pending UI");
        check(!lease.request.getAsBoolean(), "double tap does not allocate operation");
        byte[] directive = f.rpc(0x80e3, idle);
        check(
            ByteBuffer.wrap(directive).getLong(32) == 1
                && ByteBuffer.wrap(directive).getInt(40) == 1,
            "same operation on poll");
        check(Arrays.equals(directive, f.rpc(0x80e3, idle)), "lost directive is idempotent");
        check(
            f.rpc(0x80e3, report(key, 2, 2)).length == 0 && !lease.applied,
            "unsolicited future Safe rejected");
        f.rpc(0x80e3, report(key, 1, 3));
        check(
            lease.state == 3 && lease.can && !lease.applied,
            "Busy preserves lease and offers explicit retry");
        check(lease.request.getAsBoolean(), "explicit retry after Busy");
        byte[] retry = f.rpc(0x80e3, report(key, 1, 3));
        check(
            ByteBuffer.wrap(retry).getLong(32) == 2
                && ByteBuffer.wrap(retry).getInt(40) == 1
                && lease.state == 1
                && !lease.applied,
            "previous Busy delivers new explicit retry without finishing it");
        check(
            Arrays.equals(retry, f.rpc(0x80e3, report(key, 1, 3))),
            "lost retry directive is delivered again with the same sequence");
        check(
            f.rpc(0x80e3, report(key, 1, 2)).length == 0 && !lease.applied,
            "prior operation result cannot finish new one");
        byte[] safe = f.rpc(0x80e3, report(key, 2, 2));
        check(lease.applied && ByteBuffer.wrap(safe).getInt(40) == 2, "Safe applied before Retire");
        check(socket.getInputStream().read() == -1, "Safe releases only original socket");
        check(
            Arrays.equals(safe, f.rpc(0x80e3, report(key, 2, 2))),
            "lost safe acknowledgement stays Retire");
      }
      await(() -> f.active.isEmpty(), "USB session cleanup complete");
      try (Socket next = f.connect(true)) {
        byte[] key = f.importDevice(next, true, 0);
        Lease lease = f.leases.get(1);
        check(!Arrays.equals(previous, key), "new import never reuses key");
        f.rpc(0x80e3, report(previous, 2, 2));
        check(!lease.applied, "old result never affects replacement");
        f.rpc(0x80e3, report(key, 0, 0));
        check(lease.request.getAsBoolean(), "new exact request");
        next.close();
        await(() -> f.active.isEmpty(), "USB EOF precedes final OS result");
        f.rpc(0x80e3, report(key, 1, 2));
        check(lease.applied, "late Safe after EOF retained");
      }
      await(() -> f.active.isEmpty(), "late result quiescent");
      f.allowBind = false;
      try (Socket failed = f.connect(true)) {
        f.importDevice(failed, false, 1);
      }
      await(() -> f.active.isEmpty(), "failed binding cleans claimed USB");
      f.allowBind = true;
      List<Socket> sockets = new ArrayList<>();
      try {
        for (int i = 0; i < 4; i++) {
          Socket socket = f.connect(true);
          sockets.add(socket);
          f.importDevice(socket, false, 0);
        }
        int before = f.claims.get();
        try (Socket full = f.connect(true)) {
          f.importDevice(full, false, 1);
        }
        check(f.claims.get() == before, "capacity checked before USB claim");
        Lease stale = f.leases.get(f.leases.size() - 1);
        stale.current = false;
        try (Socket replaced = f.connect(true)) {
          f.importDevice(replaced, false, 0);
        }
      } finally {
        for (Socket socket : sockets) socket.close();
      }
      await(() -> f.active.isEmpty(), "all synthetic USB returned");
      // Legacy IMPORT still sends exactly its descriptor before the ordinary URB stream.
      try (Socket legacy = f.connect(true)) {
        legacy.getOutputStream().write(header(0x8003));
        legacy.getOutputStream().write(Arrays.copyOf(f.importRequest("1-7", false), 32));
        check(ByteBuffer.wrap(read(legacy, 8)).getShort(2) == 3, "legacy response code");
        read(legacy, 312);
        legacy.setSoTimeout(100);
        try {
          legacy.getInputStream().read();
          throw new AssertionError("metadata leaked into legacy stream");
        } catch (SocketTimeoutException expected) {
          checks++;
        }
      }
    }
    try (Fixture plain = new Fixture(false, 30000)) {
      try (Socket socket = plain.connect(false)) {
        socket.getOutputStream().write(header(0x80e1));
        check(
            socket.getInputStream().read() == -1,
            "plain server never advertises management even with adapter");
      }
      check(plain.claims.get() == 0, "plain management cannot claim");
    }
    try (Fixture replacement = new Fixture(true, 30000)) {
      check(
          replacement.rpc(0x80e3, report(previous, 2, 2)).length == 0,
          "previous service nonce rejected");
    }
  }

  private static void deadlineAndReplacement() throws Exception {
    try (Fixture f = new Fixture(true, 60)) {
      try (Socket socket = f.connect(true)) {
        byte[] key = f.importDevice(socket, false, 0);
        Lease lease = f.leases.get(0);
        f.rpc(0x80e3, report(key, 0, 0));
        check(lease.request.getAsBoolean(), "deadline request");
        await(() -> lease.state == 7, "UI timeout becomes uncertain without claiming safe");
        check(!lease.applied && !lease.request.getAsBoolean(), "uncertain forbids new execution");
        check(
            f.rpc(0x80e3, report(key, 0, 0)).length == 0,
            "idle peer cannot execute after uncertain delivery");
        f.rpc(0x80e3, report(key, 1, 1));
        check(lease.state == 7, "late Running does not erase uncertain");
        f.rpc(0x80e3, report(key, 1, 2));
        check(lease.applied, "real late Safe resolves uncertainty");
      }
      try (Socket socket = f.connect(true)) {
        byte[] key = f.importDevice(socket, false, 0);
        Lease lease = f.leases.get(1);
        f.rpc(0x80e3, report(key, 0, 0));
        lease.request.getAsBoolean();
        lease.current = false;
        byte[] retired = f.rpc(0x80e3, report(key, 1, 2));
        check(
            ByteBuffer.wrap(retired).getInt(40) == 2 && !lease.applied,
            "explicit selection replacement ignores late Safe");
      }
    }
  }

  public static void main(String[] args) throws Exception {
    codec();
    wire();
    deadlineAndReplacement();
    System.out.println(
        "PASS managed server + real JNI: " + checks + " checks (synthetic leases, loopback TCP)");
  }
}
