//! How long a document is read: a span runs while its tab is active, the
//! window has the focus and input keeps coming, and ends after
//! [`IDLE`] without input, when the document changes or the window loses
//! the focus. Finished spans go to the activity log, which home sums into
//! `12h read this week` and `38 min today`, and to the library's reading
//! sessions, with the pages they went over, for the heatmaps.

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
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
    /// When it started, in seconds since the epoch.
    started_at: u64,
    last_input: Instant,
    /// The first and last page shown (from 1), when the view has pages.
    pages: Option<(usize, usize)>,
}

/// A span that ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Read {
    pub path: PathBuf,
    /// When it started, in seconds since the epoch.
    pub start: u64,
    pub seconds: u64,
    pub pages: Option<(usize, usize)>,
}

impl ReadingClock {
    /// Notes a frame at `now` with `document` shown (`None` on home) at
    /// `page` (from 1), whether there was `input` and whether the window is
    /// `focused`. Returns a span that ended.
    pub fn tick(
        &mut self,
        now: Instant,
        document: Option<&Path>,
        page: Option<usize>,
        input: bool,
        focused: bool,
    ) -> Option<Read> {
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
                Some(span) if input => {
                    span.last_input = now;
                    if let (Some((_, last)), Some(page)) = (&mut span.pages, page) {
                        *last = page;
                    }
                }
                Some(_) => {}
                None if input => {
                    self.span = Some(Span {
                        path: path.to_path_buf(),
                        started: now,
                        started_at: SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map_or(0, |since| since.as_secs()),
                        last_input: now,
                        pages: page.map(|page| (page, page)),
                    });
                }
                None => {}
            }
        }
        ended
    }

    /// Ends the span running, as when the app closes.
    pub fn finish(&mut self) -> Option<Read> {
        let span = self.span.take()?;
        // Reading ends with the last input, not with the idle wait.
        let read = span.last_input.saturating_duration_since(span.started);
        (read >= SHORTEST).then_some(Read {
            path: span.path,
            start: span.started_at,
            seconds: read.as_secs(),
            pages: span.pages,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl ReadingClock {
        /// [`Self::tick`] without pages, as the document and seconds read.
        fn tick_at(
            &mut self,
            now: Instant,
            document: Option<&Path>,
            input: bool,
            focused: bool,
        ) -> Option<(PathBuf, u64)> {
            self.tick(now, document, None, input, focused)
                .map(|read| (read.path, read.seconds))
        }
    }

    #[test]
    fn a_span_records_the_pages_it_went_over() {
        let mut clock = ReadingClock::default();
        let start = Instant::now();
        let at = |seconds: u64| start + Duration::from_secs(seconds);
        let paper = Path::new("/a.pdf");
        clock.tick(at(0), Some(paper), Some(4), true, true);
        clock.tick(at(30), Some(paper), Some(5), true, true);
        // Scrolled on without input (a page turning by itself): not read.
        clock.tick(at(40), Some(paper), Some(9), false, true);
        let read = clock.tick(at(41), None, None, false, true).unwrap();
        assert_eq!((read.seconds, read.pages), (30, Some((4, 5))));
    }

    #[test]
    fn spans_run_with_input_and_end_at_the_last_of_it() {
        let mut clock = ReadingClock::default();
        let start = Instant::now();
        let paper = Path::new("/a.pdf");
        let at = |seconds: u64| start + Duration::from_secs(seconds);
        assert_eq!(clock.tick_at(at(0), Some(paper), true, true), None);
        assert_eq!(clock.tick_at(at(60), Some(paper), true, true), None);
        assert_eq!(clock.tick_at(at(100), Some(paper), false, true), None);
        // Idle for two minutes after the last input: 60 seconds were read.
        assert_eq!(
            clock.tick_at(at(60) + IDLE, Some(paper), false, true),
            Some((paper.to_path_buf(), 60))
        );
        assert_eq!(clock.tick_at(at(400), Some(paper), false, true), None);
    }

    #[test]
    fn switching_documents_or_losing_focus_ends_a_span() {
        let mut clock = ReadingClock::default();
        let start = Instant::now();
        let at = |seconds: u64| start + Duration::from_secs(seconds);
        let (a, b) = (Path::new("/a.pdf"), Path::new("/b.pdf"));
        clock.tick_at(at(0), Some(a), true, true);
        clock.tick_at(at(30), Some(a), true, true);
        assert_eq!(
            clock.tick_at(at(31), Some(b), true, true),
            Some((a.to_path_buf(), 30))
        );
        clock.tick_at(at(50), Some(b), true, true);
        assert_eq!(
            clock.tick_at(at(51), Some(b), false, false),
            Some((b.to_path_buf(), 19))
        );
        // Too short to log.
        clock.tick_at(at(60), Some(a), true, true);
        assert_eq!(clock.tick_at(at(62), None, false, true), None);
    }
}
