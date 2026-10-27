//! The caller check (PROTOCOL.md, "Caller check"): both sockets accept a
//! connection only from MonoCode.
//!
//! The pure part, compiled and tested everywhere: building the peer's code
//! requirement from the daemon's own designated requirement, and the exact
//! path check for unsigned development builds. The macOS part (`ffi`) is
//! FFI glue only and is NOT compiled in the cloud; the Mac test covers it.

use std::path::Path;

/// The daemon's own signing identifier.
pub const OWN_ID: &str = "com.monocode.desktop.activity-collector";
/// The app's signing identifier: the only accepted peer.
pub const PEER_ID: &str = "com.monocode.desktop";

/// The peer's requirement: the daemon's designated requirement with the
/// identifier swapped for the peer's. `None` (refuse every caller) unless
/// the requirement starts with exactly `identifier "<own_id>" and ` and has
/// a non-empty rest with no NUL.
pub fn peer_requirement(
    own_designated_requirement: &str,
    own_id: &str,
    peer_id: &str,
) -> Option<String> {
    let ok_id = |id: &str| !id.is_empty() && !id.contains(['"', '\\', '\0']);
    if !ok_id(own_id) || !ok_id(peer_id) {
        return None;
    }
    let prefix = format!("identifier \"{own_id}\" and ");
    let rest = own_designated_requirement.strip_prefix(&prefix)?;
    if rest.trim().is_empty() || rest.contains('\0') {
        return None;
    }
    Some(format!("identifier \"{peer_id}\" and {rest}"))
}

/// Development builds only: the peer's executable must be exactly the
/// configured absolute path.
pub fn dev_peer_ok(dev_peer_path: Option<&Path>, peer_exe: &Path) -> bool {
    match dev_peer_path {
        // Byte equality: `Path ==` would fold `//` and a trailing `/`.
        Some(allowed) => {
            allowed.is_absolute()
                && peer_exe.is_absolute()
                && allowed.as_os_str() == peer_exe.as_os_str()
        }
        None => false,
    }
}

/// The pid inside an `audit_token_t` (`val[5]`, as `audit_token_to_pid`).
pub fn pid_from_audit_token(token: &[u32; 8]) -> u32 {
    token[5]
}

#[cfg(target_os = "macos")]
pub mod ffi {
    //! NOT COMPILED IN THE CLOUD. macOS FFI glue for the caller check:
    //! `getsockopt(LOCAL_PEERTOKEN)`, Security.framework and `proc_pidpath`.
    //! Kept to calls and releases; the decisions live in the parent module.

    use std::ffi::{c_char, c_void, CStr};
    use std::os::fd::RawFd;
    use std::path::{Path, PathBuf};

    type OSStatus = i32;
    type CFTypeRef = *const c_void;
    type CFStringRef = *const c_void;
    type CFDataRef = *const c_void;
    type CFDictionaryRef = *const c_void;
    type CFAllocatorRef = *const c_void;
    type CFIndex = isize;
    type SecCodeRef = *const c_void;
    type SecRequirementRef = *const c_void;

    const ERR_SEC_SUCCESS: OSStatus = 0;
    const K_SEC_CS_DEFAULT_FLAGS: u32 = 0;
    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    const SOL_LOCAL: libc::c_int = 0;
    const LOCAL_PEERTOKEN: libc::c_int = 6;
    const PROC_PIDPATHINFO_MAXSIZE: usize = 4096;

    #[repr(C)]
    struct Opaque {
        _private: [u8; 0],
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFTypeDictionaryKeyCallBacks: Opaque;
        static kCFTypeDictionaryValueCallBacks: Opaque;
        fn CFStringCreateWithBytes(
            alloc: CFAllocatorRef,
            bytes: *const u8,
            num_bytes: CFIndex,
            encoding: u32,
            is_external_representation: u8,
        ) -> CFStringRef;
        fn CFDataCreate(alloc: CFAllocatorRef, bytes: *const u8, length: CFIndex) -> CFDataRef;
        fn CFDictionaryCreate(
            alloc: CFAllocatorRef,
            keys: *const *const c_void,
            values: *const *const c_void,
            num_values: CFIndex,
            key_call_backs: *const Opaque,
            value_call_backs: *const Opaque,
        ) -> CFDictionaryRef;
        fn CFRelease(cf: CFTypeRef);
        fn CFStringGetCString(
            the_string: CFStringRef,
            buffer: *mut c_char,
            buffer_size: CFIndex,
            encoding: u32,
        ) -> u8;
    }

    #[link(name = "Security", kind = "framework")]
    extern "C" {
        static kSecGuestAttributeAudit: CFStringRef;
        fn SecCodeCopySelf(flags: u32, self_: *mut SecCodeRef) -> OSStatus;
        fn SecCodeCopyDesignatedRequirement(
            code: SecCodeRef,
            flags: u32,
            requirement: *mut SecRequirementRef,
        ) -> OSStatus;
        fn SecRequirementCopyString(
            requirement: SecRequirementRef,
            flags: u32,
            text: *mut CFStringRef,
        ) -> OSStatus;
        fn SecRequirementCreateWithString(
            text: CFStringRef,
            flags: u32,
            requirement: *mut SecRequirementRef,
        ) -> OSStatus;
        fn SecCodeCopyGuestWithAttributes(
            host: SecCodeRef,
            attributes: CFDictionaryRef,
            flags: u32,
            guest: *mut SecCodeRef,
        ) -> OSStatus;
        fn SecCodeCheckValidity(
            code: SecCodeRef,
            flags: u32,
            requirement: SecRequirementRef,
        ) -> OSStatus;
    }

    /// An owned CF object, released on drop.
    struct Cf(CFTypeRef);

    impl Drop for Cf {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: we own one retain count on this object.
                unsafe { CFRelease(self.0) }
            }
        }
    }

    fn cf_string(s: &str) -> Option<Cf> {
        let len = CFIndex::try_from(s.len()).ok()?;
        // SAFETY: the bytes are valid for `len`; the result is owned.
        let r = unsafe {
            CFStringCreateWithBytes(
                std::ptr::null(),
                s.as_ptr(),
                len,
                K_CF_STRING_ENCODING_UTF8,
                0,
            )
        };
        (!r.is_null()).then_some(Cf(r))
    }

    fn rust_string(s: CFStringRef) -> Option<String> {
        let mut buf = vec![0 as c_char; 16 * 1024];
        let size = CFIndex::try_from(buf.len()).ok()?;
        // SAFETY: the buffer is writable for `size` bytes.
        let ok =
            unsafe { CFStringGetCString(s, buf.as_mut_ptr(), size, K_CF_STRING_ENCODING_UTF8) };
        if ok == 0 {
            return None;
        }
        // SAFETY: CFStringGetCString wrote a NUL-terminated string.
        let c = unsafe { CStr::from_ptr(buf.as_ptr()) };
        c.to_str().ok().map(str::to_owned)
    }

    /// The daemon's own designated requirement as text.
    fn own_requirement_text() -> Option<String> {
        let mut me: SecCodeRef = std::ptr::null();
        // SAFETY: out-pointer is valid.
        if unsafe { SecCodeCopySelf(K_SEC_CS_DEFAULT_FLAGS, &mut me) } != ERR_SEC_SUCCESS {
            return None;
        }
        let me = Cf(me);
        let mut req: SecRequirementRef = std::ptr::null();
        // SAFETY: `me` is a valid code object; out-pointer is valid.
        if unsafe { SecCodeCopyDesignatedRequirement(me.0, K_SEC_CS_DEFAULT_FLAGS, &mut req) }
            != ERR_SEC_SUCCESS
        {
            return None;
        }
        let req = Cf(req);
        let mut text: CFStringRef = std::ptr::null();
        // SAFETY: `req` is a valid requirement; out-pointer is valid.
        if unsafe { SecRequirementCopyString(req.0, K_SEC_CS_DEFAULT_FLAGS, &mut text) }
            != ERR_SEC_SUCCESS
        {
            return None;
        }
        let text = Cf(text);
        rust_string(text.0)
    }

    /// The caller check, built once at start.
    pub struct Checker {
        requirement: Option<Cf>,
        dev_peer_path: Option<PathBuf>,
    }

    // SAFETY: a SecRequirementRef is immutable after creation and CF objects
    // may be used from any thread.
    unsafe impl Send for Checker {}
    unsafe impl Sync for Checker {}

    impl Checker {
        /// Builds the peer requirement. Fails closed: with no usable own
        /// requirement, only the dev path (if given) can pass.
        pub fn new(dev_peer_path: Option<PathBuf>) -> Checker {
            let requirement = own_requirement_text()
                .and_then(|own| super::peer_requirement(&own, super::OWN_ID, super::PEER_ID))
                .and_then(|text| {
                    let s = cf_string(&text)?;
                    let mut req: SecRequirementRef = std::ptr::null();
                    // SAFETY: `s` is a valid CFString; out-pointer is valid.
                    let st = unsafe {
                        SecRequirementCreateWithString(s.0, K_SEC_CS_DEFAULT_FLAGS, &mut req)
                    };
                    (st == ERR_SEC_SUCCESS && !req.is_null()).then_some(Cf(req))
                });
            Checker {
                requirement,
                dev_peer_path,
            }
        }

        /// Whether the signature requirement could be built.
        pub fn signed_mode(&self) -> bool {
            self.requirement.is_some()
        }

        /// Whether the peer on `fd` may talk to the daemon.
        pub fn allows(&self, fd: RawFd) -> bool {
            let Some(token) = peer_token(fd) else {
                return false;
            };
            if let Some(req) = &self.requirement {
                if signature_ok(&token, req.0) {
                    return true;
                }
            }
            if self.dev_peer_path.is_some() {
                let pid = super::pid_from_audit_token(&token);
                if let Some(exe) = pid_path(pid) {
                    return super::dev_peer_ok(self.dev_peer_path.as_deref(), &exe);
                }
            }
            false
        }
    }

    fn peer_token(fd: RawFd) -> Option<[u32; 8]> {
        let mut token = [0u32; 8];
        let mut len = std::mem::size_of_val(&token) as libc::socklen_t;
        // SAFETY: the buffer is writable for `len` bytes.
        let r = unsafe {
            libc::getsockopt(
                fd,
                SOL_LOCAL,
                LOCAL_PEERTOKEN,
                token.as_mut_ptr().cast::<c_void>(),
                &mut len,
            )
        };
        (r == 0 && len as usize == std::mem::size_of_val(&token)).then_some(token)
    }

    fn signature_ok(token: &[u32; 8], requirement: SecRequirementRef) -> bool {
        let bytes: Vec<u8> = token.iter().flat_map(|v| v.to_ne_bytes()).collect();
        let Ok(len) = CFIndex::try_from(bytes.len()) else {
            return false;
        };
        // SAFETY: the bytes are valid for `len`; the result is owned.
        let data = Cf(unsafe { CFDataCreate(std::ptr::null(), bytes.as_ptr(), len) });
        if data.0.is_null() {
            return false;
        }
        // SAFETY: reading an immutable framework constant.
        let key = unsafe { kSecGuestAttributeAudit };
        let keys = [key];
        let values = [data.0];
        // SAFETY: arrays hold one valid CF object each; callbacks are the
        // framework's own CFType callbacks.
        let dict = Cf(unsafe {
            CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            )
        });
        if dict.0.is_null() {
            return false;
        }
        let mut guest: SecCodeRef = std::ptr::null();
        // SAFETY: valid dictionary; out-pointer is valid.
        let st = unsafe {
            SecCodeCopyGuestWithAttributes(
                std::ptr::null(),
                dict.0,
                K_SEC_CS_DEFAULT_FLAGS,
                &mut guest,
            )
        };
        if st != ERR_SEC_SUCCESS || guest.is_null() {
            return false;
        }
        let guest = Cf(guest);
        // SAFETY: valid code object and requirement.
        let st = unsafe { SecCodeCheckValidity(guest.0, K_SEC_CS_DEFAULT_FLAGS, requirement) };
        st == ERR_SEC_SUCCESS
    }

    /// The executable path of `pid` (dev mode only).
    fn pid_path(pid: u32) -> Option<PathBuf> {
        let pid = libc::c_int::try_from(pid).ok()?;
        let mut buf = vec![0u8; PROC_PIDPATHINFO_MAXSIZE];
        // SAFETY: the buffer is writable for its length.
        let n =
            unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast::<c_void>(), buf.len() as u32) };
        let n = usize::try_from(n)
            .ok()
            .filter(|n| *n > 0 && *n <= buf.len())?;
        buf.truncate(n);
        let s = String::from_utf8(buf).ok()?;
        Some(Path::new(&s).to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DR: &str = "identifier \"com.monocode.desktop.activity-collector\" and anchor apple generic and certificate leaf[subject.OU] = \"ABCDE12345\"";

    #[test]
    fn swaps_the_identifier() {
        assert_eq!(
            peer_requirement(DR, OWN_ID, PEER_ID).as_deref(),
            Some("identifier \"com.monocode.desktop\" and anchor apple generic and certificate leaf[subject.OU] = \"ABCDE12345\"")
        );
    }

    #[test]
    fn fails_closed() {
        for dr in [
            "",
            "anchor apple generic",
            "identifier \"com.other\" and anchor apple generic",
            "identifier \"com.monocode.desktop.activity-collector\" and ",
            "identifier \"com.monocode.desktop.activity-collector\" and    ",
            "identifier \"com.monocode.desktop.activity-collector\" or anchor apple",
            "identifier com.monocode.desktop.activity-collector and anchor apple",
            " identifier \"com.monocode.desktop.activity-collector\" and anchor apple",
            "identifier \"com.monocode.desktop.activity-collector\" and anchor\0apple",
            "identifier \"com.monocode.desktop.activity-collector\"and anchor apple",
        ] {
            assert_eq!(peer_requirement(dr, OWN_ID, PEER_ID), None, "{dr:?}");
        }
        assert_eq!(peer_requirement(DR, OWN_ID, "a\" or \"b"), None);
        assert_eq!(peer_requirement(DR, "", PEER_ID), None);
    }

    #[test]
    fn dev_path_is_exact() {
        let allowed = Path::new("/Users/a/monocode/target/debug/monocode");
        assert!(dev_peer_ok(Some(allowed), allowed));
        assert!(!dev_peer_ok(None, allowed));
        assert!(!dev_peer_ok(
            Some(allowed),
            Path::new("/Users/a/monocode/target/debug/monocode2")
        ));
        assert!(!dev_peer_ok(
            Some(Path::new("monocode")),
            Path::new("monocode")
        ));
        assert!(!dev_peer_ok(
            Some(allowed),
            Path::new("/Users/a//monocode/target/debug/monocode")
        ));
        assert!(!dev_peer_ok(
            Some(allowed),
            Path::new("/Users/a/monocode/target/debug/../debug/monocode")
        ));
    }

    #[test]
    fn token_pid() {
        assert_eq!(pid_from_audit_token(&[0, 0, 0, 0, 0, 4242, 0, 0]), 4242);
    }
}
