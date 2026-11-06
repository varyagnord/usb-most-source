package ru.levchatkin.rutokenbridge;

import android.app.Instrumentation;
import android.content.Context;
import android.content.SharedPreferences;
import android.os.ParcelFileDescriptor;
import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyProperties;
import java.io.*;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.security.KeyStore;
import java.util.*;
import java.util.concurrent.*;
import javax.crypto.*;
import org.cgutman.usbip.connection.ComputerRepository;
import org.cgutman.usbip.connection.ComputerVault;
import org.cgutman.usbip.jni.NativeConnection;
import org.json.JSONObject;

/**
 * Disposable emulator only. Real Keystore/AES-GCM/atomic files, no test IPC. Run each scenario in a
 * fresh app process and cleared application sandbox.
 */
final class ComputerStoreSuite {
  private static final String V1 = "usb-most.connection.v1", V2 = "usb-most.computers.v2";
  private static final String MARKER = "SYNTHETIC-COMPUTER-PASSWORD";
  private final Instrumentation instrumentation;
  private final Context context;
  private int checks;

  ComputerStoreSuite(Instrumentation instrumentation) {
    this.instrumentation = instrumentation;
    context = instrumentation.getTargetContext();
  }

  private void check(boolean condition, String label) {
    checks++;
    if (!condition) throw new AssertionError(label);
  }

  private static byte[] bytes(String s) {
    return s.getBytes(StandardCharsets.UTF_8);
  }

  private static String text(byte[] data) {
    return new String(data, StandardCharsets.UTF_8);
  }

  private static <T> T await(CompletableFuture<T> future) throws Exception {
    return future.get(30, TimeUnit.SECONDS);
  }

  private void rejected(CompletableFuture<?> future, String label) throws Exception {
    try {
      await(future);
      throw new AssertionError(label);
    } catch (ExecutionException expected) {
      checks++;
    }
  }

  private File file(String name) {
    return new File(context.getNoBackupFilesDir(), name);
  }

  private KeyStore keys() throws Exception {
    KeyStore store = KeyStore.getInstance("AndroidKeyStore");
    store.load(null);
    return store;
  }

  private JSONObject ssh() throws Exception {
    return new JSONObject()
        .put("transport", "ssh")
        .put("endpoint", new JSONObject().put("host", "fixture.local").put("port", 22))
        .put("user", "fixture")
        .put("host_key", JSONObject.NULL)
        .put("reverse_port", 0)
        .put("prepare", false);
  }

  private byte[] initial() {
    return NativeConnection.computers(new byte[0], bytes("{\"op\":\"create\"}"), false);
  }

  private void legacySecure(boolean mismatch) throws Exception {
    String profile = ssh().toString();
    context
        .getSharedPreferences("bridge", 0)
        .edit()
        .putBoolean("secure_connection", true)
        .putString(
            "secure_profile",
            mismatch ? profile.replace("fixture.local", "different.local") : profile)
        .putString("connection_client_id", "11111111111111111111111111111111")
        .commit();
    KeyGenerator generator =
        KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore");
    generator.init(
        new KeyGenParameterSpec.Builder(
                V1, KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setKeySize(256)
            .build());
    Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
    cipher.init(Cipher.ENCRYPT_MODE, generator.generateKey());
    byte[] plaintext =
        bytes(
            new JSONObject()
                .put("profile", profile)
                .put("credentials", new JSONObject().put("password", MARKER))
                .toString());
    try (FileOutputStream output = new FileOutputStream(file("connection.enc"))) {
      output.write(1);
      output.write(12);
      output.write(cipher.getIV());
      output.write(cipher.doFinal(plaintext));
      output.getFD().sync();
    } finally {
      Arrays.fill(plaintext, (byte) 0);
    }
  }

  private byte[] change(String op, ComputerRepository.Snapshot view, String id) throws Exception {
    JSONObject command = new JSONObject().put("op", op).put("revision", view.revision);
    if (id != null) command.put("id", id);
    return bytes(command.toString());
  }

  private ComputerRepository.Snapshot rename(ComputerRepository repo, String name)
      throws Exception {
    ComputerRepository.Snapshot snapshot = repo.snapshot();
    return await(
        repo.change(
            bytes(
                new JSONObject()
                    .put("op", "rename")
                    .put("revision", snapshot.revision)
                    .put("id", snapshot.activeId)
                    .put("name", name)
                    .toString())));
  }

  int run(String scenario) throws Exception {
    try (ParcelFileDescriptor fd =
            instrumentation.getUiAutomation().executeShellCommand("getprop ro.kernel.qemu");
        InputStream input = new ParcelFileDescriptor.AutoCloseInputStream(fd)) {
      ByteArrayOutputStream output = new ByteArrayOutputStream();
      byte[] buffer = new byte[32];
      int count;
      while ((count = input.read(buffer)) != -1) output.write(buffer, 0, count);
      check(output.toString("US-ASCII").trim().equals("1"), "owned emulator only");
    }
    if (scenario.equals("durable-resume") || scenario.equals("forgotten-resume"))
      return resume(scenario.equals("forgotten-resume"));
    check(
        !file("computers.enc").exists() && !file("connection.enc").exists(),
        "clean fixture sandbox required");
    check(!keys().containsAlias(V1) && !keys().containsAlias(V2), "fresh app Keystore required");
    SharedPreferences prefs = context.getSharedPreferences("bridge", 0);
    check(prefs.edit().clear().commit(), "clear only disposable emulator preferences");
    ComputerVault vault = new ComputerVault(context);
    boolean[] mainRejected = {false};
    instrumentation.runOnMainSync(() -> {
      try {
        vault.load();
      } catch (IllegalStateException expected) {
        mainRejected[0] = true;
      } catch (Exception failure) {
        throw new AssertionError("storage touched on main thread", failure);
      }
    });
    check(mainRejected[0], "Keystore/files forbidden on main thread");
    if (scenario.equals("secure")
        || scenario.equals("mismatch")
        || scenario.equals("cleanup")
        || scenario.equals("empty-v2")
        || scenario.equals("durable-seed")) legacySecure(scenario.equals("mismatch"));
    if (scenario.equals("legacy") || scenario.equals("corrupt") || scenario.equals("missing-key"))
      check(
          prefs.edit().putString("server", "192.168.4.2").putInt("network_mode", 1).commit(),
          "legacy fixture");
    if (scenario.equals("corrupt")) Files.write(file("computers.enc").toPath(), new byte[48]);
    if (scenario.equals("missing-key") || scenario.equals("empty-v2")) {
      vault.save(initial());
      if (scenario.equals("missing-key")) keys().deleteEntry(V2);
    }
    File cleanupBlocker = file("connection.enc.new");
    if (scenario.equals("cleanup")) {
      check(cleanupBlocker.mkdir(), "simulated legacy cleanup obstruction");
      Files.write(new File(cleanupBlocker, "owned-fixture").toPath(), bytes("fixture"));
    }
    ComputerRepository repo = ComputerRepository.get(context);
    if (Arrays.asList("corrupt", "missing-key", "mismatch").contains(scenario)) {
      rejected(repo.ready(), "unsafe migration/load rejected");
      check(
          repo.snapshot().status == ComputerRepository.Status.FAILED,
          "no ready cache after load failure");
      check(!keys().containsAlias(V2), "missing key is not silently regenerated");
      check(
          prefs.contains("server") || prefs.contains("secure_profile"),
          "old source retained on failure");
      if (scenario.equals("mismatch")) {
        check(
            file("connection.enc").isFile() && keys().containsAlias(V1),
            "mismatch retains legacy ciphertext and key");
        check(!file("computers.enc").exists(), "mismatch does not commit v2");
      }
      rejected(repo.begin(0), "unavailable store cannot start");
      rejected(repo.resume(), "failed storage cannot sticky resume");
      await(repo.reset());
      check(
          repo.snapshot().status == ComputerRepository.Status.READY
              && repo.snapshot().computers.isEmpty(),
          "explicit recovery creates empty authenticated catalog");
      check(
          !keys().containsAlias(V1) && keys().containsAlias(V2),
          "explicit reset retires old credentials");
      await(repo.reload());
      check(repo.snapshot().computers.isEmpty(), "recovered empty store durable");
      rejected(repo.resume(), "reset cannot auto-connect old computer");
      return checks;
    }
    if (scenario.equals("cleanup")) {
      rejected(repo.ready(), "cleanup failure cannot be reported as success");
      check(file("computers.enc").isFile(), "new catalog committed before legacy retirement");
      check(
          repo.snapshot().status == ComputerRepository.Status.FAILED,
          "cleanup failure marks unavailable");
      check(
          new File(cleanupBlocker, "owned-fixture").delete() && cleanupBlocker.delete(),
          "remove only recorded empty fixture directory");
      await(repo.reload());
    } else await(repo.ready());
    check(repo.snapshot().status == ComputerRepository.Status.READY, "loaded snapshot ready");
    check(
        !file("connection.enc").exists() && !keys().containsAlias(V1),
        "legacy key and ciphertext retired");
    check(
        !prefs.contains("server")
            && !prefs.contains("secure_profile")
            && !prefs.contains("secure_connection"),
        "legacy source removed");
    check(keys().getKey(V2, null).getEncoded() == null, "Keystore AES key nonexportable");
    if (scenario.equals("empty-v2")) {
      check(
          repo.snapshot().computers.isEmpty() && repo.snapshot().active() == null,
          "empty v2 never reimports legacy secrets");
      await(repo.reload());
      check(repo.snapshot().computers.isEmpty(), "empty catalog survives reload");
      return checks;
    }
    if (scenario.equals("legacy")) {
      check(
          repo.snapshot().active().host.equals("192.168.4.2") && repo.snapshot().active().mode == 1,
          "exact LAN destination migrated");
    } else
      check(repo.snapshot().active().profile.contains("fixture.local"), "secure profile migrated");
    try {
      repo.snapshot().computers.clear();
      throw new AssertionError("mutable public snapshot");
    } catch (UnsupportedOperationException expected) {
      checks++;
    }
    byte[] plain = vault.load();
    check(Arrays.equals(plain, vault.load()), "authenticated plaintext roundtrip");
    if (!scenario.equals("legacy"))
      check(text(plain).contains(MARKER), "credential preserved during migration");
    Arrays.fill(plain, (byte) 0);
    byte[] encrypted = Files.readAllBytes(file("computers.enc").toPath());
    check(
        !text(encrypted).contains(MARKER) && !text(encrypted).contains("fixture.local"),
        "file has no plaintext identity or password");
    long revision = repo.snapshot().revision;
    await(repo.reload());
    check(repo.snapshot().revision == revision, "reload does not duplicate migration");
    byte[] committed = Files.readAllBytes(file("computers.enc").toPath());
    Files.write(file("computers.enc.new").toPath(), bytes("interrupted-uncommitted-write"));
    await(repo.reload());
    check(
        Arrays.equals(committed, Files.readAllBytes(file("computers.enc").toPath())),
        "unfinished write never replaces committed data");
    check(!file("computers.enc.new").exists(), "orphan pending write retired");
    check(
        file("computers.enc").renameTo(file("computers.enc.bak")),
        "prepare legacy AtomicFile crash fixture");
    Files.write(file("computers.enc").toPath(), bytes("incomplete"));
    await(repo.reload());
    check(
        Arrays.equals(committed, Files.readAllBytes(file("computers.enc").toPath())),
        "committed backup restored before decryption");
    check(!file("computers.enc.bak").exists(), "recovered backup no longer duplicates ciphertext");
    rename(repo, "Рабочий компьютер");
    if (scenario.equals("durable-seed")) {
      ComputerRepository.Session seed = await(repo.begin(repo.snapshot().revision));
      await(repo.persist(seed, "trusted", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"));
      check(repo.isBusy(), "seed owns service reservation until process death");
      check(
          prefs.getString("computer_resume", "").equals(repo.snapshot().connectionKey()),
          "resume hint follows persisted trust");
      Files.write(
          file("computer-store-process-fixture").toPath(),
          bytes(Integer.toString(android.os.Process.myPid())));
      return checks;
    }
    byte[] updated = Files.readAllBytes(file("computers.enc").toPath());
    check(
        !Arrays.equals(Arrays.copyOfRange(encrypted, 2, 14), Arrays.copyOfRange(updated, 2, 14)),
        "new GCM IV for each save");
    rejected(repo.resume(), "no sticky start without prior request");
    ComputerRepository.Snapshot before = repo.snapshot();
    ComputerRepository.Session session = await(repo.begin(before.revision));
    check(repo.isBusy(), "repository busy while service owns lease");
    check(
        !session.connectionKey().equals(before.connectionKey())
            && session.connectionKey().equals(repo.snapshot().connectionKey()),
        "new connection gets distinct observation key");
    rejected(repo.reset(), "cannot reset while connected");
    byte[] resolved = session.takeConnection();
    check(
        resolved != null && session.takeConnection() == null,
        "service plaintext ownership transferred once");
    Arrays.fill(resolved, (byte) 0);
    check(
        !repo.begin(before.revision).cancel(true),
        "Future cancellation cannot orphan a reserved service");
    rejected(repo.begin(repo.snapshot().revision), "only one service owner");
    rejected(
        repo.change(change("forget", repo.snapshot(), repo.snapshot().activeId)),
        "running computer cannot be forgotten");
    String observationKey = session.connectionKey();
    rename(repo, "Переименован во время работы");
    check(
        repo.snapshot().connectionKey().equals(observationKey),
        "rename preserves observation binding");
    if (!scenario.equals("legacy")) {
      await(repo.persist(session, "trusted", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"));
      check(
          repo.snapshot().active().profile.contains("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
          "identity published after durable commit");
      check(
          text(vault.load()).contains("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
          "trust in encrypted backing store");
    }
    session.cancel();
    rejected(
        repo.persist(session, "trusted", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
        "cancelled owner cannot save");
    rejected(
        repo.change(change("forget", repo.snapshot(), repo.snapshot().activeId)),
        "stopping service still reserves active computer");
    await(repo.release(session));
    check(
        !repo.isBusy() && !prefs.contains("computer_resume"),
        "clean stop retires owner and sticky request");
    rejected(repo.resume(), "explicit stop cannot restart itself");
    ComputerRepository.Session next = await(repo.begin(repo.snapshot().revision));
    rejected(
        repo.persist(session, "trusted", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
        "old service cannot save into new session");
    String nextHint = prefs.getString("computer_resume", "");
    await(repo.release(session)); // late release must not release next
    check(
        repo.isBusy() && nextHint.equals(prefs.getString("computer_resume", "")),
        "late release preserves new resume request");
    rejected(
        repo.change(change("forget", repo.snapshot(), repo.snapshot().activeId)),
        "late old release does not unlock current owner");
    await(repo.release(next));
    // An actual filesystem failure during save must preserve the previous file
    // and invalidate the presentation cache instead of reporting success.
    File blocker = file("computers.enc.new");
    check(blocker.mkdir(), "create pending-write obstruction");
    Files.write(new File(blocker, "owned-fixture").toPath(), bytes("fixture"));
    byte[] stable = Files.readAllBytes(file("computers.enc").toPath());
    ComputerRepository.Snapshot snapshot = repo.snapshot();
    rejected(
        repo.change(
            bytes(
                new JSONObject()
                    .put("op", "rename")
                    .put("revision", snapshot.revision)
                    .put("id", snapshot.activeId)
                    .put("name", "Must not commit")
                    .toString())),
        "write failure is visible");
    check(
        repo.snapshot().status == ComputerRepository.Status.FAILED,
        "failed write invalidates cached success");
    rejected(repo.ready(), "readiness cannot reuse old successful load after write failure");
    check(
        Arrays.equals(stable, Files.readAllBytes(file("computers.enc").toPath())),
        "previous encrypted catalog survives failed write");
    check(
        new File(blocker, "owned-fixture").delete() && blocker.delete(),
        "remove only owned obstruction");
    await(repo.reload());
    check(
        await(repo.ready()).status == ComputerRepository.Status.READY,
        "explicit recovery restores readiness");
    check(
        !repo.snapshot().active().name.equals("Must not commit"),
        "reload retains previously committed value");
    await(repo.change(change("forget", repo.snapshot(), repo.snapshot().activeId)));
    await(repo.reload());
    check(
        repo.snapshot().computers.isEmpty() && repo.snapshot().active() == null,
        "forget last record durable, no fallback");
    check(!text(vault.load()).contains(MARKER), "forgotten credential absent from current catalog");
    rejected(repo.begin(repo.snapshot().revision), "forgotten catalog cannot connect");
    byte[] finalBytes = Files.readAllBytes(file("computers.enc").toPath());
    finalBytes[finalBytes.length - 1] ^= 1;
    Files.write(file("computers.enc").toPath(), finalBytes);
    rejected(repo.reload(), "GCM authentication failure rejected");
    check(
        repo.snapshot().status == ComputerRepository.Status.FAILED,
        "tamper does not reset to empty or old profile");
    return checks;
  }

  private int resume(boolean forgotten) throws Exception {
    String previous =
        new String(
            Files.readAllBytes(file("computer-store-process-fixture").toPath()),
            StandardCharsets.US_ASCII);
    check(
        !previous.equals(Integer.toString(android.os.Process.myPid())),
        "actual new Android process");
    check(
        file("computers.enc").isFile() && !file("connection.enc").exists(),
        "committed v2 survives process restart");
    check(
        keys().containsAlias(V2) && !keys().containsAlias(V1),
        "Keystore migration survives process restart");
    ComputerRepository repo = ComputerRepository.get(context);
    await(repo.ready());
    if (forgotten) {
      check(
          repo.snapshot().computers.isEmpty() && repo.snapshot().active() == null,
          "forgotten computer stays absent after process restart");
      rejected(repo.begin(repo.snapshot().revision), "restart cannot reconnect forgotten computer");
    } else {
      check(
          repo.snapshot().active().name.equals("Рабочий компьютер"),
          "name survives process restart");
      check(
          repo.snapshot().active().profile.contains("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
          "trusted identity survives process restart");
      byte[] editor = await(repo.edit(repo.snapshot().activeId, repo.snapshot().revision));
      try {
        check(text(editor).contains(MARKER), "correct record credential survives process restart");
      } finally {
        Arrays.fill(editor, (byte) 0);
      }
      String priorKey = repo.snapshot().connectionKey();
      ComputerRepository.Session restarted = await(repo.resume());
      check(
          !priorKey.equals(restarted.connectionKey()) && repo.isBusy(),
          "sticky recovery reserves a new lifetime for same computer");
      await(repo.release(restarted));
      rejected(repo.resume(), "clean recovered stop removes sticky request");
      await(repo.change(change("forget", repo.snapshot(), repo.snapshot().activeId)));
      Files.write(
          file("computer-store-process-fixture").toPath(),
          bytes(Integer.toString(android.os.Process.myPid())));
    }
    return checks;
  }
}
