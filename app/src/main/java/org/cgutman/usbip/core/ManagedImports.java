package org.cgutman.usbip.core;

import java.net.Socket;
import java.nio.ByteBuffer;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.concurrent.ScheduledFuture;
import java.util.concurrent.ScheduledThreadPoolExecutor;
import java.util.concurrent.TimeUnit;
import java.util.function.BooleanSupplier;
import org.cgutman.usbip.server.ManagementAccess;

/** Narrow lifetime adapter. Rust owns the ledger; Android owns and checks each USB lease. */
public final class ManagedImports implements ManagementAccess, AutoCloseable {
  public interface Model extends AutoCloseable {
    byte[] execute(int action, byte[] bytes);

    void close();
  }

  public interface Lease {
    boolean current();

    void state(int state, boolean canRequest);

    boolean applySafe();
  }

  public interface Binder {
    Lease bind(Socket socket, BooleanSupplier request);
  }

  private static final byte[] EMPTY = new byte[0];
  private static final int CAPS = 0,
      VALIDATE = 1,
      RESERVE = 2,
      RELEASED = 3,
      WITHDRAW = 4,
      REQUEST = 5,
      POLL = 6,
      SNAPSHOT = 7,
      ACK_SAFE = 8,
      TIMEOUT = 9,
      CAN_REQUEST = 10;
  private final Model model;
  private final Binder binder;
  private final ScheduledThreadPoolExecutor timer =
      new ScheduledThreadPoolExecutor(
          1,
          task -> {
            Thread thread = new Thread(task, "usb-eject-deadline");
            thread.setDaemon(true);
            return thread;
          });
  private final long timeoutMillis;
  private final List<Entry> entries = new ArrayList<>();
  private boolean closed;

  private static final class Entry {
    final byte[] key;
    final Lease lease;
    ScheduledFuture<?> deadline;

    Entry(byte[] key, Lease lease) {
      this.key = key.clone();
      this.lease = lease;
    }

    void cancelDeadline() {
      if (deadline != null) deadline.cancel(false);
      deadline = null;
    }
  }

  public ManagedImports(Model model, Binder binder) {
    this(model, binder, 30_000);
  }

  /** Alternate deadline is used only by deterministic loopback/host tests. */
  public ManagedImports(Model model, Binder binder, long timeoutMillis) {
    if (model == null || binder == null || timeoutMillis < 1)
      throw new IllegalArgumentException("invalid_managed_adapter");
    this.model = model;
    this.binder = binder;
    this.timeoutMillis = timeoutMillis;
    timer.setRemoveOnCancelPolicy(true);
  }

  private byte[] call(int action, byte[] input) {
    return closed ? null : model.execute(action, input);
  }

  private Entry find(byte[] key) {
    for (Entry entry : entries) if (Arrays.equals(entry.key, key)) return entry;
    return null;
  }

  private void withdraw(Entry entry) {
    entry.cancelDeadline();
    call(WITHDRAW, entry.key);
    entries.remove(entry);
  }

  private void prune() {
    for (Entry entry : new ArrayList<>(entries)) if (!entry.lease.current()) withdraw(entry);
  }

  private int state(Entry entry) {
    byte[] snapshot = call(SNAPSHOT, entry.key);
    return snapshot == null ? -1 : ByteBuffer.wrap(snapshot).getInt(32);
  }

  private void update(Entry entry) {
    int state = state(entry);
    if (state < 0) return;
    if (state != 1) entry.cancelDeadline();
    byte[] can = call(CAN_REQUEST, entry.key);
    entry.lease.state(state, can != null && can.length == 1 && can[0] == 1);
  }

  @Override
  public synchronized byte[] capabilities() {
    return call(CAPS, EMPTY);
  }

  @Override
  public synchronized byte[] validateImport(byte[] request) {
    return call(VALIDATE, request);
  }

  @Override
  public synchronized byte[] reserve() {
    prune();
    return call(RESERVE, EMPTY);
  }

  @Override
  public synchronized boolean attached(Socket socket, byte[] key) {
    if (closed
        || key == null
        || key.length != 24
        || find(key) != null
        || entries.size() >= 4
        || call(SNAPSHOT, key) == null) return false;
    byte[] stable = key.clone();
    Lease lease = binder.bind(socket, () -> request(stable));
    if (lease == null) return false;
    Entry entry = new Entry(stable, lease);
    entries.add(entry);
    if (!lease.current()) {
      withdraw(entry);
      return false;
    }
    return true;
  }

  @Override
  public synchronized void abandon(byte[] key) {
    Entry entry = find(key);
    if (entry != null) withdraw(entry);
    else call(WITHDRAW, key);
  }

  @Override
  public synchronized void released(byte[] key) {
    call(RELEASED, key);
    Entry entry = find(key);
    if (entry == null) return;
    int state = state(entry);
    // A known idle/failed lease has no late OS result to await. Running and
    // uncertain operations retain only this lightweight metadata after USB EOF.
    if (!entry.lease.current() || state != 1 && state != 7) withdraw(entry);
    else update(entry);
  }

  @Override
  public synchronized byte[] synchronize(byte[] report) {
    prune();
    byte[] reply = call(POLL, report);
    if (reply == null) return null;
    byte[] key = Arrays.copyOf(reply, 24);
    Entry entry = find(key);
    if (entry != null) {
      if (state(entry) == 2) {
        if (entry.lease.applySafe()) call(ACK_SAFE, key);
        else call(WITHDRAW, key);
        entry.cancelDeadline();
        entries.remove(entry);
        return call(POLL, report); // Retire, including retransmission of a lost acknowledgement.
      }
      update(entry);
    }
    return reply;
  }

  private synchronized boolean request(byte[] key) {
    Entry entry = find(key);
    if (entry == null || !entry.lease.current()) return false;
    byte[] sequence = call(REQUEST, key);
    if (sequence == null) return false;
    entry.cancelDeadline();
    entry.deadline =
        timer.schedule(() -> expired(entry, sequence), timeoutMillis, TimeUnit.MILLISECONDS);
    update(entry);
    return true;
  }

  private synchronized void expired(Entry entry, byte[] sequence) {
    if (closed || !entries.contains(entry) || !entry.lease.current()) return;
    byte[] snapshot = call(SNAPSHOT, entry.key);
    if (snapshot == null || !Arrays.equals(sequence, Arrays.copyOfRange(snapshot, 24, 32))) return;
    call(TIMEOUT, entry.key);
    update(entry);
  }

  @Override
  public synchronized void close() {
    if (closed) return;
    closed = true;
    timer.shutdownNow();
    for (Entry entry : entries) entry.lease.state(7, false);
    entries.clear();
    model.close();
  }
}
