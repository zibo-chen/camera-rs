//! Android USB permission integration and JNI adapter. Camera2 capture in the
//! core `camera` crate does not require this package or a Java context.
pub mod android;
pub mod error;
mod owned_fd;
use camera::{
    BackendType, Camera, CameraSystem, CaptureSession, ControlId, ControlMode, ControlValue, Frame,
    FrameRate, FrameReceiver, OutputFormat, StreamRequest,
};
use error::{CameraError, Result};
use jni::{
    objects::{JByteBuffer, JClass, JObject, JString},
    sys::{jboolean, jint, jlong, jstring, JNI_FALSE, JNI_TRUE},
    JNIEnv,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    os::fd::AsFd,
    sync::{
        atomic::{AtomicBool, AtomicI64, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::Duration,
};

#[derive(Clone, Debug)]
pub struct UsbDeviceInfo {
    pub index: u32,
    pub name: String,
    pub description: String,
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
    pub serial_number: Option<String>,
    pub device_path: Option<String>,
}
struct Reader {
    session: Arc<CaptureSession>,
    frames: FrameReceiver,
    pending: Option<Frame>,
}
struct Handle {
    closed: AtomicBool,
    lifecycle: Mutex<()>,
    camera: tokio::sync::Mutex<Camera>,
    session: Mutex<Option<Arc<CaptureSession>>>,
    reader: tokio::sync::Mutex<Option<Reader>>,
}
static HANDLES: OnceLock<Mutex<HashMap<i64, Arc<Handle>>>> = OnceLock::new();
static NEXT: AtomicI64 = AtomicI64::new(1);
fn registry() -> &'static Mutex<HashMap<i64, Arc<Handle>>> {
    HANDLES.get_or_init(|| Mutex::new(HashMap::new()))
}
fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("JNI camera runtime")
    })
}
fn invalid(message: impl Into<String>) -> CameraError {
    CameraError::Android(message.into())
}
fn handle(id: i64) -> Result<Arc<Handle>> {
    registry()
        .lock()
        .map_err(|_| invalid("Handle registry poisoned"))?
        .get(&id)
        .cloned()
        .ok_or_else(|| invalid("Unknown camera handle"))
}
fn session(handle: &Handle) -> Result<Arc<CaptureSession>> {
    handle
        .session
        .lock()
        .map_err(|_| invalid("Session state poisoned"))?
        .clone()
        .ok_or_else(|| invalid("Camera is not streaming"))
}
fn store(camera: Camera) -> Result<i64> {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    if id <= 0 {
        return Err(invalid("Handle space exhausted"));
    }
    registry()
        .lock()
        .map_err(|_| invalid("Handle registry poisoned"))?
        .insert(
            id,
            Arc::new(Handle {
                closed: AtomicBool::new(false),
                lifecycle: Mutex::new(()),
                camera: tokio::sync::Mutex::new(camera),
                session: Mutex::new(None),
                reader: tokio::sync::Mutex::new(None),
            }),
        );
    Ok(id)
}
fn backend(name: &str) -> Result<BackendType> {
    Ok(match name.to_ascii_lowercase().as_str() {
        "camera2" => BackendType::Camera2,
        "uvc" => BackendType::Uvc,
        "v4l2" => BackendType::V4l2,
        "auto" => BackendType::Auto,
        _ => return Err(invalid("Backend must be auto, camera2, uvc, or v4l2")),
    })
}
fn boundary<T: Default>(
    env: &mut JNIEnv<'_>,
    operation: impl FnOnce(&mut JNIEnv<'_>) -> Result<T>,
) -> T {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(env))) {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => {
            let _ = env.throw_new("java/lang/IllegalStateException", error.to_string());
            T::default()
        }
        Err(_) => {
            let _ = env.throw_new("java/lang/IllegalStateException", "Camera adapter panicked");
            T::default()
        }
    }
}
fn output(env: &mut JNIEnv<'_>, value: Value) -> Result<jstring> {
    Ok(env.new_string(value.to_string())?.into_raw())
}
fn text(env: &mut JNIEnv<'_>, value: JString<'_>) -> Result<String> {
    Ok(env.get_string(&value)?.into())
}

#[no_mangle]
pub extern "system" fn Java_com_medivh_camera_MedivhCameraBridge_nativeInitWithContext(
    mut env: JNIEnv,
    class: JClass,
    context: JObject,
) {
    let _ = class;
    boundary(&mut env, |env| {
        let vm = env.get_java_vm()?;
        let context = env.new_global_ref(context)?;
        unsafe {
            android::init_android_context(vm, context);
        }
        Ok(())
    });
}
#[no_mangle]
pub extern "system" fn Java_com_medivh_camera_MedivhCameraBridge_nativeDevices(
    mut env: JNIEnv,
    _class: JClass,
    name: JString,
) -> jstring {
    boundary(&mut env, |env| {
        let b = backend(&text(env, name)?)?;
        let devices = if b == BackendType::Uvc {
            unsafe{android::usb_manager_scan()?}.into_iter().map(|d|json!({"id":d.device_path,"index":d.index,"name":d.name,"description":d.description,"vendorId":d.vendor_id,"productId":d.product_id})).collect::<Vec<_>>()
        } else {
            runtime().block_on(CameraSystem::with_backend(b).devices())?.into_iter().map(|d|json!({"id":d.id.native_id(),"index":d.index,"name":d.name,"description":d.description})).collect()
        };
        output(env, json!(devices))
    })
}
#[no_mangle]
pub extern "system" fn Java_com_medivh_camera_MedivhCameraBridge_nativeHasPermission(
    mut env: JNIEnv,
    _class: JClass,
    path: JString,
) -> jboolean {
    boundary(&mut env, |env| {
        Ok(
            if unsafe { android::usb_device_has_permission(&text(env, path)?)? } {
                JNI_TRUE
            } else {
                JNI_FALSE
            },
        )
    })
}
#[no_mangle]
pub extern "system" fn Java_com_medivh_camera_MedivhCameraBridge_nativeRequestPermission(
    mut env: JNIEnv,
    _class: JClass,
    path: JString,
) {
    boundary(&mut env, |env| unsafe {
        android::usb_device_request_permission(&text(env, path)?)
    })
}
#[no_mangle]
pub extern "system" fn Java_com_medivh_camera_MedivhCameraBridge_nativeOpen(
    mut env: JNIEnv,
    _class: JClass,
    name: JString,
    id: JString,
) -> jlong {
    boundary(&mut env, |env| {
        let b = backend(&text(env, name)?)?;
        let id = text(env, id)?;
        let camera = if b == BackendType::Uvc {
            let fd = unsafe { android::usb_device_open(&id)? };
            runtime().block_on(CameraSystem::with_backend(b).open_usb_fd(fd.as_fd()))?
        } else {
            runtime().block_on(async {
                let system = CameraSystem::with_backend(b);
                let device = system
                    .devices()
                    .await?
                    .into_iter()
                    .find(|d| d.id.native_id() == id)
                    .ok_or_else(|| camera::CameraError::DeviceNotFound(id.clone()))?;
                system.open(&device.id).await
            })?
        };
        store(camera)
    })
}
#[no_mangle]
pub extern "system" fn Java_com_medivh_camera_MedivhCameraBridge_nativeOpenUsbFd(
    mut env: JNIEnv,
    _class: JClass,
    fd: jint,
) -> jlong {
    boundary(&mut env, |_| {
        if fd < 0 {
            return Err(invalid("Negative USB descriptor"));
        }
        let owned = owned_fd::duplicate(fd)?;
        store(
            runtime().block_on(
                CameraSystem::with_backend(BackendType::Uvc).open_usb_fd(owned.as_fd()),
            )?,
        )
    })
}
#[no_mangle]
pub extern "system" fn Java_com_medivh_camera_MedivhCameraBridge_nativeStart(
    mut env: JNIEnv,
    _class: JClass,
    id: jlong,
    width: jint,
    height: jint,
    numerator: jint,
    denominator: jint,
    native: jboolean,
) -> jstring {
    boundary(&mut env, |env| {
        if width <= 0 || height <= 0 || numerator <= 0 || denominator <= 0 {
            return Err(invalid("Dimensions and frame rate must be positive"));
        }
        let handle = handle(id)?;
        let request = StreamRequest::builder()
            .resolution(width as u32, height as u32)
            .frame_rate(FrameRate::new(numerator as u32, denominator as u32)?)
            .output(if native == JNI_TRUE {
                OutputFormat::Native
            } else {
                OutputFormat::Rgb8
            })
            .build()?;
        let _gate = handle
            .lifecycle
            .lock()
            .map_err(|_| invalid("Lifecycle state poisoned"))?;
        if handle.closed.load(Ordering::Acquire) {
            return Err(invalid("Camera is closed"));
        }
        let session = runtime().block_on(async {
            let mut camera = handle.camera.lock().await;
            camera.start(request).await
        })?;
        let config = session.negotiated_config();
        let result = json!({"width":config.capture.width,"height":config.capture.height,"frameRateNumerator":config.frame_rate.numerator(),"frameRateDenominator":config.frame_rate.denominator(),"captureFormat":format!("{:?}",config.capture.format),"output":format!("{:?}",config.output),"adjustments":config.adjustments});
        *handle
            .session
            .lock()
            .map_err(|_| invalid("Session state poisoned"))? = Some(Arc::new(session));
        output(env, result)
    })
}
/// The caller exclusively owns the direct buffer for the duration of this call.
/// The adapter copies one frame and returns metadata in JSON. A small buffer
/// leaves the frame pending, so callers may resize and retry without losing it.
#[no_mangle]
pub extern "system" fn Java_com_medivh_camera_MedivhCameraBridge_nativeNextFrame(
    mut env: JNIEnv,
    _class: JClass,
    id: jlong,
    buffer: JByteBuffer,
    timeout_ms: jint,
) -> jstring {
    boundary(&mut env, |env| {
        if !(0..=60000).contains(&timeout_ms) {
            return Err(invalid("Frame timeout must be 0..60000 ms"));
        }
        if env.call_method(&buffer, "isReadOnly", "()Z", &[])?.z()? {
            return Err(invalid("Frame buffer is read-only"));
        }
        let capacity = env.get_direct_buffer_capacity(&buffer)?;
        let pointer = env.get_direct_buffer_address(&buffer)?;
        let handle = handle(id)?;
        let current = session(&handle)?;
        let frame = runtime().block_on(async {
            let mut reader = handle.reader.lock().await;
            if reader
                .as_ref()
                .is_none_or(|r| !Arc::ptr_eq(&r.session, &current))
            {
                *reader = Some(Reader {
                    frames: current.subscribe(),
                    session: current,
                    pending: None,
                });
            }
            let reader = reader.as_mut().unwrap();
            if reader.pending.is_none() {
                reader.pending = Some(
                    reader
                        .frames
                        .next_timeout(Duration::from_millis(timeout_ms as u64))
                        .await?,
                );
            }
            let frame = reader.pending.as_ref().unwrap();
            if capacity < frame.bytes().len() {
                return Err(invalid(format!(
                    "Direct buffer needs {} bytes",
                    frame.bytes().len()
                )));
            }
            Ok(reader.pending.take().unwrap())
        })?;
        unsafe {
            std::ptr::copy_nonoverlapping(frame.bytes().as_ptr(), pointer, frame.bytes().len());
        }
        let layout = frame.layout();
        output(
            env,
            json!({"session":frame.key.session,"sequence":frame.key.sequence,"byteLength":frame.bytes().len(),"width":layout.width,"height":layout.height,"pixelFormat":format!("{:?}",layout.format),"planes":layout.planes.iter().map(|p|json!({"offset":p.offset,"length":p.length,"rowStride":p.row_stride,"pixelStride":p.pixel_stride})).collect::<Vec<_>>(),"colorMatrix":format!("{:?}",layout.color.matrix),"colorRange":format!("{:?}",layout.color.range),"sourceTimestampNs":frame.source_timestamp_ns,"clock":frame.source_timestamp().map(|t|format!("{:?}",t.clock)),"rotation":layout.orientation.rotation_degrees,"mirrored":layout.orientation.mirrored,"bottomUp":layout.bottom_up}),
        )
    })
}
#[no_mangle]
pub extern "system" fn Java_com_medivh_camera_MedivhCameraBridge_nativeMetrics(
    mut env: JNIEnv,
    _class: JClass,
    id: jlong,
) -> jstring {
    boundary(&mut env, |env| {
        let handle = handle(id)?;
        let session = session(&handle)?;
        let m = session.metrics();
        let dropped = runtime().block_on(async {
            handle
                .reader
                .lock()
                .await
                .as_ref()
                .map(|r| r.frames.dropped_frames())
                .unwrap_or(0)
        });
        output(
            env,
            json!({"received":m.received,"published":m.published,
            "poolDrops":m.pool_drops,"conversionErrors":m.conversion_errors,
            "allocatedBuffers":m.allocated_buffers,"allocatedBytes":m.allocated_bytes,
            "retainedBuffers":m.retained_buffers,"conversionTotalNs":m.conversion_total_ns,
            "conversionP50Ns":m.conversion_p50_ns,"conversionP95Ns":m.conversion_p95_ns,
            "subscriberDrops":dropped,
            "firstFrameLatencyMs":session.negotiated_config().first_frame_latency.as_secs_f64()*1000.0}),
        )
    })
}

#[no_mangle]
pub extern "system" fn Java_com_medivh_camera_MedivhCameraBridge_nativeStop(
    mut env: JNIEnv,
    _class: JClass,
    id: jlong,
) {
    boundary(&mut env, |_| {
        let handle = handle(id)?;
        let _gate = handle
            .lifecycle
            .lock()
            .map_err(|_| invalid("Lifecycle state poisoned"))?;
        let session = handle
            .session
            .lock()
            .map_err(|_| invalid("Session state poisoned"))?
            .take();
        if let Some(session) = session {
            runtime().block_on(session.stop())?;
        }
        Ok(())
    })
}
#[no_mangle]
pub extern "system" fn Java_com_medivh_camera_MedivhCameraBridge_nativeClose(
    mut env: JNIEnv,
    _class: JClass,
    id: jlong,
) {
    boundary(&mut env, |_| {
        let handle = registry()
            .lock()
            .map_err(|_| invalid("Handle registry poisoned"))?
            .remove(&id);
        if let Some(handle) = handle {
            handle.closed.store(true, Ordering::Release);
            let _gate = handle
                .lifecycle
                .lock()
                .map_err(|_| invalid("Lifecycle state poisoned"))?;
            let session = handle
                .session
                .lock()
                .map_err(|_| invalid("Session state poisoned"))?
                .take();
            if let Some(session) = session {
                runtime().block_on(session.stop())?;
            }
        }
        Ok(())
    })
}
#[no_mangle]
pub extern "system" fn Java_com_medivh_camera_MedivhCameraBridge_nativeControls(
    mut env: JNIEnv,
    _class: JClass,
    id: jlong,
) -> jstring {
    boundary(&mut env, |env| {
        let session = session(&*handle(id)?)?;
        let controls = runtime().block_on(session.controls())?;
        output(env,json!(controls.into_iter().map(|c|json!({"id":format!("{:?}",c.id),"unit":format!("{:?}",c.unit),"readable":c.readable,"writable":c.writable,"modes":c.modes.iter().map(|m|format!("{:?}",m)).collect::<Vec<_>>(),"range":c.range.map(|r|json!({"min":r.min,"max":r.max,"step":r.step})),"requiresManual":c.requires_manual.map(|m|format!("{:?}",m))})).collect::<Vec<_>>()))
    })
}
#[no_mangle]
pub extern "system" fn Java_com_medivh_camera_MedivhCameraBridge_nativeControl(
    mut env: JNIEnv,
    _class: JClass,
    id: jlong,
    name: JString,
) -> jstring {
    boundary(&mut env, |env| {
        let control = control_id(&text(env, name)?)?;
        let value = runtime().block_on(session(&*handle(id)?)?.control(control))?;
        let (kind, value) = match value {
            camera::ControlReadback::Actual(value) => ("Actual", Some(value)),
            camera::ControlReadback::Requested(value) => ("Requested", Some(value)),
            camera::ControlReadback::Unknown => ("Unknown", None),
        };
        let value = value.map(|v| match v {
            ControlValue::Number(v) => json!(v),
            ControlValue::Mode(v) => json!(format!("{v:?}")),
        });
        output(env, json!({"kind":kind,"value":value}))
    })
}
fn control_id(name: &str) -> Result<ControlId> {
    use ControlId::*;
    [
        ExposureMode,
        ExposureTime,
        ExposureCompensation,
        FocusMode,
        FocusPosition,
        WhiteBalanceMode,
        WhiteBalanceTemperature,
        Zoom,
        Brightness,
        Contrast,
        Hue,
        Saturation,
        Sharpness,
        Gamma,
        BacklightCompensation,
        Gain,
        Pan,
        Tilt,
        Iris,
    ]
    .into_iter()
    .find(|v| format!("{v:?}") == name)
    .ok_or_else(|| invalid("Unknown control ID"))
}
#[no_mangle]
pub extern "system" fn Java_com_medivh_camera_MedivhCameraBridge_nativeSetControl(
    mut env: JNIEnv,
    _class: JClass,
    id: jlong,
    name: JString,
    value: JString,
) {
    boundary(&mut env, |env| {
        let control = control_id(&text(env, name)?)?;
        let value: Value =
            serde_json::from_str(&text(env, value)?).map_err(|e| invalid(e.to_string()))?;
        let value = if let Some(n) = value.as_f64() {
            ControlValue::Number(n)
        } else {
            let mode = match value.as_str() {
                Some("Manual") => ControlMode::Manual,
                Some("Automatic") => ControlMode::Automatic,
                Some("Locked") => ControlMode::Locked,
                Some("Single") => ControlMode::Single,
                Some("Continuous") => ControlMode::Continuous,
                _ => return Err(invalid("Control expects a JSON number or mode string")),
            };
            ControlValue::Mode(mode)
        };
        runtime().block_on(session(&*handle(id)?)?.set_control(control, value))?;
        Ok(())
    })
}
