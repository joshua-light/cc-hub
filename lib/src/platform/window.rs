//! Window-manager abstraction.
//!
//! Linux: Hyprland natively, X11 via `xdotool` as a fallback.
//! macOS: CoreGraphics for window lookup, Accessibility API to raise the
//! matching window and bring its owning app to the front. The first AX call
//! triggers macOS's "Accessibility access" permission prompt — until the
//! user grants it (System Settings → Privacy & Security → Accessibility),
//! focus/close calls fail and the caller sees the standard "no window"
//! status.
//!
//! Detection runs once at first use and caches the selected chain in a
//! `OnceLock`. Headless environments end up with an empty chain, where
//! every operation is a no-op.

use log::info;
use std::sync::OnceLock;

pub trait WindowManager: Send + Sync {
    fn name(&self) -> &'static str;

    /// Focus the window owning any pid in `pids`. Returns true on success.
    fn focus(&self, pids: &[u32]) -> bool;

    /// Close the window owning any pid in `pids` (graceful WM_DELETE /
    /// closewindow). Returns true on success.
    fn close(&self, pids: &[u32]) -> bool;
}

static CURRENT: OnceLock<Chain> = OnceLock::new();

/// Globally-cached WindowManager for the current host. Cheap to call.
pub fn current() -> &'static dyn WindowManager {
    CURRENT.get_or_init(detect)
}

fn detect() -> Chain {
    let mut managers: Vec<Box<dyn WindowManager>> = Vec::new();
    if hyprland::available() {
        managers.push(Box::new(hyprland::Hyprland));
    }
    if xdotool::available() {
        managers.push(Box::new(xdotool::Xdotool));
    }
    #[cfg(target_os = "macos")]
    if macos::available() {
        managers.push(Box::new(macos::Macos));
    }
    let names: Vec<&str> = managers.iter().map(|m| m.name()).collect();
    info!("window: detected managers = {:?}", names);
    Chain { managers }
}

/// Runs each underlying manager in order until one succeeds. Gives us the
/// "try Hyprland, fall back to xdotool" behaviour we already relied on.
struct Chain {
    managers: Vec<Box<dyn WindowManager>>,
}

impl WindowManager for Chain {
    fn name(&self) -> &'static str {
        "chain"
    }

    fn focus(&self, pids: &[u32]) -> bool {
        self.managers.iter().any(|m| m.focus(pids))
    }

    fn close(&self, pids: &[u32]) -> bool {
        self.managers.iter().any(|m| m.close(pids))
    }
}

mod hyprland {
    use super::WindowManager;
    use log::{debug, info, warn};
    use std::process::Command;

    pub struct Hyprland;

    pub fn available() -> bool {
        // Hyprland exports this to every client; avoids paying for a hyprctl
        // spawn just to probe. If it's set but hyprctl is broken, individual
        // calls still fail gracefully and the chain falls through.
        std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
    }

    /// Fetch Hyprland clients and return the first `(pid, client_value)` whose
    /// pid matches one in `pids`.
    fn find_client(pids: &[u32]) -> Option<(u32, serde_json::Value)> {
        let output = Command::new("hyprctl")
            .args(["clients", "-j"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let clients: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).ok()?;
        for p in pids {
            for client in &clients {
                let cpid = client.get("pid").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                if cpid == *p {
                    return Some((cpid, client.clone()));
                }
            }
        }
        None
    }

    fn dispatch(command: &str, pids: &[u32]) -> bool {
        let Some((p, _)) = find_client(pids) else {
            debug!("no ancestor PID matched a hyprland client");
            return false;
        };
        let addr = format!("pid:{}", p);
        info!("hyprctl: {} pid {}", command, p);
        match Command::new("hyprctl")
            .args(["dispatch", command, &addr])
            .output()
        {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                info!(
                    "  hyprctl dispatch {} status={}, stdout={:?}, stderr={:?}",
                    command,
                    out.status,
                    stdout.trim(),
                    stderr.trim()
                );
                out.status.success()
            }
            Err(e) => {
                warn!("  hyprctl dispatch {} failed: {}", command, e);
                false
            }
        }
    }

    impl WindowManager for Hyprland {
        fn name(&self) -> &'static str {
            "hyprland"
        }

        fn focus(&self, pids: &[u32]) -> bool {
            dispatch("focuswindow", pids)
        }

        fn close(&self, pids: &[u32]) -> bool {
            dispatch("closewindow", pids)
        }
    }
}

mod xdotool {
    use super::WindowManager;
    use log::{debug, info, warn};
    use std::process::Command;

    pub struct Xdotool;

    pub fn available() -> bool {
        // Pay the probe cost once at startup. The result is cached by the
        // top-level OnceLock, so subsequent calls don't re-exec `command -v`.
        Command::new("sh")
            .args(["-c", "command -v xdotool >/dev/null 2>&1"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn act(pids: &[u32], action: &str) -> bool {
        for p in pids {
            let output = Command::new("xdotool")
                .args(["search", "--pid", &p.to_string()])
                .output();

            match output {
                Ok(out) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    if let Some(window_id) = stdout.lines().next().filter(|s| !s.is_empty()) {
                        info!("found window {} for pid {}, {}", window_id, p, action);
                        let result = Command::new("xdotool").args([action, window_id]).output();
                        match result {
                            Ok(a) => {
                                let astderr = String::from_utf8_lossy(&a.stderr);
                                info!(
                                    "  {} status={}, stderr={:?}",
                                    action,
                                    a.status,
                                    astderr.trim()
                                );
                                return a.status.success();
                            }
                            Err(e) => {
                                warn!("  {} failed to spawn: {}", action, e);
                                return false;
                            }
                        }
                    }
                }
                Err(e) => {
                    debug!("  xdotool not available: {}", e);
                    return false;
                }
            }
        }
        false
    }

    impl WindowManager for Xdotool {
        fn name(&self) -> &'static str {
            "xdotool"
        }

        fn focus(&self, pids: &[u32]) -> bool {
            act(pids, "windowactivate")
        }

        fn close(&self, pids: &[u32]) -> bool {
            act(pids, "windowclose")
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
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
    //!    CGWindowID from step 1. We then set the app's
    //!    `kAXFrontmostAttribute` to true (brings the app forward) and
    //!    perform `kAXRaiseAction` on the window (raises it above its
    //!    siblings).
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
            if !app.is_null() {
                with_ax_window(app, win_id, |window| {
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
            } else {
                warn!("macos focus: AXUIElementCreateApplication returned null");
            }

            // Cross-process app activation. `AXFrontmost`, `open -a <bundle>`,
            // and `System Events set frontmost by pid` all silently no-op for
            // some apps (kitty being the motivating example). `tell
            // application "<name>" to activate` is the canonical AppleEvent
            // path and every cocoa app honors it.
            activate_via_osascript(owner)
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
                let ge = unsafe {
                    AXUIElementCopyAttributeValue(window, close_btn_attr.as_ref(), &mut btn)
                };
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
}
