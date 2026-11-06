package org.cgutman.usbip.connection;

import android.Manifest;
import android.app.Activity;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.graphics.*;
import android.hardware.camera2.*;
import android.hardware.camera2.params.StreamConfigurationMap;
import android.media.Image;
import android.media.ImageReader;
import android.os.*;
import android.util.Size;
import android.view.*;
import android.widget.*;
import java.nio.ByteBuffer;
import java.util.*;
import org.cgutman.usbip.jni.NativeConnection;

/** Camera lifecycle is Android-owned; bounded QR decoding/validation is Rust-owned. */
public final class PairScanner extends Activity {
  private TextureView preview;
  private TextView hint;
  private Capture capture;
  private boolean resumed, asked;

  @Override
  public void onCreate(Bundle state) {
    super.onCreate(state);
    setTitle("Код с компьютера");
    getWindow().addFlags(WindowManager.LayoutParams.FLAG_SECURE);
    asked = state != null && state.getBoolean("asked");
    LinearLayout layout = new LinearLayout(this);
    layout.setOrientation(LinearLayout.VERTICAL);
    layout.setBackgroundColor(0xFFFAFBFC);
    int pad = Math.round(20 * getResources().getDisplayMetrics().density);
    layout.setPadding(pad, pad, pad, pad);
    hint = new TextView(this);
    hint.setText("Наведите камеру на QR-код USB Мост на компьютере");
    hint.setTextColor(0xFF172B3A);
    hint.setTextSize(17);
    layout.addView(hint);
    preview = new TextureView(this);
    layout.addView(preview, new LinearLayout.LayoutParams(-1, 0, 1));
    Button back = new Button(this);
    back.setText("Ввести код вручную");
    back.setOnClickListener(v -> finish());
    layout.addView(back);
    setContentView(layout);
    preview.setSurfaceTextureListener(
        new TextureView.SurfaceTextureListener() {
          public void onSurfaceTextureAvailable(SurfaceTexture texture, int width, int height) {
            begin();
          }

          public void onSurfaceTextureSizeChanged(SurfaceTexture texture, int width, int height) {
            if (capture != null) capture.transform();
          }

          public boolean onSurfaceTextureDestroyed(SurfaceTexture texture) {
            end();
            return true;
          }

          public void onSurfaceTextureUpdated(SurfaceTexture texture) {}
        });
  }

  @Override
  protected void onResume() {
    super.onResume();
    resumed = true;
    if (checkSelfPermission(Manifest.permission.CAMERA) != PackageManager.PERMISSION_GRANTED) {
      if (!asked) {
        asked = true;
        requestPermissions(new String[] {Manifest.permission.CAMERA}, 42);
      } else
        hint.setText(
            "Для сканирования разрешите камеру в настройках приложения. Можно ввести код вручную.");
    } else begin();
  }

  @Override
  protected void onPause() {
    resumed = false;
    end();
    super.onPause();
  }

  @Override
  protected void onSaveInstanceState(Bundle state) {
    state.putBoolean("asked", asked);
    super.onSaveInstanceState(state);
  }

  @Override
  public void onRequestPermissionsResult(int request, String[] permissions, int[] results) {
    super.onRequestPermissionsResult(request, permissions, results);
    if (request == 42 && results.length > 0 && results[0] == PackageManager.PERMISSION_GRANTED)
      begin();
    else hint.setText("Камера не разрешена. Введите код подключения вручную.");
  }

  private void begin() {
    if (!resumed
        || capture != null
        || !preview.isAvailable()
        || checkSelfPermission(Manifest.permission.CAMERA) != PackageManager.PERMISSION_GRANTED)
      return;
    capture = new Capture(preview.getSurfaceTexture());
    capture.handler.post(capture::open);
  }

  private void end() {
    Capture old = capture;
    capture = null;
    if (old != null) old.stop();
  }

  /** Each resumed capture owns its own thread/resources; old callbacks never close a new camera. */
  private final class Capture {
    final HandlerThread thread = new HandlerThread("usb-most-qr");
    final Handler handler;
    final SurfaceTexture texture;
    volatile boolean closed;
    boolean opening;
    CameraDevice camera;
    CameraCaptureSession session;
    ImageReader reader;
    Surface surface;
    volatile Size size;
    volatile int angle;
    long decodedAt;

    Capture(SurfaceTexture texture) {
      this.texture = texture;
      thread.start();
      handler = new Handler(thread.getLooper());
    }

    void fail() {
      runOnUiThread(() -> {
        if (capture == this && resumed)
          hint.setText("Камера недоступна. Можно ввести код вручную.");
      });
      closed = true;
      closeOwned();
    }

    void stop() {
      closed = true;
      handler.post(() -> {
        if (!opening) closeOwned();
      });
    }

    void closeOwned() {
      if (session != null) {
        session.close();
        session = null;
      }
      if (camera != null) {
        camera.close();
        camera = null;
      }
      if (reader != null) {
        reader.close();
        reader = null;
      }
      if (surface != null) {
        surface.release();
        surface = null;
      }
      thread.quitSafely();
    }

    @android.annotation.SuppressLint(
        "MissingPermission") // UI checks permission; revocation is caught.
    void open() {
      if (closed) {
        closeOwned();
        return;
      }
      try {
        CameraManager manager = (CameraManager) getSystemService(CAMERA_SERVICE);
        String selected = null;
        for (String id : manager.getCameraIdList()) {
          if (selected == null) selected = id;
          if (Objects.equals(
              manager.getCameraCharacteristics(id).get(CameraCharacteristics.LENS_FACING),
              CameraCharacteristics.LENS_FACING_BACK)) {
            selected = id;
            break;
          }
        }
        if (selected == null) {
          fail();
          return;
        }
        CameraCharacteristics characteristics = manager.getCameraCharacteristics(selected);
        StreamConfigurationMap map =
            characteristics.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP);
        if (map == null) {
          fail();
          return;
        }
        Size[] yuv = map.getOutputSizes(ImageFormat.YUV_420_888);
        Size[] textures = map.getOutputSizes(SurfaceTexture.class);
        if (yuv == null || textures == null) {
          fail();
          return;
        }
        List<Size> allowed = Arrays.asList(textures);
        long distance = Long.MAX_VALUE;
        for (Size option : yuv) {
          long area = (long) option.getWidth() * option.getHeight();
          if (option.getWidth() < 32
              || option.getHeight() < 32
              || option.getWidth() > 1280
              || option.getHeight() > 1280
              || area > 1024 * 1024
              || !allowed.contains(option)) continue;
          long score = Math.abs(area - 640L * 480);
          if (score < distance) {
            size = option;
            distance = score;
          }
        }
        if (size == null) {
          fail();
          return;
        }
        Integer sensor = characteristics.get(CameraCharacteristics.SENSOR_ORIENTATION);
        angle = sensor == null ? 0 : sensor;
        runOnUiThread(this::transform);
        texture.setDefaultBufferSize(size.getWidth(), size.getHeight());
        surface = new Surface(texture);
        reader =
            ImageReader.newInstance(size.getWidth(), size.getHeight(), ImageFormat.YUV_420_888, 2);
        reader.setOnImageAvailableListener(this::frame, handler);
        opening = true;
        manager.openCamera(
            selected,
            new CameraDevice.StateCallback() {
              public void onOpened(CameraDevice device) {
                opening = false;
                if (closed) {
                  device.close();
                  closeOwned();
                  return;
                }
                camera = device;
                try {
                  CaptureRequest.Builder request =
                      device.createCaptureRequest(CameraDevice.TEMPLATE_PREVIEW);
                  request.addTarget(surface);
                  request.addTarget(reader.getSurface());
                  request.set(CaptureRequest.CONTROL_MODE, CameraMetadata.CONTROL_MODE_AUTO);
                  int[] modes =
                      characteristics.get(CameraCharacteristics.CONTROL_AF_AVAILABLE_MODES);
                  if (modes != null)
                    for (int mode : modes)
                      if (mode == CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_PICTURE)
                        request.set(CaptureRequest.CONTROL_AF_MODE, mode);
                  device.createCaptureSession(
                      Arrays.asList(surface, reader.getSurface()),
                      new CameraCaptureSession.StateCallback() {
                        public void onConfigured(CameraCaptureSession ready) {
                          if (closed) {
                            ready.close();
                            return;
                          }
                          session = ready;
                          try {
                            ready.setRepeatingRequest(request.build(), null, handler);
                          } catch (CameraAccessException | IllegalStateException error) {
                            fail();
                          }
                        }

                        public void onConfigureFailed(CameraCaptureSession failed) {
                          failed.close();
                          fail();
                        }
                      },
                      handler);
                } catch (CameraAccessException | RuntimeException error) {
                  fail();
                }
              }

              public void onDisconnected(CameraDevice device) {
                opening = false;
                device.close();
                fail();
              }

              public void onError(CameraDevice device, int error) {
                opening = false;
                device.close();
                fail();
              }
            },
            handler);
      } catch (CameraAccessException | RuntimeException error) {
        opening = false;
        fail();
      }
    }

    void transform() {
      if (capture != this
          || closed
          || size == null
          || preview.getWidth() == 0
          || preview.getHeight() == 0) return;
      int rotation = getWindowManager().getDefaultDisplay().getRotation() * 90;
      int degrees = (angle - rotation + 360) % 360;
      float w = size.getWidth(), h = size.getHeight();
      float rw = degrees % 180 == 0 ? w : h, rh = degrees % 180 == 0 ? h : w;
      float scale = Math.min(preview.getWidth() / rw, preview.getHeight() / rh);
      Matrix matrix = new Matrix();
      matrix.setRectToRect(
          new RectF(0, 0, preview.getWidth(), preview.getHeight()),
          new RectF(-w / 2, -h / 2, w / 2, h / 2),
          Matrix.ScaleToFit.FILL);
      matrix.postRotate(degrees);
      matrix.postScale(scale, scale);
      matrix.postTranslate(preview.getWidth() / 2f, preview.getHeight() / 2f);
      preview.setTransform(matrix);
    }

    void frame(ImageReader source) {
      byte[] gray = null;
      try (Image image = source.acquireLatestImage()) {
        if (image == null || closed || SystemClock.elapsedRealtime() - decodedAt < 250) return;
        decodedAt = SystemClock.elapsedRealtime();
        Rect crop = image.getCropRect();
        int w = crop.width(), h = crop.height();
        if (w < 32 || h < 32 || w > 1280 || h > 1280 || (long) w * h > 1024 * 1024) return;
        Image.Plane plane = image.getPlanes()[0];
        ByteBuffer buffer = plane.getBuffer();
        int row = plane.getRowStride(), pixel = plane.getPixelStride(), base = buffer.position();
        long last = (long) base + (long) (crop.bottom - 1) * row + (long) (crop.right - 1) * pixel;
        if (row <= 0 || pixel <= 0 || crop.left < 0 || crop.top < 0 || last >= buffer.limit())
          return;
        gray = new byte[w * h];
        for (int y = 0; y < h; y++)
          for (int x = 0; x < w; x++)
            gray[y * w + x] = buffer.get(base + (crop.top + y) * row + (crop.left + x) * pixel);
        String code = NativeConnection.decodeQr(gray, w, h);
        if (code != null && !closed)
          runOnUiThread(() -> {
            if (capture != this || closed || !resumed) return;
            setResult(RESULT_OK, new Intent().putExtra("pair_code", code));
            finish();
          });
      } catch (RuntimeException error) {
        if (!closed) fail();
      } finally {
        if (gray != null) Arrays.fill(gray, (byte) 0);
      }
    }
  }
}
