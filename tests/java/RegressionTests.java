import java.io.*;
import java.net.*;
import java.nio.*;
import java.util.*;
import java.util.concurrent.*;
import org.cgutman.usbip.core.*;
import org.cgutman.usbip.server.*;
import org.cgutman.usbip.server.protocol.dev.*;

public final class RegressionTests {
  static int checks;

  static void check(boolean value, String message) {
    checks++;
    if (!value) throw new AssertionError(message);
  }

  static byte[] wire(int seq, int dir, int length, int packets) {
    ByteBuffer b =
        ByteBuffer.allocate(48 + (dir == 0 ? Math.max(0, Math.min(length, 1048576)) : 0));
    b.putInt(1).putInt(seq).putInt(0x10003).putInt(dir).putInt(2);
    b.putInt(0).putInt(length).putInt(0).putInt(packets).putInt(0);
    return b.array();
  }

  static UsbIpSubmitUrb req(int seq, int dir, int length) throws Exception {
    return (UsbIpSubmitUrb)
        UsbIpDevicePacket.read(new ByteArrayInputStream(wire(seq, dir, length, 0)));
  }

  static UsbIpUnlinkUrb unlink(int seq, int target) {
    ByteBuffer b = ByteBuffer.allocate(20);
    b.putInt(2).putInt(seq).putInt(0x10003).putInt(0).putInt(0);
    UsbIpUnlinkUrb u = new UsbIpUnlinkUrb(b.array());
    u.seqNumToUnlink = target;
    return u;
  }

  static class Harness implements UsbBackend, TransferCoordinator.Sink {
    final List<UsbIpDevicePacket> sent = new ArrayList<>();
    final List<String> faults = new ArrayList<>();
    int submits, cancels, submitResult, cancelResult;
    TransferCoordinator c = new TransferCoordinator(this, this);

    public int submit(UsbIpSubmitUrb r, int type, int endpoint) {
      submits++;
      return submitResult;
    }

    public int cancel(int id) {
      cancels++;
      return cancelResult;
    }

    public void send(UsbIpDevicePacket p) {
      p.serialize();
      sent.add(p);
    }

    public void fault(String reason) {
      faults.add(reason);
    }

    public void event(String event) {}

    void complete(int id, int status, int n, byte[] data, long time) {
      c.completed(new Completion(id, status, n, data, false), time);
    }
  }

  static void transfers() throws Exception {
    for (int n :
        new int[] {
          0, 1, 16, 63, 64, 65, 127, 128, 129, 130, 256, 270, 271, 512, 65536, 65556, 131072,
          131073, 520192, 524288, 1048576
        }) {
      Harness h = new Harness();
      int capacity = Math.max(n, 271);
      h.c.submit(req(1, 1, capacity), 3, 130, 0);
      byte[] data = new byte[n];
      Arrays.fill(data, (byte) 0xa5);
      h.complete(1, 0, n, data, 100);
      UsbIpSubmitUrbReply r = (UsbIpSubmitUrbReply) h.sent.get(0);
      check(
          r.actualLength == n && r.serialize().length == 48 + n, "short/large exact response " + n);
      check(h.submits == 1 && h.c.pendingCount() == 0 && h.faults.isEmpty(), "no extra read");
    }
    Harness h = new Harness();
    h.c.submit(req(1, 0, 64), 3, 2, 0);
    h.complete(1, -110, 32, null, 200);
    check(
        h.submits == 1 && ((UsbIpSubmitUrbReply) h.sent.get(0)).actualLength == 32,
        "partial OUT never replayed");
    check(h.sent.get(0).serialize().length == 48, "OUT reply has no payload");
    h = new Harness();
    h.c.submit(req(1, 1, 271), 3, 130, 0);
    h.complete(1, -110, 130, new byte[130], 100);
    check(
        ((UsbIpSubmitUrbReply) h.sent.get(0)).actualLength == 130, "partial IN retained on error");
    h = new Harness();
    h.c.submit(req(1, 1, 271), 3, 130, 0);
    h.c.unlink(unlink(2, 1), 100);
    check(h.sent.isEmpty() && h.cancels == 1, "cancel ACK waits for kernel");
    h.complete(1, -104, 0, null, 101);
    check(
        h.sent.size() == 1 && h.sent.get(0) instanceof UsbIpUnlinkUrbReply,
        "no RET_SUBMIT after unlink");
    check(((UsbIpUnlinkUrbReply) h.sent.get(0)).status == -104, "unlink ECONNRESET");
    h.c.unlink(unlink(3, 1), 102);
    check(((UsbIpUnlinkUrbReply) h.sent.get(1)).status == 0, "unlink completed request");
    h = new Harness();
    h.c.submit(req(1, 1, 271), 3, 130, 0);
    h.cancelResult = -22;
    h.c.unlink(unlink(2, 1), 10);
    h.complete(1, 0, 1, new byte[1], 11);
    check(
        h.sent.size() == 2 && h.sent.get(1) instanceof UsbIpSubmitUrbReply,
        "completion wins cancel race");
    h = new Harness();
    h.c.submit(req(1, 1, 271), 3, 130, 0);
    h.c.tick(59999);
    check(h.cancels == 0, "deadline not early");
    h.c.tick(60000);
    check(h.cancels == 1, "deadline cancels once");
    h.c.tick(61000);
    check(h.cancels == 1, "no repeat cancel");
    h.complete(1, -104, 0, null, 61001);
    check(
        ((UsbIpSubmitUrbReply) h.sent.get(0)).status == -110 && !h.faults.isEmpty(),
        "deadline completes and reconnects");
    h = new Harness();
    h.c.submit(req(1, 1, 16), 1, 129, 0);
    h.c.tick(9999999);
    check(h.cancels == 0 && h.faults.isEmpty(), "idle interrupt stays alive");
    h = new Harness();
    h.c.submit(req(1, 0, 64), 3, 2, 0);
    h.c.tick(15000);
    h.c.tick(18000);
    check(h.faults.size() == 1, "stuck cancellation bounded");
    h = new Harness();
    h.c.submit(req(1, 1, 271), 3, 130, 0);
    h.cancelResult = -22;
    h.c.tick(63000);
    check(h.faults.size() == 1, "missing completion bounded");
    h = new Harness();
    h.c.submit(req(1, 1, 271), 3, 130, 0);
    h.c.submit(req(1, 1, 271), 3, 130, 0);
    check(h.submits == 1 && h.faults.size() == 1, "duplicate rejected");
    h = new Harness();
    for (int i = 0; i < 65; i++) h.c.submit(req(i, 1, 16), 1, 129, 0);
    check(h.submits == 64 && h.faults.size() == 1, "bounded queue");
    h = new Harness();
    h.submitResult = -19;
    h.c.submit(req(1, 0, 1), 3, 2, 0);
    check(h.submits == 1 && h.c.pendingCount() == 0 && h.faults.size() == 1, "disconnect submit");
    h = new Harness();
    h.c.submit(req(1, 1, 1), 3, 130, 0);
    h.complete(1, 0, 2, new byte[2], 1);
    check(h.faults.size() == 1 && h.sent.isEmpty(), "invalid completion rejected");
    h = new Harness();
    h.c.submit(req(1, 1, 1), 3, 130, 0);
    h.c.close();
    h.complete(1, 0, 1, new byte[1], 1);
    check(h.sent.isEmpty() && h.c.pendingCount() == 0, "no reply after close");
    // Exercise actual synchronization with two racing callers, 1000 times.
    for (int i = 0; i < 1000; i++) {
      Harness race = new Harness();
      race.c.submit(req(1, 1, 1), 3, 130, 0);
      Thread a = new Thread(() -> race.c.unlink(unlink(2, 1), 1));
      Thread b = new Thread(() -> race.complete(1, 0, 1, new byte[1], 1));
      a.start();
      b.start();
      a.join();
      b.join();
      long originals = race.sent.stream().filter(p -> p instanceof UsbIpSubmitUrbReply).count();
      check(
          race.faults.isEmpty() && race.c.pendingCount() == 0 && originals <= 1,
          "concurrent cancel completion");
      check(race.sent.size() == (originals == 1 ? 2 : 1), "exactly one outcome");
    }
  }

  static void framing() throws Exception {
    // Captured CryptoPro/libccid request: bulk IN, 65556 bytes, flags 0x40200.
    byte[] ccid = wire(14834, 1, 65556, -1);
    ByteBuffer.wrap(ccid).putInt(20, 0x40200);
    UsbIpSubmitUrb captured =
        (UsbIpSubmitUrb) UsbIpDevicePacket.read(new ByteArrayInputStream(ccid));
    check(
        captured.transferBufferLength == 65556 && captured.transferFlags == 0x40200,
        "physical CryptoPro CCID read accepted");
    Harness h = new Harness();
    h.c.submit(captured, 3, 130, 0);
    h.complete(14834, 0, 130, new byte[130], 1);
    h.c.submit(req(14835, 0, 15), 3, 2, 2);
    h.complete(14835, 0, 15, null, 3);
    check(
        h.faults.isEmpty() && h.sent.size() == 2 && h.submits == 2,
        "short reply to large CCID read keeps session usable");
    for (int size : new int[] {65556, 131072, 131073, 520192, 524288, 1048576})
      check(req(10, 0, size).outData.length == size, "large OUT frame accepted");
    byte[] bytes = wire(1, 0, 130, 0);
    InputStream fragmented =
        new ByteArrayInputStream(bytes) {
          public synchronized int read(byte[] b, int off, int len) {
            return super.read(b, off, Math.min(1, len));
          }
        };
    check(
        ((UsbIpSubmitUrb) UsbIpDevicePacket.read(fragmented)).outData.length == 130,
        "fragmented TCP");
    for (int length = 0; length < bytes.length; length++) {
      try {
        UsbIpDevicePacket.read(new ByteArrayInputStream(Arrays.copyOf(bytes, length)));
        throw new AssertionError("accepted truncated frame " + length);
      } catch (IOException expected) {
        checks++;
      }
    }
    for (int packets : new int[] {0, -1})
      check(
          ((UsbIpSubmitUrb)
                      UsbIpDevicePacket.read(new ByteArrayInputStream(wire(1, 1, 271, packets))))
                  .transferBufferLength
              == 271,
          "non-iso compatibility");
    for (int[] bad : new int[][] {{1, -1, 0}, {1, 1048577, 0}, {2, 0, 0}, {1, 1, 1}}) {
      try {
        UsbIpDevicePacket.read(new ByteArrayInputStream(wire(1, bad[0], bad[1], bad[2])));
        throw new AssertionError("accepted malformed frame");
      } catch (IOException expected) {
        checks++;
      }
    }
    byte[] u = new byte[48];
    ByteBuffer.wrap(u).putInt(2);
    for (int n = 20; n < 48; n++)
      try {
        UsbIpDevicePacket.read(new ByteArrayInputStream(Arrays.copyOf(u, n)));
        throw new AssertionError("unlink padding EOF");
      } catch (IOException expected) {
        checks++;
      }
  }

  static void network() throws Exception {
    java.util.concurrent.atomic.AtomicInteger cleanup =
        new java.util.concurrent.atomic.AtomicInteger();
    UsbIpServer server = new UsbIpServer(s -> {});
    UsbRequestHandler handler =
        new UsbRequestHandler() {
          public List<UsbDeviceInfo> getDevices() {
            return Collections.emptyList();
          }

          public UsbDeviceInfo getDeviceByBusId(String id) {
            return null;
          }

          public UsbDeviceInfo getAttachedDevice(Socket socket) {
            return null;
          }

          public boolean attachToDevice(Socket s, String id) {
            return false;
          }

          public void detachFromDevice(Socket s, String id) {}

          public void submitUrbRequest(Socket s, UsbIpSubmitUrb r) {}

          public void abortUrbRequest(Socket s, UsbIpUnlinkUrb r) {}

          public void cleanupSocket(Socket s) {
            if (s.getLocalPort() == 3240) cleanup.incrementAndGet();
          }
        };
    UsbIpServer local = new UsbIpServer(event -> {});
    byte[] proof = new byte[32];
    Arrays.fill(proof, (byte) 73);
    int privatePort = local.startLocal(handler, proof);
    try {
      try (Socket denied = new Socket("127.0.0.1", privatePort)) {
        denied.setSoTimeout(3000);
        denied.getOutputStream().write(new byte[40]);
        check(
            denied.getInputStream().read() == -1,
            "private endpoint rejects missing proof before USB");
      }
      try (Socket accepted = new Socket("127.0.0.1", privatePort)) {
        accepted.setSoTimeout(3000);
        accepted
            .getOutputStream()
            .write("UMLOCAL1".getBytes(java.nio.charset.StandardCharsets.US_ASCII));
        accepted.getOutputStream().write(proof);
        check(
            accepted.getInputStream().read() == 1, "private endpoint acknowledges verified proof");
        accepted.getOutputStream().write(new byte[] {1, 17, (byte) 128, 5, 0, 0, 0, 0});
        byte[] reply = new byte[12];
        new DataInputStream(accepted.getInputStream()).readFully(reply);
        check(
            Arrays.equals(reply, new byte[] {1, 17, 0, 5, 0, 0, 0, 0, 0, 0, 0, 0}),
            "private endpoint exposes existing devlist only after authentication");
      }
    } finally {
      local.stop();
    }
    server.start(handler, "127.0.0.1");
    try {
      for (int i = 0; i < 20; i++)
        try (Socket s = new Socket("127.0.0.1", 3240)) {
          s.setSoTimeout(3000);
          byte[] request =
              ByteBuffer.allocate(8)
                  .putShort((short) 0x111)
                  .putShort((short) 0x8005)
                  .putInt(0)
                  .array();
          for (byte b : request) s.getOutputStream().write(b);
          byte[] response = s.getInputStream().readAllBytes();
          check(
              response.length == 12 && ByteBuffer.wrap(response).getShort(2) == 5,
              "DEVLIST wire reply and EOF");
        }
      try (Socket s = new Socket("127.0.0.1", 3240)) {
        s.setSoTimeout(3000);
        s.getOutputStream().write(new byte[] {1, 2});
        s.shutdownOutput();
        // Wait for the server to accept/reject this truncated request before
        // stopping it. A TCP connect alone does not prove application admission.
        check(s.getInputStream().read() == -1, "truncated request retired before stop");
      }
    } finally {
      server.stop();
    }
    for (int i = 0; i < 100 && cleanup.get() < 21; i++) Thread.sleep(10);
    check(cleanup.get() == 21, "all sockets cleaned up");
    // Hold admission after accept but before registration, then stop. This
    // deterministic schedule reproduces the orphan connection seen on API26.
    UsbIpServer race = new UsbIpServer(s -> {});
    race.start(handler, "127.0.0.1");
    java.lang.reflect.Field field = UsbIpServer.class.getDeclaredField("lifecycle");
    field.setAccessible(true);
    Object gate = field.get(race);
    Socket pending = null;
    try {
      synchronized (gate) {
        pending = new Socket("127.0.0.1", 3240);
        boolean accepted = false;
        for (int i = 0; i < 200 && !accepted; i++) {
          for (Thread t : Thread.getAllStackTraces().keySet())
            if (t.getName().equals("usbip-listener") && t.getState() == Thread.State.BLOCKED)
              accepted = true;
          if (!accepted) Thread.sleep(5);
        }
        check(accepted, "race fixture paused accepted connection before registration");
        race.stop();
      }
      pending.setSoTimeout(2000);
      try {
        check(pending.getInputStream().read() == -1, "stop rejects not-yet-registered connection");
      } catch (SocketException reset) {
        checks++;
      }
      check(cleanup.get() == 21, "unadmitted socket never reaches USB handler");
    } finally {
      if (pending != null) pending.close();
      race.stop();
    }
  }

  static void importIdentity() throws Exception {
    UsbDeviceInfo original = new UsbDeviceInfo();
    original.dev = new org.cgutman.usbip.server.protocol.UsbIpDevice();
    original.dev.path = "/dev/bus/usb/001/001";
    original.dev.busid = "1-1";
    original.dev.idVendor = (short) 0x1234;
    original.interfaces = new org.cgutman.usbip.server.protocol.UsbIpInterface[0];
    java.util.concurrent.atomic.AtomicInteger snapshots =
        new java.util.concurrent.atomic.AtomicInteger();
    UsbIpServer server = new UsbIpServer(s -> {});
    server.start(
        new UsbRequestHandler() {
          public List<UsbDeviceInfo> getDevices() {
            return Collections.emptyList();
          }

          public UsbDeviceInfo getDeviceByBusId(String id) {
            throw new AssertionError("IMPORT must not re-resolve a reused bus ID");
          }

          public UsbDeviceInfo getAttachedDevice(Socket socket) {
            snapshots.incrementAndGet();
            return original;
          }

          public boolean attachToDevice(Socket socket, String id) {
            return id.equals("1-1");
          }

          public void detachFromDevice(Socket socket, String id) {}

          public void submitUrbRequest(Socket socket, UsbIpSubmitUrb request) {}

          public void abortUrbRequest(Socket socket, UsbIpUnlinkUrb request) {}

          public void cleanupSocket(Socket socket) {}
        },
        "127.0.0.1");
    try (Socket socket = new Socket("127.0.0.1", 3240)) {
      socket.setSoTimeout(3000);
      ByteBuffer request = ByteBuffer.allocate(40);
      request.putShort((short) 0x111).putShort((short) 0x8003).putInt(0);
      request.put(new byte[] {'1', '-', '1'});
      socket.getOutputStream().write(request.array());
      byte[] reply = new byte[320];
      new DataInputStream(socket.getInputStream()).readFully(reply);
      check(ByteBuffer.wrap(reply).getInt(4) == 0, "IMPORT succeeds with session snapshot");
      check(
          (ByteBuffer.wrap(reply).getShort(308) & 0xffff) == 0x1234,
          "IMPORT contains attached device identity, never replacement identity");
      check(snapshots.get() == 1, "IMPORT resolves exactly one attached snapshot");
    } finally {
      server.stop();
    }
  }

  static void configuration() {
    for (int fail = 0; fail < 4; fail++) {
      final int failure = fail;
      List<String> calls = new ArrayList<>();
      ConfigurationTransition.Operations ops =
          new ConfigurationTransition.Operations() {
            public boolean releaseInterfaces() {
              calls.add("release");
              return failure != 1;
            }

            public boolean selectConfiguration() {
              calls.add("configure");
              return failure != 2;
            }

            public boolean claimInterfaces() {
              calls.add("claim");
              return failure != 3;
            }
          };
      int result = ConfigurationTransition.apply(1, 1, 0, ops);
      check(result == (fail == 0 ? 0 : -5), "configuration failures are reported");
      check(
          calls.equals(
              Arrays.asList("release", "configure", "claim").subList(0, fail == 0 ? 3 : fail)),
          "real configuration ordering and early failure");
      calls.clear();
      check(
          ConfigurationTransition.apply(2, 1, 0, ops) == -22 && calls.isEmpty(),
          "unsupported configuration never touches USB");
      check(
          ConfigurationTransition.apply(1, 1, 1, ops) == -16 && calls.isEmpty(),
          "configuration cannot race a pending URB");
    }
  }

  static void unsignedInterfaceCounts() {
    for (int count : new int[] {0, 1, 127, 128, 255}) {
      UsbDeviceInfo info = new UsbDeviceInfo();
      info.dev = new org.cgutman.usbip.server.protocol.UsbIpDevice();
      info.dev.path = "/dev/bus/usb/001/001";
      info.dev.busid = "1-1";
      info.dev.bNumInterfaces = (byte) count;
      info.interfaces = new org.cgutman.usbip.server.protocol.UsbIpInterface[count];
      for (int n = 0; n < count; n++)
        info.interfaces[n] = new org.cgutman.usbip.server.protocol.UsbIpInterface();
      check(
          info.getWireSize() == 312 + count * 4, "unsigned interface count wire length: " + count);
      check(
          info.serialize().length == 312 + count * 4,
          "unsigned interface count serializes: " + count);
    }
  }

  public static void main(String[] args) throws Exception {
    unsignedInterfaceCounts();
    configuration();
    transfers();
    framing();
    network();
    importIdentity();
    System.out.println("PASS Java protocol/coordinator/network: " + checks + " checks");
  }
}
