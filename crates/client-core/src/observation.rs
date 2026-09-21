/// A presentation cursor; neither this sequence nor a cached view is business authority.
#[derive(Clone, Debug, Default)]
pub struct ObservationCursor {
    generation: u64,
    sequence: Option<u64>,
    stopped: bool,
}
impl ObservationCursor {
    pub fn restart(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.sequence = None;
        self.stopped = false;
        self.generation
    }
    pub fn stop(&mut self) {
        self.stopped = true;
    }
    pub fn accept(&mut self, generation: u64, sequence: u64) -> bool {
        if self.stopped
            || generation != self.generation
            || self.sequence.is_some_and(|last| sequence < last)
        {
            return false;
        }
        self.sequence = Some(sequence);
        true
    }
    pub fn after_sequence(&self) -> u64 {
        self.sequence.unwrap_or_default()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_generation_and_stopped_observers_cannot_replace_fresh_state() {
        let mut cursor = ObservationCursor::default();
        let old = cursor.restart();
        assert!(cursor.accept(old, 4));
        assert!(!cursor.accept(old, 2));
        cursor.stop();
        assert!(!cursor.accept(old, 5));
        let new = cursor.restart();
        assert!(!cursor.accept(old, 99));
        assert!(cursor.accept(new, 1));
    }
}
