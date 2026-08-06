#include <android/log.h>
#include <jni.h>

#define LOG_TAG "JellyrinBenignProbe"
#define PROBE_VALUE "JELLYRIN_BENIGN_PROBE_OK"

JNIEXPORT jstring JNICALL
Java_lab_jellyrin_benignprobe_MainActivity_nativeProbeValue(
        JNIEnv *env,
        jclass activity_class) {
    (void) activity_class;
    __android_log_print(ANDROID_LOG_INFO, LOG_TAG, "%s", PROBE_VALUE);
    return (*env)->NewStringUTF(env, PROBE_VALUE);
}
