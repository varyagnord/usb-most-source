import java.nio.charset.StandardCharsets;
import java.util.Arrays;
import org.cgutman.usbip.jni.NativeConnection;

/** Real JVM/JNI: bounds, lifecycle, stale handles, asynchronous failures. No USB. */
public final class ConnectionJniRegressionTests {
  private static int checks;

  private static void check(boolean value, String message) {
    checks++;
    if (!value) throw new AssertionError(message);
  }

  private static byte[] bytes(String value) {
    return value.getBytes(StandardCharsets.UTF_8);
  }

  private static long start(byte[] profile, byte[] secret) {
    return NativeConnection.start(profile, secret, 1, new byte[32], "/unused-fixture-artifacts");
  }

  private static void rejected(Runnable action) {
    try {
      action.run();
      throw new AssertionError("expected JNI exception");
    } catch (RuntimeException expected) {
      checks++;
    }
  }

  public static void main(String[] args) throws Exception {
    rejected(() -> NativeConnection.diagnose(null));
    rejected(() -> NativeConnection.diagnose(new byte[4097]));
    check(NativeConnection.diagnose(bytes("{}")) == null, "diagnosis requires full bounded schema");
    String observation = "{\"schema\":1,\"configured\":true,\"transport\":\"ssh\","
        + "\"running\":true,\"network\":true,\"phase\":\"connected\",\"receiver_verified\":true,"
        + "\"failure\":null,\"usb\":{\"total\":1,\"selected\":1,\"ready\":1,\"in_use\":0,"
        + "\"permission_missing\":0,\"consent_missing\":0,\"unsupported\":0}}";
    String report = NativeConnection.diagnose(bytes(observation));
    check(report.contains("\"summary\":\"waiting_for_import\""), "real Rust diagnosis through JNI");
    check(report.contains("\"message\":\"usb_not_imported\""), "connected is not imported");
    String redacted = NativeConnection.diagnose(bytes(observation.replace("\"failure\":null",
        "\"failure\":\"synthetic-secret private-host\"")));
    check(!redacted.contains("synthetic-secret") && !redacted.contains("private-host"), "unknown error redaction");
    check(NativeConnection.diagnose(bytes(observation.replace("\"selected\":1", "\"selected\":2"))) == null,
        "invalid counts rejected through JNI");
    check(NativeConnection.decodeQr(null, 640, 480) == null, "null QR frame");
    check(NativeConnection.decodeQr(new byte[1], 32, 32) == null, "short QR frame");
    check(
        NativeConnection.decodeQr(new byte[0], Integer.MAX_VALUE, 2) == null, "QR integer bounds");
    check(NativeConnection.decodeQr(new byte[1280 * 1280], 1280, 1280) == null, "QR pixel quota");
    byte[] blank = new byte[64 * 64];
    Arrays.fill(blank, (byte) 255);
    check(NativeConnection.decodeQr(blank, 64, 64) == null, "blank camera frame");
    if (args.length != 1) throw new AssertionError("synthetic QR fixture directory required");
    byte[] pgm = java.nio.file.Files.readAllBytes(java.nio.file.Path.of(args[0], "pairing.pgm"));
    int start = 0, lines = 0;
    while (lines < 3 && start < pgm.length) if (pgm[start++] == '\n') lines++;
    String[] header = new String(pgm, 0, start, StandardCharsets.US_ASCII).trim().split("\\s+");
    check(
        header.length == 4 && header[0].equals("P5") && header[3].equals("255"), "fixture format");
    int width = Integer.parseInt(header[1]), height = Integer.parseInt(header[2]);
    String expected = java.nio.file.Files.readString(java.nio.file.Path.of(args[0], "pairing.txt"));
    check(
        expected.equals(
            NativeConnection.decodeQr(Arrays.copyOfRange(pgm, start, pgm.length), width, height)),
        "actual QR through JVM/JNI");
    byte[] secret = bytes("{\"password\":\"synthetic-fixture\"}");
    String profile =
        "{\"transport\":\"ssh\",\"endpoint\":{\"host\":\"127.0.0.1\",\"port\":9},\"user\":\"fixture\",\"host_key\":\"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\",\"reverse_port\":0,\"prepare\":false}";
    check(start(bytes("{}"), secret) == 0, "invalid profile");
    check(start(bytes(profile), bytes("{\"unexpected\":true}")) == 0, "secret fields are strict");
    rejected(() -> start(null, secret));
    rejected(() -> start(new byte[4097], secret));
    rejected(() -> start(bytes(profile), new byte[131073]));
    check(
        NativeConnection.start(bytes(profile), secret, 0, new byte[32], "/fixture") == 0,
        "invalid loopback port");
    check(
        NativeConnection.start(bytes(profile), secret, 1, new byte[31], "/fixture") == 0,
        "invalid loopback proof");
    long a = start(bytes(profile), secret), b = start(bytes(profile), secret);
    check(a > 0 && b > a, "unique handles");
    check(start(bytes(profile), secret) == 0, "bounded sessions");
    boolean error = false;
    long end = System.nanoTime() + 5_000_000_000L;
    while (System.nanoTime() < end) {
      String event = NativeConnection.poll(a);
      if (event != null && event.contains("\"code\":\"reconnecting\"")) {
        error = true;
        break;
      }
      Thread.sleep(10);
    }
    check(error, "transient network failure enters asynchronous reconnect state");
    check(!NativeConnection.approve(a, "stale-pin", true), "no stale trust question");
    check(!NativeConnection.saved(a, 1, true), "no unrequested persistence acknowledgement");
    NativeConnection.stop(a);
    NativeConnection.stop(a);
    NativeConnection.stop(b);
    check(NativeConnection.poll(a) == null, "closed handle cannot poll");
    check(!NativeConnection.saved(a, 1, true), "closed handle cannot acknowledge persistence");
    check(!NativeConnection.approve(a, "stale-pin", true), "closed handle cannot approve");
    for (int i = 0; i < 32; i++) {
      long id = start(bytes(profile), secret);
      check(id > b, "handle cannot alias an old connection");
      NativeConnection.stop(id);
    }
    Arrays.fill(secret, (byte) 0);
    System.out.println("PASS Connection JNI " + checks + " checks (no USB)");
  }
}
