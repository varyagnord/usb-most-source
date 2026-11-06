package org.cgutman.usbip.service;

import android.annotation.SuppressLint;
import android.app.*;
import android.content.*;
import android.hardware.usb.*;
import android.net.wifi.WifiManager;
import android.os.*;
import androidx.core.app.NotificationCompat;
import java.net.Socket;
import java.util.*;
import java.util.concurrent.*;
import org.cgutman.usbip.config.QuickActions;
import org.cgutman.usbip.config.UsbIpConfig;
import org.cgutman.usbip.connection.ComputerRepository;
import org.cgutman.usbip.connection.ConnectionController;
import org.cgutman.usbip.core.HidTrace;
import org.cgutman.usbip.core.ManagedImports;
import org.cgutman.usbip.jni.NativeManagement;
import org.cgutman.usbip.jni.NativeUsb;
import org.cgutman.usbip.server.*;
import org.cgutman.usbip.server.protocol.*;
import org.cgutman.usbip.server.protocol.dev.*;
import org.cgutman.usbipserverforandroid.BuildConfig;
import org.cgutman.usbipserverforandroid.R;

public final class UsbIpService extends Service implements UsbRequestHandler {
  public static volatile boolean running;
  public static volatile String status = "Служба остановлена";
  private static final String CHANNEL = "usbip_service";
  public static final String EXTRA_COMPUTER_REVISION = "computer_revision";
  private UsbIpServer server;
  // Device requests are a process-local adapter boundary. The Android adapter
  // is this service; no intent, preference or network command can replace it.
  private UsbRequestHandler usbRequests = this;
  private ManagedImports managedImports;
  private ComputerRepository computers;
  private ComputerRepository.Session computerLease;
  private boolean requested;
  private int usbOperations;
  private boolean usbCleanupFailed;
  private final Handler main = new Handler(Looper.getMainLooper());
  private org.cgutman.usbip.connection.ConnectionController connection;
  private DiagnosticLog log;
  private DiagnosticController diagnostics;
  private QuickActions.Binding notificationBinding;
  private PendingIntent notificationCommand;
  private PowerManager.WakeLock wake;
  private WifiManager.WifiLock wifi;
  private final ConcurrentHashMap<Socket, UsbSession> sessions = new ConcurrentHashMap<>();
  private UsbInventory inventory;
  private volatile boolean stopping;
  private final Object lifecycle = new Object();
  private final UsbInventory.Listener inventoryListener =
      () -> {
        synchronized (lifecycle) {
          for (UsbSession session : sessions.values()) {
            UsbInventory.Item item = inventory.item(session.device.getDeviceId());
            if (item == null || item.generation != session.generation || !item.selected)
              server.killClient(session.socket);
          }
        }
      };

  // The explicit foreground service owns these locks for its lifetime; onDestroy releases them.
  @SuppressLint("WakelockTimeout")
  public void onCreate() {
    super.onCreate();
    QuickActions.serviceChanged(this);
    log = new DiagnosticLog(this);
    diagnostics =
        BuildConfig.DIAGNOSTICS ? ((BridgeApplication) getApplication()).diagnostics() : null;
    NotificationManager nm = getSystemService(NotificationManager.class);
    nm.createNotificationChannel(
        new NotificationChannel(CHANNEL, "USB Мост", NotificationManager.IMPORTANCE_LOW));
    status = "Подготовка подключения…";
    notification(status);
    computers = ComputerRepository.get(this);
    inventory = UsbInventory.get(this);
    inventory.addListener(inventoryListener);
    PowerManager pm = (PowerManager) getSystemService(POWER_SERVICE);
    wake = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "RutokenBridge:service");
    wake.acquire();
    WifiManager wm = (WifiManager) getApplicationContext().getSystemService(WIFI_SERVICE);
    wifi = wm.createWifiLock(WifiManager.WIFI_MODE_FULL_HIGH_PERF, "RutokenBridge:wifi");
    wifi.acquire();
    server = new UsbIpServer(log::event, diagnostics);
  }

  private void connect(ComputerRepository.Session lease) {
    if (stopping) {
      lease.cancel();
      computers.release(lease);
      return;
    }
    computerLease = lease;
    try {
      ComputerRepository.Snapshot snapshot = computers.snapshot();
      ComputerRepository.Computer selected = snapshot.active();
      if (selected == null || !snapshot.connectionKey().equals(lease.connectionKey()))
        throw new IllegalStateException("computer_changed");
      if (!inventory.bindComputer(selected)) throw new IllegalStateException("usb_still_owned");
      if (selected.kind.equals("secure")) {
        byte[] nonce = new byte[16];
        new java.security.SecureRandom().nextBytes(nonce);
        final long model = NativeManagement.create(nonce);
        if (model == 0) throw new IllegalStateException("management_model_limit");
        managedImports =
            new ManagedImports(
                new ManagedImports.Model() {
                  public byte[] execute(int action, byte[] bytes) {
                    return NativeManagement.execute(model, action, bytes);
                  }

                  public void close() {
                    NativeManagement.close(model);
                  }
                },
                (socket, request) -> {
                  synchronized (lifecycle) {
                    UsbSession session = sessions.get(socket);
                    if (stopping || session == null) return null;
                    return inventory.bindEjection(
                        session.device.getDeviceId(), session.generation, socket, request);
                  }
                });
        byte[] proof = new byte[32];
        new java.security.SecureRandom().nextBytes(proof);
        try {
          int port = server.startLocal(usbRequests, proof);
          connection =
              new ConnectionController(
                  this,
                  new ConnectionController.Events() {
                    public void status(String text) {
                      UsbIpService.status = text;
                      notification(text);
                    }

                    public void disconnected() {
                      inventory.invalidateStorageApprovals();
                    }

                    public void failure(String text) {
                      UsbIpService.status = text;
                      stopSelf();
                    }
                  },
                  lease);
          connection.start(port, proof);
        } finally {
          Arrays.fill(proof, (byte) 0);
        }
      } else {
        byte[] unused = lease.takeConnection();
        if (unused != null) Arrays.fill(unused, (byte) 0);
        if (!org.cgutman.usbip.jni.NativePolicy.validHost(selected.host, selected.mode))
          throw new IllegalStateException("invalid_legacy_address");
        server.start(usbRequests, selected.host);
      }
      status = connection == null ? "Готово. Ожидание компьютера" : "Подключение к компьютеру…";
      notification(status);
      log.event(
          "service_start version=" + BuildConfig.VERSION_NAME + " api=" + Build.VERSION.SDK_INT);
    } catch (Exception failure) {
      status =
          failure instanceof java.net.BindException
              ? "Не удалось открыть порт USB/IP. Остановите другое подключение и повторите попытку"
              : "Не удалось запустить мост. Проверьте сохранённый компьютер и повторите попытку";
      log.event("start_failed " + failure.getClass().getSimpleName());
      stopSelf();
    }
  }

  private void notification(String text) {
    QuickActions.Binding binding = QuickActions.capture(this);
    if (notificationCommand == null
        || notificationBinding.action != binding.action
        || !QuickActions.current(this, notificationBinding)) {
      notificationBinding = binding;
      notificationCommand = QuickActions.intent(this, binding, true);
    }
    PendingIntent intent =
        PendingIntent.getActivity(
            this, 0, new Intent(this, UsbIpConfig.class), PendingIntent.FLAG_IMMUTABLE);
    startForeground(
        100,
        new NotificationCompat.Builder(this, CHANNEL)
            .setSmallIcon(R.drawable.notification_icon)
            .setContentTitle("USB Мост")
            .setContentText(text)
            .setContentIntent(intent)
            .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
            .addAction(0, running ? "Остановить" : "Открыть", notificationCommand)
            .setOngoing(true)
            .build());
    QuickActions.refreshTile(this);
  }

  public int onStartCommand(Intent intent, int flags, int id) {
    notification(status); // acknowledge every foreground start, including repeats
    if (stopping) {
      stopSelf(id);
      return START_NOT_STICKY;
    }
    if (requested) return START_STICKY;
    requested = true;
    running = true;
    long revision = intent == null ? -1 : intent.getLongExtra(EXTRA_COMPUTER_REVISION, -1);
    if (intent != null && revision < 0) {
      status = "Не удалось запустить: выберите компьютер и повторите попытку";
      stopSelf(id);
      return START_NOT_STICKY;
    }
    computers
        .ready()
        .thenCompose(snapshot -> intent == null ? computers.resume() : computers.begin(revision))
        .whenComplete((lease, failure) ->
            main.post(() -> {
              if (failure != null) {
                if (!stopping) {
                  status = "Не удалось запустить: сохранённые данные изменились или недоступны";
                  log.event("computer_start_rejected");
                  stopSelf();
                }
              } else connect(lease);
            }));
    return START_STICKY;
  }

  public IBinder onBind(Intent i) {
    return null;
  }

  public void onDestroy() {
    synchronized (lifecycle) {
      stopping = true;
      running = false;
    }
    QuickActions.serviceChanged(this);
    if (computerLease != null) computerLease.cancel();
    if (connection != null) connection.close();
    if (server != null) server.stop();
    if (managedImports != null) managedImports.close();
    inventory.removeListener(inventoryListener);
    inventory.invalidateStorageApprovals();
    if (!status.startsWith("Не удалось")) status = "Завершение подключения…";
    new Thread(
            () -> {
              boolean interrupted = false;
              boolean quiescent = false;
              try {
                boolean clean = true;
                for (Socket socket : new ArrayList<>(sessions.keySet())) {
                  try {
                    cleanupSocket(socket);
                  } catch (RuntimeException | LinkageError failure) {
                    clean = false;
                    log.event("usb_cleanup_failed " + failure.getClass().getSimpleName());
                  }
                }
                synchronized (lifecycle) {
                  while (usbOperations != 0) {
                    try {
                      lifecycle.wait();
                    } catch (InterruptedException interruption) {
                      interrupted = true;
                    }
                  }
                }
                synchronized (lifecycle) {
                  quiescent = clean && !usbCleanupFailed;
                }
              } finally {
                if (quiescent)
                  computers
                      .release(computerLease)
                      .whenComplete((ignored, failure) ->
                          main.post(() -> {
                            if (!running) {
                              if (failure != null)
                                status = "Не удалось сохранить завершение подключения";
                              else if (!status.startsWith("Не удалось"))
                                status = "Служба остановлена";
                            }
                          }));
                else
                  main.post(() -> {
                    if (!running) status = "Не удалось завершить USB. Перезапустите приложение";
                  });
                if (wifi != null && wifi.isHeld()) wifi.release();
                if (wake != null && wake.isHeld()) wake.release();
                log.event("service_stop");
                if (interrupted) Thread.currentThread().interrupt();
              }
            },
            "usbip-cleanup")
        .start();
    super.onDestroy();
  }

  private void endUsbOperation() {
    synchronized (lifecycle) {
      usbOperations--;
      lifecycle.notifyAll();
    }
  }

  private void failedUsbCleanup() {
    synchronized (lifecycle) {
      usbCleanupFailed = true;
    }
  }

  private String busId(UsbDevice d) {
    return (d.getDeviceId() / 1000) + "-" + (d.getDeviceId() % 1000);
  }

  private UsbDevice find(String busid) {
    for (UsbInventory.Item entry : inventory.snapshot()) {
      UsbDevice d = entry.device;
      if (busId(d).equals(busid)) {
        UsbInventory.Item item = inventory.item(d.getDeviceId());
        if (item != null && item.selected && item.ready && inventory.authorized(d.getDeviceId()))
          return d;
      }
    }
    return null;
  }

  private UsbDeviceInfo info(UsbInventory.Item item) {
    UsbConfiguration cfg = item.device.getConfiguration(0);
    for (int index = 0; index < item.device.getConfigurationCount(); index++)
      if (item.device.getConfiguration(index).getId() == item.configuration)
        cfg = item.device.getConfiguration(index);
    return info(item, cfg);
  }

  private UsbDeviceInfo info(UsbInventory.Item item, UsbConfiguration c) {
    UsbDevice d = item.device;
    UsbDeviceInfo out = new UsbDeviceInfo();
    UsbIpDevice u = new UsbIpDevice();
    out.dev = u;
    u.path = d.getDeviceName();
    u.busid = busId(d);
    u.busnum = d.getDeviceId() / 1000;
    u.devnum = d.getDeviceId() % 1000;
    u.speed = item.speed > 0 ? item.speed : UsbIpDevice.USB_SPEED_FULL;
    u.idVendor = (short) d.getVendorId();
    u.idProduct = (short) d.getProductId();
    u.bcdDevice = (short) item.bcdDevice;
    u.bDeviceClass = (byte) d.getDeviceClass();
    u.bDeviceSubClass = (byte) d.getDeviceSubclass();
    u.bDeviceProtocol = (byte) d.getDeviceProtocol();
    u.bConfigurationValue = (byte) c.getId();
    u.bNumConfigurations = (byte) d.getConfigurationCount();
    java.util.List<UsbInterface> defaults = new java.util.ArrayList<>();
    for (int index = 0; index < c.getInterfaceCount(); index++)
      if (c.getInterface(index).getAlternateSetting() == 0) defaults.add(c.getInterface(index));
    u.bNumInterfaces = (byte) defaults.size();
    out.interfaces = new UsbIpInterface[defaults.size()];
    for (int i = 0; i < out.interfaces.length; i++) {
      UsbInterface f = defaults.get(i);
      UsbIpInterface v = new UsbIpInterface();
      out.interfaces[i] = v;
      v.bInterfaceClass = (byte) f.getInterfaceClass();
      v.bInterfaceSubClass = (byte) f.getInterfaceSubclass();
      v.bInterfaceProtocol = (byte) f.getInterfaceProtocol();
    }
    return out;
  }

  public List<UsbDeviceInfo> getDevices() {
    List<UsbDeviceInfo> out = new ArrayList<>();
    inventory.refresh();
    for (UsbInventory.Item item : inventory.snapshot())
      if (item.selected && item.ready && inventory.authorized(item.device.getDeviceId()))
        out.add(info(item));
    return out;
  }

  public UsbDeviceInfo getDeviceByBusId(String id) {
    UsbDevice d = find(id);
    UsbInventory.Item item = d == null ? null : inventory.item(d.getDeviceId());
    return item == null || item.device != d ? null : info(item);
  }

  public UsbDeviceInfo getAttachedDevice(Socket socket) {
    UsbSession session = sessions.get(socket);
    if (session == null) return null;
    UsbInventory.Item current = inventory.item(session.device.getDeviceId());
    return current != null && current.generation == session.generation && current.selected
        ? session.description
        : null;
  }

  public boolean supportsWan(String id) {
    UsbDevice d = find(id);
    UsbInventory.Item item = d == null ? null : inventory.item(d.getDeviceId());
    return d != null && item != null && item.selected && item.ready && WanProfile.receiver(d);
  }

  public void wanControl(Socket s, org.cgutman.usbip.server.protocol.dev.WanControl c) {
    UsbSession session = sessions.get(s);
    if (session == null) server.killClient(s);
    else session.wanControl(c);
  }

  public boolean attachToDevice(Socket socket, String id) {
    return attachToDevice(socket, id, false);
  }

  @Override
  public ManagementAccess management() {
    return managedImports;
  }

  public boolean attachToDevice(Socket socket, String id, boolean wan) {
    if (wan && !supportsWan(id)) return false;
    UsbDevice d = find(id);
    UsbInventory.Item candidate = d == null ? null : inventory.item(d.getDeviceId());
    final HidTrace trace;
    synchronized (lifecycle) {
      if (stopping
          || candidate == null
          || candidate.device != d
          || (wan && !WanProfile.receiver(d))
          || !candidate.selected
          || !candidate.ready
          || !inventory.acquire(d.getDeviceId(), candidate.generation, socket)) {
        log.event("attach_rejected unavailable_or_busy");
        return false;
      }
      try {
        trace = diagnostics == null ? null : diagnostics.begin(d, wan);
      } catch (RuntimeException error) {
        inventory.released(d.getDeviceId(), candidate.generation, socket, 5);
        log.event("diagnostic_attach_rejected " + error.getClass().getSimpleName());
        return false;
      }
      usbOperations++;
    }
    log.event("attach_requested");
    UsbDeviceConnection connection = null;
    java.util.Set<Integer> restoreInterfaces = new java.util.LinkedHashSet<>();
    UsbSession created = null;
    boolean attached = false;
    int failure = 0;
    try {
      if (!inventory.authorized(d.getDeviceId())) return false;
      if (stopping || socket.isClosed()) return false;
      connection = inventory.open(d.getDeviceId(), candidate.generation);
      if (connection == null) {
        failure = 2;
        log.event("usb_open_failed");
        return false;
      }
      // Another Android app may have changed configuration since metadata was read.
      int actual = candidate.configuration;
      byte[] currentConfiguration = new byte[1];
      if (connection.controlTransfer(0x80, 8, 0, 0, currentConfiguration, 1, 1000) == 1)
        actual = currentConfiguration[0] & 255;
      UsbConfiguration cfg = null;
      for (int index = 0; index < d.getConfigurationCount(); index++)
        if (d.getConfiguration(index).getId() == actual) cfg = d.getConfiguration(index);
      if (actual == 0) {
        cfg = d.getConfiguration(0);
        if (!connection.setConfiguration(cfg)) {
          failure = 3;
          return false;
        }
      }
      if (cfg == null) {
        failure = 3;
        return false;
      }
      inventory.configuration(d.getDeviceId(), candidate.generation, socket, cfg.getId());
      for (int j = 0; j < cfg.getInterfaceCount(); j++)
        if (cfg.getInterface(j).getAlternateSetting() == 0) {
          restoreInterfaces.add(cfg.getInterface(j).getId());
          if (!connection.claimInterface(cfg.getInterface(j), true)) {
            failure = 4;
            log.event("usb_claim_failed interface=" + cfg.getInterface(j).getId());
            return false;
          }
        }
      if (stopping || socket.isClosed()) return false;
      created =
          new UsbSession(
              d,
              candidate.generation,
              cfg,
              info(candidate, cfg),
              connection,
              socket,
              server,
              log,
              value ->
                  inventory.configuration(d.getDeviceId(), candidate.generation, socket, value),
              diagnostics,
              trace,
              wan);
      connection = null; // Session owns the Android and native descriptors from here.
      synchronized (lifecycle) {
        UsbInventory.Item current = inventory.item(d.getDeviceId());
        if (stopping
            || socket.isClosed()
            || !inventory.isSelected(d.getDeviceId())
            || current == null
            || current.generation != candidate.generation) return false;
        sessions.put(socket, created);
        attached = true;
        status = "USB-подключений: " + sessions.size();
        notification(status);
      }
      inventory.state(d.getDeviceId(), candidate.generation, socket, "Подключено к компьютеру");
      log.event("attached");
      return true;
    } catch (Exception | LinkageError e) {
      failure = 5;
      log.event("attach_failed " + e.getClass().getSimpleName());
      return false;
    } finally {
      boolean cleaned = false;
      try {
        if (connection != null) {
          for (int interfaceId : restoreInterfaces)
            NativeUsb.releaseInterface(connection.getFileDescriptor(), interfaceId, true);
          connection.close();
        }
        if (!attached) {
          if (created != null && !created.closeAndRelease()) failedUsbCleanup();
          if (diagnostics != null) diagnostics.ended(trace);
          inventory.released(d.getDeviceId(), candidate.generation, socket, failure);
        }
        cleaned = true;
      } finally {
        if (!cleaned) failedUsbCleanup();
        endUsbOperation();
      }
    }
  }

  public void submitUrbRequest(Socket s, UsbIpSubmitUrb r) {
    UsbSession session = sessions.get(s);
    if (session == null) server.killClient(s);
    else session.submitRequest(r);
  }

  public void abortUrbRequest(Socket s, UsbIpUnlinkUrb r) {
    UsbSession session = sessions.get(s);
    if (session == null) server.killClient(s);
    else session.unlinkRequest(r);
  }

  public void detachFromDevice(Socket s, String id) {
    server.killClient(s);
  }

  public void cleanupSocket(Socket socket) {
    UsbSession session;
    synchronized (lifecycle) {
      session = sessions.remove(socket);
      if (session == null) return;
      usbOperations++;
    }
    boolean closed = false;
    try {
      try {
        closed = session.closeAndRelease();
      } finally {
        if (!closed) failedUsbCleanup();
        inventory.released(session.device.getDeviceId(), session.generation, socket);
      }
    } finally {
      endUsbOperation();
    }
    synchronized (lifecycle) {
      if (!stopping) {
        status =
            sessions.isEmpty()
                ? "Готово. Ожидание компьютера"
                : "USB-подключений: " + sessions.size();
        notification(status);
      }
    }
  }
}
