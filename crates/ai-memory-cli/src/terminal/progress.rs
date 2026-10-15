//! One in-place indicator, scoped to a real future and cleared on cancellation.

use super::{Terminal, Tone};
use std::future::Future;
use std::io::{Write as _, stdout};
use std::time::Duration;

struct Line;
impl Drop for Line {
    fn drop(&mut self) {
        print!("\r\x1b[2K");
        let _ = stdout().flush();
    }
}

impl Terminal {
    pub(crate) async fn waiting<T>(&self, label: &str, future: impl Future<Output = T>) -> T {
        if !self.motion {
            return future.await;
        }
        let _line = Line;
        let mut tick = tokio::time::interval(Duration::from_millis(140));
        let frames = ["● · ·", "· ● ·", "· · ●", "· ● ·"];
        let mut index = 0;
        let label: String = label.chars().take(self.width.saturating_sub(9)).collect();
        tokio::pin!(future);
        loop {
            tokio::select! {
                biased;
                result = &mut future => return result,
                _ = tick.tick() => {
                    print!("\r\x1b[2K  {} {}", self.ink(frames[index % frames.len()], Tone::Accent), label);
                    let _ = stdout().flush(); index += 1;
                }
            }
        }
    }
}
