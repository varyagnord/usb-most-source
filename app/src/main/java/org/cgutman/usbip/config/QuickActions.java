package org.cgutman.usbip.config;

import android.app.KeyguardManager;
import android.app.PendingIntent;
import android.content.*;
import android.net.Uri;
import android.os.SystemClock;
import android.service.quicksettings.TileService;
import java.util.UUID;
import org.cgutman.usbip.connection.ComputerRepository;
import org.cgutman.usbip.service.UsbIpService;

/** Android entry-point ownership only; connection policy remains in the Rust catalog. */
public final class QuickActions {
  public static final int OPEN = 0, START = 1, STOP = 2;
  private static final String PROCESS = UUID.randomUUID().toString();
  private static long serviceEpoch = 1;
  private static Request pending;

  private QuickActions() {}

  public static final class Binding {
    public final int action;
    private final String process, computer;
    private final long epoch, created;

    private Binding(int action, String process, String computer, long epoch, long created) {
      this.action = action;
      this.process = process;
      this.computer = computer;
      this.epoch = epoch;
      this.created = created;
    }
  }

  private static final class Request {
    final Binding binding;
    final long accepted = SystemClock.elapsedRealtime();

    Request(Binding binding) {
      this.binding = binding;
    }
  }

  public static synchronized Binding capture(Context context) {
    ComputerRepository repository = ComputerRepository.get(context);
    ComputerRepository.Snapshot snapshot = repository.snapshot();
    int action =
        UsbIpService.running
            ? STOP
            : snapshot.status == ComputerRepository.Status.READY
                    && snapshot.active() != null
                    && !repository.isBusy()
                ? START
                : OPEN;
    return new Binding(
        action, PROCESS, snapshot.connectionKey(), serviceEpoch, SystemClock.elapsedRealtime());
  }

  public static boolean locked(Context context) {
    KeyguardManager keyguard = context.getSystemService(KeyguardManager.class);
    return keyguard == null || keyguard.isKeyguardLocked();
  }

  public static synchronized boolean current(Context context, Binding binding) {
    return current(context, binding, ComputerRepository.get(context).snapshot());
  }

  public static synchronized boolean current(
      Context context, Binding binding, ComputerRepository.Snapshot snapshot) {
    if (binding == null || !PROCESS.equals(binding.process) || binding.epoch != serviceEpoch)
      return false;
    ComputerRepository repository = ComputerRepository.get(context);
    if (!binding.computer.equals(snapshot.connectionKey())) return false;
    if (binding.action == STOP) return UsbIpService.running;
    return binding.action == START
        && !UsbIpService.running
        && !repository.isBusy()
        && snapshot.status == ComputerRepository.Status.READY
        && snapshot.active() != null;
  }

  public static void serviceChanged(Context context) {
    synchronized (QuickActions.class) {
      serviceEpoch++;
      pending = null;
    }
    refreshTile(context);
  }

  public static void refreshTile(Context context) {
    try {
      TileService.requestListeningState(
          context, new ComponentName(context, BridgeTileService.class));
    } catch (RuntimeException unavailable) {
      // A disabled/missing SystemUI tile cannot prevent USB service cleanup.
    }
  }

  static Intent activityIntent(Context context, Binding binding, boolean notification) {
    return new Intent(context, QuickActionActivity.class)
        .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        .setData(
            new Uri.Builder()
                .scheme("usb-most-private")
                .authority("quick")
                .appendPath(notification ? "notification" : "tile")
                .build())
        .putExtra("process", binding.process)
        .putExtra("computer", binding.computer)
        .putExtra("epoch", binding.epoch)
        .putExtra("created", binding.created)
        .putExtra("action", binding.action);
  }

  /** Two bounded PendingIntent identities; CANCEL_CURRENT never mutates an old capability. */
  public static PendingIntent intent(Context context, Binding binding, boolean notification) {
    // The short-lived startup notification only offers Open, never a new start.
    if (notification && binding.action != STOP)
      binding =
          new Binding(OPEN, binding.process, binding.computer, binding.epoch, binding.created);
    return PendingIntent.getActivity(
        context,
        notification ? 31 : 32,
        activityIntent(context, binding, notification),
        PendingIntent.FLAG_CANCEL_CURRENT | PendingIntent.FLAG_IMMUTABLE);
  }

  static Binding read(Intent intent) {
    try {
      String process = intent.getStringExtra("process"),
          computer = intent.getStringExtra("computer");
      int action = intent.getIntExtra("action", OPEN);
      if (process == null
          || computer == null
          || process.length() != 36
          || computer.length() > 256
          || action < OPEN
          || action > STOP) return null;
      return new Binding(
          action,
          process,
          computer,
          intent.getLongExtra("epoch", -1),
          intent.getLongExtra("created", -1));
    } catch (RuntimeException malformed) {
      return null;
    }
  }

  /** Called exclusively by the non-exported Activity; no launcher Intent is trusted. */
  static void accept(Context context, Binding binding) {
    long age = binding == null ? -1 : SystemClock.elapsedRealtime() - binding.created;
    if (locked(context)
        || !current(context, binding)
        || (binding.action == START && (age < 0 || age > 30000))) binding = null;
    synchronized (QuickActions.class) {
      if (binding == null)
        binding = new Binding(OPEN, PROCESS, "", serviceEpoch, SystemClock.elapsedRealtime());
      pending = new Request(binding);
    }
  }

  public static Binding consume(Context context) {
    Request request;
    synchronized (QuickActions.class) {
      request = pending;
      pending = null;
    }
    if (request == null) return null;
    long age = SystemClock.elapsedRealtime() - request.accepted;
    if (age >= 0 && age <= 10000 && !locked(context) && current(context, request.binding))
      return request.binding;
    synchronized (QuickActions.class) {
      return new Binding(OPEN, PROCESS, "", serviceEpoch, SystemClock.elapsedRealtime());
    }
  }
}
