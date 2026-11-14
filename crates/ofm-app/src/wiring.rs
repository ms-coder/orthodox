//! The worker -> channel -> main-thread pattern (ARCHITECTURE §3). Use these two helpers for
//! every piece of background work in the UI; never touch the file system on the GTK main thread.
//!
//! ```ignore
//! let rx = wiring::run_on_pool(&services.pool, move |tx| {
//!     // worker thread: blocking I/O allowed
//!     let _ = tx.send_blocking(Msg::Hello);
//! });
//! wiring::attach(rx, move |msg| { /* main thread */ ControlFlow::Continue(()) });
//! ```
//! Stale results are dropped by the consumer (generation counters, see `panel::controller`).

use std::ops::ControlFlow;

use ofm_vfs::pool::IoPool;

/// Run `work` on the shared I/O pool and return the receiving end of its (unbounded) channel.
/// `work` should stop early when `tx.send_blocking` fails (the receiver was dropped).
pub fn run_on_pool<T: Send + 'static>(
    pool: &IoPool,
    work: impl FnOnce(async_channel::Sender<T>) + Send + 'static,
) -> async_channel::Receiver<T> {
    let (tx, rx) = async_channel::unbounded();
    pool.spawn(move || work(tx));
    rx
}

/// Consume `rx` on the GTK main loop (`glib::spawn_future_local`). `handler` returns
/// `ControlFlow::Break(())` to stop early; the loop also ends when all senders are gone.
/// The future is detached; `on_closed` runs once after the loop ended (stop or channel closed).
pub fn attach<T: 'static>(
    rx: async_channel::Receiver<T>,
    mut handler: impl FnMut(T) -> ControlFlow<()> + 'static,
    on_closed: impl FnOnce() + 'static,
) {
    gtk::glib::spawn_future_local(async move {
        while let Ok(msg) = rx.recv().await {
            if handler(msg).is_break() {
                break;
            }
        }
        on_closed();
    });
}
