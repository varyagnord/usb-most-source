package org.cgutman.usbip.core;

import java.nio.ByteBuffer;
import java.util.*;
import java.util.concurrent.*;
import org.cgutman.usbip.server.protocol.dev.UsbIpSubmitUrb;

/** Diagnostic-only alternative to the Rust engine. One owner/reaper per USB fd.
 * Android-specific calls are behind Port, allowing real cancellation-race tests.
 * Only control and interrupt transfers are supported; never use for storage.
 */
public final class AndroidReferenceBackend implements UsbTransport {
  public interface Token {
    boolean queue(ByteBuffer buffer);
    boolean cancel();
    void close();
  }

  public interface Port {
    Token create(int endpoint);
    Token waitRequest(int timeoutMs) throws TimeoutException;
    int control(int type, int request, int value, int index, byte[] bytes, int timeoutMs);
  }

  public interface Observer {
    void retired(int id, int endpoint, int status, byte[] data, long originalDigest);
  }

  private static final int LIMIT = 64, CONTROL_TIMEOUT_MS = 1500;
  // On ambiguous retirement retain request objects AND buffers. Closing the
  // connection then cancels kernel I/O; never free request memory speculatively.
  private static final List<Object> QUARANTINE = new ArrayList<>();
  private static boolean quarantined;
  private final Object lock = new Object();
  private final Port port;
  private final Observer observer;
  private final Map<Integer, Request> pending = new HashMap<>();
  private final IdentityHashMap<Token, Request> asynchronous = new IdentityHashMap<>();
  private final BlockingQueue<Completion> completed = new ArrayBlockingQueue<>(LIMIT);
  private final ThreadPoolExecutor controls;
  private final Thread reader;
  private boolean closed, readerStop;
  private volatile boolean broken;

  private static final class Request {
    final int id, endpoint, length, flags;
    final boolean input;
    final byte[] setup, control;
    final ByteBuffer buffer;
    Token token;
    boolean cancelled;

    Request(UsbIpSubmitUrb r, int type, int endpoint) {
      id = r.seqNum;
      this.endpoint = endpoint;
      length = r.transferBufferLength;
      flags = r.transferFlags;
      input = r.direction == 1;
      setup = type == 2 ? r.setup.clone() : null;
      control = type == 2 ? new byte[length] : null;
      buffer = type == 1 ? ByteBuffer.allocateDirect(length) : null;
      if (!input && length != 0) {
        if (control != null) System.arraycopy(r.outData, 0, control, 0, length);
        else buffer.put(r.outData).flip();
      }
    }
  }

  public AndroidReferenceBackend(Port port, Observer observer) {
    synchronized (QUARANTINE) {
      if (quarantined) throw new IllegalStateException("SDK requests quarantined; restart process");
    }
    this.port = Objects.requireNonNull(port);
    this.observer = observer;
    controls = new ThreadPoolExecutor(1, 1, 0, TimeUnit.MILLISECONDS,
        new ArrayBlockingQueue<>(LIMIT), runnable -> new Thread(runnable, "usb-sdk-control"));
    reader = new Thread(this::readLoop, "usb-sdk-reader");
    reader.start();
  }

  public static boolean isQuarantined() {
    synchronized (QUARANTINE) { return quarantined; }
  }

  public static void quarantineResource(Object resource) {
    synchronized (QUARANTINE) { quarantined = true; QUARANTINE.add(resource); }
  }

  @Override
  public int submit(UsbIpSubmitUrb r, int type, int endpoint) {
    if (type != 1 && type != 2) return -95;
    int length = r.transferBufferLength;
    if (length < 0 || length > (type == 2 ? 65535 : 16384)
        || r.direction < 0 || r.direction > 1 || endpoint < 0 || endpoint > 255 || (endpoint & 0x70) != 0
        || (type == 2 && (r.setup == null || r.setup.length != 8))
        || (type == 1 && ((endpoint & 15) == 0 || ((endpoint & 128) != 0) != (r.direction == 1)))
        || (r.direction == 0 && length != 0 && (r.outData == null || r.outData.length != length)))
      return -22;
    if (type == 2 && (((r.setup[6] & 255) | ((r.setup[7] & 255) << 8)) != length
        || ((r.setup[0] & 128) != 0) != (r.direction == 1))) return -22;
    synchronized (lock) {
      if (closed || broken) return -19;
      if (pending.containsKey(r.seqNum)) return -17;
      if (pending.size() >= LIMIT) return -28;
      Request item = new Request(r, type, endpoint);
      pending.put(item.id, item);
      if (type == 2) {
        try {
          controls.execute(() -> runControl(item));
        } catch (RejectedExecutionException error) {
          pending.remove(item.id);
          wipe(item);
          return -28;
        }
      } else {
        try {
          item.token = port.create(endpoint);
          if (item.token == null || !item.token.queue(item.buffer)) {
            if (item.token != null) item.token.close();
            pending.remove(item.id);
            wipe(item);
            return -5;
          }
          asynchronous.put(item.token, item);
        } catch (RuntimeException error) {
          // queue throwing can be ambiguous: do not free a possibly borrowed buffer.
          broken = true;
          return -5;
        }
      }
      lock.notifyAll();
      return 0;
    }
  }

  private void runControl(Request item) {
    int result;
    synchronized (lock) {
      if (item.cancelled || closed) {
        retire(item, -104, 0, null, HidObservation.digest(null));
        return;
      }
    }
    try {
      byte[] s = item.setup;
      result = port.control(s[0] & 255, s[1] & 255,
          (s[2] & 255) | ((s[3] & 255) << 8),
          (s[4] & 255) | ((s[5] & 255) << 8), item.control, CONTROL_TIMEOUT_MS);
    } catch (RuntimeException error) {
      result = -1;
    }
    synchronized (lock) {
      int status = item.cancelled ? -104 : result < 0 ? -5 : 0;
      int actual = result < 0 ? 0 : result;
      if (actual > item.length) { status = -75; actual = 0; }
      if (status == 0 && (item.flags & 1) != 0 && actual < item.length) status = -121;
      byte[] data = item.input && actual > 0 ? Arrays.copyOf(item.control, actual) : null;
      retire(item, status, actual, data, HidObservation.digest(data));
    }
  }

  private void readLoop() {
    try {
      while (true) {
        synchronized (lock) {
          while (asynchronous.isEmpty() && !readerStop && !broken) lock.wait();
          if (readerStop || broken) return;
        }
        Token token;
        try { token = port.waitRequest(100); }
        catch (TimeoutException ignored) { continue; }
        synchronized (lock) {
          if (token == null || !asynchronous.containsKey(token)) {
            broken = true;
            lock.notifyAll();
            return;
          }
          Request item = asynchronous.remove(token);
          int actual = item.buffer.position();
          int status = item.cancelled ? -104 : 0;
          if (actual < 0 || actual > item.length) { status = -75; actual = 0; }
          if (status == 0 && (item.flags & 1) != 0 && actual < item.length) status = -121;
          byte[] data = null;
          long digest = HidObservation.digest(null);
          if (item.input && actual > 0) {
            // Compute before the Java copy; only the comparison result is exported.
            ByteBuffer input = item.buffer.duplicate();
            input.flip();
            long hash = 0xcbf29ce484222325L;
            for (int i = 0; i < actual; i++) hash = (hash ^ (input.get(i) & 255)) * 0x100000001b3L;
            digest = hash;
            data = new byte[actual];
            input.get(data);
          }
          token.close(); // Only after requestWait confirms physical retirement.
          retire(item, status, actual, data, digest);
        }
      }
    } catch (InterruptedException error) {
      Thread.currentThread().interrupt();
      broken = true;
    } catch (RuntimeException error) {
      // Ambiguous Android dequeue/overflow: preserve all outstanding allocations.
      broken = true;
    } finally {
      synchronized (lock) { lock.notifyAll(); }
    }
  }

  /** Called only after the physical operation has finished, under lock. */
  private void retire(Request item, int status, int actual, byte[] data, long digest) {
    pending.remove(item.id);
    if (observer != null) {
      try { observer.retired(item.id, item.endpoint, status, data, digest); }
      catch (RuntimeException ignored) { /* Diagnostics must not fail USB I/O. */ }
    }
    wipe(item);
    if (closed) {
      if (data != null) Arrays.fill(data, (byte) 0);
    } else if (!completed.offer(new Completion(item.id, status, actual, data, false))) {
      if (data != null) Arrays.fill(data, (byte) 0);
      broken = true;
    }
    lock.notifyAll();
  }

  /** Never call on a request whose retirement was not confirmed. */
  private static void wipe(Request item) {
    if (item.control != null) Arrays.fill(item.control, (byte) 0);
    if (item.setup != null) Arrays.fill(item.setup, (byte) 0);
    if (item.buffer != null) {
      ByteBuffer bytes = item.buffer.duplicate();
      bytes.clear();
      while (bytes.hasRemaining()) bytes.put((byte) 0);
    }
  }

  @Override
  public int cancel(int id) {
    synchronized (lock) {
      Request item = pending.get(id);
      if (item == null) return -2;
      if (item.cancelled) return 0;
      if (item.token != null && !item.token.cancel()) return -5;
      item.cancelled = true;
      // For a synchronous control, wait for controlTransfer to finish. No early ACK.
      return 0;
    }
  }

  @Override
  public boolean isFatalCompletion(Completion completion) {
    // controlTransfer collapses STALL, unsupported descriptors and other transfer
    // errors to -1. Keep the generic -EIO reply, without inventing an errno or
    // treating a physically completed control as proof of connection loss.
    // Ambiguous async dequeue/retirement still reports disconnected=true.
    return completion.disconnected || completion.status == -19;
  }

  @Override
  public Completion reap(int timeoutMs) {
    if (timeoutMs < 0 || timeoutMs > 100) throw new IllegalArgumentException("timeout");
    if (broken) return new Completion(0, -5, 0, null, true);
    try { return completed.poll(timeoutMs, TimeUnit.MILLISECONDS); }
    catch (InterruptedException error) {
      Thread.currentThread().interrupt();
      return new Completion(0, -5, 0, null, true);
    }
  }

  @Override
  public void close() {
    boolean interrupted = false;
    synchronized (lock) {
      if (closed) return;
      closed = true;
      for (Request item : pending.values()) {
        item.cancelled = true;
        if (item.token != null) {
          try { item.token.cancel(); } catch (RuntimeException ignored) { broken = true; }
        }
      }
    }
    controls.shutdown(); // Queued cancelled controls retire without executing USB commands.
    long deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(2);
    synchronized (lock) {
      while (!pending.isEmpty() && !broken && System.nanoTime() < deadline) {
        try { lock.wait(10); } catch (InterruptedException error) { interrupted = true; }
      }
      readerStop = true;
      lock.notifyAll();
      if (!pending.isEmpty()) {
        synchronized (QUARANTINE) {
          quarantined = true;
          QUARANTINE.addAll(pending.values());
        }
      }
    }
    long joinDeadline = System.nanoTime() + TimeUnit.MILLISECONDS.toNanos(500);
    while (reader.isAlive() && System.nanoTime() < joinDeadline) {
      try { reader.join(50); } catch (InterruptedException error) { interrupted = true; }
    }
    try { controls.awaitTermination(2, TimeUnit.SECONDS); }
    catch (InterruptedException error) { interrupted = true; }
    if (!canReleaseConnection()) quarantineResource(this);
    Completion done;
    while ((done = completed.poll()) != null) if (done.data != null) Arrays.fill(done.data, (byte) 0);
    if (interrupted) Thread.currentThread().interrupt();
  }

  @Override
  public boolean canReleaseConnection() {
    synchronized (lock) {
      return closed && pending.isEmpty() && !reader.isAlive() && controls.isTerminated();
    }
  }
}
