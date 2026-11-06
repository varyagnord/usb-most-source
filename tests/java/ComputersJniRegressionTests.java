import java.nio.charset.StandardCharsets;
import java.util.Arrays;
import java.util.regex.Matcher;
import java.util.regex.Pattern;
import org.cgutman.usbip.jni.NativeConnection;

/** Actual optimized Rust/JVM boundary. All values are disposable fixtures. */
public final class ComputersJniRegressionTests {
  private static int checks;
  private static byte[] bytes(String s) { return s.getBytes(StandardCharsets.UTF_8); }
  private static String text(byte[] b) { return new String(b, StandardCharsets.UTF_8); }
  private static void check(boolean condition, String label) {
    checks++;
    if (!condition) throw new AssertionError(label);
  }
  private static byte[] call(byte[] catalog, String request, boolean running) {
    byte[] encoded = bytes(request);
    try { return NativeConnection.computers(catalog, encoded, running); }
    finally { Arrays.fill(encoded, (byte) 0); }
  }
  private static void rejects(Runnable action, String message) {
    try { action.run(); throw new AssertionError("must reject " + message); }
    catch (RuntimeException expected) {
      check(!String.valueOf(expected.getMessage()).contains("SYNTHETIC-SECRET"), "no input echoed");
      checks++;
    }
  }
  private static String lease(byte[] data) {
    String active = text(call(data, "{\"op\":\"active\"}", false));
    Matcher m = Pattern.compile("\\\"lease\\\":(\\{[^}]+\\})").matcher(active);
    if (!m.find()) throw new AssertionError("missing lease");
    return m.group(1);
  }
  public static void main(String[] args) {
    rejects(() -> NativeConnection.computers(null, bytes("{}"), false), "null catalog");
    rejects(() -> NativeConnection.computers(new byte[0], null, false), "null request");
    rejects(() -> NativeConnection.computers(new byte[4 * 1024 * 1024 + 1], bytes("{}"), false), "oversize catalog");
    rejects(() -> NativeConnection.computers(new byte[0], new byte[144 * 1024 + 1], false), "oversize request");
    rejects(() -> call(bytes("SYNTHETIC-SECRET"), "{\"op\":\"create\"}", false), "corrupt cannot initialize");
    byte[] empty = call(new byte[0], "{\"op\":\"create\"}", false);
    check(text(empty).contains("\"computers\":[]"), "explicit empty catalog");
    byte[] a = call(empty, "{\"op\":\"add\",\"revision\":0,\"name\":\"Работа\",\"activate\":true,"
        + "\"connection\":{\"kind\":\"secure\",\"profile\":{\"transport\":\"ssh\","
        + "\"endpoint\":{\"host\":\"fixture.local\",\"port\":22},\"user\":\"fixture\","
        + "\"host_key\":null,\"reverse_port\":0,\"prepare\":false},"
        + "\"credentials\":{\"password\":\"SYNTHETIC-SECRET\"}}}", false);
    check(text(empty).contains("\"computers\":[]"), "transaction does not mutate old byte array");
    String view = text(call(a,"{\"op\":\"view\"}",false));
    check(view.contains("Работа"), "UTF-8 display name roundtrip");
    check(!view.contains("SYNTHETIC-SECRET") && !view.contains("credentials") && !view.contains("password"), "public DTO has no secrets");
    String oldLease = lease(a);
    byte[] b = call(a,"{\"op\":\"begin\",\"revision\":1}",false);
    String pin = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    rejects(() -> call(b,"{\"op\":\"trust\",\"lease\":" + oldLease + ",\"pin\":\"" + pin + "\"}",true), "stale session");
    byte[] trusted = call(b,"{\"op\":\"trust\",\"lease\":" + lease(b) + ",\"pin\":\"" + pin + "\"}",true);
    check(text(call(trusted,"{\"op\":\"view\"}",false)).contains("\"host_key\":\"" + pin + "\""), "saved identity public view");
    rejects(() -> call(trusted,"{\"op\":\"view\",\"unexpected\":\"SYNTHETIC-SECRET\"}",false), "strict schema");
    rejects(() -> call(bytes("{}"),"{\"op\":\"view\"}",false), "missing fields");
    rejects(() -> call(trusted,"{\"op\":\"begin\",\"revision\":1}",false), "stale revision");
    check(text(trusted).contains("\"revision\":3"), "trust committed candidate");
    String identity = text(call(new byte[0], "{\"op\":\"usb_identity\",\"vendor\":1,\"product\":2,\"serial\":\"SYNTHETIC-SECRET\",\"layout\":\"hid\"}", false));
    check(identity.contains("serial_hash") && !identity.contains("SYNTHETIC-SECRET"), "USB recognition never returns raw serial");
    check(text(call(new byte[0],"{\"op\":\"usb_name\",\"name\":\"Мышь\"}",false)).equals("true"), "UTF-8 USB label validated by Rust");
    String rule = "{\"identity\":" + identity + ",\"name\":\"Мышь\",\"automatic\":true,\"storage\":false}";
    Matcher id = Pattern.compile("\\\"id\\\":\\\"([a-f0-9]{32})\\\"").matcher(oldLease);
    check(id.find(), "opaque computer identity available");
    String setRule = "{\"op\":\"set_usb_rule\",\"revision\":3,\"id\":\"" + id.group(1) + "\",\"rule\":" + rule + "}";
    byte[] ruled = call(trusted,setRule,true);
    String publicRules = text(call(ruled,"{\"op\":\"view\"}",false));
    check(publicRules.contains("Мышь") && !publicRules.contains("SYNTHETIC-SECRET"), "rules view excludes SSH password and serial");
    rejects(() -> call(ruled,setRule,true), "stale USB rule revision");
    String device = "{\"id\":123,\"generation\":42,\"vendor\":1,\"product\":2,\"identity\":" + identity + ",\"storage\":false,\"supported\":true,\"permission\":true}";
    String evaluate = "{\"op\":\"usb_evaluate\",\"rules\":[" + rule + "],\"devices\":[" + device + "]}";
    String decision = text(call(new byte[0],evaluate,false));
    check(decision.contains("\"automatic\":true") && decision.contains("\"generation\":42"), "automatic decision binds current generation");
    check(text(call(new byte[0],evaluate.replace("\"permission\":true","\"permission\":false"),false)).contains("\"automatic\":false"), "permission blocks automatic choice");
    check(text(call(new byte[0],evaluate.replace("\"id\":123,\"generation\":42", "\"id\":987,\"generation\":99"),false)).contains("\"generation\":99"), "hotplug port is not persistent identity");
    rejects(() -> call(new byte[0],evaluate.replace("\"storage\":false","\"storage\":true"),false), "storage automatic rule rejected");
    Arrays.fill(ruled,(byte)0);
    for (byte[] data : new byte[][] {empty,a,b,trusted}) Arrays.fill(data,(byte)0);
    System.out.println("PASS Computers JNI " + checks + " checks (synthetic settings, no USB)");
  }
}
