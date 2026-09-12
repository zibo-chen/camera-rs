//! Android USB permission integration and JNI adapter. Camera2 capture in the
//! core `camera` crate does not require this package or a Java context.
#![warn(missing_docs)]
#![warn(unsafe_op_in_unsafe_fn)]
pub mod android;
/// Stable Android adapter error types and JNI boundary payloads.
pub mod error;
mod owned_fd;
use camera::{
    BackendId, BackendPolicy, CameraSystem, CaptureRequest, ControlId, ControlMode, ControlValue,
    Device, DeviceSelector, Frame, FrameRate, FrameReceiver, Session, SubscriptionOptions,
};
#[cfg(feature = "convert-rgb")]
use camera::{ConversionRequest, PixelFormat, RgbConverter};
use error::{CameraError, Result};
use jni::{
    objects::{JByteBuffer, JClass, JObject, JString},
    sys::{jboolean, jint, jlong, jstring, JNI_FALSE, JNI_TRUE},
    JNIEnv,
};
use serde_json::{json, Value};
#[cfg(feature = "uvc")]
use std::os::fd::AsFd;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicI64, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::Duration,
};

#[derive(Clone, Debug)]
/// Description of a USB video device discovered through Android's `UsbManager`.
pub struct UsbDeviceInfo {
    /// Stable index within the current scan result.
    pub index: u32,
    /// User-facing device name.
    pub name: String,
    /// Additional device description supplied by Android.
    pub description: String,
    /// USB vendor identifier, when Android exposes it.
    pub vendor_id: Option<u16>,
    /// USB product identifier, when Android exposes it.
    pub product_id: Option<u16>,
    /// USB serial number, when the application has permission to read it.
    pub serial_number: Option<String>,
    /// Android USB device path used by the permission and open calls.
    pub device_path: Option<String>,
}
struct Reader {
    session: Arc<Session>,
    frames: FrameReceiver,
    pending: Option<Frame>,
    #[cfg(feature = "convert-rgb")]
    converter: RgbConverter,
    #[cfg(feature = "convert-rgb")]
    rgb: Vec<u8>,
}
struct Handle {
    closed: AtomicBool,
    lifecycle: Mutex<()>,
    camera: tokio::sync::Mutex<Device>,
    session: Mutex<Option<Arc<Session>>>,
    reader: tokio::sync::Mutex<Option<Reader>>,
    native_output: AtomicBool,
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
    CameraError::adapter(camera::CameraErrorKind::InvalidState, message)
}
fn handle(id: i64) -> Result<Arc<Handle>> {
    registry()
        .lock()
        .map_err(|_| invalid("Handle registry poisoned"))?
        .get(&id)
        .cloned()
        .ok_or_else(|| invalid("Unknown camera handle"))
}
fn session(handle: &Handle) -> Result<Arc<Session>> {
    handle
        .session
        .lock()
        .map_err(|_| invalid("Session state poisoned"))?
        .clone()
        .ok_or_else(|| invalid("Camera is not streaming"))
}
fn store(camera: Device) -> Result<i64> {
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
                native_output: AtomicBool::new(true),
            }),
        );
    Ok(id)
}
fn backend(name: &str) -> Result<Option<BackendId>> {
    Ok(match name.to_ascii_lowercase().as_str() {
        "camera2" => Some(BackendId::CAMERA2),
        "uvc" => Some(BackendId::UVC),
        "v4l2" => Some(BackendId::V4L2),
        "auto" => None,
        _ => return Err(invalid("Backend must be auto, camera2, uvc, or v4l2")),
    })
}

fn capture_format(name: &str) -> Result<camera::CaptureFormat> {
    use camera::CaptureFormat;
    match name.trim().to_ascii_lowercase().as_str() {
        "mjpeg" | "mjpg" => Ok(CaptureFormat::Mjpeg),
        "yuyv" | "yuy2" => Ok(CaptureFormat::Yuyv),
        "uyvy" => Ok(CaptureFormat::Uyvy),
        "nv12" => Ok(CaptureFormat::Nv12),
        "rgb" | "rgb8" => Ok(CaptureFormat::Rgb8),
        "h264" => Ok(CaptureFormat::H264),
        "gray" | "gray8" => Ok(CaptureFormat::Gray8),
        _ => Err(invalid("Unknown capture format")),
    }
}

fn frame_interval_payload(interval: camera::FrameInterval) -> Value {
    json!({"numerator":interval.numerator,"denominator":interval.denominator})
}

fn capability_payload(capabilities: camera::DeviceCapabilities) -> Value {
    let (knowledge, reason, modes) = match capabilities.capture_modes {
        camera::CapabilityKnowledge::Known(modes) => ("known", None, modes),
        camera::CapabilityKnowledge::Representative { values, reason } => {
            ("representative", Some(reason), values)
        }
        camera::CapabilityKnowledge::Unknown { reason } => ("unknown", Some(reason), Vec::new()),
    };
    let modes = modes
        .into_iter()
        .map(|mode| {
            json!({
                "format":format!("{:?}",mode.format),
                "width":mode.width,
                "height":mode.height,
                "frameRateNumerator":mode.frame_rate.numerator(),
                "frameRateDenominator":mode.frame_rate.denominator(),
                "framesPerSecond":mode.frame_rate.as_f64()
            })
        })
        .collect::<Vec<_>>();
    let ranges = capabilities
        .capture_mode_ranges
        .into_iter()
        .map(|range| {
            json!({
                "format":format!("{:?}",range.format),
                "kind":format!("{:?}",range.kind),
                "width":{"min":range.width.minimum,"max":range.width.maximum,"step":range.width.step},
                "height":{"min":range.height.minimum,"max":range.height.maximum,"step":range.height.step},
                "frameIntervals":range.frame_intervals.into_iter().map(|interval|json!({
                    "kind":format!("{:?}",interval.kind),
                    "minimum":frame_interval_payload(interval.minimum),
                    "maximum":frame_interval_payload(interval.maximum),
                    "step":interval.step.map(frame_interval_payload),
                    "atResolution":{"width":interval.at_resolution.0,"height":interval.at_resolution.1}
                })).collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    let conversions = capabilities
        .conversions
        .into_iter()
        .map(|conversion| {
            json!({
                "input":format!("{:?}",conversion.input),
                "output":format!("{:?}",conversion.output),
                "available":conversion.available,
                "requirement":conversion.requirement
            })
        })
        .collect::<Vec<_>>();
    json!({
        "knowledge":knowledge,
        "reason":reason,
        "modes":modes,
        "ranges":ranges,
        "nativeFormats":capabilities.native_formats.into_iter().map(|format|format!("{format:?}")).collect::<Vec<_>>(),
        "conversions":conversions,
        "limitations":capabilities.limitations
    })
}

fn system_for(backend: Option<BackendId>) -> Result<CameraSystem> {
    let policy = backend
        .map(BackendPolicy::Require)
        .unwrap_or(BackendPolicy::PlatformDefault);
    Ok(CameraSystem::builder().backend_policy(policy).build()?)
}

#[cfg(feature = "uvc")]
fn usb_devices() -> Result<Vec<Value>> {
    Ok(android::usb_manager_scan()?
        .into_iter()
        .map(|d| json!({"id":d.device_path,"index":d.index,"name":d.name,"description":d.description,"vendorId":d.vendor_id,"productId":d.product_id}))
        .collect())
}
#[cfg(not(feature = "uvc"))]
fn usb_devices() -> Result<Vec<Value>> {
    Err(camera::CameraError::backend_not_compiled(BackendId::UVC).into())
}

#[cfg(feature = "uvc")]
fn open_usb_path(path: &str) -> Result<Device> {
    let fd = android::usb_device_open(path)?;
    Ok(runtime().block_on(system_for(Some(BackendId::UVC))?.open_usb_fd(fd.as_fd()))?)
}
#[cfg(not(feature = "uvc"))]
fn open_usb_path(_path: &str) -> Result<Device> {
    Err(camera::CameraError::backend_not_compiled(BackendId::UVC).into())
}

#[cfg(feature = "uvc")]
fn open_usb_descriptor(fd: jint) -> Result<Device> {
    let owned = owned_fd::duplicate(fd)?;
    Ok(runtime().block_on(system_for(Some(BackendId::UVC))?.open_usb_fd(owned.as_fd()))?)
}
#[cfg(not(feature = "uvc"))]
fn open_usb_descriptor(_fd: jint) -> Result<Device> {
    Err(camera::CameraError::backend_not_compiled(BackendId::UVC).into())
}
fn boundary<T: Default>(
    env: &mut JNIEnv<'_>,
    operation: impl FnOnce(&mut JNIEnv<'_>) -> Result<T>,
) -> T {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(env))) {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => {
            let _ = env.throw_new(
                "com/medivh/camera/CameraException",
                error.boundary_payload(),
            );
            T::default()
        }
        Err(_) => {
            let payload =
                CameraError::adapter(camera::CameraErrorKind::Internal, "Camera adapter panicked")
                    .boundary_payload();
            let _ = env.throw_new("com/medivh/camera/CameraException", payload);
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
/// JNI entry point that stores the application context used for USB access.
pub extern "system" fn Java_com_medivh_camera_MedivhCameraBridge_nativeInitWithContext(
    mut env: JNIEnv,
    class: JClass,
    context: JObject,
) {
    let _ = class;
    boundary(&mut env, |env| {
        let vm = env.get_java_vm()?;
        let context = env.new_global_ref(context)?;
        android::init_android_context(vm, context);
        Ok(())
    });
}
#[no_mangle]
/// JNI entry point that enumerates devices for the requested backend.
pub extern "system" fn Java_com_medivh_camera_MedivhCameraBridge_nativeDevices(
    mut env: JNIEnv,
    _class: JClass,
    name: JString,
) -> jstring {
    boundary(&mut env, |env| {
        let selected = backend(&text(env, name)?)?;
        let devices = if selected.as_ref() == Some(&BackendId::UVC) {
            usb_devices()?
        } else {
            runtime().block_on(system_for(selected)?.devices())?.into_iter().map(|d|json!({"id":d.id.native_id(),"persistentId":d.id.to_persistent_string(),"index":d.index,"name":d.name,"description":d.description,"facing":format!("{:?}",d.facing)})).collect()
        };
        output(env, json!(devices))
    })
}

#[no_mangle]
/// JNI entry point that returns the device capability matrix as JSON.
pub extern "system" fn Java_com_medivh_camera_MedivhCameraBridge_nativeCapabilities(
    mut env: JNIEnv,
    _class: JClass,
    name: JString,
    id: JString,
) -> jstring {
    boundary(&mut env, |env| {
        let selected = backend(&text(env, name)?)?;
        let native_id = text(env, id)?;
        let capabilities = if selected.as_ref() == Some(&BackendId::UVC) {
            let camera = open_usb_path(&native_id)?;
            runtime().block_on(camera.capabilities())?
        } else {
            let system = system_for(selected)?;
            let device = runtime()
                .block_on(system.devices())?
                .into_iter()
                .find(|device| device.id.native_id() == native_id)
                .ok_or_else(|| camera::CameraError::device_not_found(native_id.clone()))?;
            runtime().block_on(system.capabilities(&device.id))?
        };
        output(env, capability_payload(capabilities))
    })
}
#[no_mangle]
/// JNI entry point that reports whether a USB device path is authorized.
pub extern "system" fn Java_com_medivh_camera_MedivhCameraBridge_nativeHasPermission(
    mut env: JNIEnv,
    _class: JClass,
    path: JString,
) -> jboolean {
    boundary(&mut env, |env| {
        Ok(if android::usb_device_has_permission(&text(env, path)?)? {
            JNI_TRUE
        } else {
            JNI_FALSE
        })
    })
}
#[no_mangle]
/// JNI entry point that starts Android's USB permission flow.
pub extern "system" fn Java_com_medivh_camera_MedivhCameraBridge_nativeRequestPermission(
    mut env: JNIEnv,
    _class: JClass,
    path: JString,
) {
    boundary(&mut env, |env| {
        android::usb_device_request_permission(&text(env, path)?)
    })
}
#[no_mangle]
/// JNI entry point that opens a camera by backend name and native device ID.
pub extern "system" fn Java_com_medivh_camera_MedivhCameraBridge_nativeOpen(
    mut env: JNIEnv,
    _class: JClass,
    name: JString,
    id: JString,
) -> jlong {
    boundary(&mut env, |env| {
        let selected = backend(&text(env, name)?)?;
        let id = text(env, id)?;
        let camera = if selected.as_ref() == Some(&BackendId::UVC) {
            open_usb_path(&id)?
        } else {
            let system = system_for(selected)?;
            runtime().block_on(async {
                let device = system
                    .devices()
                    .await?
                    .into_iter()
                    .find(|d| d.id.native_id() == id)
                    .ok_or_else(|| camera::CameraError::device_not_found(id.clone()))?;
                system.open(DeviceSelector::Id(device.id)).await
            })?
        };
        store(camera)
    })
}
#[no_mangle]
/// JNI entry point that opens UVC capture from an authorized USB descriptor.
pub extern "system" fn Java_com_medivh_camera_MedivhCameraBridge_nativeOpenUsbFd(
    mut env: JNIEnv,
    _class: JClass,
    fd: jint,
) -> jlong {
    boundary(&mut env, |_| {
        if fd < 0 {
            return Err(invalid("Negative USB descriptor"));
        }
        store(open_usb_descriptor(fd)?)
    })
}

fn start_handle(
    id: jlong,
    width: jint,
    height: jint,
    numerator: jint,
    denominator: jint,
    format: Option<camera::CaptureFormat>,
    native_output: bool,
) -> Result<Value> {
    if width <= 0 || height <= 0 || numerator <= 0 || denominator <= 0 {
        return Err(invalid("Dimensions and frame rate must be positive"));
    }
    #[cfg(not(feature = "convert-rgb"))]
    if !native_output {
        return Err(invalid(
            "RGB delivery was requested but camera-android/convert-rgb is not enabled",
        ));
    }
    let handle = handle(id)?;
    let mut builder = CaptureRequest::builder()
        .preferred_frame_rate(FrameRate::new(numerator as u32, denominator as u32)?);
    if let Some(format) = format {
        builder = builder
            .exact_resolution(width as u32, height as u32)
            .preferred_formats([format]);
    } else {
        builder = builder.preferred_resolution(width as u32, height as u32);
    }
    let request = builder.build()?;
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
    handle.native_output.store(native_output, Ordering::Release);
    let config = session.negotiated();
    let result = json!({"width":config.capture.width,"height":config.capture.height,"frameRateNumerator":config.capture.frame_rate.numerator(),"frameRateDenominator":config.capture.frame_rate.denominator(),"captureFormat":format!("{:?}",config.capture.format),"output":if native_output {"Native"} else {"Rgb8"},"adjustments":config.adjustments});
    *handle
        .session
        .lock()
        .map_err(|_| invalid("Session state poisoned"))? = Some(Arc::new(session));
    Ok(result)
}

#[no_mangle]
/// JNI entry point that starts capture using automatic format negotiation.
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
        output(
            env,
            start_handle(
                id,
                width,
                height,
                numerator,
                denominator,
                None,
                native == JNI_TRUE,
            )?,
        )
    })
}

#[no_mangle]
/// JNI entry point that starts capture with an explicitly requested format.
pub extern "system" fn Java_com_medivh_camera_MedivhCameraBridge_nativeStartWithFormat(
    mut env: JNIEnv,
    _class: JClass,
    id: jlong,
    width: jint,
    height: jint,
    numerator: jint,
    denominator: jint,
    format: JString,
    native: jboolean,
) -> jstring {
    boundary(&mut env, |env| {
        let format = capture_format(&text(env, format)?)?;
        output(
            env,
            start_handle(
                id,
                width,
                height,
                numerator,
                denominator,
                Some(format),
                native == JNI_TRUE,
            )?,
        )
    })
}
#[cfg(feature = "convert-rgb")]
fn contiguous_rgb(frame: &Frame) -> Option<&[u8]> {
    let layout = frame.layout();
    let plane = layout.planes.first()?;
    let row = usize::try_from(layout.width).ok()?.checked_mul(3)?;
    let length = row.checked_mul(usize::try_from(layout.height).ok()?)?;
    if layout.format != PixelFormat::Rgb8
        || layout.bottom_up
        || layout.planes.len() != 1
        || plane.pixel_stride != 3
        || plane.row_stride != row
        || plane.length < length
    {
        return None;
    }
    frame
        .bytes()
        .get(plane.offset..plane.offset.checked_add(length)?)
}

#[cfg(feature = "convert-rgb")]
fn rgb_to_android_argb(rgb: &[u8], output: &mut [u8]) -> Result<()> {
    if !rgb.len().is_multiple_of(3) || output.len() < rgb.len() / 3 * 4 {
        return Err(invalid("Invalid RGB/ARGB buffer lengths"));
    }
    let (source_pixels, _) = rgb.as_chunks::<3>();
    let (target_pixels, _) = output.as_chunks_mut::<4>();
    for (source, target) in source_pixels.iter().zip(target_pixels) {
        let argb = 0xff00_0000
            | (u32::from(source[0]) << 16)
            | (u32::from(source[1]) << 8)
            | u32::from(source[2]);
        target.copy_from_slice(&argb.to_ne_bytes());
    }
    Ok(())
}

fn next_frame_into(
    id: jlong,
    pointer: *mut u8,
    capacity: usize,
    timeout_ms: jint,
    android_argb: bool,
) -> Result<Value> {
    let handle = handle(id)?;
    let current = session(&handle)?;
    let native_output = handle.native_output.load(Ordering::Acquire);
    if android_argb && native_output {
        return Err(invalid(
            "Android ARGB output requires an RGB capture session",
        ));
    }
    let (frame, byte_length) = runtime().block_on(async {
        let mut reader = handle.reader.lock().await;
        if reader
            .as_ref()
            .is_none_or(|r| !Arc::ptr_eq(&r.session, &current))
        {
            *reader = Some(Reader {
                frames: current.subscribe(SubscriptionOptions::latest())?,
                session: current,
                pending: None,
                #[cfg(feature = "convert-rgb")]
                converter: RgbConverter::new(),
                #[cfg(feature = "convert-rgb")]
                rgb: Vec::new(),
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
        let frame = reader.pending.as_ref().unwrap().clone();
        let byte_length = if native_output {
            let required = frame.bytes().len();
            if capacity < required {
                return Err(invalid(format!("Direct buffer needs {required} bytes")));
            }
            unsafe { std::ptr::copy_nonoverlapping(frame.bytes().as_ptr(), pointer, required) };
            required
        } else {
            #[cfg(feature = "convert-rgb")]
            {
                let request = ConversionRequest::for_layout(frame.layout());
                let rgb_length = request.output_len()?;
                let required = if android_argb {
                    rgb_length
                        .checked_div(3)
                        .and_then(|pixels| pixels.checked_mul(4))
                        .ok_or_else(|| invalid("ARGB frame size overflow"))?
                } else {
                    rgb_length
                };
                if capacity < required {
                    return Err(invalid(format!("Direct buffer needs {required} bytes")));
                }
                let output = unsafe { std::slice::from_raw_parts_mut(pointer, required) };
                if let Some(rgb) = contiguous_rgb(&frame) {
                    if android_argb {
                        rgb_to_android_argb(rgb, output)?;
                    } else {
                        output.copy_from_slice(rgb);
                    }
                } else {
                    reader.rgb.resize(rgb_length, 0);
                    let Reader { converter, rgb, .. } = &mut *reader;
                    converter.convert_into(&frame, request, rgb)?;
                    if android_argb {
                        rgb_to_android_argb(rgb, output)?;
                    } else {
                        output.copy_from_slice(rgb);
                    }
                }
                required
            }
            #[cfg(not(feature = "convert-rgb"))]
            unreachable!("RGB start is rejected when conversion is not compiled");
        };
        reader.pending.take();
        Ok((frame, byte_length))
    })?;
    let layout = frame.layout();
    let (pixel_format, planes) = if native_output {
        (
            format!("{:?}", layout.format),
            layout.planes.iter().map(|p|json!({"offset":p.offset,"length":p.length,"rowStride":p.row_stride,"pixelStride":p.pixel_stride})).collect::<Vec<_>>(),
        )
    } else {
        let pixel_stride = if android_argb { 4 } else { 3 };
        (
            if android_argb { "Argb8" } else { "Rgb8" }.to_owned(),
            vec![
                json!({"offset":0,"length":byte_length,"rowStride":layout.width as usize * pixel_stride,"pixelStride":pixel_stride}),
            ],
        )
    };
    Ok(
        json!({"session":frame.key.session,"sequence":frame.key.sequence,"byteLength":byte_length,"width":layout.width,"height":layout.height,"pixelFormat":pixel_format,"planes":planes,"colorMatrix":format!("{:?}",layout.color.matrix),"colorRange":format!("{:?}",layout.color.range),"sourceTimestampNs":frame.source_timestamp_ns,"clock":frame.source_timestamp().map(|t|format!("{:?}",t.clock)),"rotation":layout.orientation.rotation_degrees,"mirrored":layout.orientation.mirrored,"bottomUp":if native_output {layout.bottom_up} else {false}}),
    )
}

fn next_frame_boundary(
    env: &mut JNIEnv<'_>,
    id: jlong,
    buffer: JByteBuffer,
    timeout_ms: jint,
    android_argb: bool,
) -> Result<jstring> {
    if !(0..=60000).contains(&timeout_ms) {
        return Err(invalid("Frame timeout must be 0..60000 ms"));
    }
    if env.call_method(&buffer, "isReadOnly", "()Z", &[])?.z()? {
        return Err(invalid("Frame buffer is read-only"));
    }
    let capacity = env.get_direct_buffer_capacity(&buffer)?;
    let pointer = env.get_direct_buffer_address(&buffer)?;
    output(
        env,
        next_frame_into(id, pointer, capacity, timeout_ms, android_argb)?,
    )
}

/// The caller exclusively owns the direct buffer for the duration of this call.
/// The adapter copies one frame and returns metadata in JSON. A small buffer
/// leaves the frame pending, so callers may resize and retry without losing it.
#[no_mangle]
/// JNI entry point that copies the next frame into a direct byte buffer.
pub extern "system" fn Java_com_medivh_camera_MedivhCameraBridge_nativeNextFrame(
    mut env: JNIEnv,
    _class: JClass,
    id: jlong,
    buffer: JByteBuffer,
    timeout_ms: jint,
) -> jstring {
    boundary(&mut env, |env| {
        next_frame_boundary(env, id, buffer, timeout_ms, false)
    })
}

/// Writes native-endian ARGB words suitable for Bitmap.Config.ARGB_8888.
#[no_mangle]
/// JNI entry point that copies the next frame as Android-compatible ARGB pixels.
pub extern "system" fn Java_com_medivh_camera_MedivhCameraBridge_nativeNextFrameArgb(
    mut env: JNIEnv,
    _class: JClass,
    id: jlong,
    buffer: JByteBuffer,
    timeout_ms: jint,
) -> jstring {
    boundary(&mut env, |env| {
        next_frame_boundary(env, id, buffer, timeout_ms, true)
    })
}
#[no_mangle]
/// JNI entry point that returns capture and delivery metrics as JSON.
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
            "firstFrameLatencyMs":session.negotiated().first_frame_latency.as_secs_f64()*1000.0}),
        )
    })
}

#[no_mangle]
/// JNI entry point that stops capture while retaining the camera handle.
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
            runtime().block_on(session.close())?;
        }
        Ok(())
    })
}
#[no_mangle]
/// JNI entry point that stops capture and releases the camera handle.
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
                runtime().block_on(session.close())?;
            }
        }
        Ok(())
    })
}

fn control_value_payload(value: ControlValue) -> Value {
    match value {
        ControlValue::Number(value) => json!(value),
        ControlValue::Mode(value) => json!(format!("{value:?}")),
    }
}

fn control_payload(control: camera::ControlDescriptor) -> Value {
    json!({
        "id":format!("{:?}",control.id),
        "unit":format!("{:?}",control.unit),
        "readable":control.readable,
        "writable":control.writable,
        "modes":control.modes.into_iter().map(|mode|format!("{mode:?}")).collect::<Vec<_>>(),
        "range":control.range.map(|range|json!({
            "min":range.min,
            "max":range.max,
            "step":range.step,
            "default":range.default.map(control_value_payload)
        })),
        "requiresManual":control.requires_manual.map(|id|format!("{id:?}"))
    })
}

#[no_mangle]
/// JNI entry point that returns supported controls as JSON.
pub extern "system" fn Java_com_medivh_camera_MedivhCameraBridge_nativeControls(
    mut env: JNIEnv,
    _class: JClass,
    id: jlong,
) -> jstring {
    boundary(&mut env, |env| {
        let session = session(&*handle(id)?)?;
        let controls = runtime().block_on(session.controls())?;
        output(
            env,
            json!(controls
                .into_iter()
                .map(control_payload)
                .collect::<Vec<_>>()),
        )
    })
}
#[no_mangle]
/// JNI entry point that reads a control value and its readback status.
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
        let value = value.map(control_value_payload);
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
/// JNI entry point that applies a numeric value or mode to a camera control.
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

#[cfg(test)]
mod demo_contract_tests {
    use super::*;

    #[test]
    fn demo_supports_every_android_backend_and_capture_format() {
        assert_eq!(backend("camera2").unwrap(), Some(BackendId::CAMERA2));
        assert_eq!(backend("uvc").unwrap(), Some(BackendId::UVC));
        assert_eq!(backend("v4l2").unwrap(), Some(BackendId::V4L2));
        assert_eq!(
            capture_format("Mjpeg").unwrap(),
            camera::CaptureFormat::Mjpeg
        );
        assert_eq!(capture_format("Yuyv").unwrap(), camera::CaptureFormat::Yuyv);
        assert_eq!(capture_format("Nv12").unwrap(), camera::CaptureFormat::Nv12);
        assert!(capture_format("not-a-format").is_err());
    }

    #[test]
    fn capability_payload_exposes_format_resolution_and_fractional_frame_rate() {
        let capabilities = camera::DeviceCapabilities::single_native(
            camera::CaptureFormat::Nv12,
            1920,
            1080,
            FrameRate::new(30_000, 1_001).unwrap(),
        );

        let payload = capability_payload(capabilities);
        let mode = &payload["modes"][0];
        assert_eq!(payload["knowledge"], "known");
        assert_eq!(mode["format"], "Nv12");
        assert_eq!(mode["width"], 1920);
        assert_eq!(mode["height"], 1080);
        assert_eq!(mode["frameRateNumerator"], 30_000);
        assert_eq!(mode["frameRateDenominator"], 1_001);
    }

    #[test]
    fn control_payload_includes_the_reset_default() {
        let descriptor = camera::ControlDescriptor {
            id: ControlId::Brightness,
            unit: camera::ControlUnit::Native,
            readable: true,
            writable: Some(true),
            range: Some(camera::ControlRange {
                min: 0.0,
                max: 255.0,
                step: Some(1.0),
                default: Some(ControlValue::Number(128.0)),
            }),
            modes: Vec::new(),
            requires_manual: None,
        };

        assert_eq!(control_payload(descriptor)["range"]["default"], 128.0);
    }

    #[test]
    fn rgb_to_argb_uses_android_argb8888_native_words() {
        let mut output = [0u8; 8];
        rgb_to_android_argb(&[0x11, 0x22, 0x33, 0xaa, 0xbb, 0xcc], &mut output).unwrap();

        assert_eq!(
            u32::from_ne_bytes(output[0..4].try_into().unwrap()),
            0xff11_2233
        );
        assert_eq!(
            u32::from_ne_bytes(output[4..8].try_into().unwrap()),
            0xffaa_bbcc
        );
    }
}
