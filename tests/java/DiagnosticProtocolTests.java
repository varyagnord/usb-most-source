import java.net.*;
import java.io.*;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.*;
import java.util.concurrent.atomic.AtomicInteger;
import org.cgutman.usbip.server.*;
import org.cgutman.usbip.server.protocol.cli.DiagnosticRequest;
import org.cgutman.usbip.server.protocol.dev.*;

public final class DiagnosticProtocolTests {
  static int checks;
  static void check(boolean value, String why) { checks++; if (!value) throw new AssertionError(why); }
  static final class Access implements DiagnosticAccess {
    int mode;
    boolean active;
    int calls;
    public byte[] snapshot() { return ("{\"mode\":" + mode + "}").getBytes(StandardCharsets.UTF_8); }
    public boolean selectBackend(int mode) { calls++; if (active) return false; this.mode = mode; return true; }
    public boolean clear() { calls++; return !active; }
    public byte[] selftest() { calls++; return "{\"success\":true}".getBytes(StandardCharsets.UTF_8); }
  }
  static final class Handler implements UsbRequestHandler {
    final AtomicInteger usbCalls = new AtomicInteger();
    public List<UsbDeviceInfo> getDevices() { return Collections.emptyList(); }
    public UsbDeviceInfo getDeviceByBusId(String id) { return null; }
    public UsbDeviceInfo getAttachedDevice(Socket socket) { return null; }
    public boolean attachToDevice(Socket socket, String id) { usbCalls.incrementAndGet(); return false; }
    public void detachFromDevice(Socket socket, String id) { usbCalls.incrementAndGet(); }
    public void submitUrbRequest(Socket socket, UsbIpSubmitUrb r) { usbCalls.incrementAndGet(); }
    public void abortUrbRequest(Socket socket, UsbIpUnlinkUrb r) { usbCalls.incrementAndGet(); }
    public void cleanupSocket(Socket socket) {}
  }
  static byte[] packet(short code, Integer mode) {
    ByteBuffer b = ByteBuffer.allocate(mode == null ? 8 : 12);
    b.putShort((short) 0x111).putShort(code).putInt(0);
    if (mode != null) b.putInt(mode);
    return b.array();
  }
  static int query(short code, Integer mode, String expected) throws IOException {
    try (Socket s = new Socket("127.0.0.1", 3240)) {
      s.setSoTimeout(2000); s.getOutputStream().write(packet(code, mode));
      DataInputStream in = new DataInputStream(s.getInputStream());
      check(in.readUnsignedShort() == 0x111, "reply version");
      check(in.readUnsignedShort() == (code & 0x7fff), "reply code");
      int status = in.readInt(), length = in.readInt();
      check(length > 0 && length <= DiagnosticAccess.MAX_BYTES, "bounded body");
      byte[] body = new byte[length]; in.readFully(body);
      check(new String(body, StandardCharsets.UTF_8).equals(expected), "snapshot frame exact");
      return status;
    }
  }
  static void rejected(byte[] request, String source) throws IOException {
    try (Socket s = new Socket()) {
      s.bind(new InetSocketAddress(source, 0));
      s.connect(new InetSocketAddress("127.0.0.1", 3240), 1000);
      s.setSoTimeout(2000);
      try {
        s.getOutputStream().write(request);
        check(s.getInputStream().read() == -1, "request rejected without response");
      } catch (SocketException reset) { check(true, "rejected with reset"); }
    }
  }
  public static void main(String[] args) throws Exception {
    Handler handler = new Handler();
    UsbIpServer production = new UsbIpServer(message -> {});
    production.start(handler, "127.0.0.1");
    try { for (short code : new short[] {DiagnosticRequest.GET, DiagnosticRequest.BACKEND, DiagnosticRequest.CLEAR, DiagnosticRequest.SELFTEST}) rejected(packet(code, null), "127.0.0.1"); }
    finally { production.stop(); }
    Access access = new Access();
    UsbIpServer diagnostic = new UsbIpServer(message -> {}, access);
    diagnostic.start(handler, "127.0.0.1");
    try {
      check(query(DiagnosticRequest.GET, null, "{\"mode\":0}") == 0, "diagnostic get");
      check(query(DiagnosticRequest.BACKEND, 1, "{\"mode\":1}") == 0, "select SDK");
      access.active = true;
      check(query(DiagnosticRequest.BACKEND, 0, "{\"mode\":1}") == 1, "busy backend unchanged");
      check(query(DiagnosticRequest.CLEAR, null, "{\"mode\":1}") == 1, "busy clear rejected");
      access.active = false;
      check(query(DiagnosticRequest.CLEAR, null, "{\"mode\":1}") == 0, "clear accepted without device I/O");
      check(query(DiagnosticRequest.SELFTEST, null, "{\"success\":true}") == 0, "selftest response");
      int before = access.calls;
      rejected(packet(DiagnosticRequest.BACKEND, 2), "127.0.0.1");
      byte[] badVersion = packet(DiagnosticRequest.BACKEND, 0); badVersion[1] = 0x12;
      rejected(badVersion, "127.0.0.1");
      rejected(packet(DiagnosticRequest.BACKEND, 0), "127.0.0.2");
      check(access.calls == before && access.mode == 1, "invalid requests cannot mutate backend");
      check(handler.usbCalls.get() == 0, "diagnostic commands never call USB handler");
    } finally { diagnostic.stop(); }
    System.out.println("PASS diagnostic protocol gating/framing/peer isolation: " + checks + " checks");
  }
}
