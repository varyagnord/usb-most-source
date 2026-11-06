package org.cgutman.usbip.connection;

import android.content.Context;
import android.net.nsd.*;
import android.os.*;
import java.net.*;
import java.util.*;

/** Advisory Android NSD adapter. Discovery never creates a trusted profile. */
final class ComputerDiscovery implements AutoCloseable {
  static final class Computer {
    final String name, host;
    final int port;

    Computer(String name, String host, int port) {
      this.name = name;
      this.host = host;
      this.port = port;
    }

    @Override
    public String toString() {
      return name + "\n" + host;
    }
  }

  interface Listener {
    void update(List<Computer> computers, boolean available);
  }

  private final NsdManager manager;
  private final Handler ui = new Handler(Looper.getMainLooper());
  private final Listener listener;
  private final Map<String, NsdServiceInfo> present = new LinkedHashMap<>();
  private final Map<String, Computer> found = new LinkedHashMap<>();
  private final ArrayDeque<NsdServiceInfo> pending = new ArrayDeque<>();
  private boolean closed, resolving;
  private final NsdManager.DiscoveryListener discovery =
      new NsdManager.DiscoveryListener() {
        public void onDiscoveryStarted(String type) {
          ui.post(() -> {
            if (closed) stopDiscovery();
          });
        }

        public void onStartDiscoveryFailed(String type, int error) {
          ui.post(() -> {
            if (!closed) listener.update(new ArrayList<>(found.values()), false);
            stopDiscovery();
          });
        }

        public void onStopDiscoveryFailed(String type, int error) {
          ui.post(() -> {
            if (!closed) listener.update(new ArrayList<>(found.values()), false);
          });
        }

        public void onDiscoveryStopped(String type) {}

        public void onServiceFound(NsdServiceInfo service) {
          ui.post(() -> add(service));
        }

        public void onServiceLost(NsdServiceInfo service) {
          ui.post(() -> {
            if (closed) return;
            String name = service.getServiceName();
            present.remove(name);
            found.remove(name);
            pending.removeIf(value -> Objects.equals(value.getServiceName(), name));
            emit();
          });
        }
      };

  ComputerDiscovery(Context context, Listener listener) {
    this.manager =
        (NsdManager) context.getApplicationContext().getSystemService(Context.NSD_SERVICE);
    this.listener = listener;
  }

  void start() {
    if (closed) return;
    try {
      manager.discoverServices("_usbmost._tcp.", NsdManager.PROTOCOL_DNS_SD, discovery);
      ui.postDelayed(
          () -> {
            if (!closed) {
              listener.update(new ArrayList<>(found.values()), false);
              close();
            }
          },
          30000);
    } catch (RuntimeException error) {
      listener.update(Collections.emptyList(), false);
    }
  }

  private void add(NsdServiceInfo service) {
    if (closed
        || !("_usbmost._tcp.".equals(service.getServiceType())
            || "_usbmost._tcp".equals(service.getServiceType()))) return;
    String name = service.getServiceName();
    if (name == null || name.length() > 128 || present.containsKey(name) || present.size() >= 32)
      return;
    present.put(name, service);
    pending.add(service);
    resolveNext();
  }

  private void resolveNext() {
    if (closed || resolving || pending.isEmpty()) return;
    NsdServiceInfo original = pending.removeFirst();
    resolving = true;
    try {
      manager.resolveService(
          original,
          new NsdManager.ResolveListener() {
            public void onResolveFailed(NsdServiceInfo service, int error) {
              ui.post(() -> {
                resolving = false;
                resolveNext();
              });
            }

            public void onServiceResolved(NsdServiceInfo service) {
              ui.post(() -> {
                resolving = false;
                if (!closed && present.get(original.getServiceName()) == original) {
                  Computer computer = validate(service);
                  if (computer != null) {
                    found.put(original.getServiceName(), computer);
                    emit();
                  }
                }
                resolveNext();
              });
            }
          });
    } catch (RuntimeException error) {
      resolving = false;
      ui.post(this::resolveNext);
    }
  }

  private static Computer validate(NsdServiceInfo service) {
    if (service.getPort() < 1024 || service.getPort() > 65535) return null;
    byte[] version = service.getAttributes().get("v");
    if (version == null || version.length != 1 || version[0] != '1') return null;
    List<InetAddress> addresses;
    if (Build.VERSION.SDK_INT >= 34) addresses = service.getHostAddresses();
    else addresses = Collections.singletonList(service.getHost());
    InetAddress selected = null;
    for (InetAddress address : addresses) {
      if (address == null
          || address.isAnyLocalAddress()
          || address.isLoopbackAddress()
          || address.isMulticastAddress()
          || address.isLinkLocalAddress()) continue;
      if (selected == null || address instanceof Inet4Address) selected = address;
      if (selected instanceof Inet4Address) break;
    }
    if (selected == null) return null;
    String name = service.getServiceName();
    if (name == null) return null;
    StringBuilder clean = new StringBuilder();
    for (int i = 0; i < name.length() && clean.length() < 64; i++) {
      char c = name.charAt(i);
      if (!Character.isISOControl(c) && Character.getType(c) != Character.FORMAT) clean.append(c);
    }
    if (clean.length() == 0) return null;
    return new Computer(clean.toString(), selected.getHostAddress(), service.getPort());
  }

  private void emit() {
    if (!closed) listener.update(new ArrayList<>(found.values()), true);
  }

  private void stopDiscovery() {
    try {
      manager.stopServiceDiscovery(discovery);
    } catch (RuntimeException ignored) {
    }
  }

  @Override
  public void close() {
    if (closed) return;
    closed = true;
    ui.removeCallbacksAndMessages(null);
    stopDiscovery();
    present.clear();
    pending.clear();
    found.clear();
  }
}
