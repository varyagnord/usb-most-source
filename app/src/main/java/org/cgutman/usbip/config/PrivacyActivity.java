package org.cgutman.usbip.config;

import android.app.Activity;
import android.content.Intent;
import android.content.res.ColorStateList;
import android.graphics.Typeface;
import android.os.Bundle;
import android.view.Gravity;
import android.view.View;
import android.widget.Button;
import android.widget.FrameLayout;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;
import org.cgutman.usbipserverforandroid.R;

/** Offline explanation. Reading it never opens a connection or requests permission. */
public final class PrivacyActivity extends Activity {
  private static final int PAPER = 0xFFFAFBFC, INK = 0xFF172B3A, ACCENT = 0xFF225FA5;

  @Override
  public void onWindowFocusChanged(boolean hasFocus) {
    super.onWindowFocusChanged(hasFocus);
    if (hasFocus) {
      // Apply appearance to the focused window after the Activity transition.
      // AndroidX retains the API26 implementation without a newer theme item.
      androidx.core.view.WindowInsetsControllerCompat controller =
          androidx.core.view.WindowCompat.getInsetsController(getWindow(), getWindow().getDecorView());
      controller.setAppearanceLightStatusBars(true);
      controller.setAppearanceLightNavigationBars(true);
    }
  }

  @Override
  public void onCreate(Bundle state) {
    super.onCreate(state);
    if (getActionBar() != null) getActionBar().hide();
    setTitle(R.string.privacy_title);
    getWindow().setStatusBarColor(PAPER);
    getWindow().setNavigationBarColor(PAPER);
    getWindow().getDecorView().setSystemUiVisibility(
        View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR | View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR);
    ScrollView scroll = new ScrollView(this);
    scroll.setId(R.id.privacy_scroll);
    scroll.setBackgroundColor(PAPER);
    scroll.setFillViewport(true);
    FrameLayout centered = new FrameLayout(this);
    LinearLayout body = new LinearLayout(this) {
      @Override
      protected void onMeasure(int widthSpec, int heightSpec) {
        int width = Math.min(View.MeasureSpec.getSize(widthSpec), dp(640));
        int padding = width < dp(360) ? dp(16) : dp(24);
        setPadding(padding, dp(16), padding, dp(24));
        super.onMeasure(View.MeasureSpec.makeMeasureSpec(width, View.MeasureSpec.EXACTLY), heightSpec);
      }
    };
    body.setOrientation(LinearLayout.VERTICAL);
    centered.addView(body, new FrameLayout.LayoutParams(-1, -2, Gravity.TOP | Gravity.CENTER_HORIZONTAL));
    scroll.addView(centered);
    setContentView(scroll);
    Button back = button(body, "Назад", R.id.privacy_back);
    back.setOnClickListener(v -> finish());
    text(body, R.string.privacy_title, 26, true);
    text(body, R.string.privacy_intro, 16, false);
    int[][] sections = {
      {R.string.privacy_usb_heading, R.string.privacy_usb_text},
      {R.string.privacy_saved_heading, R.string.privacy_saved_text},
      {R.string.privacy_network_heading, R.string.privacy_network_text},
      {R.string.privacy_permissions_heading, R.string.privacy_permissions_text},
      {R.string.privacy_diagnostics_heading, R.string.privacy_diagnostics_text},
      {R.string.privacy_removal_heading, R.string.privacy_removal_text}
    };
    for (int[] section : sections) {
      text(body, section[0], 20, true);
      text(body, section[1], 16, false);
    }
    Button access = button(body, getString(R.string.privacy_access_action), R.id.privacy_access);
    access.setOnClickListener(v -> startActivity(new Intent(this, ComputersActivity.class)));
  }

  private void text(LinearLayout body, int resource, int size, boolean heading) {
    TextView text = new TextView(this);
    text.setText(resource);
    text.setTextSize(size);
    text.setTextColor(INK);
    text.setTextIsSelectable(true);
    text.setLineSpacing(dp(3), 1);
    if (heading) {
      text.setTypeface(Typeface.DEFAULT, Typeface.BOLD);
      androidx.core.view.ViewCompat.setAccessibilityHeading(text, true);
    }
    LinearLayout.LayoutParams layout = new LinearLayout.LayoutParams(-1, -2);
    layout.topMargin = dp(heading ? 24 : 10);
    body.addView(text, layout);
  }

  private Button button(LinearLayout body, String label, int id) {
    Button button = new Button(this);
    button.setId(id);
    button.setText(label);
    button.setAllCaps(false);
    button.setTextSize(16);
    button.setTextColor(ACCENT);
    button.setBackgroundTintList(ColorStateList.valueOf(0xFFEAF0F6));
    button.setMinHeight(dp(52));
    button.setPadding(dp(12), dp(8), dp(12), dp(8));
    LinearLayout.LayoutParams layout = new LinearLayout.LayoutParams(-1, -2);
    layout.topMargin = dp(12);
    body.addView(button, layout);
    return button;
  }

  private int dp(int value) {
    return Math.round(value * getResources().getDisplayMetrics().density);
  }
}
