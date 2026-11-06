package org.cgutman.usbip.core;

import java.nio.ByteBuffer;
import java.util.*;

/** Bounded, numeric, metadata-only export. No exported field can hold a digest/payload. */
public final class HidTrace {
  private static final int EVENT_LIMIT = 256;
  public final long number;
  public final int backend;
  private final java.util.function.LongSupplier clock;
  private final Map<Integer, Pending> pending = new HashMap<>();
  private final Map<Integer, Integer> unlinks = new HashMap<>();
  private final ArrayDeque<long[]> events = new ArrayDeque<>();
  private final int[][] buttons = new int[4][8];
  private final long[] packets = new long[4], typed = new long[4];
  private long nativeBase, eventDrops, nativeDrops, invalidNative, unmatchedNative;
  private long jniChecks, jniMismatches, sdkChecks, sdkMismatches, wireChecks, wireMismatches;
  private long classifierMismatches, observationDrops;
  private boolean closed;
  private int pendingAtClose;

  private static final class Pending {
    long digest;
    boolean hasDigest;
  }

  public HidTrace(long number, int backend, java.util.function.LongSupplier clock) {
    this.number = number;
    this.backend = backend;
    this.clock = clock;
    for (int[] row : buttons) Arrays.fill(row, -1);
  }

  public synchronized void nativeClock(long millis) { nativeBase = millis; }

  public synchronized void submitted(int id, int type, int endpoint) {
    if (type != 1 || endpoint != 0x83) return;
    if (pending.size() >= 64 && !pending.containsKey(id)) { observationDrops++; return; }
    pending.put(id, new Pending());
  }

  public synchronized void unlink(int seq, int target) {
    if (!pending.containsKey(target)) return;
    if (unlinks.size() >= 64 && !unlinks.containsKey(seq)) { observationDrops++; return; }
    unlinks.put(seq, target);
  }

  private void event(int stage, int id, int status, int actual, long at, int[] meta) {
    packets[stage]++;
    if (meta[0] != 0) typed[stage]++;
    if (meta[0] == 0 && status == 0) return;
    if (meta[0] == 1) {
      if (buttons[stage][meta[1]] == meta[2]) return;
      buttons[stage][meta[1]] = meta[2];
    }
    if ((meta[0] == 2 || meta[0] == 3) && meta[2] == 1) buttons[stage][meta[1]] = -1;
    if (events.size() == EVENT_LIMIT) { events.removeFirst(); eventDrops++; }
    // Fixed public schema: time, source, seq, status, length, kind, slot, value.
    events.addLast(new long[] {at, stage, id, status, actual, meta[0], meta[1], meta[2]});
  }

  public synchronized void nativeBatch(long[] batch, UsbBackend.Completion completion) {
    if (batch == null) { invalidNative++; return; }
    try {
      if (batch.length < 6 || batch[0] != 1 || batch[1] != 9 || batch[2] < 0
          || batch[2] > 256 || batch.length != 6 + batch[2] * 9) { invalidNative++; return; }
      nativeDrops = batch[5];
      for (int at = 6; at < batch.length; at += 9) {
        long seq = batch[at], status = batch[at + 1], length = batch[at + 2], time = batch[at + 3];
        long kind = batch[at + 4], slot = batch[at + 5], value = batch[at + 6];
        if (seq < Integer.MIN_VALUE || seq > Integer.MAX_VALUE || status < Integer.MIN_VALUE
            || status > Integer.MAX_VALUE || length < 0 || length > TransferLimits.MAX_TRANSFER_BYTES
            || time < 0 || time > Long.MAX_VALUE - nativeBase || kind < 0 || kind > 5
            || slot < 0 || slot > 7 || value < 0 || value > 255 || batch[at + 8] != 0x83) {
          invalidNative++; continue;
        }
        int[] meta = {(int) kind, (int) slot, (int) value};
        event(0, (int) seq, (int) status, (int) length, nativeBase + time, meta);
        if (completion != null && completion.id == seq) {
          jniChecks++;
          if (completion.actualLength != length || HidObservation.digest(completion.data) != batch[at + 7]) jniMismatches++;
          int[] javaMeta = completion.status == 0 ? HidObservation.classify(completion.data) : new int[3];
          if (!Arrays.equals(meta, javaMeta)) classifierMismatches++;
        } else unmatchedNative++;
      }
    } finally {
      // The transient digest never enters the event array, JSON or any journal.
      Arrays.fill(batch, 0);
    }
  }

  public synchronized void sdkRetired(int id, int endpoint, int status, byte[] data, long originalDigest) {
    if (endpoint != 0x83) return;
    sdkChecks++;
    if (HidObservation.digest(data) != originalDigest) sdkMismatches++;
    event(1, id, status, data == null ? 0 : data.length, clock.getAsLong(),
        status == 0 ? HidObservation.classify(data) : new int[3]);
  }

  public synchronized void javaCompleted(UsbBackend.Completion completion) {
    Pending item = pending.get(completion.id);
    if (item == null) return;
    item.digest = HidObservation.digest(completion.data);
    item.hasDigest = true;
    event(2, completion.id, completion.status, completion.actualLength, clock.getAsLong(),
        completion.status == 0 ? HidObservation.classify(completion.data) : new int[3]);
  }

  public synchronized void tcpWritten(byte[] packet) {
    if (packet.length < 48) return;
    ByteBuffer header = ByteBuffer.wrap(packet);
    int command = header.getInt(0), seq = header.getInt(4);
    if (command == 4) {
      Integer target = unlinks.remove(seq);
      int status = header.getInt(20);
      if (target != null && (status == -104 || status == 0)) pending.remove(target);
      return;
    }
    if (command != 3) return;
    Pending item = pending.remove(seq);
    if (item == null) return;
    int status = header.getInt(20), actual = header.getInt(24);
    if (actual < 0 || actual > 32 || packet.length != 48 + actual) { observationDrops++; return; }
    byte[] bytes = Arrays.copyOfRange(packet, 48, packet.length);
    try {
      if (item.hasDigest) {
        wireChecks++;
        if (HidObservation.digest(bytes) != item.digest) wireMismatches++;
      }
      event(3, seq, status, actual, clock.getAsLong(), status == 0 ? HidObservation.classify(bytes) : new int[3]);
    } finally { Arrays.fill(bytes, (byte) 0); }
  }

  public synchronized void closed() {
    closed = true;
    pendingAtClose = pending.size();
    for (Pending item : pending.values()) item.digest = 0;
    pending.clear();
    unlinks.clear();
  }

  public synchronized String json() {
    StringBuilder out = new StringBuilder(1024 + events.size() * 100);
    out.append("{\"session\":").append(number).append(",\"backend\":").append(backend)
        .append(",\"closed\":").append(closed).append(",\"pending\":").append(pending.size())
        .append(",\"pending_at_close\":").append(pendingAtClose)
        .append(",\"packets\":").append(Arrays.toString(packets))
        .append(",\"typed\":").append(Arrays.toString(typed))
        .append(",\"jni_checks\":").append(jniChecks).append(",\"jni_mismatches\":").append(jniMismatches)
        .append(",\"sdk_checks\":").append(sdkChecks).append(",\"sdk_mismatches\":").append(sdkMismatches)
        .append(",\"wire_checks\":").append(wireChecks).append(",\"wire_mismatches\":").append(wireMismatches)
        .append(",\"classifier_mismatches\":").append(classifierMismatches)
        .append(",\"event_drops\":").append(eventDrops).append(",\"native_drops\":").append(nativeDrops)
        .append(",\"invalid_native\":").append(invalidNative).append(",\"unmatched_native\":").append(unmatchedNative)
        .append(",\"observation_drops\":").append(observationDrops).append(",\"events\":[");
    boolean comma = false;
    for (long[] row : events) {
      if (comma) out.append(',');
      comma = true;
      out.append(Arrays.toString(row));
    }
    return out.append("]}").toString();
  }
}
