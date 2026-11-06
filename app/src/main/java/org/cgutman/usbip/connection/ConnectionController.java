package org.cgutman.usbip.connection;

import android.content.Context;
import android.os.Handler;
import android.os.Looper;
import java.io.*;
import java.nio.charset.StandardCharsets;
import java.util.Arrays;
import org.cgutman.usbip.jni.NativeConnection;
import org.json.JSONObject;

/** Service-owned adapter. Networking and protected storage run off the UI thread. */
public final class ConnectionController implements AutoCloseable {
  public interface Events {
    void status(String text);

    void failure(String text);

    void disconnected();
  }

  public static final class TrustRequest {
    public final String pin;
    private final ConnectionController owner;

    private TrustRequest(ConnectionController owner, String pin) {
      this.owner = owner;
      this.pin = pin;
    }
  }

  public static volatile TrustRequest trustRequest;
  private static volatile ConnectionController active;
  private final Context context;
  private final Events events;
  private final Handler handler = new Handler(Looper.getMainLooper());
  private volatile long id;
  private volatile boolean closed;
  private volatile String observationKey;
  private final ComputerRepository repository;
  private final ComputerRepository.Session lease;

  public ConnectionController(Context context, Events events, ComputerRepository.Session lease) {
    this.context = context.getApplicationContext();
    this.events = events;
    this.lease = lease;
    repository = ComputerRepository.get(context);
  }

  public static boolean enabled(Context context) {
    ComputerRepository.Computer active = ComputerRepository.get(context).snapshot().active();
    return active != null && active.kind.equals("secure");
  }

  public static void approve(TrustRequest request, boolean accept) {
    ConnectionController current = active;
    if (request != null
        && trustRequest == request
        && current == request.owner
        && !current.closed
        && !current.lease.cancelled()
        && current.id != 0) {
      NativeConnection.approve(current.id, request.pin, accept);
      trustRequest = null;
    }
  }

  public void start(int port, byte[] proof) throws Exception {
    byte[] envelope = lease.takeConnection();
    if (envelope == null) throw new IOException("connection_lease_consumed");
    observationKey = lease.connectionKey();
    ConnectionObservation.begin(this, observationKey);
    byte[] ownedProof = proof.clone();
    Arrays.fill(proof, (byte) 0);
    new Thread(
            () -> {
              byte[] profile = null, secret = null;
              boolean handed = false;
              try {
                JSONObject connection =
                    new JSONObject(new String(envelope, StandardCharsets.UTF_8))
                        .getJSONObject("connection");
                if (!connection.getString("kind").equals("secure"))
                  throw new IOException("wrong_transport");
                JSONObject config = connection.getJSONObject("profile");
                if (config.getString("transport").equals("ssh")
                    && config.optBoolean("prepare", false)) {
                  try {
                    ReceiverAssets.ensure(context);
                  } catch (Exception failure) {
                    throw new IOException("receiver_assets", failure);
                  }
                }
                profile = config.toString().getBytes(StandardCharsets.UTF_8);
                secret =
                    connection
                        .getJSONObject("credentials")
                        .toString()
                        .getBytes(StandardCharsets.UTF_8);
                final byte[] ownedProfile = profile, ownedSecret = secret;
                handler.post(() -> {
                  try {
                    if (closed || lease.cancelled()) return;
                    id =
                        NativeConnection.start(
                            ownedProfile,
                            ownedSecret,
                            port,
                            ownedProof,
                            new File(context.getNoBackupFilesDir(), "receivers").getAbsolutePath());
                    if (id == 0) throw new IOException("invalid_profile");
                    active = this;
                    handler.post(poll);
                  } catch (Exception failure) {
                    ConnectionObservation.event(this, "idle", "internal_error", false);
                    events.failure("Не удалось открыть защищённое подключение");
                    close();
                  } finally {
                    Arrays.fill(ownedProfile, (byte) 0);
                    Arrays.fill(ownedSecret, (byte) 0);
                    Arrays.fill(ownedProof, (byte) 0);
                  }
                });
                handed = true;
              } catch (Exception failure) {
                handler.post(() -> {
                  if (!closed) {
                    ConnectionObservation.event(
                        this,
                        "idle",
                        "receiver_assets".equals(failure.getMessage())
                            ? "invalid_receiver"
                            : "storage_failed",
                        false);
                    events.failure("Не удалось подготовить файлы подключения");
                    close();
                  }
                });
              } finally {
                Arrays.fill(envelope, (byte) 0);
                if (!handed) {
                  if (profile != null) Arrays.fill(profile, (byte) 0);
                  if (secret != null) Arrays.fill(secret, (byte) 0);
                  Arrays.fill(ownedProof, (byte) 0);
                }
              }
            },
            "usb-most-profile")
        .start();
  }

  private final Runnable poll =
      new Runnable() {
        public void run() {
          if (closed || id == 0) return;
          try {
            for (int n = 0; n < 32; n++) {
              String raw = NativeConnection.poll(id);
              if (raw == null) break;
              JSONObject event = new JSONObject(raw);
              String kind = event.getString("event");
              if (kind.equals("trust")) {
                ConnectionObservation.event(ConnectionController.this, "trust", null, false);
                trustRequest = new TrustRequest(ConnectionController.this, event.getString("pin"));
                events.status("Подтвердите ключ компьютера");
              } else if (kind.equals("disconnected")) {
                ConnectionObservation.event(ConnectionController.this, "reconnecting", null, false);
                events.disconnected();
                NativeConnection.saved(id, event.getLong("save_id"), true);
              } else if (kind.equals("trusted") || kind.equals("paired")) {
                persist(event, id);
              } else if (kind.equals("state")) {
                ConnectionObservation.event(
                    ConnectionController.this,
                    event.getString("code"),
                    null,
                    event.optBoolean("receiver_verified", false));
                events.status(label(event.getString("code")));
              } else if (kind.equals("error")) {
                ConnectionObservation.event(
                    ConnectionController.this, "idle", event.getString("code"), false);
                events.failure("Не удалось подключиться: " + error(event.getString("code")));
                close();
                return;
              }
            }
          } catch (Exception error) {
            ConnectionObservation.event(ConnectionController.this, "idle", "storage_failed", false);
            events.failure("Не удалось сохранить защищённое подключение");
            close();
            return;
          }
          handler.postDelayed(this, 200);
        }
      };

  private void persist(JSONObject event, long handle) throws Exception {
    long operation = event.getLong("save_id");
    String kind = event.getString("event");
    String value = event.getString(kind.equals("trusted") ? "pin" : "token");
    repository
        .persist(lease, kind, value)
        .whenComplete((snapshot, failure) ->
            handler.post(() -> {
              boolean success = failure == null && !closed && id == handle && !lease.cancelled();
              if (success) {
                String next = lease.connectionKey();
                ConnectionObservation.rebind(this, observationKey, next);
                observationKey = next;
              }
              NativeConnection.saved(handle, operation, success);
              if (!success && !closed && id == handle) {
                ConnectionObservation.event(this, "idle", "storage_failed", false);
                events.failure("Не удалось сохранить защищённое подключение");
                close();
              }
            }));
  }

  private static String label(String code) {
    switch (code) {
      case "connecting":
        return "Подключение к компьютеру…";
      case "reconnecting":
        return "Восстановление подключения…";
      case "checking":
        return "Проверка компьютера…";
      case "preparing":
        return "Подготовка компьютера…";
      case "connected":
        return "Защищённое подключение";
      default:
        return "Подключение…";
    }
  }

  private static String error(String code) {
    switch (code) {
      case "host_key_rejected":
        return "ключ компьютера не подтверждён или изменился";
      case "authentication_failed":
        return "проверьте данные входа";
      case "invalid_key":
        return "не удалось прочитать приватный ключ";
      case "forwarding_denied":
        return "SSH-сервер запретил туннель";
      case "preparation_failed":
        return "подготовка не завершена; проверьте права администратора";
      case "linux_usb_driver_unavailable":
        return "в текущем ядре компьютера недоступен драйвер USB/IP (vhci_hcd)";
      case "linux_systemd_required":
        return "для приёмника Linux нужна система со службой systemd";
      case "missing_receiver":
        return "в сборке нет приёмника для этого компьютера";
      case "restart_required":
        return "перезагрузите компьютер и запустите подключение снова";
      case "windows_administrator_required":
        return "нужна учётная запись Windows с правами администратора";
      case "internal_error":
        return "подключение остановлено; запустите мост снова";
      case "computer_busy":
        return "компьютер занят другим подключением";
      case "pairing_required":
        return "сопряжение отозвано; получите новый код на компьютере";
      case "storage_failed":
        return "не удалось сохранить защищённые данные";
      case "timeout":
        return "компьютер не ответил вовремя";
      case "connection_lost":
        return "связь прервана";
      default:
        return "проверьте адрес, доступность компьютера и настройки";
    }
  }

  @Override
  public void close() {
    if (closed) return;
    closed = true;
    ConnectionObservation.close(this);
    handler.removeCallbacks(poll);
    lease.cancel();
    if (id != 0) {
      NativeConnection.stop(id);
      id = 0;
    }
    if (active == this) {
      active = null;
      trustRequest = null;
    }
  }
}
