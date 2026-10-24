//! The stream reader thread (PC port): archive reads a stream queues run on it one after
//! another, as on `cAssetStreamer`'s device thread, so the thread that queued them never waits
//! for the disk.
use std::sync::{OnceLock, mpsc};

type Read = Box<dyn FnOnce() + Send>;

/// Runs `read` on the stream reader, after the reads queued before it; on the calling thread
/// when the reader cannot run.
pub(crate) fn queue(read: impl FnOnce() + Send + 'static) {
    static READER: OnceLock<Option<mpsc::Sender<Read>>> = OnceLock::new();
    let reader = READER.get_or_init(|| {
        let (sender, receiver) = mpsc::channel::<Read>();
        crate::host_thread::spawn_worker(
            "sk8 stream read",
            move || {
                for read in receiver {
                    read();
                }
            },
        )
        .ok()
        .map(|_| sender)
    });
    let read: Read = Box::new(read);
    let refused = match reader {
        Some(sender) => sender.send(read).err().map(|refused| refused.0),
        None => Some(read),
    };
    if let Some(read) = refused {
        read();
    }
}
