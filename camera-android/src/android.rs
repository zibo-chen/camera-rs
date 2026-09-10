//! Android JNI 层直接调用 Java API
//!
//! 通过 JNI 反射调用 Android USB API，减少 Java 端代码

use std::os::fd::OwnedFd;
use std::{ffi::c_void, sync::OnceLock};

use jni::objects::{AutoLocal, GlobalRef, JObject, JString};
use jni::{AttachGuard, JavaVM};

use crate::error::{CameraError, Result};
use crate::UsbDeviceInfo as CameraDeviceInfo;

// UVC 接口类和子类常量
const INTERFACE_CLASS_VIDEO: u8 = 14; // CC_VIDEO
const INTERFACE_SUBCLASS_VIDEO_CONTROL: u8 = 1; // SC_VIDEOCONTROL
const INTERFACE_SUBCLASS_VIDEO_STREAMING: u8 = 2; // SC_VIDEOSTREAMING

/// Android 上下文，存储 JavaVM 和 Android Context
static ANDROID_CONTEXT: OnceLock<AndroidContext> = OnceLock::new();

pub(crate) struct AndroidContext {
    java_vm: *mut c_void,
    context_jobject: GlobalRef,
}

unsafe impl Send for AndroidContext {}
unsafe impl Sync for AndroidContext {}

impl AndroidContext {
    pub fn vm(&self) -> Result<JavaVM> {
        java_vm_from_raw(self.java_vm)
    }

    pub fn context(&self) -> GlobalRef {
        self.context_jobject.clone()
    }
}

fn java_vm_from_raw(java_vm: *mut c_void) -> Result<JavaVM> {
    if java_vm.is_null() {
        return Err(CameraError::Android(
            "Android JavaVM pointer is null".into(),
        ));
    }

    unsafe { JavaVM::from_raw(java_vm.cast()).map_err(CameraError::from) }
}

/// 初始化 Android 上下文
///
/// # Safety
/// 必须在应用启动时调用一次
pub unsafe fn init_android_context(vm: JavaVM, context: GlobalRef) {
    let android_context = AndroidContext {
        java_vm: vm.get_java_vm_pointer().cast(),
        context_jobject: context,
    };

    if ANDROID_CONTEXT.set(android_context).is_ok() {
        log::info!("Android context initialized");
    } else {
        log::warn!("Android context already initialized; ignoring duplicate init");
    }
}

/// 获取 Android 上下文
pub(crate) unsafe fn get_android_context() -> Result<&'static AndroidContext> {
    ANDROID_CONTEXT.get().ok_or(CameraError::Android(
        "Android context not initialized".into(),
    ))
}

/// 从 Java 对象获取字符串
unsafe fn get_string(
    env: &mut AttachGuard,
    object: &JObject,
    method: &str,
) -> Result<Option<String>> {
    let string_ret = match env.call_method(object, method, "()Ljava/lang/String;", &[]) {
        Ok(ret) => ret,
        Err(e) => {
            log::debug!("Error on call get string {method}: {e}");
            if env.exception_check()? {
                env.exception_clear()?;
            }
            return Err(CameraError::Android(format!("JNI call failed: {}", e)));
        }
    };

    let string = string_ret.l()?;
    let string: AutoLocal<JString> = env.auto_local(string.into());

    if string.is_null() {
        Ok(None)
    } else {
        Ok(Some(env.get_string_unchecked(&string)?.into()))
    }
}

/// 扫描 USB 摄像头设备列表
///
/// 通过 JNI 调用 Android UsbManager API 扫描设备
pub unsafe fn usb_manager_scan() -> Result<Vec<CameraDeviceInfo>> {
    let ctx = get_android_context()?;

    let context = ctx.context();
    let vm = ctx.vm()?;
    let mut env = vm.attach_current_thread()?;

    let class_ctx = env.find_class("android/content/Context")?;
    let usb_service = &env.get_static_field(class_ctx, "USB_SERVICE", "Ljava/lang/String;")?;

    let usb_manager = env
        .call_method(
            context.as_obj(),
            "getSystemService",
            "(Ljava/lang/String;)Ljava/lang/Object;",
            &[usb_service.into()],
        )?
        .l()?;
    let usb_manager = env.auto_local(usb_manager);

    let device_hashmap = env
        .call_method(&usb_manager, "getDeviceList", "()Ljava/util/HashMap;", &[])?
        .l()?;
    let device_hashmap = env.auto_local(device_hashmap);

    let device_values = env
        .call_method(&device_hashmap, "values", "()Ljava/util/Collection;", &[])?
        .l()?;
    let device_values = env.auto_local(device_values);

    let device_values_iterator = env
        .call_method(&device_values, "iterator", "()Ljava/util/Iterator;", &[])?
        .l()?;
    let device_values_iterator = env.auto_local(device_values_iterator);

    let mut result = Vec::new();

    while env
        .call_method(&device_values_iterator, "hasNext", "()Z", &[])?
        .z()?
    {
        let item = env
            .call_method(&device_values_iterator, "next", "()Ljava/lang/Object;", &[])?
            .l()?;
        let device_next = env.auto_local(item);

        if device_next.is_null() {
            break;
        }

        let vid = env
            .call_method(&device_next, "getVendorId", "()I", &[])?
            .i()? as u16;

        let pid = env
            .call_method(&device_next, "getProductId", "()I", &[])?
            .i()? as u16;

        // 检查是否是 UVC 设备
        if !scan_uvc_interface(&mut env, &device_next)? {
            continue;
        }

        // 获取设备详细信息
        // getProductName - 产品名称（可能为 null）
        let product_name = get_string(&mut env, &device_next, "getProductName")?;

        // getManufacturerName - 制造商名称（可能为 null，需要 Android 5.0+）
        let manufacturer = get_string(&mut env, &device_next, "getManufacturerName")?;

        // getSerialNumber - 序列号（可能为 null，需要权限）
        let serial_number = get_string(&mut env, &device_next, "getSerialNumber")
            .ok()
            .flatten();

        // getDeviceName - 设备路径（如 /dev/bus/usb/001/002）
        let device_path = get_string(&mut env, &device_next, "getDeviceName")?.unwrap_or_default();

        // getVersion - USB 版本（Android 13+）
        let usb_version = get_string(&mut env, &device_next, "getVersion")
            .ok()
            .flatten();

        // 构建显示名称：优先使用产品名称，其次是制造商+VID:PID
        let display_name = if let Some(ref name) = product_name {
            if !name.is_empty() {
                name.clone()
            } else if let Some(ref mfr) = manufacturer {
                format!("{} Camera", mfr)
            } else {
                format!("USB Camera {:04X}:{:04X}", vid, pid)
            }
        } else if let Some(ref mfr) = manufacturer {
            format!("{} Camera", mfr)
        } else {
            format!("USB Camera {:04X}:{:04X}", vid, pid)
        };

        log::debug!(
            "Found UVC device: {} (VID:{:04X} PID:{:04X}, Manufacturer: {:?}, Serial: {:?}, USB: {:?})",
            display_name, vid, pid, manufacturer, serial_number, usb_version
        );

        result.push(CameraDeviceInfo {
            index: result.len() as u32,
            name: display_name.clone(),
            description: format!("USB Camera VID:{:04X} PID:{:04X}", vid, pid),
            vendor_id: Some(vid),
            product_id: Some(pid),
            serial_number,
            device_path: Some(device_path),
        });
    }

    log::info!("Found {} UVC cameras", result.len());
    Ok(result)
}

/// 扫描设备接口，检查是否是 UVC 摄像头
unsafe fn scan_uvc_interface(env: &mut AttachGuard, device: &JObject) -> Result<bool> {
    let interface_count = env
        .call_method(device, "getInterfaceCount", "()I", &[])?
        .i()?;

    for i in 0..interface_count {
        let interface = env
            .call_method(
                device,
                "getInterface",
                "(I)Landroid/hardware/usb/UsbInterface;",
                &[i.into()],
            )?
            .l()?;
        let interface = env.auto_local(interface);

        let interface_class = env
            .call_method(&interface, "getInterfaceClass", "()I", &[])?
            .i()? as u8;

        if interface_class != INTERFACE_CLASS_VIDEO {
            continue;
        }

        let interface_sub_class = env
            .call_method(&interface, "getInterfaceSubclass", "()I", &[])?
            .i()? as u8;

        if interface_sub_class == INTERFACE_SUBCLASS_VIDEO_CONTROL
            || interface_sub_class == INTERFACE_SUBCLASS_VIDEO_STREAMING
        {
            return Ok(true);
        }
    }

    Ok(false)
}

/// 检查设备是否已有权限
pub unsafe fn usb_device_has_permission(device_path: &str) -> Result<bool> {
    let ctx = get_android_context()?;

    let context = ctx.context();
    let vm = ctx.vm()?;
    let mut env = vm.attach_current_thread()?;

    let class_ctx = env.find_class("android/content/Context")?;
    let usb_service = &env.get_static_field(class_ctx, "USB_SERVICE", "Ljava/lang/String;")?;

    let usb_manager = env
        .call_method(
            context.as_obj(),
            "getSystemService",
            "(Ljava/lang/String;)Ljava/lang/Object;",
            &[usb_service.into()],
        )?
        .l()?;
    let usb_manager = env.auto_local(usb_manager);

    let device_hashmap = env
        .call_method(&usb_manager, "getDeviceList", "()Ljava/util/HashMap;", &[])?
        .l()?;
    let device_hashmap = env.auto_local(device_hashmap);

    let device_name: JObject = env.new_string(device_path)?.into();
    let device_name = env.auto_local(device_name);

    let device = env
        .call_method(
            &device_hashmap,
            "get",
            "(Ljava/lang/Object;)Ljava/lang/Object;",
            &[(&device_name).into()],
        )?
        .l()?;
    let device = env.auto_local(device);

    if device.is_null() {
        return Err(CameraError::DeviceNotFound(device_path.to_string()));
    }

    let has_permission = env
        .call_method(
            &usb_manager,
            "hasPermission",
            "(Landroid/hardware/usb/UsbDevice;)Z",
            &[(&device).into()],
        )?
        .z()?;

    Ok(has_permission)
}

/// 请求 USB 设备权限
pub unsafe fn usb_device_request_permission(device_path: &str) -> Result<()> {
    let ctx = get_android_context()?;

    let context = ctx.context();
    let vm = ctx.vm()?;
    let mut env = vm.attach_current_thread()?;

    let class_ctx = env.find_class("android/content/Context")?;
    let usb_service = &env.get_static_field(class_ctx, "USB_SERVICE", "Ljava/lang/String;")?;

    let usb_manager = env
        .call_method(
            context.as_obj(),
            "getSystemService",
            "(Ljava/lang/String;)Ljava/lang/Object;",
            &[usb_service.into()],
        )?
        .l()?;
    let usb_manager = env.auto_local(usb_manager);

    let device_hashmap = env
        .call_method(&usb_manager, "getDeviceList", "()Ljava/util/HashMap;", &[])?
        .l()?;
    let device_hashmap = env.auto_local(device_hashmap);

    let device_name: JObject = env.new_string(device_path)?.into();
    let device_name = env.auto_local(device_name);

    let device = env
        .call_method(
            &device_hashmap,
            "get",
            "(Ljava/lang/Object;)Ljava/lang/Object;",
            &[(&device_name).into()],
        )?
        .l()?;
    let device = env.auto_local(device);

    if device.is_null() {
        return Err(CameraError::DeviceNotFound(device_path.to_string()));
    }

    // 创建 PendingIntent
    let permission = "com.medivh.camera.USB_PERMISSION";
    let permission_str: JObject = env.new_string(permission)?.into();
    let permission_str = env.auto_local(permission_str);

    let class_intent = env.find_class("android/content/Intent")?;
    let intent = env.new_object(
        &class_intent,
        "(Ljava/lang/String;)V",
        &[(&permission_str).into()],
    )?;

    // Android 12+ 需要为 Intent 设置 package 才能正确接收广播
    let package_name = env
        .call_method(
            context.as_obj(),
            "getPackageName",
            "()Ljava/lang/String;",
            &[],
        )?
        .l()?;
    env.call_method(
        &intent,
        "setPackage",
        "(Ljava/lang/String;)Landroid/content/Intent;",
        &[(&package_name).into()],
    )?;
    let intent = env.auto_local(intent);

    let class_pending_intent = env.find_class("android/app/PendingIntent")?;

    // Android 12+ 需要指定 FLAG_MUTABLE 或 FLAG_IMMUTABLE
    // FLAG_MUTABLE = 0x02000000
    let flags = 0x02000000i32; // FLAG_MUTABLE

    let permission_intent = env
        .call_static_method(
            &class_pending_intent,
            "getBroadcast",
            "(Landroid/content/Context;ILandroid/content/Intent;I)Landroid/app/PendingIntent;",
            &[
                ctx.context().as_obj().into(),
                0.into(),
                (&intent).into(),
                flags.into(),
            ],
        )?
        .l()?;
    let permission_intent = env.auto_local(permission_intent);

    env.call_method(
        &usb_manager,
        "requestPermission",
        "(Landroid/hardware/usb/UsbDevice;Landroid/app/PendingIntent;)V",
        &[(&device).into(), (&permission_intent).into()],
    )?;

    log::info!("USB permission requested for device: {}", device_path);
    Ok(())
}

/// 打开 USB 设备并获取文件描述符
///
/// 这是 Android 上打开 UVC 摄像头的核心方法
/// Returns an owned duplicate of the USB fd. The caller must close it.
/// The temporary Java UsbDeviceConnection is closed before returning.
pub unsafe fn usb_device_open(device_path: &str) -> Result<OwnedFd> {
    let ctx = get_android_context()?;

    let context = ctx.context();
    let vm = ctx.vm()?;
    let mut env = vm.attach_current_thread()?;

    let class_ctx = env.find_class("android/content/Context")?;
    let usb_service = &env.get_static_field(class_ctx, "USB_SERVICE", "Ljava/lang/String;")?;

    let usb_manager = env
        .call_method(
            context.as_obj(),
            "getSystemService",
            "(Ljava/lang/String;)Ljava/lang/Object;",
            &[usb_service.into()],
        )?
        .l()?;
    let usb_manager = env.auto_local(usb_manager);

    let device_hashmap = env
        .call_method(&usb_manager, "getDeviceList", "()Ljava/util/HashMap;", &[])?
        .l()?;
    let device_hashmap = env.auto_local(device_hashmap);

    let device_name: JObject = env.new_string(device_path)?.into();
    let device_name = env.auto_local(device_name);

    let device = env
        .call_method(
            &device_hashmap,
            "get",
            "(Ljava/lang/Object;)Ljava/lang/Object;",
            &[(&device_name).into()],
        )?
        .l()?;
    let device = env.auto_local(device);

    if device.is_null() {
        return Err(CameraError::DeviceNotFound(device_path.to_string()));
    }

    // 检查权限
    let has_permission = env
        .call_method(
            &usb_manager,
            "hasPermission",
            "(Landroid/hardware/usb/UsbDevice;)Z",
            &[(&device).into()],
        )?
        .z()?;

    if !has_permission {
        log::warn!("No permission for USB device: {}", device_path);
        // 请求权限
        usb_device_request_permission_internal(&mut env, &usb_manager, &device, ctx)?;
        return Err(CameraError::PermissionDenied(device_path.to_string()));
    }

    // 打开设备
    let usb_connection = env
        .call_method(
            &usb_manager,
            "openDevice",
            "(Landroid/hardware/usb/UsbDevice;)Landroid/hardware/usb/UsbDeviceConnection;",
            &[(&device).into()],
        )?
        .l()?;
    let usb_connection = env.auto_local(usb_connection);

    if usb_connection.is_null() {
        return Err(CameraError::Android("Failed to open USB device".into()));
    }

    // Duplicate while Java still owns the original descriptor. Always close
    // the temporary connection, including descriptor/duplication failures.
    let owned = env
        .call_method(&usb_connection, "getFileDescriptor", "()I", &[])
        .and_then(|v| v.i())
        .map_err(CameraError::from)
        .and_then(crate::owned_fd::duplicate);
    let closed = env.call_method(&usb_connection, "close", "()V", &[]);
    let owned = owned?;
    closed?;
    Ok(owned)
}

/// 内部函数：请求权限
unsafe fn usb_device_request_permission_internal<'a>(
    env: &mut AttachGuard<'a>,
    usb_manager: &JObject<'a>,
    device: &JObject<'a>,
    ctx: &AndroidContext,
) -> Result<()> {
    let permission = "com.medivh.camera.USB_PERMISSION";
    let permission_str: JObject = env.new_string(permission)?.into();
    let permission_str = env.auto_local(permission_str);

    let class_intent = env.find_class("android/content/Intent")?;
    let intent = env.new_object(
        &class_intent,
        "(Ljava/lang/String;)V",
        &[(&permission_str).into()],
    )?;

    // Android 12+ 需要为 Intent 设置 package 才能正确接收广播
    let package_name = env
        .call_method(
            ctx.context().as_obj(),
            "getPackageName",
            "()Ljava/lang/String;",
            &[],
        )?
        .l()?;
    env.call_method(
        &intent,
        "setPackage",
        "(Ljava/lang/String;)Landroid/content/Intent;",
        &[(&package_name).into()],
    )?;
    let intent = env.auto_local(intent);

    let class_pending_intent = env.find_class("android/app/PendingIntent")?;
    let flags = 0x02000000i32;

    let permission_intent = env
        .call_static_method(
            &class_pending_intent,
            "getBroadcast",
            "(Landroid/content/Context;ILandroid/content/Intent;I)Landroid/app/PendingIntent;",
            &[
                ctx.context().as_obj().into(),
                0.into(),
                (&intent).into(),
                flags.into(),
            ],
        )?
        .l()?;
    let permission_intent = env.auto_local(permission_intent);

    env.call_method(
        usb_manager,
        "requestPermission",
        "(Landroid/hardware/usb/UsbDevice;Landroid/app/PendingIntent;)V",
        &[device.into(), (&permission_intent).into()],
    )?;

    Ok(())
}

/// 通过 VID/PID 打开设备
pub unsafe fn usb_device_open_by_vid_pid(vid: u16, pid: u16) -> Result<OwnedFd> {
    let devices = usb_manager_scan()?;

    for device in devices {
        if device.vendor_id == Some(vid) && device.product_id == Some(pid) {
            if let Some(path) = &device.device_path {
                return usb_device_open(path);
            }
        }
    }

    Err(CameraError::DeviceNotFound(format!(
        "VID:{:04x} PID:{:04x}",
        vid, pid
    )))
}

/// 通过设备索引打开设备
pub unsafe fn usb_device_open_by_index(index: u32) -> Result<OwnedFd> {
    let devices = usb_manager_scan()?;

    if let Some(device) = devices.get(index as usize) {
        if let Some(path) = &device.device_path {
            return usb_device_open(path);
        }
    }

    Err(CameraError::DeviceNotFound(format!(
        "Device index {}",
        index
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_vm_from_raw_rejects_null_pointer_without_panicking() {
        let result = java_vm_from_raw(std::ptr::null_mut());

        assert!(matches!(
            result,
            Err(CameraError::Android(message)) if message.contains("JavaVM pointer is null")
        ));
    }

    #[test]
    fn get_android_context_returns_error_before_init() {
        let result = unsafe { get_android_context() };

        assert!(matches!(
            result,
            Err(CameraError::Android(message)) if message.contains("not initialized")
        ));
    }
}
