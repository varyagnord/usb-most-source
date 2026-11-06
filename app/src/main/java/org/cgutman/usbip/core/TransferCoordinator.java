package org.cgutman.usbip.core;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.HashMap;
import java.util.Map;
import org.cgutman.usbip.server.protocol.dev.*;

/** Session-local state machine; Sink queues writes without blocking this monitor. */
public final class TransferCoordinator {
  public interface Sink {
    void send(UsbIpDevicePacket packet);

    void fault(String reason);

    void event(String event);
  }

  private static final int MAX_PENDING = 64;
  private static final long BULK_IN_MS = 60_000, COMMAND_MS = 15_000, CANCEL_MS = 3_000;
  private final Map<Integer, Pending> pending = new HashMap<>();
  private final UsbBackend backend;
  private final Sink sink;
  private boolean closed;

  private static final class Pending {
    final UsbIpSubmitUrb request;
    final long started, deadline;
    UsbIpUnlinkUrb unlink;
    long cancelAt;
    boolean timedOut;

    Pending(UsbIpSubmitUrb r, long now, long timeout) {
      request = r;
      started = now;
      deadline = timeout == 0 ? Long.MAX_VALUE : now + timeout;
    }
  }

  public TransferCoordinator(UsbBackend backend, Sink sink) {
    this.backend = backend;
    this.sink = sink;
  }

  public synchronized void submit(UsbIpSubmitUrb r, int type, int endpoint, long now) {
    if (closed) return;
    if (pending.containsKey(r.seqNum) || pending.size() >= MAX_PENDING) {
      fail("duplicate_sequence_or_queue_limit");
      return;
    }
    long timeout =
        type == 1 && r.direction == 1 ? 0 : type == 3 && r.direction == 1 ? BULK_IN_MS : COMMAND_MS;
    Pending p = new Pending(r, now, timeout);
    pending.put(r.seqNum, p);
    sink.event("submit seq=" + r.seqNum + " ep=" + endpoint + " length=" + r.transferBufferLength);
    int result = backend.submit(r, type, endpoint);
    if (r.outData != null) {
      Arrays.fill(r.outData, (byte) 0);
      r.outData = null;
    }
    if (result != 0) {
      pending.remove(r.seqNum);
      sink.send(reply(r, result, 0, null));
      if (result == -19 || result == -5) fail("usb_submit_" + result);
    }
  }

  public synchronized void immediate(UsbIpSubmitUrb r, int status) {
    if (closed) return;
    if (pending.containsKey(r.seqNum)) {
      fail("duplicate_sequence");
      return;
    }
    sink.send(reply(r, status, 0, null));
    if (r.outData != null) {
      Arrays.fill(r.outData, (byte) 0);
      r.outData = null;
    }
  }

  public synchronized void unlink(UsbIpUnlinkUrb unlink, long now) {
    if (closed) return;
    Pending p = pending.get(unlink.seqNumToUnlink);
    if (p == null) {
      sendUnlink(unlink, 0);
      return;
    }
    if (p.unlink != null || p.timedOut) {
      sendUnlink(unlink, -2);
      return;
    }
    int result = backend.cancel(p.request.seqNum);
    if (result != 0) {
      sendUnlink(unlink, result);
      return;
    }
    p.unlink = unlink;
    p.cancelAt = now;
    sink.event("cancel seq=" + p.request.seqNum);
    // ACK only after physical URB reaped; no old read can steal the next response.
  }

  public synchronized void completed(UsbBackend.Completion c, long now) {
    if (closed) return;
    if (c.disconnected) {
      fail("usb_transport_" + c.status);
      return;
    }
    Pending p = pending.remove(c.id);
    if (p == null) {
      fail("unknown_usb_completion");
      return;
    }
    sink.event(
        "complete seq="
            + c.id
            + " status="
            + c.status
            + " actual="
            + c.actualLength
            + " elapsed_ms="
            + (now - p.started));
    if (p.unlink != null) {
      sendUnlink(p.unlink, -104);
      return;
    }
    int status = p.timedOut ? -110 : c.status;
    if (c.actualLength < 0
        || c.actualLength > p.request.transferBufferLength
        || (p.request.direction == 1
            && c.actualLength > 0
            && (c.data == null || c.data.length < c.actualLength))) {
      fail("invalid_usb_completion_length");
      return;
    }
    sink.send(reply(p.request, status, c.actualLength, c.data));
    if (p.timedOut || backend.isFatalCompletion(c)) fail("usb_request_" + status);
  }

  public synchronized void tick(long now) {
    if (closed) return;
    for (Pending p : new ArrayList<>(pending.values())) {
      if (p.unlink != null || p.timedOut) {
        if (now - p.cancelAt >= CANCEL_MS) {
          fail("usb_cancel_not_completed");
          return;
        }
      } else if (now >= p.deadline) {
        int result = backend.cancel(p.request.seqNum);
        if (result == 0) {
          p.timedOut = true;
          p.cancelAt = now;
          sink.event("deadline seq=" + p.request.seqNum);
        } else if (result != -2 && result != -22) {
          fail("usb_cancel_" + result);
          return;
        } else if (now - p.deadline >= CANCEL_MS) {
          fail("usb_completion_missing");
          return;
        }
      }
    }
  }

  public synchronized void close() {
    closed = true;
    pending.clear();
  }

  public synchronized int pendingCount() {
    return pending.size();
  }

  private void fail(String reason) {
    closed = true;
    sink.fault(reason);
  }

  private void sendUnlink(UsbIpUnlinkUrb r, int status) {
    UsbIpUnlinkUrbReply out = new UsbIpUnlinkUrbReply(r.seqNum, 0, 0, 0);
    out.status = status;
    sink.send(out);
  }

  public static UsbIpSubmitUrbReply reply(UsbIpSubmitUrb r, int status, int actual, byte[] data) {
    UsbIpSubmitUrbReply out = new UsbIpSubmitUrbReply(r.seqNum, 0, 0, 0);
    out.status = status;
    out.actualLength = actual;
    out.inData = r.direction == 1 ? data : null;
    return out;
  }
}
