package org.cgutman.usbip.service;

import android.app.PendingIntent;
import android.content.*;
import android.hardware.usb.*;
import android.os.*;
import androidx.core.content.ContextCompat;
import java.util.*;
import java.util.concurrent.*;
import org.cgutman.usbip.connection.ComputerRepository;
import org.cgutman.usbip.connection.UsbRules;
import org.cgutman.usbip.core.ManagedImports;
import org.cgutman.usbip.jni.NativePolicy;
import org.json.*;

/**
 * Android USB adapter. Rust owns selection, capability and descriptor policy. Snapshots never
 * expose mutable state; listeners run after locks are released.
 */
public final class UsbInventory implements AutoCloseable {
  private static final String PERMISSION = "ru.levchatkin.rutokenbridge.USB_ACCESS";

  public static UsbInventory get(Context context) {
    return ((BridgeApplication) context.getApplicationContext()).inventory();
  }

  public interface Listener {
    void changed();
  }

  public static final class Item {
    public final UsbDevice device;
    public final String name, detail, state, identity, ruleReason;
    public final int capability, notice;
    public final long generation;
    public final boolean storage, selected, approved, ready, inUse;
    public final int ejectionState;
    public final boolean canEject;
    public final int speed, bcdDevice, configuration;

    Item(Entry e, boolean selected) {
      device = e.device;
      generation = e.generation;
      name = !e.sessionName.isEmpty() ? e.sessionName : !e.ruleName.isEmpty() ? e.ruleName : e.name;
      identity = e.identity;
      ruleReason = e.ruleReason;
      detail = e.detail;
      state = e.state;
      capability = e.capability;
      notice = e.notice;
      storage = e.storage;
      this.selected = selected;
      approved = e.approved;
      ready = e.ready;
      inUse = e.owner != null;
      ejectionState = e.ejectionState;
      canEject = e.canEject;
      speed = e.speed;
      bcdDevice = e.bcdDevice;
      configuration = e.configuration;
    }
  }

  private static final class Entry {
    final UsbDevice device;
    final long generation = System.nanoTime();
    final String name, detail;
    final int capability;
    final boolean storage;
    boolean approved, ready, preparing, denied;
    Object owner;
    Object selectionRevision = new Object();
    EjectionLease ejection;
    int ejectionState = -1;
    boolean canEject;
    int notice; // 1 returned storage, 2 open, 3 configuration, 4 claim, 5 native initialization.
    int speed = 0, bcdDevice = 0, configuration;
    String state = "Не выбрано";
    String identity, ruleName = "", sessionName = "", ruleReason = "no_identity";
    boolean identified, identifying, autoAttempted;

    Entry(UsbDevice d) {
      device = d;
      String label = d.getProductName();
      name = label == null || label.trim().isEmpty() ? "USB-устройство" : label;
      int flags = 0, interfaces = 0;
      boolean disk = false, hasBot = false, hasUas = false;
      for (int c = 0; c < d.getConfigurationCount(); c++) {
        UsbConfiguration cfg = d.getConfiguration(c);
        for (int i = 0; i < cfg.getInterfaceCount(); i++) {
          UsbInterface f = cfg.getInterface(i);
          interfaces++;
          if (f.getInterfaceClass() == 9) flags |= 1;
          if (f.getInterfaceClass() == 8) {
            disk = true;
            if (f.getInterfaceProtocol() == 0x50) hasBot = true;
            if (f.getInterfaceProtocol() == 0x62) hasUas = true;
          }
          for (int j = 0; j < f.getEndpointCount(); j++)
            if (f.getEndpoint(j).getType() == 1) flags |= 2;
        }
      }
      if (hasUas && !hasBot) flags |= 4;
      capability =
          NativePolicy.capability(d.getDeviceClass(), d.getConfigurationCount(), interfaces, flags);
      storage = disk;
      detail =
          (disk
                  ? "Накопитель"
                  : d.getDeviceClass() == 3 || hasClass(d, 3)
                      ? "Устройство ввода"
                      : hasClass(d, 11) ? "Ключ / смарт-карта" : "USB-устройство")
              + " · USB "
              + (d.getDeviceId() / 1000)
              + "-"
              + (d.getDeviceId() % 1000);
      configuration = d.getConfigurationCount() > 0 ? d.getConfiguration(0).getId() : 0;
    }
  }

  /** Injectable system boundary; no IPC test controls or fake-device mode is exported. */
  public interface Access {
    Map<String, UsbDevice> devices();

    boolean hasPermission(UsbDevice device);

    void requestPermission(UsbDevice device, long generation, PendingIntent result);

    Description describe(UsbDevice device);

    UsbDeviceConnection open(UsbDevice device);

    default String serial(UsbDevice device) {
      return device.getSerialNumber();
    }
  }

  public static final class Description {
    final byte[] raw;
    final int speed, configuration;

    public Description(byte[] raw, int speed, int configuration) {
      this.raw = raw == null ? null : raw.clone();
      this.speed = speed;
      this.configuration = configuration;
    }
  }

  private static final class AndroidAccess implements Access {
    private final UsbManager manager;

    AndroidAccess(Context context) {
      manager = (UsbManager) context.getSystemService(Context.USB_SERVICE);
    }

    public Map<String, UsbDevice> devices() {
      return manager.getDeviceList();
    }

    public boolean hasPermission(UsbDevice d) {
      return manager.hasPermission(d);
    }

    public void requestPermission(UsbDevice d, long generation, PendingIntent result) {
      manager.requestPermission(d, result);
    }

    public UsbDeviceConnection open(UsbDevice d) {
      return manager.openDevice(d);
    }

    public Description describe(UsbDevice d) {
      UsbDeviceConnection connection = open(d);
      if (connection == null) return null;
      try {
        byte[] config = new byte[1];
        int value =
            connection.controlTransfer(0x80, 8, 0, 0, config, 1, 1000) == 1 ? config[0] & 255 : 0;
        return new Description(
            connection.getRawDescriptors(),
            NativePolicy.speed(connection.getFileDescriptor()),
            value);
      } finally {
        connection.close();
      }
    }
  }

  private final Context context;
  private final Access access;
  private final long selectionModel = NativePolicy.selectionCreate();
  private final BroadcastReceiver receiver;
  private volatile boolean closed;
  private long askingGeneration;
  private final Handler main = new Handler(Looper.getMainLooper());
  private final ExecutorService metadata =
      new ThreadPoolExecutor(
          1,
          1,
          0,
          TimeUnit.MILLISECONDS,
          new ArrayBlockingQueue<>(128),
          r -> {
            Thread t = new Thread(r, "usb-descriptors");
            t.setDaemon(true);
            return t;
          },
          new ThreadPoolExecutor.AbortPolicy());
  private final Map<Integer, Entry> entries = new LinkedHashMap<>();
  private final CopyOnWriteArrayList<Listener> listeners = new CopyOnWriteArrayList<>();
  private final ArrayDeque<Integer> permissionQueue = new ArrayDeque<>();
  private Integer asking;
  private long revision;
  private String computerScope = "", computerRules = "[]";
  private ComputerRepository computers;
  private final Runnable computerListener =
      () -> {
        ComputerRepository.Snapshot snapshot = computers.snapshot();
        // Failure is not permission to replace the current computer while USB is owned.
        bindComputer(snapshot.status == ComputerRepository.Status.READY ? snapshot.active() : null);
      };

  public void bindComputers(ComputerRepository repository) {
    if (computers != null) throw new IllegalStateException("computer_repository_already_bound");
    computers = repository;
    repository.addListener(computerListener);
    computerListener.run();
  }

  /** Selection belongs to a destination, not an Activity or a network address. */
  public boolean bindComputer(ComputerRepository.Computer computer) {
    boolean update = false;
    synchronized (this) {
      if (closed) return false;
      String scope = computer == null ? "" : computer.id + ":" + computer.usbEpoch;
      String rules = computer == null ? "[]" : computer.usbRules;
      if (!scope.equals(computerScope)) {
        for (Entry entry : entries.values()) if (entry.owner != null) return false;
        NativePolicy.selection(selectionModel, 2, new int[] {0});
        permissionQueue.clear();
        for (Entry entry : entries.values()) {
          resetEjection(entry);
          entry.approved = false;
          entry.autoAttempted = false;
          entry.sessionName = entry.ruleName = "";
          entry.state = "Не выбрано";
        }
        computerScope = scope;
        update = true;
        syncSelection();
      }
      if (!rules.equals(computerRules)) {
        computerRules = rules;
        update = true;
      }
      if (update) applyRules();
    }
    if (update) changed();
    return true;
  }

  /** An unnamed/ambiguous device can still have an explicit attachment-only label. */
  public boolean nameAttachment(int id, long generation, String scope, String name) {
    synchronized (this) {
      Entry entry = entries.get(id);
      if (entry == null || entry.generation != generation || !computerScope.equals(scope))
        return false;
      entry.sessionName = name;
    }
    changed();
    return true;
  }

  private void identify(Entry entry) {
    if (entry.identified || entry.identifying || !access.hasPermission(entry.device)) return;
    entry.identifying = true;
    try {
      metadata.execute(() -> {
        String identity = null;
        try {
          identity = UsbRules.identity(entry.device, access.serial(entry.device));
        } catch (Exception ignored) {
        }
        synchronized (UsbInventory.this) {
          if (closed || entries.get(entry.device.getDeviceId()) != entry) return;
          entry.identifying = false;
          entry.identified = access.hasPermission(entry.device);
          entry.identity = entry.identified ? identity : null;
          applyRules();
        }
        changed();
      });
    } catch (RejectedExecutionException full) {
      entry.identifying = false;
    }
  }

  private void applyRules() {
    if (closed) return;
    try {
      JSONArray devices = new JSONArray();
      for (Entry entry : entries.values())
        devices.put(
            new JSONObject()
                .put("id", entry.device.getDeviceId())
                .put("generation", entry.generation)
                .put("vendor", entry.device.getVendorId())
                .put("product", entry.device.getProductId())
                .put(
                    "identity",
                    entry.identity == null ? JSONObject.NULL : new JSONObject(entry.identity))
                .put("storage", entry.storage)
                .put("supported", entry.capability == 0)
                .put("permission", access.hasPermission(entry.device)));
      JSONArray decisions =
          new JSONArray(
              UsbRules.query(
                  new JSONObject()
                      .put("op", "usb_evaluate")
                      .put("rules", new JSONArray(computerRules))
                      .put("devices", devices)));
      for (int i = 0; i < decisions.length(); i++) {
        JSONObject decision = decisions.getJSONObject(i);
        int id = decision.getInt("id");
        Entry entry = entries.get(id);
        if (entry == null || entry.generation != decision.getLong("generation")) continue;
        entry.ruleName = decision.getString("name");
        entry.ruleReason = decision.getString("reason");
        if (!computerScope.isEmpty()
            && decision.getBoolean("automatic")
            && !entry.autoAttempted
            && !isSelected(id)
            && entry.owner == null
            && !entry.storage) {
          entry.autoAttempted = true;
          NativePolicy.selection(selectionModel, 1, new int[] {id, 1});
          entry.state = entry.ready ? "Ожидает подключения ПК" : "Подготовка…";
          request(entry);
        }
      }
    } catch (Exception invalid) {
      for (Entry entry : entries.values()) {
        entry.ruleName = "";
        entry.ruleReason = "unavailable";
      }
    }
  }

  UsbInventory(Context c) {
    this(c, new AndroidAccess(c));
  }

  public UsbInventory(Context c, Access access) {
    if (selectionModel == 0) throw new IllegalStateException("selection_model_limit");
    context = c;
    this.access = access;
    receiver =
        new BroadcastReceiver() {
          public void onReceive(Context ignored, Intent intent) {
            if (closed) return;
            UsbDevice device = intent.getParcelableExtra(UsbManager.EXTRA_DEVICE);
            if (device == null) return;
            if (PERMISSION.equals(intent.getAction())) {
              synchronized (UsbInventory.this) {
                Entry e = entries.get(device.getDeviceId());
                // Ignore a stale callback for a different device/path after physical detach.
                if (e != null
                    && e.device.getDeviceName().equals(device.getDeviceName())
                    && intent.getLongExtra("generation", Long.MIN_VALUE) == e.generation) {
                  e.denied = !intent.getBooleanExtra(UsbManager.EXTRA_PERMISSION_GRANTED, false);
                  e.state = e.denied ? "Нет доступа · выберите повторно" : "Подготовка…";
                  if (!e.denied) {
                    identify(e);
                    if (isSelected(e.device.getDeviceId())) prepare(e);
                  }
                }
                if (Objects.equals(asking, device.getDeviceId())
                    && intent.getLongExtra("generation", Long.MIN_VALUE) == askingGeneration)
                  asking = null;
                applyRules();
              }
              changed();
              nextPermission();
            } else {
              if (UsbManager.ACTION_USB_DEVICE_DETACHED.equals(intent.getAction())) {
                synchronized (UsbInventory.this) {
                  entries.remove(device.getDeviceId());
                  if (Objects.equals(asking, device.getDeviceId())) asking = null;
                  permissionQueue.remove(device.getDeviceId());
                  syncSelection();
                  applyRules();
                  revision++;
                }
                changed();
              }
              refresh();
            }
          }
        };
    IntentFilter filter = new IntentFilter(PERMISSION);
    filter.addAction(UsbManager.ACTION_USB_DEVICE_ATTACHED);
    filter.addAction(UsbManager.ACTION_USB_DEVICE_DETACHED);
    ContextCompat.registerReceiver(context, receiver, filter, ContextCompat.RECEIVER_NOT_EXPORTED);
    refresh();
  }

  private static boolean hasClass(UsbDevice d, int type) {
    for (int c = 0; c < d.getConfigurationCount(); c++)
      for (int i = 0; i < d.getConfiguration(c).getInterfaceCount(); i++)
        if (d.getConfiguration(c).getInterface(i).getInterfaceClass() == type) return true;
    return false;
  }

  public void addListener(Listener l) {
    listeners.add(l);
  }

  public void removeListener(Listener l) {
    listeners.remove(l);
  }

  private void changed() {
    synchronized (this) {
      revision++;
    }
    for (Listener l : listeners) l.changed();
  }

  public synchronized long revision() {
    return revision;
  }

  private void syncSelection() {
    int[] pairs = new int[entries.size() * 2];
    int at = 0;
    for (Entry e : entries.values()) {
      pairs[at++] = e.device.getDeviceId();
      pairs[at++] = e.capability == 0 && (!e.storage || e.approved) ? 1 : 0;
    }
    if (NativePolicy.selection(selectionModel, 0, pairs) == null)
      throw new IllegalStateException("invalid_usb_inventory");
  }

  public void refresh() {
    if (closed) return;
    boolean update = false;
    synchronized (this) {
      if (closed) return;
      Map<String, UsbDevice> actual = access.devices();
      Set<Integer> live = new HashSet<>();
      List<UsbDevice> sorted = new ArrayList<>(actual.values());
      sorted.sort(Comparator.comparing(UsbDevice::getDeviceName));
      for (UsbDevice d : sorted) {
        live.add(d.getDeviceId());
        if (!entries.containsKey(d.getDeviceId()) && entries.size() < 128) {
          entries.put(d.getDeviceId(), new Entry(d));
          update = true;
        }
      }
      if (entries.keySet().removeIf(id -> !live.contains(id))) update = true;
      permissionQueue.removeIf(id -> !entries.containsKey(id));
      if (asking != null && !entries.containsKey(asking)) asking = null;
      if (update) syncSelection();
      for (Entry e : entries.values())
        if ((e.ready || e.identified || e.identity != null) && !access.hasPermission(e.device)) {
          resetEjection(e);
          e.ready = false;
          e.identity = null;
          e.identified = false;
          e.denied = true;
          e.state = "Нет доступа · выберите повторно";
          update = true;
        }
      for (Entry e : entries.values()) identify(e);
      if (update) applyRules();
      for (Entry e : entries.values())
        if (isSelected(e.device.getDeviceId()) && !e.ready && !e.preparing && !e.denied) request(e);
    }
    if (update) changed();
  }

  public synchronized List<Item> snapshot() {
    List<Item> result = new ArrayList<>();
    for (Entry e : entries.values()) result.add(new Item(e, isSelected(e.device.getDeviceId())));
    return result;
  }

  public synchronized Item item(int id) {
    Entry e = entries.get(id);
    return e == null ? null : new Item(e, isSelected(id));
  }

  public boolean isSelected(int id) {
    int[] s = NativePolicy.selection(selectionModel, 3, new int[0]);
    if (s == null) return false;
    for (int i = 1; i < s.length; i++) if (s[i] == id) return true;
    return false;
  }

  public boolean all() {
    int[] snapshot = NativePolicy.selection(selectionModel, 3, new int[0]);
    return snapshot != null && snapshot[0] != 0;
  }

  public void select(int id, long generation, boolean selected, boolean storageApproved) {
    synchronized (this) {
      Entry e = entries.get(id);
      if (e == null || e.generation != generation) return;
      e.autoAttempted = true;
      if (selected && e.owner != null && !isSelected(id)) return;
      resetEjection(e);
      if (storageApproved) e.approved = true;
      if (!selected && e.storage) e.approved = false;
      e.denied = false;
      syncSelection();
      NativePolicy.selection(selectionModel, 1, new int[] {id, selected ? 1 : 0});
      e.notice = 0;
      if (!selected) e.state = "Не выбрано";
      else if (e.owner == null) e.state = e.ready ? "Ожидает подключения ПК" : "Подготовка…";
      if (selected) {
        if (e.identity == null) e.identified = false;
        identify(e);
        request(e);
      }
    }
    changed();
  }

  public void selectAll(boolean selected, java.util.List<Item> approvals) {
    synchronized (this) {
      for (Entry e : entries.values()) {
        e.autoAttempted = true;
        resetEjection(e);
      }
      for (Item item : approvals) {
        Entry e = entries.get(item.device.getDeviceId());
        if (e != null
            && e.storage
            && e.generation == item.generation
            && (e.owner == null || isSelected(e.device.getDeviceId()))) e.approved = true;
      }
      if (!selected) for (Entry e : entries.values()) if (e.storage) e.approved = false;
      syncSelection();
      NativePolicy.selection(selectionModel, 2, new int[] {selected ? 1 : 0});
      for (Entry e : entries.values())
        if (isSelected(e.device.getDeviceId())) {
          e.denied = false;
          e.notice = 0;
          request(e);
        }
    }
    changed();
  }

  public boolean state(int id, long generation, Object owner, String state) {
    synchronized (this) {
      Entry e = entries.get(id);
      if (e == null || owner == null || e.generation != generation || e.owner != owner)
        return false;
      if (state.equals(e.state)) return true;
      e.state = state;
    }
    changed();
    return true;
  }

  private void request(Entry e) {
    if (e.capability != 0 || e.preparing || e.ready) return;
    if (access.hasPermission(e.device)) {
      prepare(e);
      return;
    }
    int id = e.device.getDeviceId();
    if (!permissionQueue.contains(id) && !Objects.equals(asking, id)) permissionQueue.add(id);
    e.state = "Нужно разрешение USB";
    main.post(this::nextPermission);
  }

  private void nextPermission() {
    synchronized (this) {
      if (asking != null) return;
      while (!permissionQueue.isEmpty()) {
        int id = permissionQueue.remove();
        Entry e = entries.get(id);
        if (e == null || !isSelected(id)) continue;
        if (access.hasPermission(e.device)) {
          prepare(e);
          continue;
        }
        asking = id;
        askingGeneration = e.generation;
        final long generation = e.generation;
        Intent intent =
            new Intent(PERMISSION)
                .setPackage(context.getPackageName())
                .putExtra("generation", e.generation);
        int flags =
            PendingIntent.FLAG_UPDATE_CURRENT
                | (Build.VERSION.SDK_INT >= 31 ? PendingIntent.FLAG_MUTABLE : 0);
        try {
          access.requestPermission(
              e.device, e.generation, PendingIntent.getBroadcast(context, id, intent, flags));
        } catch (RuntimeException denied) {
          e.denied = true;
          e.state = "Не удалось запросить доступ";
          asking = null;
          main.post(this::changed);
          continue;
        }
        // Never leave the permission queue permanently blocked after a disappearing dialog.
        main.postDelayed(
            () -> {
              synchronized (UsbInventory.this) {
                if (!Objects.equals(asking, id) || askingGeneration != generation) return;
                asking = null;
                Entry current = entries.get(id);
                if (current != null) {
                  current.denied = true;
                  current.state = "Разрешение не получено";
                }
              }
              changed();
              nextPermission();
            },
            30000);
        break;
      }
    }
  }

  private void prepare(Entry e) {
    if (closed || e.preparing || e.ready) return;
    e.preparing = true;
    e.state = "Подготовка…";
    try {
      metadata.execute(() -> {
        synchronized (UsbInventory.this) {
          if (closed
              || entries.get(e.device.getDeviceId()) != e
              || !isSelected(e.device.getDeviceId())) {
            e.preparing = false;
            return;
          }
        }
        int[] description = null;
        int speed = 0, configuration = e.configuration;
        try {
          Description metadata = access.describe(e.device);
          if (metadata != null) {
            description = NativePolicy.describe(metadata.raw);
            speed = metadata.speed;
            if (metadata.configuration != 0) configuration = metadata.configuration;
          }
        } catch (RuntimeException ignored) {
        }
        synchronized (UsbInventory.this) {
          if (closed || entries.get(e.device.getDeviceId()) != e) return;
          e.preparing = false;
          e.ready = description != null && description[7] == 0;
          if (e.ready) {
            e.bcdDevice = description[0];
            e.speed = speed;
            e.configuration = configuration;
            e.state = "Ожидает подключения ПК";
          } else {
            e.denied = true;
            e.state = "Не удалось прочитать USB · выберите повторно";
          }
        }
        changed();
      });
    } catch (RejectedExecutionException full) {
      e.preparing = false;
      e.denied = true;
      e.state = "Очередь USB занята · выберите повторно";
      main.post(this::changed);
    }
  }

  public synchronized boolean authorized(int id) {
    Entry e = entries.get(id);
    return e != null && isSelected(id) && e.ready && access.hasPermission(e.device);
  }

  /** Read-only permission check for an explicit diagnostic snapshot. */
  public synchronized boolean permissionGranted(int id, long generation) {
    Entry entry = entries.get(id);
    return entry != null && entry.generation == generation && access.hasPermission(entry.device);
  }

  public UsbDeviceConnection open(int id, long generation) {
    Entry entry;
    synchronized (this) {
      entry = entries.get(id);
      if (closed || !authorized(id) || entry.generation != generation) return null;
    }
    UsbDeviceConnection connection = access.open(entry.device);
    synchronized (this) {
      if (closed || entries.get(id) != entry || !isSelected(id)) {
        if (connection != null) connection.close();
        return null;
      }
    }
    return connection;
  }

  void configuration(int id, long generation, Object owner, int value) {
    synchronized (this) {
      Entry e = entries.get(id);
      if (e == null || e.generation != generation || e.owner != owner || e.configuration == value)
        return;
      e.configuration = value;
    }
    changed();
  }

  /** Returning a claimed disk to Android may remount it; consent cannot survive that handoff. */
  public boolean acquire(int id, long generation, Object owner) {
    synchronized (this) {
      Entry e = entries.get(id);
      int active = 0;
      for (Entry item : entries.values()) if (item.owner != null) active++;
      if (closed
          || owner == null
          || e == null
          || e.generation != generation
          || e.owner != null
          || active >= 4
          || !authorized(id)) return false;
      e.owner = owner;
      resetEjection(e);
      e.notice = 0;
      e.state = "Подключение…";
    }
    changed();
    return true;
  }

  public void released(int id, long generation, Object owner) {
    released(id, generation, owner, 0);
  }

  public void released(int id, long generation, Object owner, int failure) {
    synchronized (this) {
      Entry e = entries.get(id);
      if (e == null || owner == null || e.generation != generation || e.owner != owner) return;
      e.owner = null;
      e.canEject = false;
      e.notice = failure >= 2 && failure <= 5 ? failure : e.storage ? 1 : 0;
      e.state = "Ожидает подключения ПК";
      if (e.storage) {
        e.approved = false;
        e.state = "Сначала извлеките в Android";
        syncSelection();
      }
    }
    changed();
  }

  public void invalidateStorageApprovals() {
    synchronized (this) {
      if (closed) return;
      for (Entry e : entries.values())
        if (e.storage) {
          resetEjection(e);
          if (e.approved || e.owner != null) e.notice = 1;
          e.approved = false;
        }
      syncSelection();
    }
    changed();
  }

  private static void resetEjection(Entry entry) {
    entry.selectionRevision = new Object();
    entry.ejection = null;
    entry.ejectionState = -1;
    entry.canEject = false;
  }

  /** Keeps only lease identity, never UsbSession, native buffers or a server Client. */
  private final class EjectionLease implements ManagedImports.Lease {
    final int id;
    final long generation;
    final Object owner, selectionRevision;
    final String scope;
    final java.util.function.BooleanSupplier request;

    EjectionLease(Entry entry, Object owner, java.util.function.BooleanSupplier request) {
      id = entry.device.getDeviceId();
      generation = entry.generation;
      this.owner = owner;
      selectionRevision = entry.selectionRevision;
      scope = computerScope;
      this.request = request;
    }

    private Entry exact() {
      Entry entry = entries.get(id);
      return !closed
              && entry != null
              && entry.generation == generation
              && entry.ejection == this
              && entry.selectionRevision == selectionRevision
              && computerScope.equals(scope)
              && (entry.owner == owner || entry.owner == null)
          ? entry
          : null;
    }

    public boolean current() {
      synchronized (UsbInventory.this) {
        return exact() != null;
      }
    }

    public void state(int state, boolean canRequest) {
      synchronized (UsbInventory.this) {
        Entry entry = exact();
        if (entry == null || !entry.storage) return;
        boolean can = canRequest && entry.owner == owner && isSelected(id);
        if (entry.ejectionState == state && entry.canEject == can) return;
        entry.ejectionState = state;
        entry.canEject = can;
      }
      changed();
    }

    public boolean applySafe() {
      synchronized (UsbInventory.this) {
        Entry entry = exact();
        if (entry == null || !entry.storage) return false;
        entry.approved = false;
        entry.canEject = false;
        entry.ejectionState = 2;
        entry.ejection = null;
        entry.notice = 1;
        entry.state = "Передача завершена";
        syncSelection();
        NativePolicy.selection(selectionModel, 1, new int[] {id, 0});
      }
      changed();
      return true;
    }
  }

  public ManagedImports.Lease bindEjection(
      int id, long generation, Object owner, java.util.function.BooleanSupplier request) {
    synchronized (this) {
      Entry entry = entries.get(id);
      if (closed
          || entry == null
          || owner == null
          || entry.owner != owner
          || entry.generation != generation
          || !isSelected(id)
          || entry.ejection != null) return null;
      EjectionLease lease = new EjectionLease(entry, owner, request);
      entry.ejection = lease;
      return lease;
    }
  }

  public boolean requestEjection(int id, long generation) {
    EjectionLease lease;
    synchronized (this) {
      Entry entry = entries.get(id);
      if (closed || entry == null || entry.generation != generation || !entry.canEject)
        return false;
      lease = entry.ejection;
    }
    // Never call the management model while holding the inventory monitor.
    return lease != null && lease.request.getAsBoolean();
  }

  @Override
  public void close() {
    synchronized (this) {
      if (closed) return;
      closed = true;
      entries.clear();
      permissionQueue.clear();
    }
    context.unregisterReceiver(receiver);
    if (computers != null) computers.removeListener(computerListener);
    main.removeCallbacksAndMessages(null);
    metadata.shutdownNow();
    listeners.clear();
    NativePolicy.selectionClose(selectionModel);
  }
}
