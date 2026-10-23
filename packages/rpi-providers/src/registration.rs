//! Shared ABI adapters without exporting a plugin entry per vendor module.
macro_rules! register_provider {
    ($id:expr, $base:expr, $api:expr, $manifest:expr, $factory:path) => {
        extern "C" fn request(
            req: rpi_plugin_sdk::StbStringRef,
            out: *mut rpi_plugin_sdk::StbString,
            context: *mut std::ffi::c_void,
        ) -> i32 {
            unsafe {
                rpi_plugin_sdk::provider::drive_with_context(
                    $factory,
                    context,
                    req,
                    None,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    out,
                )
            }
        }

        extern "C" fn stream(
            req: rpi_plugin_sdk::StbStringRef,
            emit: Option<rpi_plugin_sdk::ProviderStreamEmitFn>,
            data: *mut std::ffi::c_void,
            control: *const rpi_plugin_sdk::ProviderStreamControl,
            _: Option<rpi_plugin_sdk::ProviderStreamCompletionFn>,
            _: *mut std::ffi::c_void,
            out: *mut rpi_plugin_sdk::StbString,
            context: *mut std::ffi::c_void,
        ) -> i32 {
            unsafe {
                rpi_plugin_sdk::provider::drive_with_context(
                    $factory, context, req, emit, data, control, out,
                )
            }
        }

        pub(crate) fn register(api: &rpi_plugin_sdk::PluginApi) -> i32 {
            rpi_plugin_sdk::provider::register_with_context(
                api, $id, $base, $api, $manifest, request, stream,
            )
        }
    };
}

pub(crate) use register_provider;
