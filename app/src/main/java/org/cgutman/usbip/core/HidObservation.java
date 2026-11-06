package org.cgutman.usbip.core;

/** Independent Java oracle for safe metadata; deliberately does not call Rust. */
public final class HidObservation {
  private HidObservation() {}

  public static int[] classify(byte[] data) {
    if (data == null || data.length < 2 || data[1] < 1 || data[1] > 7) return new int[3];
    int slot = data[1], report = data[0] & 255;
    if (data.length == 15 && report == 0x20) {
      if (data[2] == 2) return new int[] {1, slot, data[3] & 31};
      if (data[2] == 0x42) return new int[] {2, slot, data[3] == 1 ? 1 : 0};
    }
    if (data.length == 7 && report == 0x10) {
      if (data[2] == 0x41) return new int[] {3, slot, (data[4] & 64) != 0 ? 1 : 0};
      if ((data[2] & 255) == 0x8f) return new int[] {4, slot, data[5] & 255};
    }
    if (data.length == 20 && report == 0x11 && data[2] == 0x0a && data[6] == 0) {
      int code = data[5] & 255;
      if (code == 0 || code == 0xae || code == 0xaf || code == 0xb0)
        return new int[] {5, slot, code};
    }
    return new int[3];
  }

  /** Internal accidental-corruption check. Never serialize or log this value. */
  public static long digest(byte[] data) {
    long hash = 0xcbf29ce484222325L;
    if (data != null) for (byte value : data) hash = (hash ^ (value & 255)) * 0x100000001b3L;
    return hash;
  }
}
