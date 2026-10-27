//! One optional provider package, one native plugin entry, independent identities.
pub mod deepseek;
pub mod llama_cpp;
pub mod openrouter;
pub mod proxy;
mod registration;
mod transport;

/// Register the provider bundle with the host's SDK ABI checks.
///
/// # Safety
/// `api` must satisfy `rpi_plugin_sdk::register_entrypoint`'s host vtable contract.
#[no_mangle]
pub unsafe extern "C" fn rpi_plugin_register(api: *const rpi_plugin_sdk::PluginApi) -> i32 {
    // SAFETY: the loading host supplies the vtable required by this entrypoint.
    unsafe { rpi_plugin_sdk::register_entrypoint(api, register) }
}

fn register(api: &rpi_plugin_sdk::PluginApi) -> i32 {
    for register in [
        deepseek::register,
        openrouter::register,
        llama_cpp::register,
        proxy::register,
    ] {
        let result = register(api);
        if result != 0 {
            return result;
        }
    }
    0
}
