package org.cgutman.usbip.connection;

import android.content.Context;
import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyProperties;
import android.util.AtomicFile;
import java.io.*;
import java.security.KeyStore;
import java.util.Arrays;
import javax.crypto.*;
import javax.crypto.spec.GCMParameterSpec;

/** Non-exportable Android Keystore key; no backups, password logs or plaintext files. */
public final class SecretVault {
  public static final Object LOCK = new Object();
  private static final String ALIAS = "usb-most.connection.v1";
  private final AtomicFile file;

  public SecretVault(Context context) {
    file = new AtomicFile(new File(context.getNoBackupFilesDir(), "connection.enc"));
  }

  private static synchronized SecretKey key(boolean create) throws Exception {
    KeyStore store = KeyStore.getInstance("AndroidKeyStore");
    store.load(null);
    if (!store.containsAlias(ALIAS)) {
      if (!create) throw new IOException("credentials_key_missing");
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
    return (SecretKey) store.getKey(ALIAS, null);
  }

  public void save(byte[] plain) throws Exception {
    synchronized (LOCK) {
      saveLocked(plain);
    }
  }

  private void saveLocked(byte[] plain) throws Exception {
    if (plain.length > 131072) throw new IOException("credentials_size");
    Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
    cipher.init(Cipher.ENCRYPT_MODE, key(true));
    byte[] encrypted = cipher.doFinal(plain);
    FileOutputStream output = null;
    try {
      output = file.startWrite();
      output.write(1);
      output.write(cipher.getIV().length);
      output.write(cipher.getIV());
      output.write(encrypted);
      file.finishWrite(output);
    } catch (Exception error) {
      if (output != null) file.failWrite(output);
      throw error;
    } finally {
      Arrays.fill(encrypted, (byte) 0);
    }
  }

  public byte[] load() throws Exception {
    synchronized (LOCK) {
      return loadLocked();
    }
  }

  private byte[] loadLocked() throws Exception {
    try (InputStream input = file.openRead()) {
      ByteArrayOutputStream bytes = new ByteArrayOutputStream();
      byte[] part = new byte[4096];
      int count;
      while ((count = input.read(part)) != -1) {
        if (bytes.size() + count > 131200) throw new IOException("credentials_size");
        bytes.write(part, 0, count);
      }
      byte[] stored = bytes.toByteArray();
      try {
        if (stored.length < 30 || stored[0] != 1 || stored[1] != 12)
          throw new IOException("credentials_format");
        Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
        cipher.init(Cipher.DECRYPT_MODE, key(false), new GCMParameterSpec(128, stored, 2, 12));
        return cipher.doFinal(stored, 14, stored.length - 14);
      } finally {
        Arrays.fill(stored, (byte) 0);
        Arrays.fill(part, (byte) 0);
      }
    }
  }

  /** Only after a complete v2 catalog was committed, read back and validated. */
  void retire() throws Exception {
    synchronized (LOCK) {
      File base = file.getBaseFile();
      for (String suffix : new String[] {"", ".bak", ".new"}) {
        File old = new File(base.getPath() + suffix);
        if (old.exists() && (!old.isFile() || !old.delete()))
          throw new IOException("legacy_credentials_cleanup");
      }
      KeyStore store = KeyStore.getInstance("AndroidKeyStore");
      store.load(null);
      if (store.containsAlias(ALIAS)) store.deleteEntry(ALIAS);
      if (store.containsAlias(ALIAS)) throw new IOException("legacy_key_cleanup");
    }
  }
}
