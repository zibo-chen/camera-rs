# Add project specific ProGuard rules here.
# You can control the set of applied configuration files using the
# proguardFiles setting in build.gradle.

# Keep JNI methods
-keepclasseswithmembernames class * {
    native <methods>;
}

# Keep medivh camera classes
-keep class com.medivh.camera.** { *; }
