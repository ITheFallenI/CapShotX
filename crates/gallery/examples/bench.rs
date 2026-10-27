//! Scan + thumbnail timing on a large folder.
//! `cargo run --release -p gallery --example bench -- <folder>`

use std::time::Instant;

fn main() {
    let folder = std::env::args().nth(1).expect("folder argument");
    let started = Instant::now();
    let entries = gallery::scan(std::path::Path::new(&folder));
    println!("scan: {} images in {} ms", entries.len(), started.elapsed().as_millis());

    let cache = std::env::temp_dir().join("capshotx-bench-thumbs");
    let _ = std::fs::remove_dir_all(&cache);
    let (tx, rx) = std::sync::mpsc::channel();
    let thumbs = gallery::Thumbnailer::new(Some(cache.clone()), move || { let _ = tx.send(()); });
    for pass in ["cold", "cached"] {
        let started = Instant::now();
        let first = &entries[..30.min(entries.len())];
        for e in first.iter().rev() {
            thumbs.request(e.key(), pass == "cold" && gallery::is_video(&e.path));
        }
        for _ in first {
            rx.recv().unwrap();
        }
        let done = thumbs.poll();
        let ok = done.iter().filter(|t| t.image.is_ok()).count();
        println!("{pass}: {ok}/{} thumbnails (one screenful) in {} ms", first.len(), started.elapsed().as_millis());
    }
    let _ = std::fs::remove_dir_all(&cache);
}
