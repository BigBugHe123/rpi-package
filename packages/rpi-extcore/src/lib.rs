//! Shared services for protocol adapters. No HTTP, ACP or frontend dependencies.
#![forbid(unsafe_code)]
pub mod dialogs;
pub mod events;
pub mod history;
pub mod host;
pub mod message;
pub mod process;
pub mod projects;
pub mod protocol;
pub mod registry;
pub mod reply;
pub mod run;
pub mod service;
pub mod timing;
pub mod workspace;
