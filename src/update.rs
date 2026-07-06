//! Update check: the latest GitHub release tag comes from the redirect
//! `Location` of /releases/latest — no JSON parsing, no API rate limits.
//! Pure logic up top is cross-platform for `cargo test`; the WinHTTP call
//! lives in the win module.

/// "vX.Y.Z" → (X, Y, Z). None for anything else (dev hashes, rc suffixes).
pub fn parse_tag(tag: &str) -> Option<(u32, u32, u32)> {
    let mut parts = tag.strip_prefix('v')?.split('.').map(|p| p.parse().ok());
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(Some(x)), Some(Some(y)), Some(Some(z)), None) => Some((x, y, z)),
        _ => None,
    }
}

/// True only when both tags parse and remote is strictly newer: a local
/// dev build or a re-tag must not nag.
pub fn is_newer(remote: &str, local: &str) -> bool {
    match (parse_tag(remote), parse_tag(local)) {
        (Some(r), Some(l)) => r > l,
        _ => false,
    }
}

#[cfg(windows)]
pub use win::latest_release_tag;

#[cfg(windows)]
mod win {
    use windows::core::{w, PCWSTR};
    use windows::Win32::Networking::WinHttp::{
        WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest,
        WinHttpQueryHeaders, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetOption,
        WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_OPTION_REDIRECT_POLICY,
        WINHTTP_OPTION_REDIRECT_POLICY_NEVER, WINHTTP_QUERY_LOCATION,
    };

    /// Handle guard so every early return still closes in child-first order.
    struct H(*mut core::ffi::c_void);
    impl Drop for H {
        fn drop(&mut self) {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }

    /// GET /releases/latest with redirects disabled; the `Location` header
    /// ends in /tag/vX.Y.Z. None on any failure — the caller stays silent
    /// and retries at the next timer tick. Blocks on network: call from a
    /// worker thread only, never the message loop.
    pub fn latest_release_tag() -> Option<String> {
        unsafe {
            let session = H(WinHttpOpen(
                w!("win-app-switcher"),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            ));
            if session.0.is_null() {
                return None;
            }
            let connect = H(WinHttpConnect(session.0, w!("github.com"), 443, 0));
            if connect.0.is_null() {
                return None;
            }
            let request = H(WinHttpOpenRequest(
                connect.0,
                w!("GET"),
                w!("/pre/win-app-switcher/releases/latest"),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            ));
            if request.0.is_null() {
                return None;
            }
            WinHttpSetOption(
                Some(request.0),
                WINHTTP_OPTION_REDIRECT_POLICY,
                Some(&WINHTTP_OPTION_REDIRECT_POLICY_NEVER.to_le_bytes()),
            )
            .ok()?;
            WinHttpSendRequest(request.0, None, None, 0, 0, 0).ok()?;
            WinHttpReceiveResponse(request.0, std::ptr::null_mut()).ok()?;
            let mut buf = [0u16; 512];
            let mut len = std::mem::size_of_val(&buf) as u32;
            WinHttpQueryHeaders(
                request.0,
                WINHTTP_QUERY_LOCATION,
                PCWSTR::null(),
                Some(buf.as_mut_ptr() as *mut _),
                &mut len,
                std::ptr::null_mut(),
            )
            .ok()?;
            let location = String::from_utf16_lossy(&buf[..len as usize / 2]);
            let (_, tag) = location.rsplit_once("/tag/")?;
            Some(tag.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_release_tags() {
        assert_eq!(parse_tag("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_tag("v0.0.1"), Some((0, 0, 1)));
    }

    #[test]
    fn rejects_non_release_tags() {
        assert_eq!(parse_tag("1.2.3"), None); // no v
        assert_eq!(parse_tag("v1.2"), None); // too few parts
        assert_eq!(parse_tag("v1.2.3.4"), None); // too many
        assert_eq!(parse_tag("v1.2.3-rc1"), None); // suffix
        assert_eq!(parse_tag("ab1c4c38"), None); // dev hash
        assert_eq!(parse_tag(""), None);
    }

    #[test]
    fn newer_only_when_both_parse_and_remote_wins() {
        assert!(is_newer("v0.2.0", "v0.1.9"));
        assert!(is_newer("v1.0.0", "v0.9.9"));
        assert!(is_newer("v0.1.10", "v0.1.9")); // numeric, not lexicographic
        assert!(!is_newer("v0.1.0", "v0.1.0")); // re-tag: no nag
        assert!(!is_newer("v0.1.0", "v0.2.0"));
        assert!(!is_newer("v1.0.0", "ab1c4c38")); // dev build: no nag
        assert!(!is_newer("garbage", "v0.1.0"));
    }
}
