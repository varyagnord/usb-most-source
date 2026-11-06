package org.cgutman.usbip.connection;

/** Delayed callbacks must never attach success or errors to another computer. */
public final class ConnectionObservationTests {
  private static int checks;
  private static void check(boolean ok, String message) {
    checks++;
    if (!ok) throw new AssertionError(message);
  }
  public static void main(String[] args) {
    Object first = new Object(), second = new Object();
    check(!ConnectionObservation.forProfile(null).active, "null configuration");
    ConnectionObservation.begin(first, "first");
    ConnectionObservation.event(first, "connected", null, true);
    check(ConnectionObservation.forProfile("first").receiverVerified, "live receiver evidence");
    check(!ConnectionObservation.forProfile("second").receiverVerified, "no cross-computer state");
    ConnectionObservation.rebind(first, "wrong", "third");
    check(ConnectionObservation.forProfile("first").active, "rebind requires exact previous profile");
    ConnectionObservation.rebind(first, "first", "first-with-pin");
    check(!ConnectionObservation.forProfile("first").active, "old profile no longer matches");
    check(ConnectionObservation.forProfile("first-with-pin").receiverVerified, "trust persistence retains owner");
    ConnectionObservation.begin(second, "second");
    ConnectionObservation.event(first, "connected", null, true);
    ConnectionObservation.close(first);
    ConnectionObservation.rebind(first, "first-with-pin", "second");
    check(ConnectionObservation.forProfile("second").phase.equals("connecting"), "stale owner ignored");
    ConnectionObservation.event(second, "connected", "timeout", true);
    ConnectionObservation.State failed = ConnectionObservation.forProfile("second");
    check(!failed.active && !failed.receiverVerified && failed.phase.equals("idle"), "failure clears live success");
    ConnectionObservation.close(second);
    check(ConnectionObservation.forProfile("second").failure.equals("timeout"), "stopped failure remains diagnosable");
    ConnectionObservation.event(second, "connected", null, true);
    check(!ConnectionObservation.forProfile("second").active, "closed owner cannot publish delayed success");
    ConnectionObservation.begin(second, "second");
    check(ConnectionObservation.forProfile("second").failure == null, "explicit retry clears error");
    ConnectionObservation.event(second, "reconnecting", null, true);
    check(!ConnectionObservation.forProfile("second").receiverVerified, "reconnect invalidates receiver success");
    ConnectionObservation.close(second);
    System.out.println("PASS Connection observation " + checks + " checks");
  }
}
