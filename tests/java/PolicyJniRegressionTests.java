import java.io.*;
import java.net.*;
import java.nio.*;
import java.nio.charset.StandardCharsets;
import java.util.*;
import java.util.concurrent.*;
import org.cgutman.usbip.jni.NativePolicy;
import org.cgutman.usbip.server.*;
import org.cgutman.usbip.server.protocol.*;
import org.cgutman.usbip.server.protocol.dev.*;

/** Real Rust JNI policy + real TCP protocol; synthetic devices, not USB hardware. */
public final class PolicyJniRegressionTests {
  private static int checks;
  private static long model;

  static int[] selection(int action, int[] values) {
    return NativePolicy.selection(model, action, values);
  }

  static void check(boolean yes, String label) {
    if (!yes) throw new AssertionError(label);
    checks++;
  }

  static boolean selected(int id) {
    int[] s = selection(3, new int[0]);
    for (int i = 1; i < s.length; i++) if (s[i] == id) return true;
    return false;
  }

  static byte[] header(int op) {
    return ByteBuffer.allocate(8).putShort((short) 0x111).putShort((short) op).putInt(0).array();
  }

  static Socket connect() throws Exception {
    Socket s = new Socket("127.0.0.1", 3240);
    s.setSoTimeout(3000);
    return s;
  }

  static int list() throws Exception {
    try (Socket s = connect()) {
      s.getOutputStream().write(header(0x8005));
      byte[] reply = s.getInputStream().readAllBytes();
      check(reply.length >= 12, "DEVLIST framed");
      int n = ByteBuffer.wrap(reply).getInt(8);
      check(reply.length == 12 + n * (UsbIpDevice.WIRE_LENGTH + 4), "descriptor count exact");
      return n;
    }
  }

  static Socket attach(int id, int expected) throws Exception {
    Socket s = connect();
    byte[] busid = new byte[32];
    byte[] name = ("1-" + id).getBytes(StandardCharsets.US_ASCII);
    System.arraycopy(name, 0, busid, 0, name.length);
    s.getOutputStream().write(header(0x8003));
    s.getOutputStream().write(busid);
    DataInputStream in = new DataInputStream(s.getInputStream());
    byte[] reply = in.readNBytes(8);
    check(
        reply.length == 8 && ByteBuffer.wrap(reply).getInt(4) == expected,
        "IMPORT selected/busy policy");
    if (expected == 0)
      check(
          in.readNBytes(UsbIpDevice.WIRE_LENGTH).length == UsbIpDevice.WIRE_LENGTH,
          "import device descriptor");
    return s;
  }

  static UsbDeviceInfo device(int id, int cls) {
    UsbDeviceInfo info = new UsbDeviceInfo();
    UsbIpDevice d = new UsbIpDevice();
    d.path = "/dev/bus/usb/001/" + id;
    d.busid = "1-" + id;
    d.busnum = 1;
    d.devnum = id;
    d.speed = cls == 3 ? 1 : 3;
    d.idVendor = 0x1234;
    d.idProduct = (short) id;
    d.bNumConfigurations = 1;
    d.bConfigurationValue = 1;
    d.bNumInterfaces = 1;
    info.dev = d;
    UsbIpInterface i = new UsbIpInterface();
    i.bInterfaceClass = (byte) cls;
    info.interfaces = new UsbIpInterface[] {i};
    return info;
  }

  public static void main(String[] args) throws Exception {
    check(
        NativePolicy.connectionCommands("10.0.0.1", 1, new int[] {1004, 1003})
            .endsWith("-b 1-3\nsudo usbip attach -r 10.0.0.1 -b 1-4"),
        "Rust commands sorted through real JNI");
    for (int[] ids : new int[][] {null, {1000}, {-1}, {1001, 1001}, new int[129]})
      check(
          NativePolicy.connectionCommands("10.0.0.1", 1, ids) == null,
          "command JNI rejects malformed ids");
    check(NativePolicy.connectionCommands(null, 1, new int[0]) == null, "command JNI null host");
    check(
        NativePolicy.connectionCommands("10.0.0.1;id", 1, new int[0]) == null,
        "no command injection");
    model = NativePolicy.selectionCreate();
    check(model > 0, "owned Rust selection model");
    long other = NativePolicy.selectionCreate();
    check(other > model, "model handles monotonic");
    NativePolicy.selection(other, 0, new int[] {777, 1});
    NativePolicy.selection(other, 2, new int[] {1});
    check(selection(3, new int[0]).length == 1, "model state isolated");
    NativePolicy.selectionClose(other);
    NativePolicy.selectionClose(other);
    long[] pool = new long[7];
    for (int i = 0; i < pool.length; i++) {
      pool[i] = NativePolicy.selectionCreate();
      check(pool[i] > 0, "model pool slot");
    }
    check(NativePolicy.selectionCreate() == 0, "model pool bounded");
    for (long id : pool) NativePolicy.selectionClose(id);
    check(NativePolicy.selection(0, 3, new int[0]) == null, "null model rejected");
    check(NativePolicy.selection(model, 0, null) == null, "null inventory rejected");
    check(NativePolicy.selection(model, 0, new int[258]) == null, "oversized inventory rejected");
    check(NativePolicy.selection(other, 3, new int[0]) == null, "stale model rejected");

    for (String h : new String[] {"10.0.0.1", "172.16.0.1", "172.31.255.254", "192.168.1.2"}) {
      check(NativePolicy.validHost(h, 1), "LAN allowed");
      check(!NativePolicy.validHost(h, 0), "mode isolation");
    }
    for (String h :
        new String[] {
          null,
          "",
          "8.8.8.8",
          "127.0.0.1",
          "0.0.0.0",
          "10.0.0.01",
          "10.0.0.1:3240",
          "host.local",
          "::1"
        })
      for (int mode : new int[] {0, 1, -1, 2})
        check(!NativePolicy.validHost(h, mode), "invalid JNI host");
    check(NativePolicy.validHost("100.64.0.1", 0), "VPN allowed");
    check(!NativePolicy.validHost("100.64.0.1", 1), "VPN not LAN");
    check(NativePolicy.describe(null) == null, "null descriptors");
    for (int n : new int[] {0, 1, 17, 65537})
      check(NativePolicy.describe(new byte[n]) == null, "descriptor bound");
    byte[] descriptors = {
      18,
      1,
      0,
      2,
      0,
      0,
      0,
      64,
      1,
      0,
      2,
      0,
      0,
      1,
      0,
      0,
      0,
      1,
      9,
      2,
      25,
      0,
      1,
      1,
      0,
      (byte) 128,
      50,
      9,
      4,
      0,
      0,
      1,
      3,
      1,
      2,
      0,
      7,
      5,
      (byte) 129,
      3,
      8,
      0,
      10
    };
    check(
        Arrays.equals(NativePolicy.describe(descriptors), new int[] {256, 1, 1, 1, 0, 0, 0, 0}),
        "mouse descriptors real JNI");
    for (int n = 0; n < descriptors.length; n++)
      check(
          NativePolicy.describe(Arrays.copyOf(descriptors, n)) == null,
          "all descriptor truncations");
    check(NativePolicy.capability(9, 1, 1, 0) == 1, "hub explained");
    check(NativePolicy.capability(0, 1, 1, 2) == 2, "ISO explained");
    check(NativePolicy.speed(-1) == 0, "invalid speed fd handled");
    byte[] realToken =
        java.nio.file.Files.readAllBytes(
            java.nio.file.Path.of("tests/usb-fixtures/rutoken-lite-descriptors.bin"));
    check(
        NativePolicy.describe(realToken) != null && NativePolicy.describe(realToken)[7] == 0,
        "Linux-accepted physical Rutoken descriptor count quirk over JNI");
    check(selection(0, new int[] {101, 1, 102, 1, 103, 0}) != null, "inventory sync");
    check(selection(0, new int[] {101, 1, 101, 1}) == null, "duplicate inventory atomic reject");
    check(selection(7, new int[0]) == null, "unknown action");
    selection(2, new int[] {1});
    check(selected(101) && selected(102) && !selected(103), "all excludes unapproved storage");
    selection(0, new int[] {101, 1, 102, 1, 103, 1});
    check(selected(103), "storage approval");
    Map<Integer, UsbDeviceInfo> inventory = new LinkedHashMap<>();
    inventory.put(101, device(101, 11));
    inventory.put(102, device(102, 3));
    inventory.put(103, device(103, 8));
    ConcurrentHashMap<Socket, Integer> imports = new ConcurrentHashMap<>();
    Set<Integer> busy = ConcurrentHashMap.newKeySet();
    UsbIpServer server = new UsbIpServer(s -> {});
    UsbRequestHandler handler =
        new UsbRequestHandler() {
          public List<UsbDeviceInfo> getDevices() {
            List<UsbDeviceInfo> out = new ArrayList<>();
            for (var e : inventory.entrySet()) if (selected(e.getKey())) out.add(e.getValue());
            return out;
          }

          public UsbDeviceInfo getDeviceByBusId(String id) {
            try {
              int n = Integer.parseInt(id.substring(2));
              return selected(n) ? inventory.get(n) : null;
            } catch (RuntimeException bad) {
              return null;
            }
          }

          public UsbDeviceInfo getAttachedDevice(Socket socket) {
            Integer id = imports.get(socket);
            return id == null ? null : inventory.get(id);
          }

          public boolean attachToDevice(Socket socket, String id) {
            UsbDeviceInfo d = getDeviceByBusId(id);
            if (d == null || !busy.add(d.dev.devnum)) return false;
            imports.put(socket, d.dev.devnum);
            return true;
          }

          public void detachFromDevice(Socket socket, String id) {
            server.killClient(socket);
          }

          public void submitUrbRequest(Socket socket, UsbIpSubmitUrb request) {
            int id = imports.get(socket);
            if (!selected(id)) {
              server.killClient(socket);
              return;
            }
            UsbIpSubmitUrbReply r =
                new UsbIpSubmitUrbReply(
                    request.seqNum, request.devId, request.direction, request.ep);
            r.actualLength = request.transferBufferLength;
            if (request.direction == 1) {
              r.inData = new byte[r.actualLength];
              Arrays.fill(r.inData, (byte) id);
            }
            server.send(socket, r.serialize());
          }

          public void abortUrbRequest(Socket socket, UsbIpUnlinkUrb r) {
            server.killClient(socket);
          }

          public void cleanupSocket(Socket socket) {
            Integer id = imports.remove(socket);
            if (id != null) busy.remove(id);
          }
        };
    server.start(handler, "127.0.0.1");
    try {
      check(list() == 3, "key, mouse, storage discovered together");
      selection(1, new int[] {102, 0});
      check(list() == 2, "partial selection changes wire DEVLIST");
      try (Socket rejected = attach(102, 1)) {
        check(rejected.getInputStream().read() == -1, "unselected device cannot import");
      }
      selection(1, new int[] {102, 1});
      try (Socket key = attach(101, 0);
          Socket mouse = attach(102, 0);
          Socket disk = attach(103, 0)) {
        check(list() == 3, "discovery works while three devices imported");
        try (Socket duplicate = attach(103, 1)) {
          check(duplicate.getInputStream().read() == -1, "duplicate device import rejected");
        }
        Socket[] sockets = {key, mouse, disk};
        int[] ids = {101, 102, 103};
        int[] sizes = {0, 1, 4, 64, 512, 65556, 131072};
        for (int i = 0; i < sockets.length; i++)
          for (int size : sizes) {
            ByteBuffer b =
                ByteBuffer.allocate(48)
                    .putInt(1)
                    .putInt(size + 1)
                    .putInt((1 << 16) | ids[i])
                    .putInt(1)
                    .putInt(1)
                    .putInt(0)
                    .putInt(size)
                    .putInt(0)
                    .putInt(0)
                    .putInt(0)
                    .putLong(0);
            sockets[i].getOutputStream().write(b.array());
            byte[] raw = new DataInputStream(sockets[i].getInputStream()).readNBytes(48 + size);
            check(raw.length == 48 + size, "multi-device exact reply size");
            check(ByteBuffer.wrap(raw).getInt(24) == size, "actual length preserved");
            for (int n = 48; n < raw.length; n++)
              check(raw[n] == (byte) ids[i], "no cross-device data mixing");
          }
        selection(1, new int[] {103, 0});
        server.killClient(disk);
        check(list() == 2, "deselect storage hides only storage");
        selection(0, new int[] {101, 1, 102, 1});
        check(!selected(103), "physical detach clears old selection");
        selection(0, new int[] {101, 1, 102, 1, 103, 1});
        check(!selected(103), "reused USB ID is not automatically shared in partial mode");
      }
    } finally {
      server.stop();
      selection(2, new int[] {0});
      selection(0, new int[0]);
    }
    NativePolicy.selectionClose(model);
    System.out.println(
        "PASS Rust policy/JNI + three synthetic devices over real TCP: " + checks + " checks");
  }
}
