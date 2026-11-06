import java.awt.image.BufferedImage;
import java.nio.file.Files;
import java.nio.file.Path;
import javax.imageio.ImageIO;
import org.cgutman.usbip.jni.NativeConnection;

/** Actual browser screenshots -> Java image pixels -> production Rust/JNI QR decoder. */
public final class RenderedQrRegressionTests {
  private static String decode(Path file) throws Exception {
    BufferedImage image = ImageIO.read(file.toFile());
    if (image == null || (long) image.getWidth() * image.getHeight() > 1024 * 1024)
      throw new AssertionError("invalid rendered fixture");
    int width = image.getWidth(), height = image.getHeight();
    byte[] gray = new byte[width * height];
    for (int y = 0; y < height; y++)
      for (int x = 0; x < width; x++) {
        int rgb = image.getRGB(x, y);
        gray[y * width + x] = (byte) ((((rgb >> 16) & 255) + ((rgb >> 8) & 255) + (rgb & 255)) / 3);
      }
    return NativeConnection.decodeQr(gray, width, height);
  }

  public static void main(String[] args) throws Exception {
    int checks = 0;
    for (String directory : args) {
      Path root = Path.of(directory);
      String expected = Files.readString(root.resolve("pairing.txt"));
      if (!expected.equals(decode(root.resolve("pairing-rendered.png"))))
        throw new AssertionError("rendered PC pairing QR must survive production decoder");
      checks++;
      Path store = root.resolve("store-rendered.png");
      if (Files.exists(store)) {
        if (decode(store) != null)
          throw new AssertionError("store QR must never become pairing credentials");
        checks++;
      }
    }
    if (checks < 3) throw new AssertionError("both listing fixtures required");
    System.out.println("PASS rendered browser QR -> production JNI: " + checks + " checks");
  }
}
