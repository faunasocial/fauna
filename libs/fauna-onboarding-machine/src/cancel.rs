use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Default)]
pub struct CancelFlag {
    inner: Arc<AtomicBool>,
}

impl CancelFlag {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn raise(&self) {
        self.inner.store(true, Ordering::SeqCst);
    }
    pub fn reset(&self) {
        self.inner.store(false, Ordering::SeqCst);
    }
    pub fn is_raised(&self) -> bool {
        self.inner.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_flag_is_not_raised() {
        let f = CancelFlag::new();
        assert!(!f.is_raised());
    }
    #[test]
    fn raise_makes_is_raised_true() {
        let f = CancelFlag::new();
        f.raise();
        assert!(f.is_raised());
    }
    #[test]
    fn reset_clears_raised() {
        let f = CancelFlag::new();
        f.raise();
        f.reset();
        assert!(!f.is_raised());
    }
    #[test]
    fn clones_share_state() {
        let f = CancelFlag::new();
        let f2 = f.clone();
        f.raise();
        assert!(f2.is_raised());
    }
}
