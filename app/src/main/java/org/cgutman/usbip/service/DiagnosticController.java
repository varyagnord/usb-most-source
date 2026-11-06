package org.cgutman.usbip.service;

import android.app.*;
import android.content.*;
import android.hardware.usb.*;
import android.os.*;
import java.nio.charset.StandardCharsets;
import java.util.*;
import org.cgutman.usbip.core.*;
import org.cgutman.usbip.server.DiagnosticAccess;
import org.cgutman.usbipserverforandroid.BuildConfig;
import org.json.*;

/** Explicit internal build feature. No diagnostics API is enabled in release. */
final class DiagnosticController implements DiagnosticAccess {
  private final Context context;
  private final ArrayDeque<HidTrace> history = new ArrayDeque<>();
  private final Set<HidTrace> active = new HashSet<>();
  private int mode;
  private long next = 1, historyDrops;
  private boolean testing;

  DiagnosticController(Context context) {
    if (!BuildConfig.DIAGNOSTICS) throw new IllegalStateException("not a diagnostic build");
    this.context = context.getApplicationContext();
    mode = context.getSharedPreferences("bridge", Context.MODE_PRIVATE).getInt("diagnostic_backend", 0);
    if (mode != 0 && mode != 1) mode = 0;
  }

  static boolean receiver(UsbDevice device) {
    if (device.getVendorId() != 0x046d || device.getProductId() != 0xc52b) return false;
    for (int c = 0; c < device.getConfigurationCount(); c++) {
      UsbConfiguration cfg = device.getConfiguration(c);
      for (int i = 0; i < cfg.getInterfaceCount(); i++) {
        UsbInterface f = cfg.getInterface(i);
        if (f.getId() != 2 || f.getInterfaceClass() != UsbConstants.USB_CLASS_HID) continue;
        for (int j = 0; j < f.getEndpointCount(); j++) {
          UsbEndpoint ep = f.getEndpoint(j);
          if (ep.getAddress() == 0x83 && ep.getType() == UsbConstants.USB_ENDPOINT_XFER_INT
              && ep.getMaxPacketSize() == 32) return true;
        }
      }
    }
    return false;
  }

  synchronized HidTrace begin(UsbDevice device) { return begin(device,false); }

  synchronized HidTrace begin(UsbDevice device, boolean wan) {
    if (!receiver(device)) return null;
    if (testing) throw new IllegalStateException("diagnostic_selftest_busy");
    if (AndroidReferenceBackend.isQuarantined()) throw new IllegalStateException("sdk_quarantine_restart_required");
    if (active.size() >= 4 || next == Long.MAX_VALUE) throw new IllegalStateException("diagnostic_session_limit");
    while (history.size() >= 4) {
      HidTrace old = null;
      for (HidTrace trace : history) if (!active.contains(trace)) { old = trace; break; }
      if (old == null) throw new IllegalStateException("diagnostic_history_busy");
      history.remove(old);
      historyDrops++;
    }
    HidTrace trace = new HidTrace(next++, wan ? 0 : mode, SystemClock::elapsedRealtime);
    history.addLast(trace);
    active.add(trace);
    return trace;
  }

  synchronized void ended(HidTrace trace) {
    if (trace != null && active.remove(trace)) trace.closed();
  }

  public synchronized boolean selectBackend(int mode) {
    if ((mode != 0 && mode != 1) || !active.isEmpty() || testing) return false;
    this.mode = mode;
    context.getSharedPreferences("bridge", Context.MODE_PRIVATE).edit().putInt("diagnostic_backend", mode).apply();
    return true;
  }

  public synchronized boolean clear() {
    if (!active.isEmpty() || testing) return false;
    history.clear();
    historyDrops = 0;
    return true;
  }

  public byte[] selftest() {
    synchronized (this) {
      if (testing || !active.isEmpty()) return "{\"selftest_busy\":true}".getBytes(StandardCharsets.UTF_8);
      testing = true;
    }
    try { return DiagnosticSelfTest.run(); }
    finally { synchronized (this) { testing = false; } }
  }

  private JSONArray exits() {
    JSONArray out = new JSONArray();
    if (Build.VERSION.SDK_INT >= 30) {
      try {
        ActivityManager manager = context.getSystemService(ActivityManager.class);
        for (ApplicationExitInfo exit : manager.getHistoricalProcessExitReasons(context.getPackageName(), 0, 4)) {
          // Numeric OS facts only. Do not export descriptions, traces or process payloads.
          out.put(new JSONObject().put("reason", exit.getReason()).put("status", exit.getStatus())
              .put("timestamp", exit.getTimestamp()).put("importance", exit.getImportance()));
        }
      } catch (RuntimeException | JSONException ignored) { /* Optional OEM API. */ }
    }
    return out;
  }

  public byte[] snapshot() {
    try {
      JSONObject out = new JSONObject().put("schema", 1).put("diagnostic_build", true)
          .put("version", BuildConfig.VERSION_NAME).put("version_code", BuildConfig.VERSION_CODE)
          .put("api", Build.VERSION.SDK_INT).put("elapsed_ms", SystemClock.elapsedRealtime())
          .put("wall_ms", System.currentTimeMillis()).put("service_running", UsbIpService.running)
          .put("service_status", UsbIpService.status).put("sdk_quarantined", AndroidReferenceBackend.isQuarantined());
      JSONArray traces = new JSONArray();
      synchronized (this) {
        out.put("selected_backend", mode).put("active_hid", active.size()).put("history_drops", historyDrops)
            .put("selftest_running", testing);
        for (HidTrace trace : history) traces.put(new JSONObject(trace.json()));
      }
      out.put("sessions", traces).put("exits", exits())
          .put("journal", DiagnosticLog.snapshotTail(context, 32768));
      byte[] bytes = out.toString().getBytes(StandardCharsets.UTF_8);
      if (bytes.length > MAX_BYTES) {
        out.remove("journal");
        out.put("journal_omitted", true);
        bytes = out.toString().getBytes(StandardCharsets.UTF_8);
      }
      if (bytes.length > MAX_BYTES) {
        out.put("sessions", new JSONArray()).put("trace_omitted", true);
        bytes = out.toString().getBytes(StandardCharsets.UTF_8);
      }
      return bytes;
    } catch (JSONException error) {
      return "{\"diagnostics_error\":true}".getBytes(StandardCharsets.UTF_8);
    }
  }
}
