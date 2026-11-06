import java.nio.ByteBuffer;
import java.util.*;
import java.util.concurrent.*;
import java.util.concurrent.atomic.AtomicInteger;
import org.cgutman.usbip.core.*;
import org.cgutman.usbip.server.protocol.dev.UsbIpSubmitUrb;

public final class DiagnosticBackendTests {
  static final AtomicInteger checks = new AtomicInteger();
  static void check(boolean test, String reason) {
    checks.incrementAndGet();
    if (!test) throw new AssertionError(reason);
  }

  static final class FakePort implements AndroidReferenceBackend.Port {
    final BlockingQueue<FakeToken> ready = new LinkedBlockingQueue<>();
    final List<FakeToken> created = new CopyOnWriteArrayList<>();
    final AtomicInteger controls = new AtomicInteger();
    volatile boolean automaticCancel = true;
    volatile CountDownLatch entered, release;
    volatile byte[] lastControl;
    volatile boolean shortControl, failControl;
    public AndroidReferenceBackend.Token create(int endpoint) {
      FakeToken token = new FakeToken(this);
      created.add(token);
      return token;
    }
    public AndroidReferenceBackend.Token waitRequest(int timeout) throws TimeoutException {
      try {
        FakeToken token = ready.poll(timeout, TimeUnit.MILLISECONDS);
        if (token == null) throw new TimeoutException();
        token.retired = true;
        return token;
      } catch (InterruptedException e) {
        Thread.currentThread().interrupt();
        throw new IllegalStateException(e);
      }
    }
    public int control(int type, int req, int value, int index, byte[] data, int timeout) {
      controls.incrementAndGet();
      check(timeout == 1500, "bounded native control timeout");
      if (entered != null) entered.countDown();
      if (release != null) try { release.await(); } catch (InterruptedException e) {
        Thread.currentThread().interrupt();
        throw new IllegalStateException(e);
      }
      lastControl = data.clone();
      if ((type & 128) != 0) Arrays.fill(data, (byte) 0x5a);
      return failControl ? -1 : shortControl ? Math.max(0, data.length - 1) : data.length;
    }
  }

  static final class FakeToken implements AndroidReferenceBackend.Token {
    final FakePort port;
    volatile ByteBuffer buffer;
    volatile boolean retired, closed;
    FakeToken(FakePort port) { this.port = port; }
    public boolean queue(ByteBuffer bytes) { buffer = bytes; return true; }
    public boolean cancel() {
      if (port.automaticCancel) finish(new byte[0]);
      return true;
    }
    void finish(byte[] data) { buffer.clear(); buffer.put(data); port.ready.add(this); }
    public void close() {
      check(retired, "request memory freed only after confirmed retirement");
      closed = true;
    }
  }

  static UsbIpSubmitUrb request(int id, int length, boolean input, boolean control) {
    byte[] header = new byte[20];
    UsbIpSubmitUrb r = new UsbIpSubmitUrb(header);
    r.seqNum = id;
    r.direction = input ? 1 : 0;
    r.ep = control ? 0 : 3;
    r.transferBufferLength = length;
    r.setup = control ? new byte[] {(byte) (input ? 0xa1 : 0x21), 9, 0, 2, 2, 0, (byte) length, (byte) (length >> 8)} : null;
    r.outData = input ? null : new byte[length];
    return r;
  }

  static UsbBackend.Completion completion(AndroidReferenceBackend backend) {
    long end = System.nanoTime() + TimeUnit.SECONDS.toNanos(2);
    while (System.nanoTime() < end) {
      UsbBackend.Completion c = backend.reap(100);
      if (c != null) return c;
    }
    throw new AssertionError("completion did not arrive");
  }

  static void burstAndCopy() {
    FakePort port = new FakePort();
    AtomicInteger observed = new AtomicInteger();
    AndroidReferenceBackend backend = new AndroidReferenceBackend(port, (id, ep, status, data, digest) -> {
      check(HidObservation.digest(data) == digest, "direct buffer copy verified");
      observed.incrementAndGet();
    });
    for (int id = 0; id < 64; id++)
      check(backend.submit(request(id, 32, true, false), 1, 0x83) == 0, "interrupt accepted");
    check(backend.submit(request(0, 32, true, false), 1, 0x83) == -17, "duplicate rejected");
    check(backend.submit(request(100, 32, true, false), 1, 0x83) == -28, "queue bounded");
    for (int i = 0; i < 64; i++) port.created.get(i).finish(new byte[] {0x20, 1, 2, (byte) i});
    for (int i = 0; i < 64; i++) {
      UsbBackend.Completion c = completion(backend);
      check(c.id == i && c.actualLength == 4 && c.status == 0 && c.data[3] == (byte) i,
          "ordered independent completions");
      check(port.created.get(i).closed, "closed retired request");
      check(port.created.get(i).buffer.get(0) == 0, "retired native buffer wiped");
    }
    check(observed.get() == 64, "all source observations delivered");
    backend.close();
    check(backend.canReleaseConnection(), "normal close releases connection");
    backend.close();
  }

  static void cancellationIsNotEarlyCompletion() throws Exception {
    FakePort port = new FakePort();
    port.automaticCancel = false;
    AndroidReferenceBackend backend = new AndroidReferenceBackend(port, null);
    check(backend.submit(request(1, 32, true, false), 1, 0x83) == 0, "cancel fixture submitted");
    check(backend.cancel(1) == 0, "cancel accepted");
    check(backend.reap(20) == null, "cancel is not premature retirement");
    check(!port.created.get(0).closed, "live request remains allocated");
    port.created.get(0).finish(new byte[0]);
    UsbBackend.Completion c = completion(backend);
    check(c.id == 1 && c.status == -104, "cancellation emitted after physical completion");
    check(backend.cancel(1) == -2, "retired id unavailable");
    backend.close();
    check(backend.canReleaseConnection(), "cancelled close safe");
  }

  static void controlNotReplayedOrAckedEarly() throws Exception {
    FakePort port = new FakePort();
    port.entered = new CountDownLatch(1);
    port.release = new CountDownLatch(1);
    AndroidReferenceBackend backend = new AndroidReferenceBackend(port, null);
    UsbIpSubmitUrb r = request(7, 7, false, true);
    Arrays.fill(r.outData, (byte) 0x61);
    check(backend.submit(r, 2, 0) == 0, "control accepted");
    Arrays.fill(r.outData, (byte) 0);
    check(port.entered.await(1, TimeUnit.SECONDS), "native control started");
    check(backend.cancel(7) == 0, "in-progress control cancellation accepted");
    check(backend.reap(20) == null, "control cannot ack before synchronous I/O returns");
    port.release.countDown();
    UsbBackend.Completion c = completion(backend);
    check(c.status == -104, "completed cancelled control");
    check(port.controls.get() == 1, "OUT command executed exactly once");
    check(Arrays.equals(port.lastControl, new byte[] {97,97,97,97,97,97,97}), "OUT copied before caller zeroization");
    backend.close();
    check(backend.canReleaseConnection(), "control worker stopped");
  }

  static void observerAndBounds() {
    FakePort port = new FakePort();
    AndroidReferenceBackend backend = new AndroidReferenceBackend(port, (a,b,c,d,e) -> { throw new IllegalStateException("diagnostic failure"); });
    check(backend.submit(request(1, 32, true, false), 1, 0x83) == 0, "observer fixture");
    port.created.get(0).finish(new byte[] {1});
    check(completion(backend).status == 0, "observer failure does not fail USB");
    check(backend.submit(request(2, 32, true, false), 3, 0x83) == -95, "storage/bulk not a reference backend target");
    check(backend.submit(request(3, 16385, true, false), 1, 0x83) == -22, "API26 async length bound");
    UsbIpSubmitUrb bad = request(4, 7, true, true); bad.setup[0] = 0x21;
    check(backend.submit(bad, 2, 128) == -22, "control direction independent validation");
    check(backend.submit(request(5, 32, true, false), 1, 0x93) == -22, "reserved endpoint bits refused");
    UsbIpSubmitUrb shortRequest = request(6, 7, true, true);
    shortRequest.transferFlags = 1;
    port.shortControl = true;
    check(backend.submit(shortRequest, 2, 0x80) == 0, "short control accepted");
    UsbBackend.Completion shortResult = completion(backend);
    check(shortResult.status == -121 && shortResult.actualLength == 6, "SHORT_NOT_OK applies to control");
    for (int i = 0; i < 256; i++) {
      byte[] a = new byte[15], b = new byte[15];
      a[0] = b[0] = 0x20; a[1] = b[1] = 1; a[2] = b[2] = 2; a[3] = b[3] = (byte) i;
      Arrays.fill(b, 4, 15, (byte) 0xff);
      check(Arrays.equals(HidObservation.classify(a), HidObservation.classify(b)), "coordinates never enter exported metadata");
      a[2] = 1;
      check(Arrays.equals(HidObservation.classify(a), new int[3]), "keyboard never enters metadata");
    }
    check(HidObservation.digest("hello".getBytes(java.nio.charset.StandardCharsets.UTF_8)) == 0xa430d84680aabd0bL,
        "independent digest matches Rust vector");
    backend.close();
  }

  static byte[] reply(int seq, byte[] data) {
    ByteBuffer p = ByteBuffer.allocate(48 + data.length);
    p.putInt(0, 3); p.putInt(4, seq); p.putInt(24, data.length);
    p.position(48); p.put(data);
    return p.array();
  }

  static void traceDetectsCorruptionWithoutExportingDigests() {
    byte[] original = new byte[15];
    original[0] = 0x20; original[1] = 1; original[2] = 2; original[3] = 1;
    HidTrace trace = new HidTrace(1, 0, () -> 1000);
    trace.nativeClock(1000);
    trace.submitted(1, 1, 0x83);
    byte[] corrupted = original.clone(); corrupted[4] = 42;
    UsbBackend.Completion c = new UsbBackend.Completion(1, 0, 15, corrupted, false);
    long digest = HidObservation.digest(original);
    long[] nativeRecord = {1,9,1,1,1,0,1,0,15,0,1,1,1,digest,0x83};
    trace.nativeBatch(nativeRecord, c);
    check(Arrays.equals(nativeRecord, new long[nativeRecord.length]), "transient native digests erased");
    trace.javaCompleted(c);
    trace.unlink(50, 1);
    byte[] failedUnlink = new byte[48];
    ByteBuffer.wrap(failedUnlink).putInt(0, 4).putInt(4, 50).putInt(20, -5);
    trace.tcpWritten(failedUnlink);
    check(trace.json().contains("\"pending\":1"), "failed unlink retains pending observation");
    byte[] wire = reply(1, corrupted); wire[52] ^= 1;
    trace.tcpWritten(wire);
    String json = trace.json();
    check(json.contains("\"jni_mismatches\":1"), "coordinate-byte JNI corruption detected");
    check(json.contains("\"wire_mismatches\":1"), "serialization-byte corruption detected");
    check(json.contains("\"classifier_mismatches\":0"), "copy check independent of safe metadata");
    check(!json.contains(Long.toString(digest)), "native digest never exported");
    for (int i = 2; i < 600; i++) {
      byte[] data = original.clone(); data[3] = (byte) (i & 1);
      trace.submitted(i, 1, 0x83);
      trace.javaCompleted(new UsbBackend.Completion(i, 0, data.length, data, false));
      trace.tcpWritten(reply(i, data));
    }
    check(trace.json().length() < 100000, "event export bounded under sustained changes");
    trace.submitted(1000, 1, 0x83);
    trace.closed();
    check(trace.json().contains("\"pending_at_close\":1") && trace.json().contains("\"pending\":0"),
        "close preserves count and erases pending digest references");
  }

  static void closingDoesNotExecuteQueuedControls() throws Exception {
    FakePort port = new FakePort();
    port.entered = new CountDownLatch(1); port.release = new CountDownLatch(1);
    AndroidReferenceBackend backend = new AndroidReferenceBackend(port, null);
    check(backend.submit(request(1, 7, false, true), 2, 0) == 0, "close fixture control");
    check(port.entered.await(1, TimeUnit.SECONDS), "control active before stop");
    for (int i = 2; i < 10; i++) check(backend.submit(request(i, 7, false, true), 2, 0) == 0, "queued control");
    Thread close = new Thread(backend::close); close.start();
    Thread.sleep(30);
    check(close.isAlive() && port.controls.get() == 1, "close waits for in-flight control");
    port.release.countDown(); close.join(3000);
    check(!close.isAlive() && backend.canReleaseConnection(), "close fully drains before fd release");
    check(port.controls.get() == 1, "queued controls never executed during teardown");
  }

  static void unretiredRequestsAreQuarantined() {
    FakePort port = new FakePort(); port.automaticCancel = false;
    AndroidReferenceBackend backend = new AndroidReferenceBackend(port, null);
    check(backend.submit(request(1, 32, true, false), 1, 0x83) == 0, "unretired fixture");
    backend.close();
    check(!backend.canReleaseConnection() && AndroidReferenceBackend.isQuarantined(), "unknown retirement retains Android connection");
    FakeToken token = port.created.get(0);
    check(!token.closed, "unretired token never freed");
    token.finish(new byte[] {0x55});
    check(token.buffer.get(0) == 0x55, "late fake-kernel write remains valid after close");
    try {
      new AndroidReferenceBackend(new FakePort(), null);
      throw new AssertionError("quarantined backend reopened");
    } catch (IllegalStateException expected) { check(true, "fail closed until process restart"); }
  }

  static void optionalDescriptorFailureKeepsSdkSession() {
    FakePort port = new FakePort();
    AndroidReferenceBackend backend = new AndroidReferenceBackend(port, null);
    List<byte[]> replies = new ArrayList<>();
    List<String> faults = new ArrayList<>();
    TransferCoordinator coordinator = new TransferCoordinator(backend, new TransferCoordinator.Sink() {
      public void send(org.cgutman.usbip.server.protocol.dev.UsbIpDevicePacket p) { replies.add(p.serialize()); }
      public void fault(String reason) { faults.add(reason); }
      public void event(String event) { }
    });
    try {
      port.failControl = true;
      coordinator.submit(request(501, 10, true, true), 2, 0x80, 0);
      UsbBackend.Completion failed = completion(backend);
      check(failed.status == -5 && !failed.disconnected, "SDK preserves generic completed-control error");
      coordinator.completed(failed, 1);
      check(faults.isEmpty() && replies.size() == 1, "optional descriptor error does not close SDK session");
      check(ByteBuffer.wrap(replies.get(0)).getInt(20) == -5, "error delivered without pretending success/STALL");
      port.failControl = false;
      coordinator.submit(request(502, 18, true, true), 2, 0x80, 2);
      coordinator.completed(completion(backend), 3);
      check(replies.size() == 2 && ByteBuffer.wrap(replies.get(1)).getInt(20) == 0,
          "next descriptor succeeds on same connection");
      check(port.controls.get() == 2, "neither failed nor successful control replayed");
      UsbBackend ordinary = new UsbBackend() {
        public int submit(UsbIpSubmitUrb r, int t, int ep) { return 0; }
        public int cancel(int id) { return 0; }
      };
      check(ordinary.isFatalCompletion(failed), "ordinary usbfs EIO policy unchanged");
      check(backend.isFatalCompletion(new UsbBackend.Completion(0, -5, 0, null, true)),
          "ambiguous SDK connection failure remains fatal");
      coordinator.completed(new UsbBackend.Completion(0, -5, 0, null, true), 4);
      check(!faults.isEmpty(), "SDK disconnected event still closes coordinator");
    } finally { coordinator.close(); backend.close(); }
    check(backend.canReleaseConnection(), "negative descriptor teardown fully retired");
  }

  public static void main(String[] args) throws Exception {
    burstAndCopy();
    cancellationIsNotEarlyCompletion();
    controlNotReplayedOrAckedEarly();
    observerAndBounds();
    traceDetectsCorruptionWithoutExportingDigests();
    closingDoesNotExecuteQueuedControls();
    optionalDescriptorFailureKeepsSdkSession();
    // This intentionally poisons the synthetic process and must remain last.
    unretiredRequestsAreQuarantined();
    System.out.println("PASS diagnostic SDK ownership/cancellation/privacy: " + checks + " checks");
  }
}
