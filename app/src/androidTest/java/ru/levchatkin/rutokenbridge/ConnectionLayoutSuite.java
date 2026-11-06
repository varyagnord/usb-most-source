package ru.levchatkin.rutokenbridge;

import android.app.*;
import android.content.*;
import android.graphics.*;
import android.os.SystemClock;
import android.view.*;
import android.view.inputmethod.InputMethodManager;
import android.widget.*;
import java.io.*;
import java.util.*;
import java.util.concurrent.TimeUnit;
import org.cgutman.usbip.connection.ComputerRepository;

/** Empty synthetic forms only: no network, stored credentials or physical USB. */
final class ConnectionLayoutSuite {
  private final Instrumentation test;
  private int checks;

  ConnectionLayoutSuite(Instrumentation test) {
    this.test = test;
  }

  private void check(boolean ok, String message) {
    checks++;
    if (!ok) throw new AssertionError(message);
  }

  private void main(Runnable action) {
    Throwable[] error = {null};
    test.runOnMainSync(() -> {
      try {
        action.run();
      } catch (Throwable e) {
        error[0] = e;
      }
    });
    if (error[0] != null) throw new AssertionError("connection layout", error[0]);
  }

  private void collect(View v, List<View> result) {
    if (v.isShown()) {
      result.add(v);
      if (v instanceof ViewGroup)
        for (int i = 0; i < ((ViewGroup) v).getChildCount(); i++)
          collect(((ViewGroup) v).getChildAt(i), result);
    }
  }

  private boolean contains(android.view.accessibility.AccessibilityNodeInfo node, String text) {
    if (node == null) return false;
    if (node.getText() != null && node.getText().toString().equalsIgnoreCase(text)) return true;
    for (int i = 0; i < node.getChildCount(); i++)
      if (contains(node.getChild(i), text)) return true;
    return false;
  }

  private void snapshot(Activity activity, String name) {
    // Render this empty fixture's own view tree, not a screenshot of other apps.
    View root = activity.getWindow().getDecorView();
    Bitmap image = Bitmap.createBitmap(root.getWidth(), root.getHeight(), Bitmap.Config.ARGB_8888);
    try (OutputStream out =
        new FileOutputStream(new File(test.getTargetContext().getExternalFilesDir(null), name))) {
      root.draw(new Canvas(image));
      check(image.compress(Bitmap.CompressFormat.PNG, 100, out), "fixture render saved");
    } catch (IOException e) {
      throw new AssertionError(e);
    } finally {
      image.recycle();
    }
  }

  int run() throws Exception {
    Context context = test.getTargetContext();
    ComputerRepository repository = ComputerRepository.get(context);
    ComputerRepository.Snapshot before = repository.ready().get(30, TimeUnit.SECONDS);
    Activity activity = null;
    try {
      activity =
          test.startActivitySync(
              new Intent()
                  .setClassName(
                      "ru.levchatkin.rutokenbridge", "org.cgutman.usbip.connection.ConnectionSetup")
                  .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                  .putExtra("computer_revision", before.revision));
      Activity current = activity;
      for (int mode = 0; mode < 3; mode++) {
        final int selected = mode;
        main(() -> {
          List<View> views = new ArrayList<>();
          collect(current.getWindow().getDecorView(), views);
          for (View view : views)
            if (view instanceof Spinner) ((Spinner) view).setSelection(selected);
        });
        test.waitForIdleSync();
        Thread.sleep(250);
        List<View> views = new ArrayList<>();
        main(() -> {
          View root = current.getWindow().getDecorView();
          collect(root, views);
          for (View view : views) {
            if (view instanceof TextView) {
              TextView text = (TextView) view;
              if (text.getTag() instanceof View)
                check(
                    text.getLabelFor() == ((View) text.getTag()).getId(),
                    "field label follows its final stable ID");
              check(text.getWidth() > 0 && text.getWidth() <= root.getWidth(), "field fits width");
              if (!(text instanceof EditText) && text.getText().length() > 0) {
                android.text.Layout content = text.getLayout();
                check(content != null, "text layout ready");
                for (int line = 0; line < content.getLineCount(); line++)
                  check(
                      content.getEllipsisCount(line) == 0,
                      "visible label not ellipsized: " + text.getText());
                check(
                    text.getHeight()
                        >= content.getHeight()
                            + text.getCompoundPaddingTop()
                            + text.getCompoundPaddingBottom(),
                    "visible label fits height: " + text.getText());
              }
              if (text instanceof Button) {
                check(
                    text.getHeight()
                        >= Math.round(48 * context.getResources().getDisplayMetrics().density),
                    "48dp button target");
                android.text.Layout layout = text.getLayout();
                check(layout != null, "button layout ready");
                for (int line = 0; line < layout.getLineCount(); line++)
                  check(layout.getEllipsisCount(line) == 0, "button label not ellipsized");
                check(
                    text.getHeight()
                        >= layout.getHeight()
                            + text.getCompoundPaddingTop()
                            + text.getCompoundPaddingBottom(),
                    "button label fits height");
              }
            }
            if (view instanceof ScrollView) ((ScrollView) view).scrollTo(0, 0);
          }
          snapshot(current, "connection-layout-" + selected + "-top.png");
        });
        for (View view : views)
          if (view instanceof Button) {
            main(() ->
                view.requestRectangleOnScreen(
                    new Rect(0, 0, view.getWidth(), view.getHeight()), true));
            Thread.sleep(80);
            main(() -> {
              Rect rect = new Rect();
              check(
                  view.getGlobalVisibleRect(rect) && rect.height() >= view.getHeight() - 1,
                  "button reachable by scrolling: " + ((Button) view).getText());
            });
          }
        main(() -> snapshot(current, "connection-layout-" + selected + "-bottom.png"));
        if (mode == 0) {
          EditText input = null;
          Button save = null;
          for (View view : views) {
            if (input == null && view instanceof EditText) input = (EditText) view;
            if (view instanceof Button
                && ((Button) view).getText().toString().equals("Сохранить компьютер"))
              save = (Button) view;
          }
          check(input != null && save != null, "SSH input and save present");
          EditText field = input;
          Button button = save;
          InputMethodManager ime =
              (InputMethodManager) context.getSystemService(Context.INPUT_METHOD_SERVICE);
          main(() -> {
            field.requestFocus();
            ime.showSoftInput(field, InputMethodManager.SHOW_IMPLICIT);
          });
          Thread.sleep(700);
          Rect visible = new Rect();
          boolean[] reachable = {false};
          long scrollDeadline = SystemClock.elapsedRealtime() + 5000;
          while (!reachable[0] && SystemClock.elapsedRealtime() < scrollDeadline) {
            main(() ->
                button.requestRectangleOnScreen(
                    new Rect(0, 0, button.getWidth(), button.getHeight()), true));
            Thread.sleep(150);
            main(() ->
                reachable[0] =
                    button.getGlobalVisibleRect(visible)
                        && visible.height() >= button.getHeight() - 1);
          }
          check(reachable[0], "save reachable with keyboard");
          long now = SystemClock.uptimeMillis();
          MotionEvent down =
              MotionEvent.obtain(
                  now, now, MotionEvent.ACTION_DOWN, visible.centerX(), visible.centerY(), 0);
          MotionEvent up =
              MotionEvent.obtain(
                  now, now + 40, MotionEvent.ACTION_UP, visible.centerX(), visible.centerY(), 0);
          down.setSource(InputDevice.SOURCE_TOUCHSCREEN);
          up.setSource(InputDevice.SOURCE_TOUCHSCREEN);
          try {
            check(
                test.getUiAutomation().injectInputEvent(down, true)
                    && test.getUiAutomation().injectInputEvent(up, true),
                "real touch save");
          } finally {
            down.recycle();
            up.recycle();
          }
          Thread.sleep(250);
          check(
              !current.isFinishing() && repository.snapshot().revision == before.revision,
              "empty form not saved or connected");
          long dialogDeadline = SystemClock.elapsedRealtime() + 5000;
          while (!contains(test.getUiAutomation().getRootInActiveWindow(), "Понятно")
              && SystemClock.elapsedRealtime() < dialogDeadline) Thread.sleep(100);
          check(
              contains(test.getUiAutomation().getRootInActiveWindow(), "Понятно"),
              "real touch reached validation dialog; target=" + visible);
          check(
              test.getUiAutomation()
                  .performGlobalAction(
                      android.accessibilityservice.AccessibilityService.GLOBAL_ACTION_BACK),
              "close validation dialog");
          main(() -> ime.hideSoftInputFromWindow(field.getWindowToken(), 0));
          Thread.sleep(300);
        }
      }
    } finally {
      Activity current = activity;
      if (current != null) main(current::finish);
    }
    return checks;
  }
}
