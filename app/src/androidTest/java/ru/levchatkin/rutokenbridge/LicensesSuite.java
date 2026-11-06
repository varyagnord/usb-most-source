package ru.levchatkin.rutokenbridge;

import android.app.*;
import android.content.*;
import android.os.SystemClock;
import android.view.*;
import android.view.accessibility.AccessibilityNodeInfo;
import android.widget.*;

/** Real optimized activity and packaged notices; does not open the external source URL. */
final class LicensesSuite {
  private final Instrumentation test;
  private int checks;

  LicensesSuite(Instrumentation test) {
    this.test = test;
  }

  private void check(boolean ok, String message) {
    checks++;
    if (!ok) throw new AssertionError(message);
  }

  private ListView list(View view) {
    if (view instanceof ListView) return (ListView) view;
    if (view instanceof ViewGroup)
      for (int i = 0; i < ((ViewGroup) view).getChildCount(); i++) {
        ListView found = list(((ViewGroup) view).getChildAt(i));
        if (found != null) return found;
      }
    return null;
  }

  private boolean contains(AccessibilityNodeInfo node, String text) {
    if (node == null) return false;
    if (node.getText() != null && node.getText().toString().contains(text)) return true;
    for (int i = 0; i < node.getChildCount(); i++)
      if (contains(node.getChild(i), text)) return true;
    return false;
  }

  int run() throws Exception {
    Activity activity =
        test.startActivitySync(
            new Intent()
                .setClassName(
                    "ru.levchatkin.rutokenbridge", "org.cgutman.usbip.config.LicensesActivity")
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
    try {
      test.waitForIdleSync();
      ListView list = list(activity.getWindow().getDecorView());
      check(
          list != null && list.getCount() > 100 && list.getCount() <= 512,
          "packaged notice index displayed");
      for (String[] target :
          new String[][] {
            {"USB Мост", "GNU GENERAL PUBLIC LICENSE"},
            {"usbip-win2", "BSD 2-Clause License"},
            {"Google Material Icons: USB", "Apache License"},
            {"qrcodegen", "Project Nayuki"},
            {"russh 0.", "Apache License"}
          }) {
        int index = -1;
        for (int i = 0; i < list.getCount(); i++)
          if (list.getItemAtPosition(i).toString().startsWith(target[0])) {
            index = i;
            break;
          }
        check(index >= 0, "expected dependency is listed");
        final int selected = index;
        test.runOnMainSync(() -> list.setSelection(selected));
        test.waitForIdleSync();
        Thread.sleep(150);
        test.runOnMainSync(() ->
            list.performItemClick(
                list.getChildAt(selected - list.getFirstVisiblePosition()),
                selected,
                list.getAdapter().getItemId(selected)));
        long deadline = SystemClock.elapsedRealtime() + 5000;
        while (!contains(test.getUiAutomation().getRootInActiveWindow(), target[1])
            && SystemClock.elapsedRealtime() < deadline) Thread.sleep(100);
        check(
            contains(test.getUiAutomation().getRootInActiveWindow(), target[1]),
            "actual packaged license text opened");
        test.getUiAutomation()
            .performGlobalAction(
                android.accessibilityservice.AccessibilityService.GLOBAL_ACTION_BACK);
        Thread.sleep(150);
      }
    } finally {
      test.runOnMainSync(activity::finish);
    }
    return checks;
  }
}
