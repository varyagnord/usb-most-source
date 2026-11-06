use crate::policy::{self, Selection};
use jni::{
    EnvUnowned,
    errors::{Error, ThrowRuntimeExAndDefault},
    objects::{JByteArray, JClass, JIntArray, JString},
};
use std::sync::Mutex;
struct Models {
    slots: [Option<(i64, Selection)>; 8],
    next: i64,
}
static MODELS: Mutex<Models> = Mutex::new(Models {
    slots: [const { None }; 8],
    next: 1,
});
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_cgutman_usbip_jni_NativePolicy_connectionCommands<'a>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    host: JString<'a>,
    mode: i32,
    ids: JIntArray<'a>,
) -> JString<'a> {
    env.with_env(|e| {
        if host.is_null() || ids.is_null() || ids.len(e)? > policy::MAX_DEVICES {
            return Ok(JString::null());
        }
        let mut values = vec![0; ids.len(e)?];
        ids.get_region(e, 0, &mut values)?;
        let Some(commands) = policy::connection_commands(&host.try_to_string(e)?, mode, &values)
        else {
            return Ok(JString::null());
        };
        e.new_string(commands)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_cgutman_usbip_jni_NativePolicy_selectionCreate(
    mut env: EnvUnowned<'_>,
    _: JClass<'_>,
) -> i64 {
    env.with_env(|_| {
        let mut models = MODELS.lock().unwrap_or_else(|e| e.into_inner());
        let Some(slot) = models.slots.iter().position(Option::is_none) else {
            return Ok::<_, Error>(0);
        };
        let id = models.next;
        let Some(next) = id.checked_add(1) else {
            return Ok(0);
        };
        models.next = next;
        models.slots[slot] = Some((id, Selection::default()));
        Ok(id)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_cgutman_usbip_jni_NativePolicy_selectionClose(
    mut env: EnvUnowned<'_>,
    _: JClass<'_>,
    id: i64,
) {
    env.with_env(|_| {
        let mut models = MODELS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = models
            .slots
            .iter_mut()
            .find(|slot| slot.as_ref().is_some_and(|(key, _)| *key == id))
        {
            *slot = None;
        }
        Ok::<_, Error>(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_cgutman_usbip_jni_NativePolicy_validHost(
    mut env: EnvUnowned<'_>,
    _: JClass<'_>,
    host: JString<'_>,
    mode: i32,
) -> bool {
    env.with_env(|e| {
        Ok::<_, Error>(!host.is_null() && policy::valid_host(&host.try_to_string(e)?, mode))
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_cgutman_usbip_jni_NativePolicy_capability(
    mut env: EnvUnowned<'_>,
    _: JClass<'_>,
    class: i32,
    configs: i32,
    interfaces: i32,
    flags: i32,
) -> i32 {
    env.with_env(|_| Ok::<_, Error>(policy::capability(class, configs, interfaces, flags)))
        .resolve::<ThrowRuntimeExAndDefault>()
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_cgutman_usbip_jni_NativePolicy_describe<'a>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    raw: JByteArray<'a>,
) -> JIntArray<'a> {
    env.with_env(|e| {
        if raw.is_null() || raw.len(e)? > 65536 {
            return Ok(JIntArray::null());
        }
        let mut bytes = vec![0i8; raw.len(e)?];
        raw.get_region(e, 0, &mut bytes)?;
        let raw: Vec<u8> = bytes.into_iter().map(|b| b as u8).collect();
        let Some(description) = policy::describe(&raw) else {
            return Ok(JIntArray::null());
        };
        let array = e.new_int_array(description.len())?;
        array.set_region(e, 0, &description)?;
        Ok::<_, Error>(array)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_cgutman_usbip_jni_NativePolicy_selection<'a>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    id: i64,
    action: i32,
    data: JIntArray<'a>,
) -> JIntArray<'a> {
    env.with_env(|e| {
        if data.is_null() || data.len(e)? > policy::MAX_DEVICES * 2 {
            return Ok(JIntArray::null());
        }
        let mut values = vec![0; data.len(e)?];
        data.get_region(e, 0, &mut values)?;
        let mut models = MODELS.lock().unwrap_or_else(|p| p.into_inner());
        let Some((_, model)) = models
            .slots
            .iter_mut()
            .flatten()
            .find(|(key, _)| *key == id)
        else {
            return Ok(JIntArray::null());
        };
        let valid = match action {
            0 => model.sync(&values),
            1 => {
                values.len() == 2
                    && matches!(values[1], 0 | 1)
                    && model.select(values[0], values[1] != 0)
            }
            2 => {
                if values.len() != 1 || !matches!(values[0], 0 | 1) {
                    false
                } else {
                    model.select_all(values[0] != 0);
                    true
                }
            }
            3 => values.is_empty(),
            _ => false,
        };
        if !valid {
            return Ok(JIntArray::null());
        }
        let snapshot = model.snapshot();
        let array = e.new_int_array(snapshot.len())?;
        array.set_region(e, 0, &snapshot)?;
        Ok::<_, Error>(array)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_cgutman_usbip_jni_NativePolicy_speed(
    mut env: EnvUnowned<'_>,
    _: JClass<'_>,
    fd: i32,
) -> i32 {
    env.with_env(|_| {
        // SAFETY: GET_SPEED is a non-borrowing ioctl; the live Android connection owns fd.
        let speed = unsafe { libc::ioctl(fd, 0x551f as _, std::ptr::null_mut::<libc::c_void>()) };
        Ok::<_, Error>(if (1..=6).contains(&speed) { speed } else { 0 })
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}
