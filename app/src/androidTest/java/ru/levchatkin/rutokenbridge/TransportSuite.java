package ru.levchatkin.rutokenbridge;

import android.app.Instrumentation;
import android.content.Context;
import android.os.Bundle;
import android.os.SystemClock;
import android.util.Base64;
import java.io.*;
import java.nio.charset.StandardCharsets;
import java.util.Arrays;
import org.cgutman.usbip.connection.ReceiverAssets;
import org.cgutman.usbip.jni.NativeConnection;
import org.json.*;

/** Test APK only: optimized ART/JNI transport to owned VM and synthetic USB endpoint. */
final class TransportSuite {
  private final Instrumentation test;
  private int checks;

  TransportSuite(Instrumentation test) {
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

  int run() throws Exception {
    Context context = test.getTargetContext();
    File input = new File(context.getExternalFilesDir(null), "transport-fixture.json");
    check(input.length() > 0 && input.length() <= 131200, "bounded owned fixture");
    byte[] bytes = new byte[(int) input.length()];
    try (DataInputStream stream = new DataInputStream(new FileInputStream(input))) {
      stream.readFully(bytes);
      check(stream.read() == -1, "fixture exact size");
    }
    JSONObject fixture;
    try {
      fixture = new JSONObject(new String(bytes, StandardCharsets.UTF_8));
    } finally {
      Arrays.fill(bytes, (byte) 0);
    }
    check(fixture.getString("scope").equals("owned-emulator-QEMU-synthetic-USB"), "fixture scope");
    if (fixture.optString("scenario").equals("recovery")) {
      return checks + new TransportRecoverySuite(test).run(context, fixture, input);
    }
    JSONObject profile = fixture.getJSONObject("profile");
    check(
        profile.getJSONObject("endpoint").getString("host").equals("10.0.2.2"),
        "owned host forward only");
    JSONObject secret = fixture.getJSONObject("secret");
    byte[] proof =
        Base64.decode(
            fixture.getString("proof"), Base64.URL_SAFE | Base64.NO_WRAP | Base64.NO_PADDING);
    check(proof.length == 32, "bridge proof size");
    ReceiverAssets.ensure(context);
    String artifacts = new File(context.getNoBackupFilesDir(), "receivers").getAbsolutePath();
    int steadySeconds = fixture.optInt("steady_seconds", 20);
    boolean mixed = fixture.optBoolean("mixed", false);
    String mixedToken = fixture.optString("mixed_done_token");
    check(!mixed || mixedToken.matches("[0-9a-f]{32}"), "owned mixed completion marker");
    check(steadySeconds >= 20 && steadySeconds <= (mixed ? 600 : 180),
        "bounded steady interval");
    try {
      for (int round = 0; round < 2; round++) {
        byte[] config = profile.toString().getBytes(StandardCharsets.UTF_8);
        byte[] credentials = secret.toString().getBytes(StandardCharsets.UTF_8);
        long id;
        try {
          id =
              NativeConnection.start(
                  config, credentials, fixture.getInt("bridge_port"), proof, artifacts);
        } finally {
          Arrays.fill(config, (byte) 0);
          Arrays.fill(credentials, (byte) 0);
        }
        check(id > 0, "native session allocated");
        long deadline = SystemClock.elapsedRealtime() + 180000 + steadySeconds * 1000L;
        long connected = 0;
        boolean mixedCompleted = false;
        File mixedDone = new File(context.getExternalFilesDir(null),
            "mixed-done-" + mixedToken + "-" + round + ".go");
        try {
          while (SystemClock.elapsedRealtime() < deadline) {
            String text = NativeConnection.poll(id);
            if (text != null) {
              JSONObject event = new JSONObject(text);
              String kind = event.getString("event");
              if (kind.equals("trust")) {
                check(round == 0, "no repeated trust prompt");
                String pin = event.getString("pin");
                JSONArray pins = fixture.getJSONArray("host_pins");
                boolean known = false;
                for (int n = 0; n < pins.length(); n++) known |= pin.equals(pins.getString(n));
                check(known && NativeConnection.approve(id, pin, true), "exact fixture SSH pin");
              } else if (kind.equals("trusted")) {
                profile.put("host_key", event.getString("pin"));
                check(
                    NativeConnection.saved(id, event.getLong("save_id"), true),
                    "host pin acknowledged");
              } else if (kind.equals("paired")) {
                check(round == 0, "pairing not repeated");
                secret = new JSONObject().put("token", event.getString("token"));
                check(
                    NativeConnection.saved(id, event.getLong("save_id"), true),
                    "pairing token acknowledged");
              } else if (kind.equals("state")) {
                String code = event.getString("code");
                check(!code.equals("reconnecting"), "stable synthetic transport");
                if (code.equals("connected")) {
                  check(connected == 0, "one connection per round");
                  connected = SystemClock.elapsedRealtime();
                  phase("TRANSPORT_CONNECTED round=" + round);
                }
              } else {
                throw new AssertionError(
                    "unexpected transport event: " + kind + " " + event.optString("code"));
              }
            }
            if (mixed && connected != 0) {
              if (mixedDone.exists()) {
                check(mixedDone.length() == 1, "bounded mixed completion marker");
                try (FileInputStream done = new FileInputStream(mixedDone)) {
                  check(done.read() == 1 && done.read() == -1, "exact mixed completion marker");
                }
                check(mixedDone.delete(), "owned mixed marker consumed");
                mixedCompleted = true;
                break;
              }
              check(SystemClock.elapsedRealtime() - connected < steadySeconds * 1000L,
                  "mixed guest workload acknowledged before deadline");
            } else if (connected != 0
                && SystemClock.elapsedRealtime() - connected >= steadySeconds * 1000L) break;
            Thread.sleep(25);
          }
          check(
              connected != 0 && SystemClock.elapsedRealtime() < deadline && (!mixed || mixedCompleted),
              "stable connected interval");
        } finally {
          NativeConnection.stop(id);
        }
        check(NativeConnection.poll(id) == null, "closed native handle");
        Thread.sleep(5000);
      }
    } finally {
      Arrays.fill(proof, (byte) 0);
      check(input.delete(), "owned fixture removed");
    }
    phase("ART JNI transport and saved-credential reconnect finished");
    return checks;
  }
}
