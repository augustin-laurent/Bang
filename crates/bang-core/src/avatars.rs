//! Bounded, asynchronous public avatar lookup with a persistent local cache.
use crate::{events::MailEvent, organization::sender_key};
use image::{GenericImageView, ImageFormat, ImageReader};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::Cursor,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{mpsc::UnboundedSender, Semaphore};
use url::Url;

const MAX_BYTES: usize = 1024 * 1024;
const DAY: u64 = 86400;

/// Avatar worker; it never handles message content or account credentials.
pub struct AvatarCache {
    dir: Option<PathBuf>,
    enabled: AtomicBool,
    pending: Mutex<HashMap<String, u64>>,
    slots: Semaphore,
}
impl AvatarCache {
    /// Create a lazy cache. No files or requests are created until lookup.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            dir: directories::ProjectDirs::from("", "", "bang")
                .map(|d| d.cache_dir().join("avatars")),
            enabled: AtomicBool::new(true),
            pending: Mutex::new(HashMap::new()),
            slots: Semaphore::new(4),
        })
    }
    /// Enable or disable future online lookups.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::SeqCst);
        if enabled {
            // A completion may have been ignored by the UI while avatars were
            // disabled. Replay completed cache hits, retaining in-flight dedup.
            self.pending
                .lock()
                .unwrap()
                .retain(|_, until| *until == u64::MAX);
        }
    }
    /// Deduplicate lookups; deliver only a validated local PNG path.
    pub fn request(self: &Arc<Self>, address: String, events: UnboundedSender<MailEvent>) {
        if !self.enabled.load(Ordering::SeqCst) {
            return;
        }
        let address = sender_key(&address);
        if domain(&address).is_none() {
            return;
        }
        {
            let mut pending = self.pending.lock().unwrap();
            if pending.get(&address).is_some_and(|until| *until > now()) {
                return;
            }
            pending.insert(address.clone(), u64::MAX);
        }
        let this = self.clone();
        tokio::spawn(async move {
            let Ok(_slot) = this.slots.acquire().await else {
                return;
            };
            if !this.enabled.load(Ordering::SeqCst) {
                this.pending.lock().unwrap().remove(&address);
                return;
            }
            let Some(dir) = &this.dir else {
                this.pending.lock().unwrap().remove(&address);
                return;
            };
            let hash = format!("{:x}", Sha256::digest(address.as_bytes()));
            let path = dir.join(format!("{hash}.png"));
            let stamp = dir.join(format!("{hash}.json"));
            let cached = read_cache(&path, &stamp);
            let (found, until) = match cached {
                Some(hit) => hit,
                None => {
                    let mut urls = vec![format!(
                        "https://www.gravatar.com/avatar/{hash}?s=80&d=404&r=g"
                    )];
                    if let Some(domain) = domain(&address).filter(|d| !personal_domain(d)) {
                        urls.push(format!("https://{domain}/favicon.ico"));
                    }
                    let mut image = None;
                    for url in urls {
                        if !this.enabled.load(Ordering::SeqCst) {
                            break;
                        }
                        if let Ok(Some(bytes)) =
                            tokio::time::timeout(Duration::from_secs(10), download(&url)).await
                        {
                            image = tokio::task::spawn_blocking(move || normalize(&bytes))
                                .await
                                .ok()
                                .flatten();
                            if image.is_some() {
                                break;
                            }
                        }
                    }
                    let found = if let Some(bytes) = image {
                        write_atomic(&path, &bytes).is_ok()
                    } else {
                        false
                    };
                    let until = now() + if found { 7 * DAY } else { DAY };
                    let _ = write_atomic(&stamp, format!("[{until},{found}]").as_bytes());
                    (found, until)
                }
            };
            this.pending.lock().unwrap().insert(address.clone(), until);
            if found && this.enabled.load(Ordering::SeqCst) {
                let _ = events.send(MailEvent::AvatarReady {
                    address,
                    path: path.to_string_lossy().into_owned(),
                });
            }
        });
    }
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn read_cache(path: &Path, stamp: &Path) -> Option<(bool, u64)> {
    let (until, found): (u64, bool) = serde_json::from_slice(&std::fs::read(stamp).ok()?).ok()?;
    if until <= now() {
        return None;
    }
    if found {
        let bytes = std::fs::read(path).ok()?;
        // Cache files are still validated; a corrupt file triggers a new lookup.
        if normalize(&bytes).is_none() {
            return None;
        }
    }
    Some((found, until))
}
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, bytes)?;
    std::fs::rename(temp, path)
}
fn domain(address: &str) -> Option<String> {
    let (local, host) = address.rsplit_once('@')?;
    if local.is_empty()
        || !host.contains('.')
        || host.len() > 253
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        || host
            .split('.')
            .any(|l| l.is_empty() || l.starts_with('-') || l.ends_with('-'))
    {
        return None;
    }
    Some(host.into())
}
fn personal_domain(host: &str) -> bool {
    [
        "gmail.com",
        "googlemail.com",
        "outlook.com",
        "hotmail.com",
        "hotmail.fr",
        "live.com",
        "live.fr",
        "msn.com",
        "yahoo.com",
        "yahoo.fr",
        "ymail.com",
        "icloud.com",
        "me.com",
        "mac.com",
        "aol.com",
        "proton.me",
        "protonmail.com",
        "pm.me",
        "gmx.com",
        "gmx.fr",
        "gmx.de",
        "mail.com",
        "orange.fr",
        "wanadoo.fr",
        "free.fr",
        "sfr.fr",
        "laposte.net",
        "tuta.com",
        "tutanota.com",
    ]
    .contains(&host)
}
fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_multicast()
                || ip.is_unspecified()
                || a == 0
                || a >= 240
                || (a == 100 && (64..=127).contains(&b))
                || (a == 192 && (b == 0 || b == 168))
                || (a == 198 && (b == 18 || b == 19 || b == 51))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            let s = ip.segments();
            (s[0] & 0xe000) == 0x2000
                && s[0] != 0x2002
                && !(s[0] == 0x2001 && (s[1] <= 0x1ff || s[1] == 0xdb8))
        }
    }
}
async fn download(url: &str) -> Option<Vec<u8>> {
    let mut url = Url::parse(url).ok()?;
    for _ in 0..4 {
        if url.scheme() != "https"
            || url.port_or_known_default() != Some(443)
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return None;
        }
        let host = url.host_str()?;
        let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, 443)).await.ok()?.collect();
        if addrs.is_empty() || addrs.iter().any(|a| !public_ip(a.ip())) {
            return None;
        }
        // Pin the validated DNS addresses to avoid a second DNS lookup/rebinding.
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .resolve_to_addrs(host, &addrs)
            .build()
            .ok()?;
        let response = client
            .get(url.clone())
            .header(
                reqwest::header::ACCEPT,
                "image/png,image/jpeg,image/gif,image/webp,image/x-icon",
            )
            .send()
            .await
            .ok()?;
        if response.status().is_redirection() {
            url = url
                .join(
                    response
                        .headers()
                        .get(reqwest::header::LOCATION)?
                        .to_str()
                        .ok()?,
                )
                .ok()?;
            continue;
        }
        return response_bytes(response).await;
    }
    None
}
async fn response_bytes(mut response: reqwest::Response) -> Option<Vec<u8>> {
    if !response.status().is_success()
        || response
            .content_length()
            .is_some_and(|n| n > MAX_BYTES as u64)
    {
        return None;
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if bytes.len() + chunk.len() > MAX_BYTES {
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    Some(bytes)
}
fn normalize(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() > MAX_BYTES {
        return None;
    }
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(1024);
    limits.max_image_height = Some(1024);
    limits.max_alloc = Some(16 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader.decode().ok()?;
    let (w, h) = decoded.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    let mut image = decoded
        .resize_to_fill(80, 80, image::imageops::FilterType::Lanczos3)
        .to_rgba8();
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let distance = ((x as f32 + 0.5 - 40.).powi(2) + (y as f32 + 0.5 - 40.).powi(2)).sqrt();
        pixel.0[3] = (pixel.0[3] as f32 * (40. - distance).clamp(0., 1.)) as u8;
    }
    let mut png = Cursor::new(Vec::new());
    image.write_to(&mut png, ImageFormat::Png).ok()?;
    Some(png.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identities_and_public_destinations() {
        assert_eq!(sender_key(" Alice@EXAMPLE.com "), "alice@example.com");
        assert!(domain("a@127.0.0.1").is_some());
        assert!(domain("a@host/path").is_none());
        assert!(personal_domain("gmail.com"));
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.1.1",
            "100.64.0.1",
            "192.0.2.1",
            "198.51.100.2",
            "::1",
            "::ffff:127.0.0.1",
            "2001:db8::1",
            "fd00::1",
        ] {
            assert!(!public_ip(ip.parse().unwrap()), "{ip}");
        }
        assert!(public_ip("8.8.8.8".parse().unwrap()));
        assert!(public_ip("2606:4700:4700::1111".parse().unwrap()));
    }
    #[test]
    fn invalid_images_and_expired_negative_cache() {
        assert!(normalize(b"not an image").is_none());
        assert!(normalize(&vec![0; MAX_BYTES + 1]).is_none());
        let dir = std::env::temp_dir().join(format!("bang-avatar-{}", std::process::id()));
        let path = dir.join("test.png");
        let stamp = dir.join("test.json");
        write_atomic(&stamp, format!("[{},false]", now() + DAY).as_bytes()).unwrap();
        assert_eq!(read_cache(&path, &stamp), Some((false, now() + DAY)));
        write_atomic(&stamp, b"[0,false]").unwrap();
        assert!(read_cache(&path, &stamp).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn simulated_http_success_missing_and_oversize() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
        };
        let mut png = Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(8, 8, image::Rgba([200, 40, 60, 255]))
            .write_to(&mut png, ImageFormat::Png)
            .unwrap();
        let png = png.into_inner();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for status in [200, 404, 200] {
                let (mut socket, _) = listener.accept().unwrap();
                let mut buf = [0; 1024];
                let _ = socket.read(&mut buf);
                let size = if status == 200 { png.len() } else { 0 };
                let length = if status == 200 && buf.windows(4).any(|w| w == b"/big") {
                    MAX_BYTES + 1
                } else {
                    size
                };
                write!(socket,"HTTP/1.1 {status} Test\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n").unwrap();
                if length == size && size > 0 {
                    socket.write_all(&png).unwrap();
                }
            }
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let bytes = response_bytes(
            client
                .get(format!("http://{addr}/ok"))
                .send()
                .await
                .unwrap(),
        )
        .await
        .unwrap();
        let circular = normalize(&bytes).unwrap();
        let img = image::load_from_memory(&circular).unwrap().to_rgba8();
        assert_eq!(img.dimensions(), (80, 80));
        assert_eq!(img.get_pixel(0, 0).0[3], 0);
        assert!(response_bytes(
            client
                .get(format!("http://{addr}/missing"))
                .send()
                .await
                .unwrap()
        )
        .await
        .is_none());
        assert!(response_bytes(
            client
                .get(format!("http://{addr}/big"))
                .send()
                .await
                .unwrap()
        )
        .await
        .is_none());
        server.join().unwrap();
    }
    #[tokio::test]
    async fn cached_lookup_deduplicates_and_survives_worker_restart() {
        let dir = std::env::temp_dir().join(format!("bang-avatar-worker-{}", std::process::id()));
        let address = "alice@example.test";
        let hash = format!("{:x}", Sha256::digest(address.as_bytes()));
        let path = dir.join(format!("{hash}.png"));
        let stamp = dir.join(format!("{hash}.json"));
        let mut png = Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(4, 4, image::Rgba([10, 20, 30, 255]))
            .write_to(&mut png, ImageFormat::Png)
            .unwrap();
        write_atomic(&path, &normalize(png.get_ref()).unwrap()).unwrap();
        write_atomic(&stamp, format!("[{},true]", now() + 7 * DAY).as_bytes()).unwrap();
        for _ in 0..2 {
            let cache = Arc::new(AvatarCache {
                dir: Some(dir.clone()),
                enabled: AtomicBool::new(true),
                pending: Mutex::new(HashMap::new()),
                slots: Semaphore::new(4),
            });
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            cache.request(" Alice@EXAMPLE.test ".into(), tx.clone());
            cache.request(address.into(), tx.clone());
            let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .unwrap()
                .unwrap();
            assert!(matches!(event,MailEvent::AvatarReady{address:ref key,..} if key==address));
            assert_eq!(cache.pending.lock().unwrap().len(), 1);
            assert!(rx.try_recv().is_err());
            cache.set_enabled(false);
            cache.request("bob@example.test".into(), tx);
            assert_eq!(cache.pending.lock().unwrap().len(), 1);
            cache.set_enabled(true);
            let (tx, mut replay) = tokio::sync::mpsc::unbounded_channel();
            cache.request(address.into(), tx);
            assert!(tokio::time::timeout(Duration::from_secs(2), replay.recv())
                .await
                .unwrap()
                .is_some());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
