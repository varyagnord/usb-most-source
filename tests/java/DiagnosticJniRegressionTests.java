package org.cgutman.usbip.service;

import java.io.*;
import java.lang.reflect.Field;
import java.nio.charset.StandardCharsets;
import java.util.Arrays;
import org.cgutman.usbip.jni.NativeUsb;

public final class DiagnosticJniRegressionTests {
  private static int checks;
  private static void check(boolean value, String reason) {
    checks++;
    if (!value) throw new AssertionError(reason);
  }
  public static void main(String[] args) throws Exception {
    check(NativeUsb.enableHidDiagnostics(-1) == -22, "invalid trace handle");
    check(NativeUsb.takeHidDiagnostics(-1) == null, "invalid trace read");
    try (FileInputStream input = new FileInputStream("/dev/null")) {
      Field field = FileDescriptor.class.getDeclaredField("fd");
      field.setAccessible(true);
      int fd = field.getInt(input.getFD());
      long handle = NativeUsb.open(fd);
      check(handle > 0, "native trace session");
      check(NativeUsb.enableHidDiagnostics(handle) == 0, "trace enabled before submit");
      check(NativeUsb.enableHidDiagnostics(handle) == -114, "trace cannot reset mid-session");
      check(Arrays.equals(NativeUsb.takeHidDiagnostics(handle), new long[] {1,9,0,0,0,0}), "typed diagnostic array ABI");
      check(NativeUsb.submit(handle, 1, 1, 0x83, 0, null, 32, null) == -25, "non-USB fd never accepts a kernel borrow");
      check(NativeUsb.takeHidDiagnostics(handle)[3] == 0, "failed submissions never invent completions");
      NativeUsb.close(handle);
      check(input.read() == -1, "caller fd remains owned by caller");
      check(NativeUsb.takeHidDiagnostics(handle) == null, "stale trace handle rejected");
    }
    check(Arrays.equals(NativeUsb.wanCommand(-1,1,1),new long[]{-22}),"WAN invalid handle");
    try(FileInputStream input=new FileInputStream("/dev/null")) {
      Field field=FileDescriptor.class.getDeclaredField("fd");field.setAccessible(true);
      long h=NativeUsb.open(field.getInt(input.getFD()));
      check(h>0,"WAN fixture owns duplicate fd");
      check(NativeUsb.wanCommand(h,1,2)[0]==-22,"unknown WAN profile rejected");
      long[] stats=NativeUsb.wanCommand(h,1,1);
      check(stats.length==11 && stats[0]==0 && stats[1]==1 && stats[5]==2048,"bounded WAN JNI metadata ABI");
      check(NativeUsb.wanCommand(h,1,1)[0]==-22,"WAN cannot enable twice");
      check(NativeUsb.wanCommand(h,2,1)[0]==-71,"cannot acknowledge unsent reports");
      check(NativeUsb.wanCommand(h,2,0)[0]==0,"zero consumed ACK does not grant credit");
      check(NativeUsb.wanCommand(h,99,0)[0]==-22,"unknown command rejected");
      check(NativeUsb.wanCommand(h,5,0)[0]==-25,"nonUSB file cannot start physical reads");
      NativeUsb.close(h);check(input.read()==-1,"WAN close retains caller fd");
      check(NativeUsb.wanCommand(h,4,0)[0]==-22,"stale WAN handle rejected");
    }
    String result = new String(DiagnosticSelfTest.run(), StandardCharsets.UTF_8);
    check(result.contains("\"success\":true"), "embedded positive JNI/SDK selftest: " + result);
    check(!result.contains("digest") && !result.contains("payload"), "selftest never exports byte fingerprints");
    System.out.println("PASS diagnostic real-JVM JNI: " + checks + " checks; " + result);
  }
}
