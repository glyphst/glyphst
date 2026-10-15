//! How long a document is read: a span runs while its tab is active, the
//! window has the focus and input keeps coming, and ends after
//! [`IDLE`] without input, when the document changes or the window loses
//! the focus. Finished spans go to the activity log, which home sums into
//! `12h read this week` and `38 min today`.

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Without input this long, reading has stopped.
pub const IDLE: Duration = Duration::from_secs(120);
/// Spans shorter than this are not worth a line in the log.
pub const SHORTEST: Duration = Duration::from_secs(10);

#[derive(Debug, Default)]
pub struct ReadingClock {
    span: Option<Span>,
}

#[derive(Debug)]
struct Span {
    path: PathBuf,
    started: Instant,
    last_input: Instant,
}

impl ReadingClock {
    /// Notes a frame at `now` with `document` shown (`None` on home),
    /// whether there was `input` and whether the window is `focused`.
    /// Returns a span that ended: its document and seconds read.
    pub fn tick(
        &mut self,
        now: Instant,
        document: Option<&Path>,
        input: bool,
        focused: bool,
    ) -> Option<(PathBuf, u64)> {
        let reading = document.filter(|_| focused);
        let ends = match (&self.span, reading) {
            (None, _) => false,
            (Some(span), Some(path)) => {
                span.path != path || now.saturating_duration_since(span.last_input) >= IDLE
            }
            (Some(_), None) => true,
        };
        let ended = if ends { self.finish() } else { None };
        if let Some(path) = reading {
            match &mut self.span {
                Some(span) if input => span.last_input = now,
                Some(_) => {}
                None if input => {
                    self.span = Some(Span {
                        path: path.to_path_buf(),
                        started: now,
                        last_input: now,
                    });
                }
                None => {}
            }
        }
        ended
    }

    /// Ends the span running, as when the app closes.
    pub fn finish(&mut self) -> Option<(PathBuf, u64)> {
        let span = self.span.take()?;
        // Reading ends with the last input, not with the idle wait.
        let read = span.last_input.saturating_duration_since(span.started);
        (read >= SHORTEST).then_some((span.path, read.as_secs()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_run_with_input_and_end_at_the_last_of_it() {
        let mut clock = ReadingClock::default();
        let start = Instant::now();
        let paper = Path::new("/a.pdf");
        let at = |seconds: u64| start + Duration::from_secs(seconds);
        assert_eq!(clock.tick(at(0), Some(paper), true, true), None);
        assert_eq!(clock.tick(at(60), Some(paper), true, true), None);
        assert_eq!(clock.tick(at(100), Some(paper), false, true), None);
        // Idle for two minutes after the last input: 60 seconds were read.
        assert_eq!(
            clock.tick(at(60) + IDLE, Some(paper), false, true),
            Some((paper.to_path_buf(), 60))
        );
        assert_eq!(clock.tick(at(400), Some(paper), false, true), None);
    }

    #[test]
    fn switching_documents_or_losing_focus_ends_a_span() {
        let mut clock = ReadingClock::default();
        let start = Instant::now();
        let at = |seconds: u64| start + Duration::from_secs(seconds);
        let (a, b) = (Path::new("/a.pdf"), Path::new("/b.pdf"));
        clock.tick(at(0), Some(a), true, true);
        clock.tick(at(30), Some(a), true, true);
        assert_eq!(
            clock.tick(at(31), Some(b), true, true),
            Some((a.to_path_buf(), 30))
        );
        clock.tick(at(50), Some(b), true, true);
        assert_eq!(
            clock.tick(at(51), Some(b), false, false),
            Some((b.to_path_buf(), 19))
        );
        // Too short to log.
        clock.tick(at(60), Some(a), true, true);
        assert_eq!(clock.tick(at(62), None, false, true), None);
    }
}
