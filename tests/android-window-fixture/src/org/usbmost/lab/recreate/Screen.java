package org.usbmost.lab.recreate;

import android.app.*;
import android.content.*;
import android.os.*;
import android.view.inputmethod.InputMethodManager;
import android.widget.*;

public final class Screen extends Activity {
  public static volatile Screen current;
  public Button button;
  public int clicks;
  public AlertDialog dialog;

  @Override
  public void onCreate(Bundle state) {
    super.onCreate(state);
    button = new Button(this);
    button.setText("Touch target");
    button.setOnClickListener(v -> {
      clicks++;
      button.setText("Clicked");
    });
    setContentView(button);
  }

  @Override
  public void onResume() {
    super.onResume();
    current = this;
  }

  @Override
  public void onPause() {
    if (dialog != null) {
      dialog.dismiss();
      dialog = null;
    }
    super.onPause();
  }

  public void open(boolean keyboard) {
    EditText edit = new EditText(this);
    edit.setText("Unsaved name");
    dialog =
        new AlertDialog.Builder(this)
            .setTitle("Device settings")
            .setView(edit)
            .setNegativeButton("Cancel", null)
            .create();
    dialog.show();
    if (keyboard) {
      edit.requestFocus();
      edit.postDelayed(
          () ->
              ((InputMethodManager) getSystemService(INPUT_METHOD_SERVICE))
                  .showSoftInput(edit, InputMethodManager.SHOW_IMPLICIT),
          100);
    }
  }
}
