#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReloadDisposition {
    Apply,
    DiscardAndStart(u64),
    Discard,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ReloadState {
    #[default]
    Idle,
    Initial,
    Reloading(u64),
}

#[derive(Debug, Default)]
pub struct ReloadCoordinator {
    requested_generation: u64,
    state: ReloadState,
}

impl ReloadCoordinator {
    pub fn begin_initial_load(&mut self) {
        debug_assert_eq!(self.state, ReloadState::Idle);
        self.state = ReloadState::Initial;
    }

    pub fn request(&mut self) -> Option<u64> {
        self.requested_generation += 1;

        if self.state == ReloadState::Idle {
            let generation = self.requested_generation;
            self.state = ReloadState::Reloading(generation);
            return Some(generation);
        }

        None
    }

    pub fn finish_initial_load(&mut self) -> Option<u64> {
        debug_assert_eq!(self.state, ReloadState::Initial);
        self.start_latest_request()
    }

    pub fn finish_reload(&mut self, generation: u64) -> ReloadDisposition {
        if self.state != ReloadState::Reloading(generation) {
            return ReloadDisposition::Discard;
        }

        if generation < self.requested_generation {
            let next_generation = self.requested_generation;
            self.state = ReloadState::Reloading(next_generation);
            ReloadDisposition::DiscardAndStart(next_generation)
        } else {
            self.state = ReloadState::Idle;
            ReloadDisposition::Apply
        }
    }

    pub fn is_busy(&self) -> bool {
        self.state != ReloadState::Idle
    }

    fn start_latest_request(&mut self) -> Option<u64> {
        self.state = ReloadState::Idle;
        (self.requested_generation > 0).then(|| {
            let generation = self.requested_generation;
            self.state = ReloadState::Reloading(generation);
            generation
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queues_reload_requested_during_initial_loading() {
        let mut coordinator = ReloadCoordinator::default();
        coordinator.begin_initial_load();

        assert_eq!(coordinator.request(), None);
        assert_eq!(coordinator.finish_initial_load(), Some(1));
        assert_eq!(coordinator.finish_reload(1), ReloadDisposition::Apply);
    }

    #[test]
    fn discards_stale_reload_and_starts_the_newest_request() {
        let mut coordinator = ReloadCoordinator::default();

        assert_eq!(coordinator.request(), Some(1));
        assert_eq!(coordinator.request(), None);
        assert_eq!(coordinator.request(), None);
        assert_eq!(
            coordinator.finish_reload(1),
            ReloadDisposition::DiscardAndStart(3)
        );
        assert_eq!(coordinator.finish_reload(3), ReloadDisposition::Apply);
    }

    #[test]
    fn ignores_duplicate_or_out_of_order_completions() {
        let mut coordinator = ReloadCoordinator::default();

        assert_eq!(coordinator.request(), Some(1));
        assert_eq!(coordinator.finish_reload(2), ReloadDisposition::Discard);
        assert_eq!(coordinator.finish_reload(1), ReloadDisposition::Apply);
        assert_eq!(coordinator.finish_reload(1), ReloadDisposition::Discard);
    }
}
