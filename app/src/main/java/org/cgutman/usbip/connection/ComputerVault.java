package org.cgutman.usbip.connection;

import android.content.Context;
import android.os.Build;
import android.os.Looper;
import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyProperties;
import android.system.Os;
import android.system.OsConstants;
import androidx.core.util.AtomicFile;
import java.io.*;
import java.nio.charset.StandardCharsets;
import java.security.KeyStore;
import java.security.MessageDigest;
import java.util.Arrays;
import javax.crypto.*;
import javax.crypto.spec.GCMParameterSpec;

/**
 * Worker-only Android persistence boundary for the complete saved-computer catalog. No plaintext
 * files or backups; missing keys are never silently replaced on read. The repository must
 * invalidate its cache on ANY write failure, including one after rename: failed directory sync
 * makes the commit outcome uncertain.
 */
public final class ComputerVault {
  private static final Object LOCK = new Object();
  private static final String ALIAS = "usb-most.computers.v2";
  private static final int MAX_PLAIN = 4 * 1024 * 1024;
  private static final byte[] AAD = "usb-most/computers/v2".getBytes(StandardCharsets.US_ASCII);
  private final File base;
  private final AtomicFile file;

  public ComputerVault(Context context) {
    base = new File(context.getNoBackupFilesDir(), "computers.enc");
    file = new AtomicFile(base);
  }

  private static void worker() {
    if (Looper.myLooper() == Looper.getMainLooper())
      throw new IllegalStateException("computer_storage_on_main_thread");
  }

  public boolean exists() {
    worker();
    synchronized (LOCK) {
      // A lone .new was never committed; an old v1 source may still be migrated.
      // The already pinned AndroidX implementation writes .new on every API;
      // the Android 8 framework implementation could truncate the only file.
      // openRead also supports recovery of a legacy .bak.
      return base.exists() || new File(base.getPath() + ".bak").exists();
    }
  }

  private static SecretKey key(boolean create) throws Exception {
    KeyStore store = KeyStore.getInstance("AndroidKeyStore");
    store.load(null);
    if (store.containsAlias(ALIAS)) {
      SecretKey value = (SecretKey) store.getKey(ALIAS, null);
      if (value == null) throw new IOException("computer_key_unavailable");
      return value;
    }
    if (!create) throw new IOException("computer_key_missing");
    KeyGenerator generator =
        KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore");
    generator.init(
        new KeyGenParameterSpec.Builder(
                ALIAS, KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setKeySize(256)
            .build());
    return generator.generateKey();
  }

  private byte[] readStored() throws IOException {
    try (FileInputStream input = file.openRead()) {
      long size = input.getChannel().size();
      if (size < 30 || size > MAX_PLAIN + 30) throw new IOException("computer_storage_size");
      byte[] stored = new byte[(int) size];
      try {
        int offset = 0;
        while (offset < stored.length) {
          int n = input.read(stored, offset, stored.length - offset);
          if (n < 0) throw new IOException("computer_storage_truncated");
          offset += n;
        }
        if (input.read() != -1) throw new IOException("computer_storage_size");
        return stored;
      } catch (Exception failure) {
        Arrays.fill(stored, (byte) 0);
        throw failure;
      }
    }
  }

  public byte[] load() throws Exception {
    worker();
    synchronized (LOCK) {
      byte[] stored = readStored();
      try {
        if (stored[0] != 2 || stored[1] != 12) throw new IOException("computer_storage_format");
        Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
        cipher.init(Cipher.DECRYPT_MODE, key(false), new GCMParameterSpec(128, stored, 2, 12));
        cipher.updateAAD(AAD);
        return cipher.doFinal(stored, 14, stored.length - 14);
      } finally {
        Arrays.fill(stored, (byte) 0);
      }
    }
  }

  public void save(byte[] plain) throws Exception {
    worker();
    if (plain == null || plain.length > MAX_PLAIN || plain.length == 0)
      throw new IOException("computer_storage_size");
    synchronized (LOCK) {
      // Never create a replacement key over an existing encrypted catalog.
      SecretKey key = key(!exists());
      Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
      cipher.init(Cipher.ENCRYPT_MODE, key);
      cipher.updateAAD(AAD);
      byte[] encrypted = cipher.doFinal(plain);
      byte[] iv = cipher.getIV();
      byte[] stored = new byte[14 + encrypted.length];
      FileOutputStream output = null;
      boolean finished = false;
      try {
        if (iv.length != 12) throw new IOException("computer_storage_iv");
        stored[0] = 2;
        stored[1] = 12;
        System.arraycopy(iv, 0, stored, 2, 12);
        System.arraycopy(encrypted, 0, stored, 14, encrypted.length);
        output = file.startWrite();
        output.write(stored);
        output.flush();
        output.getFD().sync(); // AtomicFile.finishWrite may only log sync errors.
        file.finishWrite(output);
        finished = true;
        // finishWrite also logs certain rename failures instead of throwing.
        byte[] readback = readStored();
        try {
          if (!MessageDigest.isEqual(stored, readback))
            throw new IOException("computer_storage_commit");
        } finally {
          Arrays.fill(readback, (byte) 0);
        }
        int flags = OsConstants.O_RDONLY;
        if (Build.VERSION.SDK_INT >= 27) flags |= OsConstants.O_CLOEXEC;
        FileDescriptor directory = Os.open(base.getParent(), flags, 0);
        try {
          if (!OsConstants.S_ISDIR(Os.fstat(directory).st_mode))
            throw new IOException("computer_storage_directory");
          Os.fsync(directory);
        } finally {
          Os.close(directory);
        }
      } catch (Exception failure) {
        if (output != null && !finished) file.failWrite(output);
        throw failure;
      } finally {
        Arrays.fill(encrypted, (byte) 0);
        Arrays.fill(stored, (byte) 0);
        Arrays.fill(iv, (byte) 0);
      }
    }
  }

  /**
   * Explicit user-requested recovery only. The old ciphertext is never read with a newly generated
   * key. An interrupted reset remains unavailable and must be retried explicitly; callers retire
   * all v1 sources beforehand.
   */
  void reset(byte[] emptyCatalog) throws Exception {
    worker();
    synchronized (LOCK) {
      KeyStore store = KeyStore.getInstance("AndroidKeyStore");
      store.load(null);
      if (store.containsAlias(ALIAS)) store.deleteEntry(ALIAS);
      if (store.containsAlias(ALIAS)) throw new IOException("computer_key_cleanup");
      key(true);
      save(emptyCatalog);
    }
  }
}
