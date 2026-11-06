package org.cgutman.usbip.connection;

import android.content.Context;
import android.content.SharedPreferences;
import android.os.Handler;
import android.os.Looper;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.util.*;
import java.util.concurrent.*;
import java.util.concurrent.atomic.AtomicBoolean;
import org.cgutman.usbip.jni.NativeConnection;
import org.cgutman.usbip.jni.NativePolicy;
import org.json.*;

/**
 * One process-owned transaction queue. Presentation reads immutable, non-secret snapshots; only
 * editors and the owning service receive temporary plaintext. Startup, migration, mutations,
 * session acquisition and retirement share this queue. No Keystore or file work runs on the Android
 * main thread.
 */
public final class ComputerRepository {
  public enum Status {
    LOADING,
    READY,
    FAILED
  }

  public static final class Computer {
    public final String id, name, kind, profile, host, usbRules;
    public final long generation, usbEpoch;
    public final int mode;

    private Computer(JSONObject value) throws JSONException {
      id = value.getString("id");
      name = value.getString("name");
      generation = value.getLong("connection_generation");
      usbEpoch = value.getLong("usb_epoch");
      usbRules = value.getJSONArray("usb_rules").toString();
      JSONObject connection = value.getJSONObject("connection");
      kind = connection.getString("kind");
      profile = kind.equals("secure") ? connection.getJSONObject("profile").toString() : "";
      host = connection.optString("host", "");
      mode = connection.optInt("mode", -1);
    }
  }

  public static final class Snapshot {
    public final Status status;
    public final long revision, epoch;
    public final String activeId;
    public final List<Computer> computers;

    private Snapshot(Status status) {
      this.status = status;
      revision = epoch = -1;
      activeId = null;
      computers = Collections.emptyList();
    }

    private Snapshot(byte[] publicView) throws JSONException {
      JSONObject value = new JSONObject(new String(publicView, StandardCharsets.UTF_8));
      status = Status.READY;
      revision = value.getLong("revision");
      epoch = value.getLong("activation_epoch");
      activeId = value.isNull("active_id") ? null : value.getString("active_id");
      ArrayList<Computer> items = new ArrayList<>();
      JSONArray list = value.getJSONArray("computers");
      for (int i = 0; i < list.length(); i++) items.add(new Computer(list.getJSONObject(i)));
      computers = Collections.unmodifiableList(items);
    }

    public Computer active() {
      for (Computer c : computers) if (c.id.equals(activeId)) return c;
      return null;
    }

    public String connectionKey() {
      Computer active = active();
      return active == null ? "" : active.id + ":" + active.generation + ":" + epoch;
    }
  }

  /**
   * Identity of one service lifetime; not constructible or recoverable by a UI. close marks
   * cancellation immediately; release is called AFTER USB/TCP close. The storage queue remains
   * reserved until release, including while stopping.
   */
  public static final class Session {
    private final AtomicBoolean cancelled = new AtomicBoolean();
    private String lease; // queue-confined, updated only after a durable commit
    private volatile String connectionKey;
    private byte[] connection;

    private Session(String lease, String connectionKey, byte[] connection) {
      this.lease = lease;
      this.connectionKey = connectionKey;
      this.connection = connection;
    }

    public String connectionKey() {
      return connectionKey;
    }

    public void cancel() {
      cancelled.set(true);
    }

    public boolean cancelled() {
      return cancelled.get();
    }

    public synchronized byte[] takeConnection() {
      byte[] owned = connection;
      connection = null;
      return owned;
    }

    private synchronized void wipe() {
      if (connection != null) Arrays.fill(connection, (byte) 0);
      connection = null;
    }
  }

  private static ComputerRepository instance;

  public static synchronized ComputerRepository get(Context context) {
    if (instance == null) instance = new ComputerRepository(context.getApplicationContext());
    return instance;
  }

  private final Context context;
  private final ComputerVault vault;
  private final ExecutorService queue =
      Executors.newSingleThreadExecutor(r -> new Thread(r, "usb-most-computers"));
  private final Handler main = new Handler(Looper.getMainLooper());
  private final CopyOnWriteArrayList<Runnable> listeners = new CopyOnWriteArrayList<>();
  private volatile Snapshot snapshot = new Snapshot(Status.LOADING);
  private final CompletableFuture<Snapshot> ready;
  private Session owner; // only accessed on the storage queue
  private volatile boolean busy;

  private ComputerRepository(Context context) {
    this.context = context;
    vault = new ComputerVault(context);
    ready = submit(this::load);
  }

  public Snapshot snapshot() {
    return snapshot;
  }

  public boolean isBusy() {
    return busy;
  }

  public CompletableFuture<Snapshot> ready() {
    Snapshot current = snapshot;
    if (current.status == Status.LOADING) return ready;
    if (current.status == Status.READY) return CompletableFuture.completedFuture(current);
    CompletableFuture<Snapshot> failed = new CompletableFuture<>();
    failed.completeExceptionally(new IOException("computer_storage_unavailable"));
    return failed;
  }

  public void addListener(Runnable listener) {
    listeners.addIfAbsent(listener);
  }

  public void removeListener(Runnable listener) {
    listeners.remove(listener);
  }

  private void publish(Snapshot next) {
    snapshot = next;
    notifyListeners();
  }

  private void notifyListeners() {
    main.post(() -> {
      for (Runnable listener : listeners) listener.run();
    });
  }

  private <T> CompletableFuture<T> submit(Callable<T> task) {
    // Cancelling a Future cannot roll back a durable write or release a service.
    // Lifecycle cancellation uses Session.cancel/release explicitly.
    CompletableFuture<T> future =
        new CompletableFuture<T>() {
          @Override
          public boolean cancel(boolean mayInterruptIfRunning) {
            return false;
          }
        };
    queue.execute(() -> {
      try {
        future.complete(task.call());
      } catch (Exception failure) {
        future.completeExceptionally(failure);
      }
    });
    return future;
  }

  private static byte[] bytes(String value) {
    return value.getBytes(StandardCharsets.UTF_8);
  }

  private static byte[] query(byte[] data, JSONObject request, boolean running) {
    byte[] encoded = bytes(request.toString());
    try {
      return NativeConnection.computers(data, encoded, running);
    } finally {
      Arrays.fill(encoded, (byte) 0);
    }
  }

  private static Snapshot view(byte[] data) throws JSONException {
    byte[] publicView = query(data, new JSONObject().put("op", "view"), false);
    try {
      return new Snapshot(publicView);
    } finally {
      Arrays.fill(publicView, (byte) 0);
    }
  }

  private void requireReady() throws IOException {
    if (snapshot.status != Status.READY) throw new IOException("computer_storage_unavailable");
  }

  private byte[] read() throws Exception {
    byte[] data = null;
    try {
      data = vault.load();
      view(data);
      return data;
    } catch (Exception failure) {
      if (data != null) Arrays.fill(data, (byte) 0);
      publish(new Snapshot(Status.FAILED));
      throw failure;
    }
  }

  private Snapshot commit(byte[] candidate) throws Exception {
    Snapshot next = view(candidate);
    try {
      vault.save(candidate);
    } catch (Exception failure) {
      // Failure after rename is ambiguous. Never keep using an old in-memory view.
      publish(new Snapshot(Status.FAILED));
      throw failure;
    }
    publish(next);
    return next;
  }

  private Snapshot load() throws Exception {
    byte[] data = null;
    try {
      if (!vault.exists()) {
        data = migrate();
        view(data); // validate complete candidate before the first write
        vault.save(data);
        Arrays.fill(data, (byte) 0);
        data = null;
      }
      data = vault.load();
      Snapshot next = view(data);
      retireLegacy(); // also resumes a cleanup interrupted after a successful migration
      publish(next);
      return next;
    } catch (Exception failure) {
      publish(new Snapshot(Status.FAILED));
      throw failure;
    } finally {
      if (data != null) Arrays.fill(data, (byte) 0);
    }
  }

  private byte[] migrate() throws Exception {
    synchronized (SecretVault.LOCK) {
      SharedPreferences prefs = context.getSharedPreferences("bridge", Context.MODE_PRIVATE);
      if (prefs.getBoolean("secure_connection", false)) {
        byte[] old = new SecretVault(context).load();
        byte[] request = null;
        try {
          // Preserve the old plaintext as bytes; Rust checks exact binding to the
          // saved profile before parsing typed credentials. No secret JSON String.
          String prefix =
              new JSONObject()
                  .put("op", "migrate")
                  .put("name", "Мой компьютер")
                  .put("expected_profile", prefs.getString("secure_profile", ""))
                  .toString();
          byte[] head = bytes(prefix.substring(0, prefix.length() - 1) + ",\"old\":");
          request = new byte[head.length + old.length + 1];
          System.arraycopy(head, 0, request, 0, head.length);
          System.arraycopy(old, 0, request, head.length, old.length);
          request[request.length - 1] = '}';
          return NativeConnection.computers(new byte[0], request, false);
        } finally {
          Arrays.fill(old, (byte) 0);
          if (request != null) Arrays.fill(request, (byte) 0);
        }
      }
      byte[] empty = query(new byte[0], new JSONObject().put("op", "create"), false);
      String host = prefs.getString("server", "");
      int mode = prefs.getInt("network_mode", 0);
      if (!NativePolicy.validHost(host, mode)) return empty;
      try {
        return query(
            empty,
            new JSONObject()
                .put("op", "add")
                .put("revision", 0)
                .put("name", "Мой компьютер")
                .put("activate", true)
                .put(
                    "connection",
                    new JSONObject().put("kind", "legacy").put("host", host).put("mode", mode)),
            false);
      } finally {
        Arrays.fill(empty, (byte) 0);
      }
    }
  }

  private void retireLegacy() throws Exception {
    synchronized (SecretVault.LOCK) {
      new SecretVault(context).retire();
      SharedPreferences.Editor edit =
          context.getSharedPreferences("bridge", Context.MODE_PRIVATE).edit();
      for (String key :
          new String[] {
            "secure_connection",
            "secure_profile",
            "connection_client_id",
            "server",
            "network_mode",
            "draft_mode",
            "draft_0",
            "draft_1"
          }) edit.remove(key);
      if (!edit.commit()) throw new IOException("legacy_preferences_cleanup");
    }
  }

  /**
   * Caller-owned command is cloned before queuing and never retained afterwards. Only UI mutations
   * are accepted here; service trust/pairing use persist().
   */
  public CompletableFuture<Snapshot> change(byte[] command) {
    if (command == null || command.length > 144 * 1024)
      throw new IllegalArgumentException("catalog_request_too_large");
    byte[] owned = command.clone();
    return submit(() -> {
      byte[] data = null, candidate = null;
      try {
        requireReady();
        String op = new JSONObject(new String(owned, StandardCharsets.UTF_8)).getString("op");
        if (!Arrays.asList(
                "add", "replace", "rename", "activate", "forget", "set_usb_rule", "forget_usb_rule")
            .contains(op)) throw new IllegalArgumentException("invalid_catalog_request");
        data = read();
        candidate = NativeConnection.computers(data, owned, owner != null);
        return commit(candidate);
      } finally {
        Arrays.fill(owned, (byte) 0);
        if (data != null) Arrays.fill(data, (byte) 0);
        if (candidate != null) Arrays.fill(candidate, (byte) 0);
      }
    });
  }

  /** Caller must wipe the returned editor plaintext; it must never enter Bundle. */
  public CompletableFuture<byte[]> edit(String id, long revision) {
    return submit(() -> {
      requireReady();
      byte[] data = read();
      try {
        return query(
            data,
            new JSONObject().put("op", "edit").put("id", id).put("revision", revision),
            false);
      } finally {
        Arrays.fill(data, (byte) 0);
      }
    });
  }

  /**
   * Capture revision when the user requests start, not when a delayed callback eventually runs. The
   * caller closes its sockets BEFORE releasing the session.
   */
  public CompletableFuture<Session> begin(long revision) {
    return submit(() -> acquire(revision));
  }

  /**
   * A non-secret resume hint can only RESTRICT startup. It never overrides the catalog
   * identity/generation/epoch or supplies credentials after a failed load.
   */
  public CompletableFuture<Session> resume() {
    return submit(() -> {
      requireReady();
      String current = snapshot.connectionKey();
      if (current.isEmpty()
          || !current.equals(
              context
                  .getSharedPreferences("bridge", Context.MODE_PRIVATE)
                  .getString("computer_resume", "")))
        throw new IOException("connection_not_requested");
      return acquire(snapshot.revision);
    });
  }

  private void remember(String key) throws IOException {
    SharedPreferences.Editor edit =
        context.getSharedPreferences("bridge", Context.MODE_PRIVATE).edit();
    if (key == null) edit.remove("computer_resume");
    else edit.putString("computer_resume", key);
    if (!edit.commit()) throw new IOException("connection_resume_storage");
  }

  private Session acquire(long revision) throws Exception {
    requireReady();
    if (owner != null) throw new IllegalStateException("connection_running");
    busy = true;
    notifyListeners();
    byte[] data = null, candidate = null, resolved = null;
    try {
      data = read();
      candidate = query(data, new JSONObject().put("op", "begin").put("revision", revision), false);
      // Resolve before committing; allocation/format errors cannot reserve a
      // service with an unusable plaintext. Only the owner receives this data.
      resolved = query(candidate, new JSONObject().put("op", "active"), false);
      Snapshot next = view(candidate);
      Computer active = next.active();
      String lease =
          new JSONObject()
              .put("id", active.id)
              .put("connection_generation", active.generation)
              .put("activation_epoch", next.epoch)
              .toString();
      Session acquired = new Session(lease, next.connectionKey(), resolved);
      commit(candidate);
      remember(next.connectionKey());
      owner = acquired;
      resolved = null;
      return acquired;
    } finally {
      if (data != null) Arrays.fill(data, (byte) 0);
      if (candidate != null) Arrays.fill(candidate, (byte) 0);
      if (resolved != null) Arrays.fill(resolved, (byte) 0);
      if (owner == null) {
        busy = false;
        notifyListeners();
      }
    }
  }

  /**
   * Complete only after durable save. The controller then acknowledges native. A late result from
   * an already cancelled/replaced owner is always rejected.
   */
  public CompletableFuture<Snapshot> persist(Session session, String kind, String value) {
    return submit(() -> {
      requireReady();
      if (owner != session || session == null || session.cancelled())
        throw new IllegalStateException("stale_connection");
      if (!kind.equals("trusted") && !kind.equals("paired"))
        throw new IllegalArgumentException("invalid_catalog_request");
      byte[] data = read(), candidate = null;
      try {
        JSONObject command =
            new JSONObject()
                .put("op", kind.equals("trusted") ? "trust" : "paired")
                .put("lease", new JSONObject(session.lease))
                .put(kind.equals("trusted") ? "pin" : "token", value);
        candidate = query(data, command, true);
        // Cancellation during crypto does not permit use of the saved result.
        // The identity may be durably saved, but the stopped service cannot ack it.
        Snapshot next = commit(candidate);
        Computer active = next.active();
        session.lease =
            new JSONObject()
                .put("id", active.id)
                .put("connection_generation", active.generation)
                .put("activation_epoch", next.epoch)
                .toString();
        session.connectionKey = next.connectionKey();
        remember(next.connectionKey());
        if (session.cancelled()) throw new IllegalStateException("stale_connection");
        return next;
      } finally {
        Arrays.fill(data, (byte) 0);
        if (candidate != null) Arrays.fill(candidate, (byte) 0);
      }
    });
  }

  public CompletableFuture<Void> release(Session session) {
    if (session != null) session.cancel();
    return submit(() -> {
      if (session != null) session.wipe();
      if (session != null && owner == session) {
        try {
          remember(null);
        } catch (Exception failure) {
          publish(new Snapshot(Status.FAILED));
          throw failure;
        } finally {
          owner = null;
          busy = false;
          notifyListeners();
        }
      }
      return null;
    });
  }

  /** Explicit retry of an unavailable store, never a reset or v1 fallback. */
  public CompletableFuture<Snapshot> reload() {
    return submit(() -> {
      if (owner != null) throw new IllegalStateException("connection_running");
      return load();
    });
  }

  /**
   * Requires an explicit destructive confirmation in the recovery screen. Never used automatically
   * after a failed read, migration or write.
   */
  public CompletableFuture<Snapshot> reset() {
    return submit(() -> {
      if (owner != null || busy) throw new IllegalStateException("connection_running");
      byte[] empty = query(new byte[0], new JSONObject().put("op", "create"), false);
      try {
        Snapshot next = view(empty);
        retireLegacy();
        remember(null);
        vault.reset(empty);
        byte[] readback = vault.load();
        try {
          if (!Arrays.equals(empty, readback)) throw new IOException("computer_storage_commit");
          view(readback);
        } finally {
          Arrays.fill(readback, (byte) 0);
        }
        publish(next);
        return next;
      } catch (Exception failure) {
        publish(new Snapshot(Status.FAILED));
        throw failure;
      } finally {
        Arrays.fill(empty, (byte) 0);
      }
    });
  }
}
