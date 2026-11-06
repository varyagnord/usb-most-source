import java.io.InputStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.MessageDigest;
import java.security.cert.CertificateFactory;
import java.security.cert.X509Certificate;
import java.util.HashSet;
import java.util.Locale;
import java.util.jar.JarFile;

/** Verify every bundle payload entry against the expected upload certificate. */
class VerifyBundleSignature {
  static boolean signingMetadata(String name) {
    String upper = name.toUpperCase(Locale.ROOT);
    if (!upper.startsWith("META-INF/")) return false;
    String tail = upper.substring(9);
    return !tail.contains("/") && (tail.equals("MANIFEST.MF") || tail.endsWith(".SF")
        || tail.endsWith(".RSA") || tail.endsWith(".DSA") || tail.endsWith(".EC"));
  }

  public static void main(String[] args) throws Exception {
    if (args.length != 2) throw new IllegalArgumentException("bundle.aab expected-certificate.pem");
    X509Certificate expected;
    try (InputStream input = Files.newInputStream(Path.of(args[1]))) {
      expected = (X509Certificate) CertificateFactory.getInstance("X.509").generateCertificate(input);
    }
    expected.checkValidity();
    byte[] certificate = expected.getEncoded();
    long total = 0;
    int count = 0;
    HashSet<String> names = new HashSet<>();
    try (JarFile jar = new JarFile(args[0], true)) {
      if (jar.getManifest() == null) throw new SecurityException("Missing signed manifest");
      var entries = jar.entries();
      byte[] buffer = new byte[65536];
      while (entries.hasMoreElements()) {
        var entry = entries.nextElement();
        if (!names.add(entry.getName())) throw new SecurityException("Duplicate entry");
        if (entry.isDirectory()) continue;
        try (InputStream input = jar.getInputStream(entry)) {
          int length;
          while ((length = input.read(buffer)) != -1) {
            total += length;
            if (total > 512L * 1024 * 1024) throw new SecurityException("Bundle exceeds verification bound");
          }
        }
        if (signingMetadata(entry.getName())) continue;
        var signers = entry.getCodeSigners();
        if (signers == null || signers.length != 1) throw new SecurityException("Unsigned or ambiguous entry: " + entry.getName());
        var actual = signers[0].getSignerCertPath().getCertificates().get(0).getEncoded();
        if (!MessageDigest.isEqual(certificate, actual)) throw new SecurityException("Unexpected upload certificate");
        count++;
      }
      for (String name : jar.getManifest().getEntries().keySet()) {
        if (!names.contains(name)) throw new SecurityException("Removed signed entry");
      }
      for (String name : new String[] {"BundleConfig.pb", "base/manifest/AndroidManifest.xml", "base/dex/classes.dex"}) {
        if (!names.contains(name)) throw new SecurityException("Missing required bundle payload");
      }
    }
    if (count == 0) throw new SecurityException("Empty bundle");
    System.out.println("Verified " + count + " payload entries against the expected upload certificate");
  }
}
