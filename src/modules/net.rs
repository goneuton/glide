//! The little networking Glide does (weather and holidays), through WinHTTP,
//! which ships with Windows. Blocking: only ever called from worker threads.

use std::ffi::c_void;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders,
    WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetOption, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
    WINHTTP_FLAG_SECURE, WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2, WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_3,
    WINHTTP_OPEN_REQUEST_FLAGS, WINHTTP_OPTION_SECURE_PROTOCOLS,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
};

/// Closes a WinHTTP handle when it goes out of scope.
struct Handle(*mut c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the handle came from WinHTTP and is closed exactly once.
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

fn option(handle: &Handle, option: u32, value: u32) {
    // SAFETY: a 4-byte option value on a live handle.
    unsafe {
        let _ = WinHttpSetOption(Some(handle.0), option, Some(&value.to_ne_bytes()));
    }
}

/// GET an `https://` address and return the body as text.
pub fn get(url: &str) -> Result<String, String> {
    request("GET", url, "", &[], 4 * 1024 * 1024).map(|body| String::from_utf8_lossy(&body).into_owned())
}

/// Send `body` (with extra `headers`, CRLF-separated) to an `https://`
/// address and return the response body, at most `limit` bytes.
pub fn request(method: &str, url: &str, headers: &str, body: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    let rest = url.strip_prefix("https://").ok_or("Only https addresses are supported")?;
    let (host, path) = rest.split_once('/').map_or((rest, String::from("/")), |(host, path)| (host, format!("/{path}")));
    let fail = |step: &str| {
        let error = windows::core::Error::from_win32();
        crate::island::trace(&format!("http {step} failed {:#x} for {host}", error.code().0));
        format!("Could not reach {host}")
    };
    // SAFETY: every handle is checked before use and closed by `Handle`; buffers outlive their calls.
    unsafe {
        let session = Handle(WinHttpOpen(w!("Glide"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0));
        if session.0.is_null() {
            return Err(fail("open"));
        }
        option(&session, WINHTTP_OPTION_SECURE_PROTOCOLS, WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2 | WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_3);
        let connection = Handle(WinHttpConnect(session.0, &HSTRING::from(host), 443, 0));
        if connection.0.is_null() {
            return Err(fail("connect"));
        }
        let request = Handle(WinHttpOpenRequest(
            connection.0,
            &HSTRING::from(method),
            &HSTRING::from(path.as_str()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_OPEN_REQUEST_FLAGS(WINHTTP_FLAG_SECURE.0),
        ));
        if request.0.is_null() {
            return Err(fail("request"));
        }
        // Uploads can be large: allow minutes, not the default 30 seconds.
        let _ = windows::Win32::Networking::WinHttp::WinHttpSetTimeouts(request.0, 0, 30_000, 300_000, 300_000);
        let headers: Vec<u16> = headers.encode_utf16().collect();
        let sent = WinHttpSendRequest(
            request.0,
            (!headers.is_empty()).then_some(headers.as_slice()),
            (!body.is_empty()).then_some(body.as_ptr().cast()),
            body.len() as u32,
            body.len() as u32,
            0,
        );
        if sent.is_err() {
            return Err(fail("send"));
        }
        if WinHttpReceiveResponse(request.0, std::ptr::null_mut()).is_err() {
            return Err(fail("receive"));
        }
        let (mut status, mut size) = (0u32, 4u32);
        let _ = WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&mut status as *mut u32).cast()),
            &mut size,
            std::ptr::null_mut(),
        );
        if !(200..300).contains(&status) {
            crate::island::trace(&format!("http status {status} for {host}{path}"));
            return Err(format!("{host} answered {status}"));
        }
        let mut body = Vec::new();
        // Read until the end.
        let mut chunk = [0u8; 16 * 1024];
        loop {
            let mut read = 0u32;
            if WinHttpReadData(request.0, chunk.as_mut_ptr().cast(), chunk.len() as u32, &mut read).is_err() {
                // Some servers drop the connection right after the last byte.
                if body.is_empty() {
                    return Err(fail("read"));
                }
                break;
            }
            body.extend_from_slice(&chunk[..read as usize]);
            if read == 0 {
                break;
            }
            if body.len() > limit {
                return Err(String::from("The download is too large"));
            }
        }
        Ok(body)
    }
}

/// Percent-encode a query value.
pub fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Two-letter country code of the Windows region ("IN", "US", ...).
pub fn region() -> String {
    let mut buffer = [0u16; 16];
    // SAFETY: the buffer is valid for its whole length.
    let len = unsafe { windows::Win32::Globalization::GetUserDefaultGeoName(&mut buffer) };
    let len = (len.max(1) - 1) as usize;
    String::from_utf16_lossy(&buffer[..len.min(buffer.len())]).to_uppercase()
}
