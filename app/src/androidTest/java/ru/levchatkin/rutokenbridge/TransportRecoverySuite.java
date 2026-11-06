package ru.levchatkin.rutokenbridge;

import android.app.Instrumentation;
import android.content.Context;
import android.os.Bundle;
import android.os.SystemClock;
import android.util.Base64;
import java.io.File;
import java.nio.charset.StandardCharsets;
import java.util.Arrays;
import org.cgutman.usbip.connection.ReceiverAssets;
import org.cgutman.usbip.jni.NativeConnection;
import org.json.JSONArray;
import org.json.JSONObject;

/** Actual ART/Rust recovery with a host-owned TCP cut and synthetic USB only.
 * Instrumentation controls the disconnect acknowledgement; this does not claim
 * to exercise Android's storage-consent adapter or physical network handoff. */
final class TransportRecoverySuite {
  private final Instrumentation test;
  private int checks;

  TransportRecoverySuite(Instrumentation test) {
    this.test = test;
  }

  private void check(boolean value, String label) {
    if (!value) throw new AssertionError(label);
    checks++;
  }

  private void phase(String label) {
    Bundle result = new Bundle();
    result.putString("stream", "PASS " + label + "\n");
    test.sendStatus(0, result);
  }

  private void quiet(long id, long millis) throws Exception {
    long until = SystemClock.elapsedRealtime() + millis;
    while (SystemClock.elapsedRealtime() < until) {
      if (NativeConnection.poll(id) != null)
        throw new AssertionError("connection advanced before permitted recovery");
      Thread.sleep(25);
    }
    check(true, "entire quiet interval observed");
  }

  int run(Context context, JSONObject fixture, File input) throws Exception {
    JSONObject profile = fixture.getJSONObject("profile");
    check(profile.getJSONObject("endpoint").getString("host").equals("10.0.2.2"),
        "owned QEMU host forward only");
    byte[] proof = Base64.decode(fixture.getString("proof"),
        Base64.URL_SAFE | Base64.NO_WRAP | Base64.NO_PADDING);
    check(proof.length == 32, "exact local bridge proof");
    String marker = fixture.getString("release_marker");
    check(marker.matches("[a-f0-9]{32}"), "bounded owned acknowledgement marker");
    File release = new File(input.getParentFile(), "recovery-" + marker + ".go");
    check(!release.exists(), "fresh acknowledgement marker");
    long id = 0;
    long lastSave = 0;
    int connected = 0, disconnected = 0, reconnecting = 0, trust = 0, trusted = 0, paired = 0;
    String persistedPin = null;
    boolean failedAsExpected = false;
    try {
      ReceiverAssets.ensure(context);
      byte[] config = profile.toString().getBytes(StandardCharsets.UTF_8);
      byte[] secret = fixture.getJSONObject("secret").toString().getBytes(StandardCharsets.UTF_8);
      try {
        id = NativeConnection.start(config, secret, fixture.getInt("bridge_port"), proof,
            new File(context.getNoBackupFilesDir(), "receivers").getAbsolutePath());
      } finally {
        Arrays.fill(config, (byte) 0);
        Arrays.fill(secret, (byte) 0);
      }
      check(id > 0, "native recovery session allocated");
      long deadline = SystemClock.elapsedRealtime() + 240000;
      while (SystemClock.elapsedRealtime() < deadline) {
        String raw = NativeConnection.poll(id);
        if (raw == null) {
          Thread.sleep(25);
          continue;
        }
        JSONObject event = new JSONObject(raw);
        String kind = event.getString("event");
        if (kind.equals("trust")) {
          check(connected == 0 && ++trust == 1, "host trust requested only once");
          String pin = event.getString("pin");
          JSONArray known = fixture.getJSONArray("host_pins");
          boolean matches = false;
          for (int n = 0; n < known.length(); n++) matches |= pin.equals(known.getString(n));
          check(matches && NativeConnection.approve(id, pin, true), "exact owned host identity");
        } else if (kind.equals("trusted") || kind.equals("paired")) {
          if (kind.equals("trusted")) {
            // Each SSH connection rechecks the repository owner through an
            // idempotent persistence acknowledgement. It is not a trust prompt.
            check(++trusted == connected + 1 && trusted <= 2, "one identity acknowledgement per SSH epoch");
            String pin = event.getString("pin");
            if (persistedPin != null) check(persistedPin.equals(pin), "same trusted identity after recovery");
            persistedPin = pin;
          } else {
            check(connected == 0 && ++paired == 1, "no repeated pairing during recovery");
          }
          check(NativeConnection.saved(id, event.getLong("save_id"), true), "persist initial trust");
        } else if (kind.equals("state")) {
          String code = event.getString("code");
          if (code.equals("connected")) {
            connected++;
            check(connected <= 2 && disconnected == connected - 1, "one confirmed connection per epoch");
            if (connected == 2) check(reconnecting >= 1 && reconnecting <= 8, "automatic recovery through bounded backoff");
            phase("RECOVERY_CONNECTED round=" + (connected - 1));
          } else if (code.equals("reconnecting")) {
            reconnecting++;
            check(disconnected == 1 && reconnecting <= 8, "no early or unbounded retry");
            long delay = event.getLong("delay_ms");
            if (reconnecting == 1) {
              check(delay >= 3000 && delay < 4000, "first bounded jittered backoff");
            } else {
              check(event.getString("reason").equals("computer_busy") && delay > 0 && delay <= 61000,
                  "bounded wait for previous receiver control without repeating USB exposure");
            }
          }
        } else if (kind.equals("disconnected")) {
          disconnected++;
          check(disconnected == connected && disconnected <= 2, "matching exposed connection lost");
          long save = event.getLong("save_id");
          check(save > lastSave, "disconnect acknowledgements have fresh identities");
          lastSave = save;
          check(!NativeConnection.saved(id, Long.MAX_VALUE, true), "wrong acknowledgement rejected");
          if (disconnected == 1) {
            phase("RECOVERY_BARRIER_HELD");
            quiet(id, 5000);
            while (!release.exists() && SystemClock.elapsedRealtime() < deadline) quiet(id, 100);
            check(release.length() == 1 && release.delete(), "host confirmed no premature TCP attempt");
            check(NativeConnection.saved(id, save, true), "release exact disconnect barrier");
            check(!NativeConnection.saved(id, save, true), "duplicate barrier acknowledgement rejected");
            phase("RECOVERY_BARRIER_RELEASED");
          } else {
            check(NativeConnection.saved(id, save, false), "deny disconnect acknowledgement");
          }
        } else if (kind.equals("error")) {
          String code = event.getString("code");
          check(disconnected == 2 && code.equals("storage_failed"),
              "expected terminal storage barrier failure, received " + code);
          phase("RECOVERY_SAVE_REJECTED");
          quiet(id, 5000);
          check(!NativeConnection.saved(id, lastSave, true), "late success cannot revive failed session");
          failedAsExpected = true;
          break;
        } else {
          throw new AssertionError("unexpected recovery event kind: " + kind);
        }
      }
      check(failedAsExpected && connected == 2 && disconnected == 2, "complete recovery and refusal sequence");
    } finally {
      if (id > 0) {
        NativeConnection.stop(id);
        check(NativeConnection.poll(id) == null, "closed recovery handle");
        check(!NativeConnection.saved(id, lastSave, true), "closed handle refuses stale acknowledgement");
      }
      Arrays.fill(proof, (byte) 0);
      if (release.exists()) check(release.delete(), "owned acknowledgement marker removed");
      check(input.delete(), "owned recovery credentials removed");
    }
    phase("RECOVERY_FINISHED");
    return checks;
  }
}
