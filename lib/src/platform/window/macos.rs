//! macOS backend.
//!
//! Two-stage lookup because the OS doesn't give us "focus the window
//! owning pid N" directly:
//!
//! 1. **CoreGraphics** (`CGWindowListCopyWindowInfo`) gives every
//!    on-screen window's `kCGWindowOwnerPID` + `kCGWindowNumber`. We
//!    scan it for a match against the pid chain to learn both the
//!    owning app's pid and the CGWindowID of the specific window.
//!
//! 2. **Accessibility API** uses that pid to build the app's
//!    `AXUIElement`, enumerate its `kAXWindowsAttribute`, and find the
//!    `AXUIElement` whose `_AXUIElementGetWindow` matches the
//!    CGWindowID from step 1, then performs `kAXRaiseAction` on it
//!    (raises it above its siblings).
//!
//! 3. **AppleScript** (`tell application "<name>" to activate`) brings the
//!    owning app to the front; see [`activate_via_osascript`].
//!
//! `_AXUIElementGetWindow` is a private symbol exported by
//! HIServices/ApplicationServices. It's been stable since 10.5 and is
//! used by Hammerspoon, Yabai, Skhd, etc. — the only reliable way to
//! correlate a CGWindowID with an AXUIElement without poking at
//! titles/bounds.

use super::WindowManager;
use libproc::proc_pid;
use log::{info, warn};
use std::ffi::{c_void, CString};
use std::path::PathBuf;
use std::process::Command;

pub struct Macos;

pub fn available() -> bool {
    true
}

#[allow(non_camel_case_types)]
type CFTypeRef = *const c_void;
#[allow(non_camel_case_types)]
type CFArrayRef = CFTypeRef;
#[allow(non_camel_case_types)]
type CFDictionaryRef = CFTypeRef;
#[allow(non_camel_case_types)]
type CFStringRef = CFTypeRef;
#[allow(non_camel_case_types)]
type CFNumberRef = CFTypeRef;
#[allow(non_camel_case_types)]
type CFIndex = isize;
#[allow(non_camel_case_types)]
type CGWindowID = u32;
#[allow(non_camel_case_types)]
type AXUIElementRef = *const c_void;
#[allow(non_camel_case_types)]
type AXError = i32;

const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const K_CF_NUMBER_SINT32_TYPE: i32 = 3;
const K_CF_NUMBER_SINT64_TYPE: i32 = 4;
const K_AX_ERROR_SUCCESS: AXError = 0;
const K_CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY: u32 = 1 << 0;
const K_CG_WINDOW_LIST_EXCLUDE_DESKTOP_ELEMENTS: u32 = 1 << 4;
const K_CG_NULL_WINDOW_ID: CGWindowID = 0;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: CFTypeRef);
    fn CFArrayGetCount(array: CFArrayRef) -> CFIndex;
    fn CFArrayGetValueAtIndex(array: CFArrayRef, idx: CFIndex) -> *const c_void;
    fn CFDictionaryGetValue(dict: CFDictionaryRef, key: *const c_void) -> *const c_void;
    fn CFNumberGetValue(number: CFNumberRef, type_id: i32, value_ptr: *mut c_void) -> bool;
    fn CFStringCreateWithCString(
        alloc: *const c_void,
        c_str: *const i8,
        encoding: u32,
    ) -> CFStringRef;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGWindowListCopyWindowInfo(option: u32, relative_to_window: CGWindowID) -> CFArrayRef;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> AXError;
    fn AXUIElementPerformAction(element: AXUIElementRef, action: CFStringRef) -> AXError;
    // Private but stable since 10.5; used by Hammerspoon, Yabai, etc.
    fn _AXUIElementGetWindow(element: AXUIElementRef, identifier: *mut CGWindowID) -> AXError;
}

/// Owned CFStringRef wrapper so we don't forget to release.
struct CfString(CFStringRef);

impl CfString {
    fn new(s: &str) -> Self {
        let c = CString::new(s).expect("AX/CF key contained NUL byte");
        let r = unsafe {
            CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), K_CF_STRING_ENCODING_UTF8)
        };
        CfString(r)
    }
    fn as_ref(&self) -> CFStringRef {
        self.0
    }
}

impl Drop for CfString {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}

/// Scan all on-screen windows for one whose `kCGWindowOwnerPID` is in
/// `pids`. Returns `(owner_pid, window_id)` on the first hit.
fn find_owner_and_window(pids: &[u32]) -> Option<(i32, CGWindowID)> {
    let list = unsafe {
        CGWindowListCopyWindowInfo(
            K_CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY | K_CG_WINDOW_LIST_EXCLUDE_DESKTOP_ELEMENTS,
            K_CG_NULL_WINDOW_ID,
        )
    };
    if list.is_null() {
        return None;
    }
    let owner_key = CfString::new("kCGWindowOwnerPID");
    let number_key = CfString::new("kCGWindowNumber");
    let count = unsafe { CFArrayGetCount(list) };
    let mut result = None;
    for i in 0..count {
        let dict = unsafe { CFArrayGetValueAtIndex(list, i) } as CFDictionaryRef;
        if dict.is_null() {
            continue;
        }
        let owner_ref = unsafe { CFDictionaryGetValue(dict, owner_key.as_ref()) };
        let id_ref = unsafe { CFDictionaryGetValue(dict, number_key.as_ref()) };
        if owner_ref.is_null() || id_ref.is_null() {
            continue;
        }
        let mut owner: i32 = 0;
        let mut id: i64 = 0;
        let ok_owner = unsafe {
            CFNumberGetValue(
                owner_ref,
                K_CF_NUMBER_SINT32_TYPE,
                &mut owner as *mut _ as *mut c_void,
            )
        };
        let ok_id = unsafe {
            CFNumberGetValue(
                id_ref,
                K_CF_NUMBER_SINT64_TYPE,
                &mut id as *mut _ as *mut c_void,
            )
        };
        if !ok_owner || !ok_id {
            continue;
        }
        if pids.iter().any(|p| *p as i32 == owner) {
            result = Some((owner, id as CGWindowID));
            break;
        }
    }
    unsafe { CFRelease(list) };
    result
}

/// Walk up from `pid`'s executable path until we hit a `.app` bundle
/// directory, and return its stem ("kitty" from
/// `/Applications/kitty.app/Contents/MacOS/kitty`).
///
/// We use this to build a `tell application "<name>" to activate`
/// AppleScript, which is the only cross-process activation path that
/// reliably works for apps like kitty: `AXSetAttributeValue(kAXFrontmost)`,
/// `open -a <bundle path>`, and `System Events set frontmost by pid` all
/// silently no-op for it. AppleScript's `tell to activate` dispatches a
/// raw AppleEvent that every cocoa app honors.
fn app_name_for_pid(pid: i32) -> Option<String> {
    let exe = proc_pid::pidpath(pid).ok()?;
    let mut p = PathBuf::from(exe);
    loop {
        if p.extension().and_then(|s| s.to_str()) == Some("app") {
            return p.file_stem().and_then(|s| s.to_str()).map(String::from);
        }
        if !p.pop() {
            return None;
        }
    }
}

/// Bring the app owning `pid` to the front via AppleScript. Returns
/// false when the app isn't bundle-resolvable or osascript itself fails;
/// caller decides whether to keep going or surface a failure.
fn activate_via_osascript(pid: i32) -> bool {
    let Some(name) = app_name_for_pid(pid) else {
        warn!("macos focus: no .app bundle resolvable for pid {}", pid);
        return false;
    };
    // AppleScript double-quoted strings escape `"` with `\"`. App names
    // never legitimately contain `"`, but escape defensively.
    let script = format!(
        "tell application \"{}\" to activate",
        name.replace('\\', "\\\\").replace('"', "\\\"")
    );
    info!("macos focus: osascript: {}", script);
    match Command::new("osascript").args(["-e", &script]).output() {
        Ok(out) if out.status.success() => true,
        Ok(out) => {
            warn!(
                "macos focus: osascript activate failed (status={}, stderr={:?})",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
            false
        }
        Err(e) => {
            warn!("macos focus: osascript spawn failed: {}", e);
            false
        }
    }
}

/// Run `op` against the AX window matching `target_window_id` inside the
/// app element. Caller is responsible for the app element's lifetime.
fn with_ax_window<F>(app: AXUIElementRef, target_window_id: CGWindowID, op: F) -> bool
where
    F: FnOnce(AXUIElementRef) -> bool,
{
    let windows_attr = CfString::new("AXWindows");
    let mut value: CFTypeRef = std::ptr::null();
    let err = unsafe { AXUIElementCopyAttributeValue(app, windows_attr.as_ref(), &mut value) };
    if err != K_AX_ERROR_SUCCESS || value.is_null() {
        warn!("macos: AXCopyAttributeValue(AXWindows) failed: {}", err);
        return false;
    }
    let windows = value as CFArrayRef;
    let count = unsafe { CFArrayGetCount(windows) };
    let mut done = false;
    for i in 0..count {
        let w = unsafe { CFArrayGetValueAtIndex(windows, i) } as AXUIElementRef;
        if w.is_null() {
            continue;
        }
        let mut id: CGWindowID = 0;
        let r = unsafe { _AXUIElementGetWindow(w, &mut id) };
        if r == K_AX_ERROR_SUCCESS && id == target_window_id {
            done = op(w);
            break;
        }
    }
    unsafe { CFRelease(windows) };
    done
}

impl WindowManager for Macos {
    fn name(&self) -> &'static str {
        "macos"
    }

    fn focus(&self, pids: &[u32]) -> bool {
        let Some((owner, win_id)) = find_owner_and_window(pids) else {
            info!(
                "macos focus: no on-screen CG window matched pid chain {:?}",
                pids
            );
            return false;
        };
        info!("macos focus: pid={} cgwindow={}", owner, win_id);

        // Per-window raise. Brings the right window to the front *within*
        // its app, but doesn't activate the app cross-process. Only works
        // when the target app exposes the _AXUIElementGetWindow ↔
        // CGWindowID mapping (most cocoa apps do; GLFW-based backends may
        // not — in that case raise is a no-op and we still get correct
        // app-level focus from osascript below).
        let app = unsafe { AXUIElementCreateApplication(owner) };
        let raised = if !app.is_null() {
            let raised = with_ax_window(app, win_id, |window| {
                let raise = CfString::new("AXRaise");
                let pe = unsafe { AXUIElementPerformAction(window, raise.as_ref()) };
                if pe != K_AX_ERROR_SUCCESS {
                    warn!("macos focus: AXPerformAction(AXRaise) failed: {}", pe);
                    false
                } else {
                    true
                }
            });
            unsafe { CFRelease(app) };
            raised
        } else {
            warn!("macos focus: AXUIElementCreateApplication returned null");
            false
        };

        // Cross-process app activation. `AXFrontmost`, `open -a <bundle>`,
        // and `System Events set frontmost by pid` all silently no-op for
        // some apps (kitty being the motivating example). `tell
        // application "<name>" to activate` is the canonical AppleEvent
        // path and every cocoa app honors it.
        let activated = activate_via_osascript(owner);
        raised && activated
    }

    fn close(&self, pids: &[u32]) -> bool {
        let Some((owner, win_id)) = find_owner_and_window(pids) else {
            info!("macos close: no on-screen CG window matched pid chain");
            return false;
        };
        info!("macos close: pid={} cgwindow={}", owner, win_id);

        let app = unsafe { AXUIElementCreateApplication(owner) };
        if app.is_null() {
            return false;
        }

        let closed = with_ax_window(app, win_id, |window| {
            let close_btn_attr = CfString::new("AXCloseButton");
            let mut btn: CFTypeRef = std::ptr::null();
            let ge =
                unsafe { AXUIElementCopyAttributeValue(window, close_btn_attr.as_ref(), &mut btn) };
            if ge != K_AX_ERROR_SUCCESS || btn.is_null() {
                warn!(
                    "macos close: AXCopyAttributeValue(AXCloseButton) failed: {}",
                    ge
                );
                return false;
            }
            let press = CfString::new("AXPress");
            let pe = unsafe { AXUIElementPerformAction(btn as AXUIElementRef, press.as_ref()) };
            unsafe { CFRelease(btn) };
            if pe != K_AX_ERROR_SUCCESS {
                warn!("macos close: AXPerformAction(AXPress) failed: {}", pe);
                false
            } else {
                true
            }
        });

        unsafe { CFRelease(app) };
        closed
    }
}
