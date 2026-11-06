package org.cgutman.usbip.config;

import android.os.*;
import android.service.quicksettings.*;
import org.cgutman.usbip.service.UsbIpService;

/** Android owns tile visibility and unlock; actions share the foreground screen's checks. */
public final class BridgeTileService extends TileService {
  private final Handler main = new Handler(Looper.getMainLooper());
  private boolean listening;
  private final Runnable refresh =
      new Runnable() {
        public void run() {
          if (!listening) return;
          Tile tile = getQsTile();
          if (tile != null) {
            int action = QuickActions.capture(BridgeTileService.this).action;
            tile.setLabel("USB Мост");
            tile.setState(UsbIpService.running ? Tile.STATE_ACTIVE : Tile.STATE_INACTIVE);
            if (Build.VERSION.SDK_INT >= 29)
              tile.setSubtitle(
                  UsbIpService.running
                      ? "Включён"
                      : action == QuickActions.OPEN ? "Настройки" : "Выключен");
            tile.setContentDescription(
                UsbIpService.running
                    ? "Остановить USB Мост"
                    : action == QuickActions.OPEN ? "Открыть USB Мост" : "Запустить USB Мост");
            tile.updateTile();
          }
          main.postDelayed(this, 500);
        }
      };

  @Override
  public void onStartListening() {
    super.onStartListening();
    listening = true;
    main.removeCallbacks(refresh);
    main.post(refresh);
  }

  @Override
  public void onStopListening() {
    listening = false;
    main.removeCallbacks(refresh);
    super.onStopListening();
  }

  @Override
  public void onDestroy() {
    listening = false;
    main.removeCallbacks(refresh);
    super.onDestroy();
  }

  @Override
  public void onClick() {
    super.onClick();
    QuickActions.Binding binding = QuickActions.capture(this);
    Runnable open =
        () -> {
          try {
            if (Build.VERSION.SDK_INT >= 34)
              startActivityAndCollapse(QuickActions.intent(this, binding, false));
            else openLegacy(binding);
          } catch (RuntimeException unavailable) {
            android.widget.Toast.makeText(
                    this, "Откройте USB Мост", android.widget.Toast.LENGTH_LONG)
                .show();
          }
        };
    if (isLocked()) unlockAndRun(open);
    else open.run();
  }

  @SuppressWarnings("deprecation")
  @android.annotation.SuppressLint(
      "StartActivityAndCollapseDeprecated") // SDK<34 only; overload absent there.
  private void openLegacy(QuickActions.Binding binding) {
    // Pre34 requires an Intent here; the destination remains non-exported.
    startActivityAndCollapse(QuickActions.activityIntent(this, binding, false));
  }
}
