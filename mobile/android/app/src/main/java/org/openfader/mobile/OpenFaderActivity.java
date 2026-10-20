package org.openfader.mobile;

import android.app.NativeActivity;
import android.content.BroadcastReceiver;
import android.content.ClipData;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.database.Cursor;
import android.media.AudioAttributes;
import android.media.AudioFocusRequest;
import android.media.AudioManager;
import android.net.Uri;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.provider.OpenableColumns;
import android.view.WindowManager;
import android.view.View;
import android.view.WindowInsets;
import android.view.WindowInsetsController;
import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.util.ArrayList;
import java.util.Locale;
import java.util.UUID;

/** Native UI/engine in Rust. Java handles Android-owned documents and audio lifecycle. */
public final class OpenFaderActivity extends NativeActivity {
    static { System.loadLibrary("openfader_mobile"); }
    private static final int PICK_MUSIC = 1;
    private final Handler main = new Handler(Looper.getMainLooper());
    private AudioManager audio;
    private AudioFocusRequest focus;
    private boolean ownsFocus;
    private volatile boolean picking;
    private volatile boolean resumed;
    private volatile boolean destroyed;
    private boolean receiverRegistered;
    private native void nativeImported(String error);
    private native void nativeImportFinished();
    private native void nativeInterrupted();

    private final BroadcastReceiver noisy = new BroadcastReceiver() {
        @Override public void onReceive(Context context, Intent intent) {
            if (AudioManager.ACTION_AUDIO_BECOMING_NOISY.equals(intent.getAction())) {
                nativeInterrupted();
                playback(false);
            }
        }
    };

    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        hideSystemBars();
        audio = (AudioManager) getSystemService(AUDIO_SERVICE);
        focus = new AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
            .setAudioAttributes(new AudioAttributes.Builder()
                .setUsage(AudioAttributes.USAGE_MEDIA).setContentType(AudioAttributes.CONTENT_TYPE_MUSIC).build())
            .setWillPauseWhenDucked(true)
            .setOnAudioFocusChangeListener(change -> {
                if (change != AudioManager.AUDIOFOCUS_GAIN) {
                    nativeInterrupted();
                    playback(false);
                }
            }, main).build();
        if (android.os.Build.VERSION.SDK_INT >= 33) {
            registerReceiver(noisy, new IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY), Context.RECEIVER_NOT_EXPORTED);
        } else {
            registerReceiver(noisy, new IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY));
        }
        receiverRegistered = true;
    }

    /** Queue UI work; NativeActivity's main thread can be waiting for the Slint thread. */
    public synchronized boolean pickMusic() {
        if (destroyed || !resumed || picking) return false;
        picking = true;
        if (!main.post(() -> {
            if (destroyed || !resumed) {
                picking = false;
                nativeImportFinished();
                return;
            }
            Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
            intent.addCategory(Intent.CATEGORY_OPENABLE);
            intent.setType("audio/*");
            intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true);
            try {
                startActivityForResult(intent, PICK_MUSIC);
            } catch (RuntimeException e) {
                picking = false;
                nativeImported("Cannot open music picker: " + e.getMessage());
                nativeImportFinished();
            }
        })) {
            picking = false;
            return false;
        }
        return true;
    }

    @Override protected void onActivityResult(int request, int result, Intent data) {
        super.onActivityResult(request, result, data);
        if (request != PICK_MUSIC) return;
        if (result != RESULT_OK || data == null) {
            picking = false;
            nativeImportFinished();
            return;
        }
        ArrayList<Uri> uris = new ArrayList<>();
        ClipData clip = data.getClipData();
        if (clip != null) {
            for (int i = 0; i < clip.getItemCount(); ++i) uris.add(clip.getItemAt(i).getUri());
        } else if (data.getData() != null) uris.add(data.getData());
        new Thread(() -> {
            try {
                for (Uri uri : uris) {
                    if (destroyed) break;
                    try { copyMusic(uri); nativeImported(null); }
                    catch (Exception e) { nativeImported("Import failed: " + e.getMessage() + ". Choose another audio file or free storage."); }
                }
            } finally {
                main.post(() -> { picking = false; if (!destroyed) nativeImportFinished(); });
            }
        }, "music-import").start();
    }

    private void copyMusic(Uri uri) throws IOException {
        String name = null;
        try (Cursor c = getContentResolver().query(uri, new String[]{OpenableColumns.DISPLAY_NAME}, null, null, null)) {
            if (c != null && c.moveToFirst()) name = c.getString(0);
        }
        if (name == null) name = "audio";
        name = name.replaceAll("[\\\\/\\p{Cntrl}]", "_");
        if (name.length() > 180) name = name.substring(name.length() - 180);
        String ext = name.lastIndexOf('.') < 0 ? "" : name.substring(name.lastIndexOf('.') + 1).toLowerCase(Locale.ROOT);
        if (!java.util.Arrays.asList("mp3", "m4a", "aac", "flac", "wav", "aif", "aiff").contains(ext)) {
            String mime = getContentResolver().getType(uri);
            if ("audio/mpeg".equals(mime)) ext = "mp3";
            else if ("audio/mp4".equals(mime)) ext = "m4a";
            else if ("audio/aac".equals(mime)) ext = "aac";
            else if ("audio/flac".equals(mime) || "audio/x-flac".equals(mime)) ext = "flac";
            else if ("audio/wav".equals(mime) || "audio/x-wav".equals(mime)) ext = "wav";
            else if ("audio/aiff".equals(mime) || "audio/x-aiff".equals(mime)) ext = "aiff";
            else throw new IOException("Unsupported audio format: " + name);
            name += "." + ext;
        }
        File dir = new File(new File(getFilesDir(), "Music"), UUID.randomUUID().toString());
        if (!dir.mkdirs()) throw new IOException("Cannot create music directory");
        File temp = new File(dir, ".import.part");
        File target = new File(dir, name);
        boolean complete = false;
        try {
            try (InputStream input = getContentResolver().openInputStream(uri);
                 FileOutputStream output = new FileOutputStream(temp)) {
                if (input == null) throw new IOException("Document provider returned no data");
                byte[] buffer = new byte[64 * 1024];
                int count;
                while ((count = input.read(buffer)) != -1) {
                    if (destroyed) throw new IOException("Import cancelled");
                    output.write(buffer, 0, count);
                }
                output.getFD().sync();
            }
            if (temp.length() == 0) throw new IOException("Audio file is empty");
            if (!temp.renameTo(target)) throw new IOException("Cannot save imported music");
            complete = true;
        } finally {
            if (!complete) { temp.delete(); dir.delete(); }
        }
    }

    /** Audio focus is thread-safe; never wait for the Java UI thread from android_main. */
    public synchronized boolean playback(boolean playing) {
        if (playing) {
            if (destroyed || !resumed || focus == null) return false;
            if (!ownsFocus) ownsFocus = audio.requestAudioFocus(focus) == AudioManager.AUDIOFOCUS_REQUEST_GRANTED;
            if (!ownsFocus) return false;
        } else if (ownsFocus) {
            ownsFocus = false;
            audio.abandonAudioFocusRequest(focus);
        }
        main.post(() -> {
            synchronized (OpenFaderActivity.this) {
                if (ownsFocus && resumed && !destroyed) {
                    getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
                } else {
                    getWindow().clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
                }
            }
        });
        return true;
    }

    public boolean background() {
        if (destroyed) return false;
        return main.post(() -> { if (!destroyed) moveTaskToBack(true); });
    }

    private void hideSystemBars() {
        if (android.os.Build.VERSION.SDK_INT >= 30) {
            getWindow().setDecorFitsSystemWindows(false);
            WindowInsetsController controller = getWindow().getInsetsController();
            if (controller != null) {
                controller.setSystemBarsBehavior(WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE);
                controller.hide(WindowInsets.Type.systemBars());
            }
        } else {
            getWindow().getDecorView().setSystemUiVisibility(
                View.SYSTEM_UI_FLAG_LAYOUT_STABLE | View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION
                | View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN | View.SYSTEM_UI_FLAG_HIDE_NAVIGATION
                | View.SYSTEM_UI_FLAG_FULLSCREEN | View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY);
        }
    }

    @Override public void onWindowFocusChanged(boolean hasFocus) {
        super.onWindowFocusChanged(hasFocus);
        if (hasFocus) hideSystemBars();
    }

    @Override protected void onResume() {
        super.onResume();
        resumed = true;
        hideSystemBars();
    }

    @Override protected void onPause() {
        resumed = false;
        nativeInterrupted();
        playback(false);
        super.onPause();
    }
    @Override protected void onDestroy() {
        destroyed = true;
        resumed = false;
        playback(false);
        if (receiverRegistered) unregisterReceiver(noisy);
        super.onDestroy();
    }
}
