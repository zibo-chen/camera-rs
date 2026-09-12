package com.medivh.camera;

import org.json.JSONObject;

/** Structured failure raised by the native camera adapter. */
public final class CameraException extends IllegalStateException {
    private final int schemaVersion;
    private final String code;
    private final String recovery;
    private final String backend;
    private final String stage;
    private final Long nativeCode;
    private final String diagnosticMessage;

    /** Creates an exception from the versioned JSON emitted by the Rust adapter. */
    public CameraException(String payload) {
        super(payload);
        JSONObject value;
        try {
            value = new JSONObject(payload);
        } catch (Exception ignored) {
            value = new JSONObject();
        }
        schemaVersion = value.optInt("schemaVersion", 0);
        code = value.optString("code", "adapter_failure");
        recovery = value.optString("recovery", "none");
        backend = nullableString(value, "backend");
        stage = nullableString(value, "stage");
        nativeCode = value.isNull("nativeCode") ? null : value.optLong("nativeCode");
        diagnosticMessage = value.optString("message", payload);
    }

    private static String nullableString(JSONObject value, String key) {
        return value.isNull(key) ? null : value.optString(key, null);
    }

    public int schemaVersion() { return schemaVersion; }
    public String code() { return code; }
    public String recovery() { return recovery; }
    public String backend() { return backend; }
    public String stage() { return stage; }
    public Long nativeCode() { return nativeCode; }
    public String diagnosticMessage() { return diagnosticMessage; }
    public boolean isRetryable() {
        return "retry".equals(recovery) || "reenumerate_device".equals(recovery);
    }
}
