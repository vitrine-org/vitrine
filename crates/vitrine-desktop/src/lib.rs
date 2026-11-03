//! Win32 desktop integration for Vitrine: locating the shell's desktop layer,
//! hosting per-widget layered windows beneath the desktop icons, and hit-testing
//! desktop icons so that pointer input can be forwarded to widgets.

#![cfg(windows)]

pub mod dcomp;
pub mod icons;
pub mod shell;
pub mod surface;
pub mod util;
