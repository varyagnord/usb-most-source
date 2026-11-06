package ru.levchatkin.rutokenbridge;

import android.app.PendingIntent;
import android.content.*;
import android.hardware.usb.*;
import android.os.*;
import java.lang.reflect.*;
import java.util.*;
import java.util.concurrent.*;
import org.cgutman.usbip.service.UsbInventory;

/**
 * Test APK only: real framework USB model objects, injected system adapter. The emulator-only
 * harness temporarily permits hidden constructors; production uses only the public Android USB API
 * and contains no fixture mode or test IPC.
 */
final class UsbFixture extends ContextWrapper implements UsbInventory.Access {
  final Map<String, UsbDevice> devices = new ConcurrentHashMap<>();
  final Set<Integer> permissions = ConcurrentHashMap.newKeySet();
  final Map<Integer, String> serials = new ConcurrentHashMap<>();
  final Map<Integer, byte[]> raw = new ConcurrentHashMap<>();
  final List<Request> requests = new CopyOnWriteArrayList<>();
  final Set<Integer> failedMetadata = ConcurrentHashMap.newKeySet();
  final Handler main = new Handler(Looper.getMainLooper());
  boolean autoGrant = true;
  volatile CountDownLatch openEntered, openRelease;
  volatile CountDownLatch serialEntered, serialRelease;
  volatile int blockedSerialId = -1;
  volatile boolean serialOnMain, serialGateTimedOut;
  volatile boolean openGateTimedOut;
  BroadcastReceiver receiver;

  static final class Request {
    final UsbDevice device;
    final PendingIntent result;
    final long generation;

    Request(UsbDevice d, long generation, PendingIntent p) {
      this.generation = generation;
      device = d;
      result = p;
    }
  }

  UsbFixture(Context context) {
    super(context);
  }

  private Intent capture(BroadcastReceiver r) {
    receiver = r;
    return null;
  }

  @Override
  public Intent registerReceiver(BroadcastReceiver r, IntentFilter f) {
    return capture(r);
  }

  @Override
  public Intent registerReceiver(BroadcastReceiver r, IntentFilter f, int flags) {
    return capture(r);
  }

  @Override
  public Intent registerReceiver(
      BroadcastReceiver r, IntentFilter f, String permission, Handler h) {
    return capture(r);
  }

  @Override
  public Intent registerReceiver(
      BroadcastReceiver r, IntentFilter f, String permission, Handler h, int flags) {
    return capture(r);
  }

  @Override
  public void unregisterReceiver(BroadcastReceiver r) {
    if (receiver != r) throw new AssertionError("unmatched receiver");
    receiver = null;
  }

  @Override
  public Map<String, UsbDevice> devices() {
    return new HashMap<>(devices);
  }

  @Override
  public boolean hasPermission(UsbDevice device) {
    return permissions.contains(device.getDeviceId());
  }

  @Override
  public String serial(UsbDevice device) {
    if (!hasPermission(device)) throw new SecurityException("fixture_permission");
    serialOnMain |= Looper.myLooper() == Looper.getMainLooper();
    String value = serials.get(device.getDeviceId());
    CountDownLatch entered = serialEntered, release = serialRelease;
    if (device.getDeviceId() == blockedSerialId && entered != null && release != null) {
      entered.countDown();
      try {
        serialGateTimedOut = !release.await(10, TimeUnit.SECONDS);
      } catch (InterruptedException cancelled) {
        Thread.currentThread().interrupt();
      }
    }
    return value;
  }

  @Override
  public void requestPermission(UsbDevice d, long generation, PendingIntent result) {
    Request request = new Request(d, generation, result);
    requests.add(request);
    if (autoGrant) main.post(() -> reply(request, true));
  }

  void reply(Request request, boolean granted) {
    try {
      Intent intent =
          new Intent("ru.levchatkin.rutokenbridge.USB_ACCESS")
              .putExtra("generation", request.generation);
      intent.putExtra(UsbManager.EXTRA_DEVICE, request.device);
      intent.putExtra(UsbManager.EXTRA_PERMISSION_GRANTED, granted);
      if (granted) permissions.add(request.device.getDeviceId());
      if (receiver != null) receiver.onReceive(this, intent);
    } catch (RuntimeException error) {
      throw new AssertionError(error);
    }
  }

  @Override
  public UsbInventory.Description describe(UsbDevice d) {
    return failedMetadata.contains(d.getDeviceId())
        ? null
        : new UsbInventory.Description(
            raw.get(d.getDeviceId()), d.getProductName().contains("Mouse") ? 1 : 3, 1);
  }

  @Override
  public UsbDeviceConnection open(UsbDevice d) {
    CountDownLatch entered = openEntered, release = openRelease;
    if (entered != null && release != null) {
      entered.countDown();
      boolean interrupted = false, finished = false;
      long deadline = SystemClock.elapsedRealtime() + 30000;
      while (!finished && SystemClock.elapsedRealtime() < deadline) {
        try {
          finished =
              release.await(
                  Math.max(1, deadline - SystemClock.elapsedRealtime()), TimeUnit.MILLISECONDS);
        } catch (InterruptedException ignored) {
          interrupted = true;
        }
      }
      openGateTimedOut = !finished;
      if (interrupted) Thread.currentThread().interrupt();
    }
    return null;
  } // No actual usbfs exists in this adapter.

  void connect(UsbDevice d, byte[] descriptors) {
    raw.put(d.getDeviceId(), descriptors);
    devices.put(d.getDeviceName(), d);
    dispatch(UsbManager.ACTION_USB_DEVICE_ATTACHED, d);
  }

  void disconnect(UsbDevice d) {
    devices.remove(d.getDeviceName());
    permissions.remove(d.getDeviceId());
    raw.remove(d.getDeviceId());
    dispatch(UsbManager.ACTION_USB_DEVICE_DETACHED, d);
  }

  void dispatch(String action, UsbDevice d) {
    if (receiver != null)
      receiver.onReceive(this, new Intent(action).putExtra(UsbManager.EXTRA_DEVICE, d));
  }

  static byte[] descriptor(int cls, boolean iso) {
    return new byte[] {
      18,
      1,
      0,
      2,
      0,
      0,
      0,
      64,
      0x34,
      0x12,
      1,
      0,
      0x34,
      0x12,
      0,
      0,
      0,
      1,
      9,
      2,
      25,
      0,
      1,
      1,
      0,
      (byte) 128,
      50,
      9,
      4,
      0,
      0,
      1,
      (byte) cls,
      0,
      (byte) (cls == 8 ? 0x50 : 0),
      0,
      7,
      5,
      (byte) 129,
      (byte) (iso ? 1 : cls == 3 ? 3 : 2),
      64,
      0,
      10
    };
  }

  static UsbDevice device(int number, String label, int cls, boolean iso) throws Exception {
    return device(number, label, cls, iso, number);
  }

  static UsbDevice device(int number, String label, int cls, boolean iso, int product)
      throws Exception {
    Constructor<UsbEndpoint> endpoint =
        UsbEndpoint.class.getDeclaredConstructor(int.class, int.class, int.class, int.class);
    endpoint.setAccessible(true);
    UsbEndpoint ep = endpoint.newInstance(129, iso ? 1 : cls == 3 ? 3 : 2, 64, 10);
    Constructor<UsbInterface> interf =
        UsbInterface.class.getDeclaredConstructor(
            int.class, int.class, String.class, int.class, int.class, int.class);
    interf.setAccessible(true);
    UsbInterface face = interf.newInstance(0, 0, "Test interface", cls, 0, cls == 8 ? 0x50 : 0);
    Method endpoints = UsbInterface.class.getDeclaredMethod("setEndpoints", Parcelable[].class);
    endpoints.setAccessible(true);
    endpoints.invoke(face, (Object) new UsbEndpoint[] {ep});
    Constructor<UsbConfiguration> config =
        UsbConfiguration.class.getDeclaredConstructor(
            int.class, String.class, int.class, int.class);
    config.setAccessible(true);
    UsbConfiguration cfg = config.newInstance(1, "Test configuration", 128, 50);
    Method interfaces =
        UsbConfiguration.class.getDeclaredMethod("setInterfaces", Parcelable[].class);
    interfaces.setAccessible(true);
    interfaces.invoke(cfg, (Object) new UsbInterface[] {face});
    Exception failure = null;
    for (Constructor<?> ctor : UsbDevice.class.getDeclaredConstructors()) {
      Class<?>[] types = ctor.getParameterTypes();
      if (types.length < 6 || types[0] != String.class) continue;
      Object[] values = new Object[types.length];
      int strings = 0, ints = 0;
      String[] texts = {
        String.format(java.util.Locale.ROOT, "/dev/bus/usb/001/%03d", number),
        "USB Most E2E",
        label,
        "1.00",
        "test-serial"
      };
      int[] numbers = {0x1234, product, 0, 0, 0};
      boolean usable = true;
      for (int i = 0; i < types.length; i++) {
        if (types[i] == String.class) values[i] = texts[Math.min(strings++, texts.length - 1)];
        else if (types[i] == int.class) values[i] = numbers[Math.min(ints++, numbers.length - 1)];
        else if (types[i] == boolean.class) values[i] = false;
        else if (types[i] == UsbConfiguration[].class) values[i] = new UsbConfiguration[] {cfg};
        else if (types[i].isInterface())
          values[i] =
              Proxy.newProxyInstance(
                  types[i].getClassLoader(),
                  new Class[] {types[i]},
                  (proxy, m, args) -> m.getReturnType() == String.class ? "test-serial" : null);
        else usable = false;
      }
      if (!usable) continue;
      try {
        ctor.setAccessible(true);
        UsbDevice result = (UsbDevice) ctor.newInstance(values);
        if (!java.util.Arrays.asList(types).contains(UsbConfiguration[].class)) {
          Method set = UsbDevice.class.getDeclaredMethod("setConfigurations", Parcelable[].class);
          set.setAccessible(true);
          set.invoke(result, (Object) new UsbConfiguration[] {cfg});
        }
        return result;
      } catch (ReflectiveOperationException e) {
        failure = e;
      }
    }
    throw new AssertionError("No supported framework USB constructor", failure);
  }
}
