package lab.jellyrin.benignprobe;

import android.app.Activity;
import android.os.Bundle;
import android.util.Log;
import android.view.Gravity;
import android.widget.TextView;

public final class MainActivity extends Activity {
    private static final String LOG_TAG = "JellyrinBenignProbe";

    static {
        System.loadLibrary("benign_probe");
    }

    private static native String nativeProbeValue();

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        String probeValue = nativeProbeValue();
        Log.i(LOG_TAG, probeValue);

        TextView resultView = new TextView(this);
        resultView.setGravity(Gravity.CENTER);
        resultView.setText(probeValue);
        resultView.setTextSize(24.0f);
        setContentView(resultView);
    }
}
