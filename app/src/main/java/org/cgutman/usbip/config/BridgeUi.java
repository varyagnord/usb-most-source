package org.cgutman.usbip.config;

import android.content.Context;
import android.content.res.ColorStateList;
import android.graphics.drawable.GradientDrawable;
import android.graphics.drawable.RippleDrawable;
import android.widget.Button;

/** Small shared native-view styles. No separate GUI runtime. */
public final class BridgeUi {
  private BridgeUi() {}

  public static Button button(Context context, String label, boolean primary) {
    Button view = new Button(context);
    view.setText(label);
    view.setAllCaps(false);
    view.setTextSize(15);
    float density = context.getResources().getDisplayMetrics().density;
    view.setMinHeight(Math.round(48 * density));
    view.setMinWidth(0);
    view.setMinimumWidth(0);
    int horizontal = Math.round(16 * density), vertical = Math.round(12 * density);
    view.setPadding(horizontal, vertical, horizontal, vertical);
    view.setTextColor(
        new ColorStateList(
            new int[][] {new int[] {-android.R.attr.state_enabled}, new int[] {}},
            new int[] {primary ? 0xFFE1E7EC : 0xFF586875, primary ? 0xFFFFFFFF : 0xFF225FA5}));
    GradientDrawable shape = new GradientDrawable();
    shape.setColor(primary ? 0xFF225FA5 : 0);
    shape.setCornerRadius(8 * density);
    view.setBackground(new RippleDrawable(ColorStateList.valueOf(0x20225FA5), shape, null));
    return view;
  }
}
