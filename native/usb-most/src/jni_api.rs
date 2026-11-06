#![allow(clippy::too_many_arguments)] // Fixed Java constructor/JNI ABI includes the Env argument.
use crate::{
    engine::{MAX_BYTES, Spec},
    linux, registry,
};
use jni::objects::JLongArray;
use jni::{
    EnvUnowned,
    errors::{Error, ThrowRuntimeExAndDefault},
    objects::{JByteArray, JClass, JObject},
};
#[cfg(feature = "diagnostics")]
use zeroize::Zeroize;

#[cfg(feature = "diagnostics")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_cgutman_usbip_jni_NativeUsb_enableHidDiagnostics(
    mut env: EnvUnowned<'_>,
    _: JClass<'_>,
    handle: i64,
) -> i32 {
    env.with_env(|_| {
        Ok::<_, Error>(
            registry::get(handle)
                .and_then(|engine| engine.enable_hid_diagnostics())
                .map_or_else(|e| -e, |()| 0),
        )
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

#[cfg(feature = "diagnostics")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_cgutman_usbip_jni_NativeUsb_takeHidDiagnostics<'a>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    handle: i64,
) -> JLongArray<'a> {
    env.with_env(|e| {
        let Ok(mut values) = registry::get(handle).and_then(|engine| engine.take_hid_diagnostics())
        else {
            return Ok::<_, Error>(JLongArray::null());
        };
        let result = (|| {
            let array = e.new_long_array(values.len())?;
            array.set_region(e, 0, &values)?;
            Ok::<_, Error>(array)
        })();
        values.zeroize();
        result
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

// The current typed JNI API caches the class and constructor once, avoiding
// repeated class/method lookup and dynamic signature parsing per completion.
jni::bind_java_type! {
    JavaCompletion => "org.cgutman.usbip.core.UsbBackend$Completion",
    constructors {
        fn new(id: jint, status: jint, actual: jint, data: [jbyte], disconnected: jboolean),
        fn new_wan(id: jint, status: jint, actual: jint, data: [jbyte], disconnected: jboolean, epoch: jlong, sequence: jlong),
    },
}

fn completion_to_java<'a>(
    env: &mut jni::Env<'a>,
    id: i32,
    status: i32,
    actual: i32,
    bytes: Option<&[u8]>,
    disconnected: bool,
) -> Result<JObject<'a>, Error> {
    let data = match bytes {
        Some(bytes) => env.byte_array_from_slice(bytes)?,
        None => JByteArray::null(),
    };
    Ok(JavaCompletion::new(env, id, status, actual, &data, disconnected)?.into())
}

/// Bounded synthetic self-test only; never calls a USB backend.
#[cfg(feature = "diagnostics")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_cgutman_usbip_jni_NativeUsb_diagnosticRoundTrip<'a>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    input: JByteArray<'a>,
) -> JObject<'a> {
    env.with_env(|e| {
        if input.is_null() || input.len(e)? > 32 {
            return Ok(JObject::null());
        }
        let length = input.len(e)?;
        let mut signed = [0i8; 32];
        input.get_region(e, 0, &mut signed[..length])?;
        let mut bytes = signed.map(|v| v as u8);
        let result = completion_to_java(e, 1, 0, length as i32, Some(&bytes[..length]), false);
        signed.zeroize();
        bytes.zeroize();
        result
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_cgutman_usbip_jni_NativeUsb_releaseInterface(
    mut env: EnvUnowned<'_>,
    _: JClass<'_>,
    fd: i32,
    interface: i32,
    reconnect: bool,
) -> i32 {
    env.with_env(|_| {
        Ok::<_, Error>(
            linux::release_interface(fd, interface, reconnect).map_or_else(|e| -e, |()| 0),
        )
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_cgutman_usbip_jni_NativeUsb_open(
    mut env: EnvUnowned<'_>,
    _: JClass<'_>,
    fd: i32,
) -> i64 {
    env.with_env(|_| Ok::<_, Error>(registry::open(fd)))
        .resolve::<ThrowRuntimeExAndDefault>()
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_cgutman_usbip_jni_NativeUsb_close(
    mut env: EnvUnowned<'_>,
    _: JClass<'_>,
    handle: i64,
) {
    env.with_env(|_| {
        registry::close(handle);
        Ok::<_, Error>(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_cgutman_usbip_jni_NativeUsb_cancel(
    mut env: EnvUnowned<'_>,
    _: JClass<'_>,
    handle: i64,
    id: i32,
) -> i32 {
    env.with_env(|_| {
        Ok::<_, Error>(
            registry::get(handle)
                .and_then(|engine| engine.cancel(id))
                .map_or_else(|e| -e, |()| 0),
        )
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_cgutman_usbip_jni_NativeUsb_configureSocket(
    mut env: EnvUnowned<'_>,
    _: JClass<'_>,
    fd: i32,
) -> i32 {
    env.with_env(|_| Ok::<_, Error>(linux::configure_socket(fd).map_or_else(|e| -e, |()| 0)))
        .resolve::<ThrowRuntimeExAndDefault>()
}
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "system" fn Java_org_cgutman_usbip_jni_NativeUsb_submit(
    mut env: EnvUnowned<'_>,
    _: JClass<'_>,
    handle: i64,
    id: i32,
    kind: i32,
    endpoint: i32,
    flags: i32,
    payload: JByteArray<'_>,
    length: i32,
    setup: JByteArray<'_>,
) -> i32 {
    env.with_env(|env| -> Result<i32, Error> {
        if !(0..=MAX_BYTES as i32).contains(&length) {
            return Ok(-libc::EINVAL);
        }
        let engine = match registry::get(handle) {
            Ok(e) => e,
            Err(e) => return Ok(-e),
        };
        if !payload.is_null() && payload.len(env)? != length as usize {
            return Ok(-libc::EINVAL);
        }
        let setup = if setup.is_null() {
            None
        } else {
            if setup.len(env)? != 8 {
                return Ok(-libc::EINVAL);
            }
            let mut bytes = [0i8; 8];
            setup.get_region(env, 0, &mut bytes)?;
            Some(bytes.map(|b| b as u8))
        };
        let spec = Spec {
            id,
            kind,
            endpoint,
            flags,
            length,
            setup,
        };
        let mut copy_error = None;
        let result = engine.submit(spec, |destination| {
            if payload.is_null() {
                return Err(libc::EINVAL);
            }
            // SAFETY: i8/u8 have identical layout. Destination is not yet submitted.
            let bytes = unsafe {
                std::slice::from_raw_parts_mut(
                    destination.as_mut_ptr().cast::<i8>(),
                    destination.len(),
                )
            };
            payload.get_region(env, 0, bytes).map_err(|error| {
                copy_error = Some(error);
                libc::EFAULT
            })
        });
        if let Some(error) = copy_error {
            return Err(error);
        }
        Ok(result.map_or_else(|e| -e, |()| 0))
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_cgutman_usbip_jni_NativeUsb_reap<'local>(
    mut env: EnvUnowned<'local>,
    _: JClass<'local>,
    handle: i64,
    timeout: i32,
) -> JObject<'local> {
    env.with_env(|env| -> Result<JObject<'_>, Error> {
        let result = registry::get(handle).and_then(|engine| engine.reap(timeout));
        match result {
            Ok(None) => Ok(JObject::null()),
            Ok(Some(completion)) if completion.wan_epoch != 0 => {
                wan_completion_to_java(env, &completion)
            }
            Ok(Some(completion)) => completion_to_java(
                env,
                completion.id,
                completion.status,
                completion.actual,
                completion.data(),
                false,
            ),
            Err(error) => completion_to_java(env, 0, -error, 0, None, true),
        }
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_cgutman_usbip_jni_NativeUsb_wanCommand<'a>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    handle: i64,
    op: i32,
    value: i32,
) -> JLongArray<'a> {
    env.with_env(|e| {
        let values = registry::get(handle)
            .and_then(|s| s.command(op, value))
            .unwrap_or_else(|err| vec![-i64::from(err)]);
        let out = e.new_long_array(values.len())?;
        out.set_region(e, 0, &values)?;
        Ok::<_, Error>(out)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

fn wan_completion_to_java<'a>(
    env: &mut jni::Env<'a>,
    completion: &crate::engine::Completion,
) -> Result<JObject<'a>, Error> {
    let data = env.byte_array_from_slice(completion.data().unwrap_or(&[]))?;
    Ok(JavaCompletion::new_wan(
        env,
        completion.id,
        completion.status,
        completion.actual,
        &data,
        false,
        i64::from(completion.wan_epoch),
        i64::from(completion.wan_sequence),
    )?
    .into())
}

#[cfg(feature = "diagnostics")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_cgutman_usbip_jni_NativeUsb_wanSelfTest<'a>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    rtt: i32,
    jitter: i32,
    loss: i32,
    count: i32,
) -> JObject<'a> {
    env.with_env(|e| {
        match crate::wan::lab::sample(rtt as u32, jitter as u32, loss as u32, count as u32) {
            Ok(c) => wan_completion_to_java(e, &c),
            Err(code) => completion_to_java(e, 0, -code, 0, None, true),
        }
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

#[cfg(feature = "diagnostics")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_cgutman_usbip_jni_NativeUsb_wanNetworkTest<'a>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    rtt: i32,
    jitter: i32,
    count: i32,
) -> JLongArray<'a> {
    env.with_env(|e| {
        let stats = crate::wan::network_lab::scenario(rtt as u32, jitter as u32, count as u32)
            .unwrap_or_else(|err| vec![-i64::from(err.raw_os_error().unwrap_or(libc::EPROTO))]);
        let array = e.new_long_array(stats.len())?;
        array.set_region(e, 0, &stats)?;
        Ok::<_, Error>(array)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}
