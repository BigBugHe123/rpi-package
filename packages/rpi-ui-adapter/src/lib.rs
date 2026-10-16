//! `--webui=<UI>` selects an independent UI adapter. The host and the ACP
//! implementation remain untouched; the local relay only carries ACP frames.
#![forbid(unsafe_code)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

mod config;
mod openwebui;
mod server;

use rpi_plugin_sdk::{
    EventTag, FreeStringFn, RuntimeActionFn, RuntimeActionId, StablePluginEvent, StbString,
    StbStringRef,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Mutex, OnceLock};

static ACTION: OnceLock<RuntimeActionFn> = OnceLock::new();
static FREE: OnceLock<FreeStringFn> = OnceLock::new();
static USER: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
static STOP: OnceLock<tokio::sync::watch::Sender<bool>> = OnceLock::new();
static THREAD: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);

fn action(id: RuntimeActionId, args: Value) -> Option<Value> {
    let mut out = StbString::empty();
    let rc = ACTION.get()?(
        id as u32,
        StbStringRef::from_str(&args.to_string()),
        &mut out,
        USER.load(Ordering::Acquire),
    );
    let result = if rc == 0 {
        serde_json::from_str(&out.to_string_lossy()).ok()
    } else {
        None
    };
    if let Some(free) = FREE.get() {
        free(out);
    }
    result
}

fn flag(name: &str) -> Option<Value> {
    action(RuntimeActionId::GetCliFlag, json!({"name":name})).and_then(|v| v.get("value").cloned())
}

extern "C" fn start(_: StablePluginEvent, _: *mut std::ffi::c_void) -> i32 {
    let mut thread = THREAD.lock().unwrap();
    if thread.is_some() {
        return 0;
    }
    let launch = match config::resolve(flag) {
        Ok(launch) => launch,
        Err(error) => {
            eprintln!("[rpi-ui-adapter] {error}");
            action(
                RuntimeActionId::StdioLifecycle,
                json!({"op":"finish","exitCode":2}),
            );
            return 0;
        }
    };
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let _ = STOP.set(stop);
    *thread = Some(std::thread::spawn(move || {
        let result = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime.block_on(server::run(launch, receiver)),
            Err(error) => Err(error.to_string()),
        };
        let code = if let Err(error) = result {
            eprintln!("[rpi-ui-adapter] {error}");
            1
        } else {
            0
        };
        action(
            RuntimeActionId::StdioLifecycle,
            json!({"op":"finish","exitCode":code}),
        );
    }));
    0
}

extern "C" fn stop(_: StablePluginEvent, _: *mut std::ffi::c_void) -> i32 {
    if let Some(stop) = STOP.get() {
        let _ = stop.send(true);
    }
    if let Some(thread) = THREAD.lock().unwrap().take() {
        let _ = thread.join();
    }
    0
}

fn register(api: &rpi_plugin_sdk::PluginApi) -> i32 {
    let _ = ACTION.set(api.runtime_action);
    let _ = FREE.set(api.free_string);
    USER.store(api.user_data, Ordering::Release);
    if let Some(register_flag) = api.register_flag {
        for (name, description) in config::FLAGS {
            let code = register_flag(
                StbStringRef::from_str(name),
                StbStringRef::from_str(description),
            );
            if code != 0 {
                return code;
            }
        }
    }
    if matches!(
        flag("webui"),
        None | Some(Value::Null) | Some(Value::Bool(false))
    ) {
        return 0;
    }
    if flag("acp").is_some_and(|v| !v.is_null() && v != false) {
        eprintln!("[rpi-ui-adapter] --webui and --acp are separate entrypoints; choose one");
        return 1;
    }
    if action(RuntimeActionId::StdioLifecycle, json!({"op":"status"})).is_none() {
        return 1;
    }
    let Some(declare) = api.declare else {
        return 1;
    };
    if declare(StbStringRef::from_str("{\"ownsStdio\":true}")) != 0 {
        return 1;
    }
    let Some(handler) = api.register_event_handler else {
        return 1;
    };
    let result = handler(EventTag::SessionShutdown, stop, api.user_data);
    if result != 0 {
        return result;
    }
    handler(EventTag::SessionStart, start, api.user_data)
}

rpi_plugin_sdk::export_plugin!(register);
