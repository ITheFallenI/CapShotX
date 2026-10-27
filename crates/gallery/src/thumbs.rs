//! Background thumbnail generation with an on-disk cache.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::ThumbKey;

/// Longest edge of generated thumbnails, in pixels.
pub const THUMB_SIZE: u32 = 320;
const MAX_WORKERS: usize = 2;
const WORKER_IDLE_TIMEOUT: Duration = Duration::from_secs(2);

pub struct Thumbnail {
    pub key: ThumbKey,
    /// Straight-alpha RGBA8, or the reason it couldn't be made.
    pub image: Result<(u32, u32, Vec<u8>), String>,
    /// Video container duration, if `key.path` is a video and was requested.
    pub duration: Option<Duration>,
}

struct Request {
    key: ThumbKey,
    need_duration: bool,
}

struct Queue {
    /// Most recent request is served first, so whatever is on screen now
    /// wins over rows the user has already scrolled past.
    stack: Vec<Request>,
    active_workers: usize,
    closed: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    cvar: Condvar,
    tx: Sender<Thumbnail>,
    notify: Box<dyn Fn() + Send + Sync>,
    cache_dir: Option<PathBuf>,
}

/// Decodes thumbnails on short-lived worker threads. Results arrive via
/// [`Self::poll`]; `notify` is called after each one (e.g. to request a repaint).
pub struct Thumbnailer {
    shared: Arc<Shared>,
    results: Receiver<Thumbnail>,
}

impl Thumbnailer {
    pub fn new(cache_dir: Option<PathBuf>, notify: impl Fn() + Send + Sync + 'static) -> Self {
        let (tx, results) = mpsc::channel();
        if let Some(dir) = &cache_dir {
            let _ = std::fs::create_dir_all(dir);
        }
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue {
                stack: Vec::new(),
                active_workers: 0,
                closed: false,
            }),
            cvar: Condvar::new(),
            tx,
            notify: Box::new(notify),
            cache_dir,
        });
        Self { shared, results }
    }

    pub fn request(&self, key: ThumbKey, need_duration: bool) {
        let mut spawn_id = None;
        {
            let mut q = self.shared.queue.lock().expect("thumbnail queue poisoned");
            if q.closed {
                return;
            }
            q.stack.retain(|r| r.key != key);
            q.stack.push(Request { key, need_duration });
            if q.active_workers < MAX_WORKERS && q.active_workers < q.stack.len() {
                spawn_id = Some(q.active_workers);
                q.active_workers += 1;
            }
        }
        if let Some(id) = spawn_id {
            let shared = self.shared.clone();
            let spawned = std::thread::Builder::new()
                .name(format!("thumbs-{id}"))
                .spawn(move || worker(shared));
            if spawned.is_err() {
                if let Ok(mut q) = self.shared.queue.lock() {
                    q.active_workers = q.active_workers.saturating_sub(1);
                }
            }
        } else {
            self.shared.cvar.notify_one();
        }
    }

    /// Drops queued work that is no longer wanted (e.g. after scrolling).
    pub fn retain(&self, wanted: impl Fn(&ThumbKey) -> bool) {
        self.shared
            .queue
            .lock()
            .expect("thumbnail queue poisoned")
            .stack
            .retain(|r| wanted(&r.key));
    }

    pub fn poll(&self) -> Vec<Thumbnail> {
        self.results.try_iter().collect()
    }
}

impl Drop for Thumbnailer {
    fn drop(&mut self) {
        if let Ok(mut q) = self.shared.queue.lock() {
            q.closed = true;
            q.stack.clear();
        }
        self.shared.cvar.notify_all();
    }
}

fn worker(shared: Arc<Shared>) {
    loop {
        let req = {
            let mut q = shared.queue.lock().expect("thumbnail queue poisoned");
            loop {
                if q.closed {
                    q.active_workers = q.active_workers.saturating_sub(1);
                    return;
                }
                if let Some(req) = q.stack.pop() {
                    break req;
                }
                let (guard, timeout) = shared
                    .cvar
                    .wait_timeout(q, WORKER_IDLE_TIMEOUT)
                    .expect("thumbnail queue poisoned");
                q = guard;
                if timeout.timed_out() && q.stack.is_empty() {
                    q.active_workers = q.active_workers.saturating_sub(1);
                    return;
                }
            }
        };
        let image = make(&req.key, shared.cache_dir.as_deref());
        let duration = if req.need_duration {
            crate::video_duration(&req.key.path)
        } else {
            None
        };
        if shared.tx.send(Thumbnail { key: req.key, image, duration }).is_err() {
            if let Ok(mut q) = shared.queue.lock() {
                q.active_workers = q.active_workers.saturating_sub(1);
            }
            return;
        }
        (shared.notify)();
    }
}

/// Generates (or reads) and caches the thumbnail for `path` right away,
/// returning the 320 px RGBA8 image. Used right after saving a recording so
/// future startups hit the PNG cache without loading Media Foundation.
pub fn warm_cache(path: &Path, cache_dir: Option<&Path>) -> Option<(u32, u32, Vec<u8>)> {
    let meta = std::fs::metadata(path).ok()?;
    if let Some(dir) = cache_dir {
        let _ = std::fs::create_dir_all(dir);
    }
    let key = ThumbKey {
        path: path.to_path_buf(),
        modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        size: meta.len(),
    };
    make(&key, cache_dir).ok()
}

fn make(key: &ThumbKey, cache_dir: Option<&Path>) -> Result<(u32, u32, Vec<u8>), String> {
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
