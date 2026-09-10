package com.medivh.camera.demo;
import android.Manifest;
import android.app.Activity;
import android.os.Bundle;
import android.content.pm.PackageManager;
import android.graphics.Bitmap;
import android.widget.*;
import com.medivh.camera.MedivhCamera;
import org.json.*;
import java.nio.ByteBuffer;
import java.util.concurrent.Executors;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.atomic.AtomicBoolean;

/** Minimal session/receiver example. Capture and shutdown never block the UI thread. */
public final class MainActivity extends Activity {
    private final ExecutorService worker = Executors.newSingleThreadExecutor();
    private final AtomicBoolean running = new AtomicBoolean(false);
    private final AtomicBoolean previewPending = new AtomicBoolean(false);
    private Spinner backend, devices;
    private ImageView preview;
    private TextView status;
    private JSONArray inventory = new JSONArray();
    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        LinearLayout layout = new LinearLayout(this); layout.setOrientation(LinearLayout.VERTICAL); layout.setPadding(24,24,24,24);
        backend = new Spinner(this); backend.setAdapter(new ArrayAdapter<>(this,android.R.layout.simple_spinner_dropdown_item,new String[]{"camera2","uvc"}));layout.addView(backend);
        devices = new Spinner(this); layout.addView(devices);
        Button scan = new Button(this); scan.setText("Refresh cameras"); scan.setOnClickListener(v -> scan()); layout.addView(scan);
        Button start = new Button(this); start.setText("Start"); start.setOnClickListener(v -> start());layout.addView(start);
        Button stop = new Button(this);stop.setText("Stop");stop.setOnClickListener(v -> running.set(false));layout.addView(stop);
        status = new TextView(this);layout.addView(status);preview=new ImageView(this);preview.setAdjustViewBounds(true);layout.addView(preview);setContentView(layout);
        if (checkSelfPermission(Manifest.permission.CAMERA)!=PackageManager.PERMISSION_GRANTED) requestPermissions(new String[]{Manifest.permission.CAMERA},1);else scan();
    }
    private void show(String message){runOnUiThread(() -> status.setText(message));}
    private void scan(){String selected=backend.getSelectedItem().toString();worker.execute(() -> {try {JSONArray found=MedivhCamera.devices(selected);String[] names=new String[found.length()];for(int i=0;i<names.length;i++)names[i]=found.getJSONObject(i).getString("name");runOnUiThread(() -> {inventory=found;devices.setAdapter(new ArrayAdapter<>(this,android.R.layout.simple_spinner_dropdown_item,names));});show("Found "+names.length+" cameras");}catch(Exception e){show(e.getMessage());}});}
    private void start(){if(!running.compareAndSet(false,true))return;
        final String selected=backend.getSelectedItem().toString();final String id;
        try{id=inventory.getJSONObject(devices.getSelectedItemPosition()).getString("id");}catch(Exception e){running.set(false);show("Refresh and select a camera");return;}
        worker.execute(() -> {try {
            if(selected.equals("uvc")&&!MedivhCamera.hasUsbPermission(id)){MedivhCamera.requestUsbPermission(id);show("Grant USB permission, then press Start");return;}
            try(MedivhCamera camera=new MedivhCamera(selected,id)){
                JSONObject config=camera.start(640,480,30);int w=config.getInt("width"),h=config.getInt("height");ByteBuffer buffer=ByteBuffer.allocateDirect(w*h*3);int[] pixels=new int[w*h];show(config.toString());
                while(running.get()){
                    camera.nextFrame(buffer,1000);if(!running.get())break;
                    if(!previewPending.compareAndSet(false,true))continue;
                    for(int i=0;i<pixels.length;i++)pixels[i]=0xff000000|((buffer.get(i*3)&255)<<16)|((buffer.get(i*3+1)&255)<<8)|(buffer.get(i*3+2)&255);
                    Bitmap bitmap=Bitmap.createBitmap(pixels,w,h,Bitmap.Config.ARGB_8888);runOnUiThread(() -> {if(running.get())preview.setImageBitmap(bitmap);previewPending.set(false);});
                }
                camera.stop();show("Stopped");
            }
        }catch(Exception e){show(e.getMessage());}finally{running.set(false);}});
    }
    @Override public void onRequestPermissionsResult(int request,String[] permissions,int[] results){super.onRequestPermissionsResult(request,permissions,results);if(results.length>0&&results[0]==PackageManager.PERMISSION_GRANTED)scan();}
    @Override protected void onStop(){running.set(false);super.onStop();}
    @Override protected void onDestroy(){running.set(false);worker.shutdown();super.onDestroy();}
}
