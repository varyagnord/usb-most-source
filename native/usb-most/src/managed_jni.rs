//! Fixed metadata only; no USB/kernel/network call occurs under this registry.
use jni::{
    EnvUnowned,
    errors::{Error, ThrowRuntimeExAndDefault},
    objects::{JByteArray, JClass},
};
use std::sync::Mutex;
use usb_most_protocol::managed::Book;
struct Models {
    next: i64,
    slots: [Option<(i64, Book)>; 8],
}
static MODELS: Mutex<Models> = Mutex::new(Models {
    next: 1,
    slots: [const { None }; 8],
});
fn bytes(e: &mut jni::Env, array: &JByteArray) -> Result<Option<Vec<u8>>, Error> {
    if array.is_null() || array.len(e)? > 52 {
        return Ok(None);
    }
    let mut signed = [0i8; 52];
    let len = array.len(e)?;
    array.get_region(e, 0, &mut signed[..len])?;
    Ok(Some(signed[..len].iter().map(|v| *v as u8).collect()))
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_cgutman_usbip_jni_NativeManagement_create<'a>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    nonce: JByteArray<'a>,
) -> i64 {
    env.with_env(|e| {
        let Some(nonce) = bytes(e, &nonce)? else {
            return Ok::<_, Error>(0);
        };
        let Ok(nonce) = nonce.as_slice().try_into() else {
            return Ok(0);
        };
        let Some(book) = Book::new(nonce) else {
            return Ok(0);
        };
        let mut models = MODELS.lock().unwrap_or_else(|e| e.into_inner());
        let Some(at) = models.slots.iter().position(Option::is_none) else {
            return Ok(0);
        };
        let id = models.next;
        let Some(next) = id.checked_add(1) else {
            return Ok(0);
        };
        models.slots[at] = Some((id, book));
        models.next = next;
        Ok(id)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_cgutman_usbip_jni_NativeManagement_close(
    mut env: EnvUnowned<'_>,
    _: JClass<'_>,
    id: i64,
) {
    env.with_env(|_| {
        let mut models = MODELS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = models
            .slots
            .iter_mut()
            .find(|s| s.as_ref().is_some_and(|(key, _)| *key == id))
        {
            *slot = None;
        }
        Ok::<_, Error>(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_cgutman_usbip_jni_NativeManagement_execute<'a>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    id: i64,
    action: i32,
    input: JByteArray<'a>,
) -> JByteArray<'a> {
    env.with_env(|e| {
        let Some(input) = bytes(e, &input)? else {
            return Ok(JByteArray::null());
        };
        let output = {
            let mut models = MODELS.lock().unwrap_or_else(|e| e.into_inner());
            models
                .slots
                .iter_mut()
                .filter_map(Option::as_mut)
                .find(|(key, _)| *key == id)
                .and_then(|(_, book)| book.execute(action as u32, &input))
        };
        match output {
            Some(out) => e.byte_array_from_slice(&out),
            None => Ok(JByteArray::null()),
        }
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}
