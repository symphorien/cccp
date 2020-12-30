use anyhow::Context;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::sync::Arc;

/// This struct allows to display a progress bar and status information during
/// operation. It leaves nothing once `done` is called.
pub struct Progress {
    /// A MultiProgress, inside Arc to be able to call join in another thread.
    multi: Arc<MultiProgress>,
    /// The progress bar for rounds and status. Filled on first call to `next_round`.
    round_bar: Option<ProgressBar>,
    /// The progress bar for bytes processed during a round. Only filled between
    /// `next_round` and `syncing`.
    bytes_bar: Option<ProgressBar>,
}

impl Progress {
    /// Creates an instance. Displays nothing yet.
    pub fn new() -> Progress {
        let multi = Arc::new(MultiProgress::new());
        Progress {
            multi,
            bytes_bar: None,
            round_bar: None,
        }
    }

    /// Display a short status message. Replaces the previous message if applicable.
    pub fn set_status(&self, msg: impl AsRef<str>) {
        if let Some(b) = self.round_bar.as_ref() {
            b.set_message(msg.as_ref())
        }
    }

    /// Call this when copy is finished and the CacheManager is asked to drop cache.
    pub fn syncing(&mut self) {
        if let Some(b) = self.bytes_bar.as_ref() {
            b.finish_and_clear()
        }
        self.set_status("Syncing");
    }

    /// Starts a round, given then total number of bytes to copy.
    /// This is the first function to call on a newly created instance.
    pub fn next_round(&mut self, total_size: u64) {
        if self.round_bar.is_none() {
            assert!(
                self.bytes_bar.is_none(),
                "did not call Progress::next_round before bytes"
            );
            let b = ProgressBar::new_spinner();
            b.set_style(ProgressStyle::default_spinner().template("{spinner} Round {pos}. {msg}"));
            self.round_bar = Some(self.multi.add(b));
            // this must be done after the bar is added to the MultiProgress
            if let Some(b) = self.round_bar.as_ref() {
                b.enable_steady_tick(200)
            }
            let multi = self.multi.clone();
            std::thread::spawn(move || multi.join().context("joining progress bar").unwrap());
        }
        self.set_status("");
        if let Some(b) = self.round_bar.as_ref() {
            b.inc(1)
        }
        self.bytes_bar = Some(self.multi.add({
            let b = ProgressBar::new(total_size);
            b.set_style(ProgressStyle::default_bar()
                          .template("[{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes}, {bytes_per_sec} ({eta_precise})")
                          .progress_chars("#>-"));
            b.set_draw_delta(std::cmp::min(1_000_000, total_size/100));
            b
        }));
    }

    /// Notifies that `n` bytes were copied.
    pub fn do_bytes(&self, n: u64) {
        let b = self
            .bytes_bar
            .as_ref()
            .expect("called do_bytes() before next_round()");
        b.inc(n);
    }

    /// Clears the progress bar. Must be called, otherwise the process will not terminate.
    pub fn done(self) {
        if let Some(b) = self.bytes_bar.as_ref() {
            b.finish_and_clear()
        }
        if let Some(b) = self.round_bar.as_ref() {
            b.finish_and_clear()
        }
    }
}
