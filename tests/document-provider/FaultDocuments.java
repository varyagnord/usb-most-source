package ru.levchatkin.usbmost.testdocs;

import android.content.Context;
import android.database.Cursor;
import android.database.MatrixCursor;
import android.os.CancellationSignal;
import android.os.ParcelFileDescriptor;
import android.provider.DocumentsContract.Document;
import android.provider.DocumentsContract.Root;
import android.provider.DocumentsProvider;
import java.io.File;
import java.io.FileNotFoundException;
import java.io.FileOutputStream;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.util.UUID;

/** Emulator-only SAF provider; no network permission, payload logging or public storage. */
public final class FaultDocuments extends DocumentsProvider {
  static synchronized void record(Context context, String kind) {
    try (FileOutputStream out = new FileOutputStream(new File(context.getFilesDir(), "events.txt"), true)) {
      out.write((kind + "\n").getBytes(StandardCharsets.UTF_8));
      out.getFD().sync();
    } catch (IOException error) {
      throw new IllegalStateException("fixture_event_write", error);
    }
  }

  @Override public boolean onCreate() { return true; }
  private String name(String id) throws FileNotFoundException {
    if (!id.matches("doc-[0-9a-f]{32}")) throw new FileNotFoundException("fixture_id");
    String name = getContext().getSharedPreferences("documents", 0).getString(id, null);
    if (name == null) throw new FileNotFoundException("fixture_missing");
    return name;
  }
  private MatrixCursor documents(String[] projection) {
    return new MatrixCursor(projection == null ? new String[] {
      Document.COLUMN_DOCUMENT_ID, Document.COLUMN_DISPLAY_NAME,
      Document.COLUMN_MIME_TYPE, Document.COLUMN_FLAGS, Document.COLUMN_SIZE
    } : projection);
  }
  private void add(MatrixCursor c, String id) throws FileNotFoundException {
    boolean root = id.equals("root");
    c.newRow().add(Document.COLUMN_DOCUMENT_ID, id)
        .add(Document.COLUMN_DISPLAY_NAME, root ? "USB Most tests" : name(id))
        .add(Document.COLUMN_MIME_TYPE, root ? Document.MIME_TYPE_DIR : "text/plain")
        .add(Document.COLUMN_FLAGS, root ? Document.FLAG_DIR_SUPPORTS_CREATE : Document.FLAG_SUPPORTS_WRITE)
        .add(Document.COLUMN_SIZE, 0);
  }
  @Override public Cursor queryRoots(String[] projection) {
    MatrixCursor c = new MatrixCursor(projection == null ? new String[] {
      Root.COLUMN_ROOT_ID, Root.COLUMN_DOCUMENT_ID, Root.COLUMN_TITLE,
      Root.COLUMN_FLAGS, Root.COLUMN_MIME_TYPES, Root.COLUMN_ICON
    } : projection);
    c.newRow().add(Root.COLUMN_ROOT_ID, "root").add(Root.COLUMN_DOCUMENT_ID, "root")
        .add(Root.COLUMN_TITLE, "USB Most tests")
        .add(Root.COLUMN_FLAGS, Root.FLAG_SUPPORTS_CREATE | Root.FLAG_LOCAL_ONLY | Root.FLAG_SUPPORTS_IS_CHILD)
        .add(Root.COLUMN_MIME_TYPES, "text/plain").add(Root.COLUMN_ICON, android.R.drawable.ic_menu_save);
    return c;
  }
  @Override public Cursor queryDocument(String id, String[] projection) throws FileNotFoundException {
    MatrixCursor c = documents(projection); add(c, id); return c;
  }
  @Override public Cursor queryChildDocuments(String parent, String[] projection, String sort) throws FileNotFoundException {
    if (!parent.equals("root")) throw new FileNotFoundException("fixture_parent");
    return documents(projection);
  }
  @Override public boolean isChildDocument(String parent, String child) {
    try { name(child); return parent.equals("root"); }
    catch (FileNotFoundException missing) { return false; }
  }
  @Override public String createDocument(String parent, String mime, String displayName) throws FileNotFoundException {
    if (!parent.equals("root") || displayName == null || displayName.length() > 200)
      throw new FileNotFoundException("fixture_create");
    String id = "doc-" + UUID.randomUUID().toString().replace("-", "");
    if (!getContext().getSharedPreferences("documents", 0).edit().putString(id, displayName).commit())
      throw new FileNotFoundException("fixture_metadata");
    record(getContext(), "create:" + displayName);
    return id;
  }
  @Override public ParcelFileDescriptor openDocument(String id, String mode, CancellationSignal cancel) throws FileNotFoundException {
    String displayName = name(id);
    if (!mode.contains("w")) throw new FileNotFoundException("fixture_write_only");
    if (displayName.startsWith("fail-open-")) {
      record(getContext(), "fail-open");
      throw new FileNotFoundException("synthetic_provider_open_failure");
    }
    try {
      if (displayName.startsWith("fail-write-")) {
        ParcelFileDescriptor[] pipe = ParcelFileDescriptor.createReliablePipe();
        pipe[0].closeWithError("synthetic_provider_write_failure");
        record(getContext(), "fail-write");
        return pipe[1];
      }
      record(getContext(), "write-ok");
      return ParcelFileDescriptor.open(new File(getContext().getFilesDir(), id),
          ParcelFileDescriptor.MODE_CREATE | ParcelFileDescriptor.MODE_TRUNCATE | ParcelFileDescriptor.MODE_WRITE_ONLY);
    } catch (IOException error) {
      throw new FileNotFoundException("fixture_pipe");
    }
  }
}
