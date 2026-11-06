package org.cgutman.usbip.connection;

/** One immutable observation bound to the current saved destination and owner. */
public final class ConnectionObservation {
  public static final class State {
    private final Object owner;
    private final String profile;
    public final String phase, failure;
    public final boolean active, receiverVerified;

    private State(Object owner, String profile, String phase, String failure,
        boolean active, boolean receiverVerified) {
      this.owner = owner;
      this.profile = profile;
      this.phase = phase;
      this.failure = failure;
      this.active = active;
      this.receiverVerified = receiverVerified;
    }
  }

  private static final State UNKNOWN = new State(null, "", "idle", null, false, false);
  private static State current = UNKNOWN;

  private ConnectionObservation() {}

  public static synchronized State forProfile(String profile) {
    return profile != null && !profile.isEmpty() && current.profile.equals(profile) ? current : UNKNOWN;
  }

  static synchronized void begin(Object owner, String profile) {
    current = new State(owner, profile, "connecting", null, true, false);
  }

  static synchronized void event(Object owner, String phase, String failure, boolean verified) {
    if (current.owner != owner || !current.active) return;
    current = new State(owner, current.profile, failure == null ? phase : "idle", failure,
        failure == null, failure == null && phase.equals("connected") && verified);
  }

  static synchronized void rebind(Object owner, String before, String after) {
    if (current.owner != owner || !current.profile.equals(before)) return;
    current = new State(owner, after, current.phase, current.failure,
        current.active, current.receiverVerified);
  }

  static synchronized void close(Object owner) {
    if (current.owner != owner) return;
    current = new State(owner, current.profile, "idle", current.failure, false, false);
  }
}
