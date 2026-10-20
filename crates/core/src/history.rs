use std::collections::VecDeque;

/// Fixed-size ring of recent samples for graphs; oldest first.
#[derive(Clone, Debug)]
pub struct History {
    buf: VecDeque<f32>,
    cap: usize,
}

impl History {
    pub fn new(cap: usize) -> Self {
        Self { buf: VecDeque::with_capacity(cap), cap }
    }

    pub fn push(&mut self, v: f32) {
        if self.buf.len() == self.cap {
            self.buf.pop_front();
        }
        self.buf.push_back(v);
    }

    pub fn capacity(&self) -> usize {
        self.cap
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = f32> + '_ {
        self.buf.iter().copied()
    }

    pub fn max(&self) -> f32 {
        self.buf.iter().copied().fold(0.0, f32::max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_oldest_when_full() {
        let mut h = History::new(3);
        for v in [1.0, 2.0, 3.0, 4.0] {
            h.push(v);
        }
        assert_eq!(h.iter().collect::<Vec<_>>(), vec![2.0, 3.0, 4.0]);
        assert_eq!(h.max(), 4.0);
    }
}
