//! Passing shelf items to another Glide with a six-character code.
//!
//! The code is stretched (PBKDF2, slow on purpose) into two things: the name
//! of a mailbox at ntfy.sh and an AES-256-GCM key. Each file is encrypted
//! here and uploaded to tmpfiles.org under a meaningless name; the list of
//! files (names, links, sizes), encrypted with the same key, is left in the
//! mailbox. Neither service ever sees a name or a byte of content.
//!
//! A wrong code leads to a different, almost certainly empty mailbox, and even
//! a message found there would not decrypt: random codes fetch nothing.
//! Mailboxes are kept by ntfy for 12 hours, so nothing outlives that.

use crate::island::{Event, Post};
use crate::modules::net;
use crate::modules::shelf::Item;
use aes_gcm::aead::rand_core::RngCore;
use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::Engine;
use std::path::PathBuf;

pub const CODE_LEN: usize = 6;
/// Codes Glide makes up skip look-alikes (0/O, 1/I); typed codes may use any letter or digit.
const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
/// tmpfiles.org takes files up to 100 MB.
const MAX_FILE: usize = 100 * 1024 * 1024;
/// How many times the code is hashed; makes guessing codes slow.
const ROUNDS: u32 = 600_000;

/// What a send or receive is doing, reported to the shelf.
pub enum News {
    Progress(String),
    /// Sent: the code, and which items went.
    Sent(String, Vec<Item>),
    /// Received and saved: where the files are now.
    Received(Vec<PathBuf>),
    Failed(String),
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Entry {
    name: String,
    page: String,
    size: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Manifest {
    v: u32,
    /// Unix seconds after which the files are gone.
    until: u64,
    files: Vec<Entry>,
}

pub fn random_code() -> String {
    let mut bytes = [0u8; CODE_LEN];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| ALPHABET[usize::from(*b) % ALPHABET.len()] as char).collect()
}

/// A typed character as it goes into a code, if it can.
pub fn code_char(c: char) -> Option<char> {
    c.is_ascii_alphanumeric().then(|| c.to_ascii_uppercase())
}

struct Secret {
    mailbox: String,
    key: Aes256Gcm,
}

fn derive(code: &str) -> Secret {
    let mut out = [0u8; 48];
    pbkdf2::pbkdf2_hmac::<sha2::Sha256>(code.as_bytes(), b"glide-shelf-v1", ROUNDS, &mut out);
    let mailbox = format!("glide-{}", out[..16].iter().map(|b| format!("{b:02x}")).collect::<String>());
    Secret { mailbox, key: Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&out[16..])) }
}

fn seal(key: &Aes256Gcm, plain: &[u8]) -> Vec<u8> {
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let mut out = nonce.to_vec();
    out.extend(key.encrypt(&nonce, plain).unwrap_or_default());
    out
}

fn open(key: &Aes256Gcm, sealed: &[u8]) -> Option<Vec<u8>> {
    let (nonce, body) = sealed.split_at_checked(12)?;
    key.decrypt(Nonce::from_slice(nonce), body).ok()
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Messages waiting in a mailbox (newest last).
fn mailbox(name: &str) -> Result<Vec<String>, String> {
    let text = net::get(&format!("https://ntfy.sh/{name}/json?poll=1&since=12h"))?;
    Ok(text
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|v| v["event"] == "message")
        .filter_map(|v| v["message"].as_str().map(str::to_string))
        .collect())
}

fn upload(sealed: &[u8], expire: u64) -> Result<String, String> {
    let mut bound = [0u8; 12];
    OsRng.fill_bytes(&mut bound);
    let boundary = format!("glide{}", bound.iter().map(|b| format!("{b:02x}")).collect::<String>());
    let mut body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"expire\"\r\n\r\n{expire}\r\n--{boundary}\r\n\
         Content-Disposition: form-data; name=\"file\"; filename=\"glide.bin\"\r\nContent-Type: application/octet-stream\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(sealed);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let headers = format!("Content-Type: multipart/form-data; boundary={boundary}");
    let answer = net::request("POST", "https://tmpfiles.org/api/v1/upload", &headers, &body, 64 * 1024)?;
    let value: serde_json::Value = serde_json::from_slice(&answer).map_err(|_| "tmpfiles.org gave an odd answer".to_string())?;
    value["data"]["url"].as_str().map(str::to_string).ok_or_else(|| "tmpfiles.org did not take the file".to_string())
}

/// tmpfiles.org hands the file out from a link on its page, with a fresh token.
fn download(page: &str) -> Result<Vec<u8>, String> {
    let html = net::get(page)?;
    let start = html.find("https://tmpfiles.org/dl/").ok_or("The files have expired")?;
    let link = &html[start..];
    let link = &link[..link.find('"').ok_or("The files have expired")?];
    net::request("GET", link, "", &[], MAX_FILE + 1024)
}

/// Where received files go: Downloads\Glide.
fn inbox_dir() -> PathBuf {
    use windows::Win32::UI::Shell::{FOLDERID_Downloads, SHGetKnownFolderPath, KNOWN_FOLDER_FLAG};
    // SAFETY: the returned string is read once and freed.
    let downloads = unsafe {
        SHGetKnownFolderPath(&FOLDERID_Downloads, KNOWN_FOLDER_FLAG(0), None).ok().map(|p| {
            let path = PathBuf::from(p.to_string().unwrap_or_default());
            windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _));
            path
        })
    };
    downloads
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join("Downloads"))
        .join("Glide")
}

/// A safe file name, and one not yet taken in `dir`.
fn free_name(dir: &std::path::Path, name: &str) -> PathBuf {
    let clean: String = name
        .chars()
        .map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c })
        .collect::<String>()
        .trim_matches(|c: char| c == '.' || c.is_whitespace())
        .to_string();
    let clean = if clean.is_empty() { "file".to_string() } else { clean };
    let path = std::path::Path::new(&clean);
    let (stem, ext) = (
        path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
        path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default(),
    );
    let mut candidate = dir.join(&clean);
    let mut n = 2;
    while candidate.exists() {
        candidate = dir.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    candidate
}

/// Encrypt and send `items` under `code`; they can be fetched for `expire` seconds.
pub fn send(items: Vec<Item>, code: String, expire: u64, post: Post) {
    std::thread::spawn(move || {
        let say = |text: &str| post.send(Event::Share(News::Progress(text.to_string())));
        let result = (|| -> Result<(), String> {
            say("Securing the code…");
            let secret = derive(&code);
            // Someone else's files already wait under this code: pick another.
            if !mailbox(&secret.mailbox)?.is_empty() {
                return Err("That code is in use. Pick another.".to_string());
            }
            let mut files = Vec::new();
            for (i, item) in items.iter().enumerate() {
                let (name, bytes) = match item {
                    Item::Text(text) => ("Note.txt".to_string(), text.as_bytes().to_vec()),
                    Item::File(path) if path.is_dir() => return Err("Folders cannot be shared yet".to_string()),
                    Item::File(path) => {
                        let size = std::fs::metadata(path).map_err(|_| "A file is missing".to_string())?.len();
                        if size as usize > MAX_FILE {
                            return Err("Files over 100 MB cannot be shared".to_string());
                        }
                        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        (name, std::fs::read(path).map_err(|_| "A file could not be read".to_string())?)
                    }
                };
                say(&format!("Sending {} of {}…", i + 1, items.len()));
                let page = upload(&seal(&secret.key, &bytes), expire)?;
                files.push(Entry { name, page, size: bytes.len() as u64 });
            }
            let manifest = Manifest { v: 1, until: now_secs() + expire, files };
            let plain = serde_json::to_vec(&manifest).map_err(|e| e.to_string())?;
            let letter = base64::engine::general_purpose::STANDARD.encode(seal(&secret.key, &plain));
            if letter.len() > 3800 {
                return Err("Too many files at once".to_string());
            }
            net::request("POST", &format!("https://ntfy.sh/{}", secret.mailbox), "", letter.as_bytes(), 64 * 1024)?;
            Ok(())
        })();
        post.send(Event::Share(match result {
            Ok(()) => News::Sent(code, items),
            Err(error) => News::Failed(error),
        }));
    });
}

/// Fetch, decrypt and save whatever waits under `code`.
pub fn receive(code: String, post: Post) {
    std::thread::spawn(move || {
        let say = |text: &str| post.send(Event::Share(News::Progress(text.to_string())));
        let result = (|| -> Result<Vec<PathBuf>, String> {
            say("Looking…");
            let secret = derive(&code);
            const NOTHING: &str = "Nothing waits under that code";
            // The newest letter that opens with this key.
            let manifest = mailbox(&secret.mailbox)?
                .iter()
                .rev()
                .filter_map(|letter| base64::engine::general_purpose::STANDARD.decode(letter).ok())
                .filter_map(|sealed| open(&secret.key, &sealed))
                .find_map(|plain| serde_json::from_slice::<Manifest>(&plain).ok())
                .ok_or(NOTHING)?;
            if manifest.until < now_secs() {
                return Err("Those files have expired".to_string());
            }
            let dir = inbox_dir();
            std::fs::create_dir_all(&dir).map_err(|_| "Could not make Downloads\\Glide".to_string())?;
            let mut saved = Vec::new();
            for (i, entry) in manifest.files.iter().enumerate() {
                say(&format!("Getting {} of {}…", i + 1, manifest.files.len()));
                let sealed = download(&entry.page)?;
                let plain = open(&secret.key, &sealed).ok_or("A file arrived damaged")?;
                let path = free_name(&dir, &entry.name);
                std::fs::write(&path, plain).map_err(|_| "Could not save a file".to_string())?;
                saved.push(path);
            }
            Ok(saved)
        })();
        post.send(Event::Share(match result {
            Ok(paths) => News::Received(paths),
            Err(error) => News::Failed(error),
        }));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_data_opens_only_with_its_code() {
        let (a, b) = (derive("K7P2QX"), derive("K7P2QY"));
        assert_ne!(a.mailbox, b.mailbox);
        let sealed = seal(&a.key, b"hello");
        assert_eq!(open(&a.key, &sealed).as_deref(), Some(&b"hello"[..]));
        assert!(open(&b.key, &sealed).is_none());
    }

    /// Talks to tmpfiles.org and ntfy.sh: run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn send_then_receive() {
        let dir = std::env::temp_dir().join("glide-share-test");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("hello share.txt");
        std::fs::write(&file, b"Glide share round trip").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let post = Post { tx, hwnd: 0, message: 0 };
        let code = random_code();
        let wait = |rx: &std::sync::mpsc::Receiver<Event>| loop {
            match rx.recv_timeout(std::time::Duration::from_secs(120)).expect("timed out") {
                Event::Share(News::Progress(p)) => println!("{p}"),
                Event::Share(other) => return other,
                _ => {}
            }
        };
        send(vec![Item::File(file.clone()), Item::Text("a note".into())], code.clone(), 600, post.clone());
        match wait(&rx) {
            News::Sent(c, _) => assert_eq!(c, code),
            News::Failed(e) => panic!("send failed: {e}"),
            _ => unreachable!(),
        }
        // ntfy takes a moment to list a new message.
        std::thread::sleep(std::time::Duration::from_secs(3));
        // The same code again is refused.
        send(vec![Item::Text("x".into())], code.clone(), 600, post.clone());
        assert!(matches!(wait(&rx), News::Failed(_)));
        // A wrong code finds nothing.
        receive("ZZZZZ9".into(), post.clone());
        assert!(matches!(wait(&rx), News::Failed(_)));
        receive(code, post);
        match wait(&rx) {
            News::Received(paths) => {
                println!("{paths:?}");
                assert_eq!(std::fs::read(&paths[0]).unwrap(), b"Glide share round trip");
                assert_eq!(std::fs::read(&paths[1]).unwrap(), b"a note");
                for p in paths {
                    let _ = std::fs::remove_file(p);
                }
            }
            News::Failed(e) => panic!("receive failed: {e}"),
            _ => unreachable!(),
        }
    }
}
