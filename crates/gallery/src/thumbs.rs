//! Background thumbnail generation with an on-disk cache.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::UNIX_EPOCH;

use crate::ThumbKey;

/// Longest edge of generated thumbnails, in pixels.
pub const THUMB_SIZE: u32 = 320;
const WORKERS: usize = 2;

pub struct Thumbnail {
    pub key: ThumbKey,
    /// Straight-alpha RGBA8, or the reason it couldn't be made.
    pub image: Result<(u32, u32, Vec<u8>), String>,
}

struct Queue {
    /// Most recent request is served first, so whatever is on screen now
    /// wins over rows the user has already scrolled past.
    stack: Vec<ThumbKey>,
    closed: bool,
}

/// Decodes thumbnails on worker threads. Results arrive via [`Self::poll`];
/// `notify` is called after each one (e.g. to request a repaint).
pub struct Thumbnailer {
    queue: Arc<(Mutex<Queue>, Condvar)>,
    results: Receiver<Thumbnail>,
}

impl Thumbnailer {
    pub fn new(cache_dir: Option<PathBuf>, notify: impl Fn() + Send + Sync + 'static) -> Self {
        let queue = Arc::new((Mutex::new(Queue { stack: Vec::new(), closed: false }), Condvar::new()));
        let (tx, results) = mpsc::channel();
        let notify = Arc::new(notify);
        if let Some(dir) = &cache_dir {
            let _ = std::fs::create_dir_all(dir);
        }
        for i in 0..WORKERS {
            let (queue, tx, notify, cache_dir) = (queue.clone(), tx.clone(), notify.clone(), cache_dir.clone());
            let _ = std::thread::Builder::new()
                .name(format!("thumbs-{i}"))
                .spawn(move || worker(queue, tx, notify, cache_dir));
        }
        Self { queue, results }
    }

    pub fn request(&self, key: ThumbKey) {
        let (lock, cvar) = &*self.queue;
        let mut q = lock.lock().expect("thumbnail queue poisoned");
        q.stack.retain(|k| k != &key);
        q.stack.push(key);
        cvar.notify_one();
    }

    /// Drops queued work that is no longer wanted (e.g. after scrolling).
    pub fn retain(&self, wanted: impl Fn(&ThumbKey) -> bool) {
        let (lock, _) = &*self.queue;
        lock.lock().expect("thumbnail queue poisoned").stack.retain(|k| wanted(k));
    }

    pub fn poll(&self) -> Vec<Thumbnail> {
        self.results.try_iter().collect()
    }
}

impl Drop for Thumbnailer {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.queue;
        if let Ok(mut q) = lock.lock() {
            q.closed = true;
            q.stack.clear();
        }
        cvar.notify_all();
    }
}

fn worker(
    queue: Arc<(Mutex<Queue>, Condvar)>,
    tx: Sender<Thumbnail>,
    notify: Arc<dyn Fn() + Send + Sync>,
    cache_dir: Option<PathBuf>,
) {
    let (lock, cvar) = &*queue;
    loop {
        let key = {
            let mut q = lock.lock().expect("thumbnail queue poisoned");
            loop {
                if q.closed {
                    return;
                }
                if let Some(key) = q.stack.pop() {
                    break key;
                }
                q = cvar.wait(q).expect("thumbnail queue poisoned");
            }
        };
        let image = make(&key, cache_dir.as_deref());
        if tx.send(Thumbnail { key, image }).is_err() {
            return;
        }
        notify();
    }
}

fn make(key: &ThumbKey, cache_dir: Option<&std::path::Path>) -> Result<(u32, u32, Vec<u8>), String> {
    let cache_path = cache_dir.map(|dir| dir.join(format!("{:016x}.png", cache_hash(key))));
    if let Some(cached) = cache_path.as_ref().and_then(|p| image::open(p).ok()) {
        let rgba = cached.to_rgba8();
        return Ok((rgba.width(), rgba.height(), rgba.into_raw()));
    }
    let full = crate::open_visual(&key.path)?;
    let thumb = full.thumbnail(THUMB_SIZE, THUMB_SIZE).to_rgba8();
    if let Some(path) = cache_path
        && let Err(err) = thumb.save(&path)
    {
        tracing::debug!("couldn't cache thumbnail {}: {err}", path.display());
    }
    Ok((thumb.width(), thumb.height(), thumb.into_raw()))
}

fn cache_hash(key: &ThumbKey) -> u64 {
    let mut h = DefaultHasher::new();
    key.path.hash(&mut h);
    key.size.hash(&mut h);
    key.modified.duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0).hash(&mut h);
    THUMB_SIZE.hash(&mut h);
    h.finish()
}
