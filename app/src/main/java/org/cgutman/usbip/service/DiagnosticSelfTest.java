package org.cgutman.usbip.service;

import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.*;
import java.util.concurrent.*;
import java.util.concurrent.atomic.AtomicInteger;
import org.cgutman.usbip.core.*;
import org.cgutman.usbip.jni.NativeUsb;
import org.cgutman.usbip.server.protocol.dev.UsbIpSubmitUrb;

/** Finite synthetic self-test. Does not open, enumerate or touch any USB device. */
final class DiagnosticSelfTest {
  private final AtomicInteger checks = new AtomicInteger();
  private void check(boolean value) { checks.incrementAndGet(); if (!value) throw new IllegalStateException("selftest_assertion"); }

  private static final class Port implements AndroidReferenceBackend.Port {
    final BlockingQueue<Token> ready = new LinkedBlockingQueue<>();
    final List<Token> tokens = new ArrayList<>();
    final CountDownLatch entered = new CountDownLatch(1), release = new CountDownLatch(1);
    final AtomicInteger controls = new AtomicInteger();
    volatile boolean failControl;
    public AndroidReferenceBackend.Token create(int endpoint) { Token t = new Token(this); tokens.add(t); return t; }
    public AndroidReferenceBackend.Token waitRequest(int timeout) throws TimeoutException {
      try {
        Token token = ready.poll(timeout, TimeUnit.MILLISECONDS);
        if (token == null) throw new TimeoutException();
        token.retired = true;
        return token;
      } catch (InterruptedException e) { Thread.currentThread().interrupt(); throw new IllegalStateException(e); }
    }
    public int control(int type, int request, int value, int index, byte[] bytes, int timeout) {
      controls.incrementAndGet(); entered.countDown();
      try { if (!release.await(1, TimeUnit.SECONDS)) return -1; }
      catch (InterruptedException e) { Thread.currentThread().interrupt(); return -1; }
      return failControl ? -1 : bytes.length;
    }
  }
  private static final class Token implements AndroidReferenceBackend.Token {
    final Port port;
    ByteBuffer buffer;
    boolean retired;
    Token(Port port) { this.port = port; }
    public boolean queue(ByteBuffer buffer) { this.buffer = buffer; return true; }
    public boolean cancel() { return true; }
    void finish(byte[] bytes) { buffer.clear(); buffer.put(bytes); port.ready.add(this); }
    public void close() { if (!retired) throw new IllegalStateException("premature_free"); }
  }
  private static UsbIpSubmitUrb request(int id, int length, boolean control) {
    UsbIpSubmitUrb r = new UsbIpSubmitUrb(new byte[20]);
    r.seqNum = id; r.direction = control ? 0 : 1; r.transferBufferLength = length;
    if (control) { r.setup = new byte[] {0x21, 9, 0, 2, 2, 0, (byte) length, 0}; r.outData = new byte[length]; }
    return r;
  }
  private UsbBackend.Completion await(AndroidReferenceBackend backend) {
    long end = System.nanoTime() + TimeUnit.SECONDS.toNanos(2);
    while (System.nanoTime() < end) { UsbBackend.Completion c = backend.reap(100); if (c != null) return c; }
    throw new IllegalStateException("completion_timeout");
  }
  private static UsbIpSubmitUrb descriptor(int id, int length, int type) {
    UsbIpSubmitUrb r = request(id, length, true);
    r.direction = 1;
    r.outData = null;
    r.setup = new byte[] {(byte) 0x80, 6, 0, (byte) type, 0, 0, (byte) length, 0};
    return r;
  }
  private void execute() throws InterruptedException {
    for (int length = 0; length <= 32; length++) for (int seed = 0; seed < 8; seed++) {
      byte[] input = new byte[length];
      for (int i = 0; i < length; i++) input[i] = (byte) (i * 37 + seed * 61);
      UsbBackend.Completion c = NativeUsb.diagnosticRoundTrip(input);
      check(c != null && c.id == 1 && c.status == 0 && !c.disconnected && c.actualLength == length);
      check(c.data != input && Arrays.equals(c.data, input));
      if (length != 0) { c.data[0] ^= 1; check(!Arrays.equals(c.data, input)); }
      Arrays.fill(input, (byte) 0); Arrays.fill(c.data, (byte) 0);
    }
    for(int rtt : new int[] {0,20,50,100,200,350,500,1000,2000}) {
      UsbBackend.Completion c=NativeUsb.wanSelfTest(rtt,100,50,256);
      check(c!=null && !c.disconnected && c.wanEpoch==1 && c.wanSequence==1 && c.actualLength==32);
      ByteBuffer stats=ByteBuffer.wrap(c.data);
      check(stats.getLong()==256 && stats.getLong()==256);
      check(stats.getLong()<=40000 && stats.getLong()<2048);
      Arrays.fill(c.data,(byte)0);
    }
    for(int rtt : new int[] {0,100,500,1000,2000}) {
      long[] stats=NativeUsb.wanNetworkTest(rtt,5,16);
      check(stats!=null && stats.length==5 && stats[0]==16 && stats[2]<45000);
      check(rtt<100 || stats[1]==1);
    }
    check(NativeUsb.diagnosticRoundTrip(null) == null);
    check(NativeUsb.diagnosticRoundTrip(new byte[33]) == null);
    Port port = new Port();
    AtomicInteger copyMismatches = new AtomicInteger();
    AndroidReferenceBackend backend = new AndroidReferenceBackend(port,
        (id, endpoint, status, data, digest) -> {
          checks.incrementAndGet();
          if (HidObservation.digest(data) != digest) copyMismatches.incrementAndGet();
        });
    try {
      for (int i = 0; i < 32; i++) check(backend.submit(request(i, 32, false), 1, 0x83) == 0);
      for (int i = 0; i < 32; i++) port.tokens.get(i).finish(new byte[] {0x20, 1, 2, (byte) i});
      for (int i = 0; i < 32; i++) {
        UsbBackend.Completion c = await(backend);
        check(c.id == i && c.status == 0 && c.actualLength == 4 && c.data[3] == (byte) i);
      }
      check(backend.submit(request(100, 32, false), 1, 0x83) == 0);
      check(backend.cancel(100) == 0 && backend.reap(10) == null);
      port.tokens.get(32).finish(new byte[0]);
      check(await(backend).status == -104);
      check(backend.submit(request(101, 7, true), 2, 0) == 0);
      check(port.entered.await(1, TimeUnit.SECONDS));
      check(backend.cancel(101) == 0 && backend.reap(10) == null);
      port.release.countDown();
      check(await(backend).status == -104 && port.controls.get() == 1);
      List<Integer> statuses = new ArrayList<>();
      AtomicInteger faults = new AtomicInteger();
      TransferCoordinator coordinator = new TransferCoordinator(backend, new TransferCoordinator.Sink() {
        public void send(org.cgutman.usbip.server.protocol.dev.UsbIpDevicePacket p) {
          statuses.add(ByteBuffer.wrap(p.serialize()).getInt(20));
        }
        public void fault(String reason) { faults.incrementAndGet(); }
        public void event(String event) { }
      });
      port.failControl = true;
      coordinator.submit(descriptor(102, 10, 6), 2, 0x80, 0);
      UsbBackend.Completion failed = await(backend);
      check(failed.status == -5 && !failed.disconnected);
      coordinator.completed(failed, 1);
      check(faults.get() == 0 && statuses.size() == 1 && statuses.get(0) == -5);
      port.failControl = false;
      coordinator.submit(descriptor(103, 18, 1), 2, 0x80, 2);
      coordinator.completed(await(backend), 3);
      check(faults.get() == 0 && statuses.size() == 2 && statuses.get(1) == 0 && port.controls.get() == 3);
      UsbBackend.Completion terminal = new UsbBackend.Completion(0, -5, 0, null, true);
      check(backend.isFatalCompletion(terminal));
      coordinator.completed(terminal, 4);
      check(faults.get() == 1);
      coordinator.close();
      check(copyMismatches.get() == 0);
    } finally {
      port.release.countDown();
      // Explicitly retire any synthetic requests on failure before closing.
      for (Token token : port.tokens) if (!token.retired) token.finish(new byte[0]);
      backend.close();
    }
    check(backend.canReleaseConnection());
  }

  static byte[] run() {
    DiagnosticSelfTest test = new DiagnosticSelfTest();
    long start = System.nanoTime();
    String failure = null;
    try { test.execute(); }
    catch (InterruptedException e) { Thread.currentThread().interrupt(); failure = "InterruptedException"; }
    catch (RuntimeException | LinkageError | AssertionError e) { failure = e.getClass().getSimpleName(); }
    String json = "{\"selftest\":true,\"success\":" + (failure == null) + ",\"checks\":" + test.checks.get()
        + ",\"elapsed_ms\":" + TimeUnit.NANOSECONDS.toMillis(System.nanoTime() - start)
        + (failure == null ? "" : ",\"failure_class\":\"" + failure + "\"") + "}";
    return json.getBytes(StandardCharsets.UTF_8);
  }
}
