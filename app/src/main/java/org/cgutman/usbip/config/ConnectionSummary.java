package org.cgutman.usbip.config;

import android.content.Context;
import org.cgutman.usbip.connection.ComputerRepository;

/** Presentation of a saved destination, never a connection or credential check. */
final class ConnectionSummary {
  final boolean configured;
  final String title, detail;

  private ConnectionSummary(boolean configured, String title, String detail) {
    this.configured = configured;
    this.title = title;
    this.detail = detail;
  }

  static ConnectionSummary read(Context context) {
    ComputerRepository.Snapshot snapshot = ComputerRepository.get(context).snapshot();
    if (snapshot.status == ComputerRepository.Status.LOADING)
      return new ConnectionSummary(false, "Загрузка компьютеров…", "");
    if (snapshot.status == ComputerRepository.Status.FAILED)
      return new ConnectionSummary(
          false,
          "Сохранённые данные недоступны",
          "Откройте «Компьютеры и доступ» для восстановления");
    ComputerRepository.Computer active = snapshot.active();
    if (active != null)
      return new ConnectionSummary(true, active.name, ComputersActivity.detail(active));
    return new ConnectionSummary(
        false, "Выберите компьютер", "Настройте подключение, затем выберите USB-устройства");
  }
}
