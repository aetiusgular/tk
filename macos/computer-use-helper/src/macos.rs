//! The two macOS calls the helper makes: the caller's code signature, and the
//! mouse buttons. Plain C APIs from Security, CoreFoundation and
//! CoreGraphics. Not compiled in the cloud (no macOS target there); the Mac
//! test is the first build.

#![cfg(target_os = "macos")]

use std::ffi::c_void;
use std::os::raw::c_int;

use crate::stop::{Button, Mouse};

type CFTypeRef = *const c_void;
type CFAllocatorRef = *const c_void;
type CFStringRef = *const c_void;
type CFDataRef = *const c_void;
type CFDictionaryRef = *const c_void;
type OSStatus = i32;

#[repr(C)]
struct CFDictionaryKeyCallBacks {
    _private: [u8; 0],
}
#[repr(C)]
struct CFDictionaryValueCallBacks {
    _private: [u8; 0],
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFTypeDictionaryKeyCallBacks: CFDictionaryKeyCallBacks;
    static kCFTypeDictionaryValueCallBacks: CFDictionaryValueCallBacks;
    fn CFDataCreate(allocator: CFAllocatorRef, bytes: *const u8, length: isize) -> CFDataRef;
    fn CFDictionaryCreate(
        allocator: CFAllocatorRef,
        keys: *const *const c_void,
        values: *const *const c_void,
        count: isize,
        key_callbacks: *const CFDictionaryKeyCallBacks,
        value_callbacks: *const CFDictionaryValueCallBacks,
    ) -> CFDictionaryRef;
    fn CFStringCreateWithBytes(
        allocator: CFAllocatorRef,
        bytes: *const u8,
        length: isize,
        encoding: u32,
        external: u8,
    ) -> CFStringRef;
    fn CFRelease(value: CFTypeRef);
}

#[link(name = "Security", kind = "framework")]
extern "C" {
    static kSecGuestAttributeAudit: CFStringRef;
    fn SecCodeCopyGuestWithAttributes(
        host: CFTypeRef,
        attributes: CFDictionaryRef,
        flags: u32,
        guest: *mut CFTypeRef,
    ) -> OSStatus;
    fn SecRequirementCreateWithString(
        text: CFStringRef,
        flags: u32,
        requirement: *mut CFTypeRef,
    ) -> OSStatus;
    fn SecCodeCheckValidity(code: CFTypeRef, flags: u32, requirement: CFTypeRef) -> OSStatus;
}

extern "C" {
    fn getsockopt(fd: c_int, level: c_int, name: c_int, value: *mut c_void, len: *mut u32)
        -> c_int;
}

const SOL_LOCAL: c_int = 0;
const LOCAL_PEERPID: c_int = 0x002;
const LOCAL_PEERTOKEN: c_int = 0x006;
const UTF8: u32 = 0x0800_0100;

/// The peer's pid on a connected Unix socket.
pub fn peer_pid(fd: c_int) -> Option<i32> {
    let mut pid: c_int = 0;
    let mut len = std::mem::size_of::<c_int>() as u32;
    let rc = unsafe {
        getsockopt(
            fd,
            SOL_LOCAL,
            LOCAL_PEERPID,
            &mut pid as *mut _ as *mut c_void,
            &mut len,
        )
    };
    (rc == 0 && pid > 0).then_some(pid)
}

/// Whether the peer's code meets `requirement`. It is identified by its audit
/// token, not its pid, so a pid reused after the peer exits cannot pass.
pub fn peer_meets(fd: c_int, requirement: &str) -> Option<bool> {
    let mut token = [0u8; 32];
    let mut len = token.len() as u32;
    let rc = unsafe {
        getsockopt(
            fd,
            SOL_LOCAL,
            LOCAL_PEERTOKEN,
            token.as_mut_ptr() as *mut c_void,
            &mut len,
        )
    };
    if rc != 0 || len as usize != token.len() {
        return None;
    }
    unsafe {
        let data = CFDataCreate(std::ptr::null(), token.as_ptr(), token.len() as isize);
        if data.is_null() {
            return None;
        }
        // Both are CFTypeRef already (`*const c_void`, the aliases above).
        let keys = [kSecGuestAttributeAudit];
        let values = [data];
        let attributes = CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        );
        CFRelease(data);
        if attributes.is_null() {
            return None;
        }
        let mut code: CFTypeRef = std::ptr::null();
        let status = SecCodeCopyGuestWithAttributes(std::ptr::null(), attributes, 0, &mut code);
        CFRelease(attributes);
        if status != 0 || code.is_null() {
            return Some(false);
        }
        let text = CFStringCreateWithBytes(
            std::ptr::null(),
            requirement.as_ptr(),
            requirement.len() as isize,
            UTF8,
            0,
        );
        if text.is_null() {
            CFRelease(code);
            return None;
        }
        let mut compiled: CFTypeRef = std::ptr::null();
        let status = SecRequirementCreateWithString(text, 0, &mut compiled);
        CFRelease(text);
        if status != 0 || compiled.is_null() {
            CFRelease(code);
            return None;
        }
        let valid = SecCodeCheckValidity(code, 0, compiled) == 0;
        CFRelease(compiled);
        CFRelease(code);
        Some(valid)
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventSourceButtonState(state: i32, button: u32) -> bool;
    fn CGEventCreate(source: CFTypeRef) -> CFTypeRef;
    fn CGEventGetLocation(event: CFTypeRef) -> CGPoint;
    fn CGEventCreateMouseEvent(source: CFTypeRef, kind: u32, at: CGPoint, button: u32)
        -> CFTypeRef;
    fn CGEventPost(tap: u32, event: CFTypeRef);
}

const COMBINED_SESSION_STATE: i32 = 0;
const HID_EVENT_TAP: u32 = 0;

fn cg_button(button: Button) -> (u32, u32) {
    // (CGMouseButton, the matching "up" event type)
    match button {
        Button::Left => (0, 2),
        Button::Right => (1, 4),
        Button::Other => (2, 26),
    }
}

pub struct SystemMouse;

impl Mouse for SystemMouse {
    fn held(&self, button: Button) -> Option<bool> {
        Some(unsafe { CGEventSourceButtonState(COMBINED_SESSION_STATE, cg_button(button).0) })
    }

    fn release(&self, button: Button) -> bool {
        let (number, up) = cg_button(button);
        unsafe {
            let probe = CGEventCreate(std::ptr::null());
            if probe.is_null() {
                return false;
            }
            let at = CGEventGetLocation(probe);
            CFRelease(probe);
            let event = CGEventCreateMouseEvent(std::ptr::null(), up, at, number);
            if event.is_null() {
                return false;
            }
            CGEventPost(HID_EVENT_TAP, event);
            CFRelease(event);
        }
        true
    }
}
