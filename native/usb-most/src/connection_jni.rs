//! Narrow bounded JNI adapter; all networking and preparation live in Rust.
use jni::{
    EnvUnowned,
    errors::{Error, ThrowRuntimeExAndDefault},
    objects::{JByteArray, JClass, JString},
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use usb_most_connection::{
    bridge::LocalBridge,
    config::Profile,
    session::{Secret, Session},
};
use zeroize::Zeroizing;
static SESSIONS: Mutex<(i64, BTreeMap<i64, Arc<Session>>)> = Mutex::new((1, BTreeMap::new()));

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_cgutman_usbip_jni_NativeConnection_computers<'a>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    snapshot: JByteArray<'a>,
    request: JByteArray<'a>,
    running: bool,
) -> JByteArray<'a> {
    env.with_env(|e| {
        use usb_most_connection::computers;
        let snapshot = bytes(e, &snapshot, computers::MAX_CATALOG)?;
        let request = bytes(e, &request, computers::MAX_REQUEST)?;
        match computers::execute(&snapshot, &request, running) {
            Ok(output) => e.byte_array_from_slice(&output),
            Err(code) => {
                // Only fixed Rust error codes; never echo caller-owned JSON.
                e.throw_new(
                    jni::jni_str!("java/lang/IllegalArgumentException"),
                    jni::strings::JNIString::from(code),
                )?;
                Ok(JByteArray::null())
            }
        }
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_cgutman_usbip_jni_NativeConnection_diagnose<'a>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    input: JByteArray<'a>,
) -> JString<'a> {
    env.with_env(|e| {
        let input = bytes(e, &input, 4096)?;
        let Ok(report) = usb_most_connection::diagnosis::diagnose(&input) else {
            return Ok(JString::null());
        };
        e.new_string(report)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}
fn bytes(e: &mut jni::Env, array: &JByteArray, max: usize) -> Result<Zeroizing<Vec<u8>>, Error> {
    if array.is_null() || array.len(e)? > max {
        return Err(Error::NullPtr("invalid_connection_input"));
    }
    let mut signed = Zeroizing::new(vec![0i8; array.len(e)?]);
    array.get_region(e, 0, &mut signed)?;
    Ok(Zeroizing::new(signed.iter().map(|b| *b as u8).collect()))
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_cgutman_usbip_jni_NativeConnection_start<'a>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    profile: JByteArray<'a>,
    secret: JByteArray<'a>,
    port: i32,
    proof: JByteArray<'a>,
    artifacts: JString<'a>,
) -> i64 {
    env.with_env(|e| {
        let profile = bytes(e, &profile, 4096)?;
        let secret = bytes(e, &secret, 131072)?;
        let proof = bytes(e, &proof, 32)?;
        if proof.len() != 32 || !(1..=65535).contains(&port) || artifacts.is_null() {
            return Ok::<_, Error>(0);
        }
        let path = artifacts.try_to_string(e)?;
        if path.len() > 4096 {
            return Ok(0);
        }
        let Ok(profile) = Profile::parse(&profile) else {
            return Ok(0);
        };
        let Ok(secret) = Secret::parse(&secret) else {
            return Ok(0);
        };
        let Ok(bridge) = LocalBridge::new(
            ([127, 0, 0, 1], port as u16).into(),
            proof.as_slice().try_into().unwrap(),
        ) else {
            return Ok(0);
        };
        let mut sessions = SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
        if sessions.1.len() >= 2 {
            return Ok(0);
        }
        let id = sessions.0;
        let Some(next) = id.checked_add(1) else {
            return Ok(0);
        };
        sessions.0 = next;
        sessions.1.insert(
            id,
            Session::start(profile, secret, bridge, PathBuf::from(path)),
        );
        Ok(id)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_cgutman_usbip_jni_NativeConnection_poll<'a>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    id: i64,
) -> JString<'a> {
    env.with_env(|e| {
        let session = SESSIONS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .1
            .get(&id)
            .cloned();
        match session.and_then(|s| s.poll()) {
            Some(event) => e.new_string(event.as_str()),
            None => Ok(JString::null()),
        }
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_cgutman_usbip_jni_NativeConnection_approve<'a>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    id: i64,
    pin: JString<'a>,
    accept: bool,
) -> bool {
    env.with_env(|e| {
        if pin.is_null() {
            return Ok::<_, Error>(false);
        }
        let pin = pin.try_to_string(e)?;
        if pin.len() > 64 {
            return Ok(false);
        }
        let session = SESSIONS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .1
            .get(&id)
            .cloned();
        Ok(session.is_some_and(|s| s.approve(&pin, accept)))
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_cgutman_usbip_jni_NativeConnection_stop(
    mut env: EnvUnowned<'_>,
    _: JClass<'_>,
    id: i64,
) {
    env.with_env(|_| {
        if let Some(session) = SESSIONS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .1
            .remove(&id)
        {
            session.stop();
        }
        Ok::<_, Error>(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_cgutman_usbip_jni_NativeConnection_saved(
    mut env: EnvUnowned<'_>,
    _: JClass<'_>,
    id: i64,
    operation: i64,
    success: bool,
) -> bool {
    env.with_env(|_| {
        if operation <= 0 {
            return Ok::<_, Error>(false);
        }
        let session = SESSIONS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .1
            .get(&id)
            .cloned();
        Ok(session.is_some_and(|session| session.saved(operation as u64, success)))
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// Camera adapter owns capture/lifecycle; Rust owns bounded decoding and strict
/// pairing validation. No QR content is interpreted as a URL or command.
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_cgutman_usbip_jni_NativeConnection_decodeQr<'a>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    gray: JByteArray<'a>,
    width: i32,
    height: i32,
) -> JString<'a> {
    env.with_env(|e| {
        if !(32..=1280).contains(&width) || !(32..=1280).contains(&height) {
            return Ok(JString::null());
        }
        let size = width as usize * height as usize;
        if size > usb_most_connection::qr::MAX_PIXELS || gray.is_null() || gray.len(e)? != size {
            return Ok(JString::null());
        }
        let frame = bytes(e, &gray, usb_most_connection::qr::MAX_PIXELS)?;
        match usb_most_connection::qr::decode(width as usize, height as usize, &frame) {
            Ok(code) => e.new_string(code.as_str()),
            Err(_) => Ok(JString::null()),
        }
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}
