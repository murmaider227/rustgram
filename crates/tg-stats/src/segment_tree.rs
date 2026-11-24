//! Range minimum/maximum query segment tree for chart Y-series.
//!
//! Port of `Telegram/SourceFiles/statistics/segment_tree.{h,cpp}`.

/// Integer chart Y-axis sample value (`Statistic::ChartValue`).
pub type ChartValue = i64;

/// Minimum array length at which the segment tree heap is built; smaller arrays
/// are scanned linearly, matching `kMinArraySize` in `segment_tree.cpp`.
const MIN_ARRAY_SIZE: usize = 30;

#[derive(Debug, Clone, Copy, Default)]
struct Node {
    sum: ChartValue,
    max: ChartValue,
    min: ChartValue,
    from: i32,
    to: i32,
}

/// Fast range minimum and maximum query structure for chart Y values.
///
/// Mirrors `Statistic::SegmentTree`: uses direct linear scans for arrays with
/// fewer than 30 elements and a 1-indexed binary segment tree otherwise.
#[derive(Debug, Clone, Default)]
pub struct SegmentTree {
    array: Vec<ChartValue>,
    heap: Vec<Node>,
}

impl SegmentTree {
    /// Builds a segment tree from `array`.
    #[must_use]
    pub fn new(array: Vec<ChartValue>) -> Self {
        let len = array.len();
        if len < MIN_ARRAY_SIZE {
            return Self {
                array,
                heap: Vec::new(),
            };
        }

        // Matches C++: 2 * 2^floor(log2(n) + 1)
        let exp = (len as f64).log2().floor() as u32 + 1;
        let heap_size = 2usize.checked_shl(exp).unwrap_or(len.saturating_mul(4) + 2);
        let mut tree = Self {
            array,
            heap: vec![Node::default(); heap_size],
        };
        tree.build(1, 0, len as i32);
        tree
    }

    /// Returns `true` if the underlying array is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.array.is_empty()
    }

    /// Returns the number of stored elements.
    #[must_use]
    pub fn len(&self) -> usize {
        self.array.len()
    }

    fn build(&mut self, v: usize, from: i32, size: i32) {
        self.heap[v].from = from;
        self.heap[v].to = from + size - 1;

        if size == 1 {
            let val = self.array[from as usize];
            self.heap[v].sum = val;
            self.heap[v].max = val;
            self.heap[v].min = val;
        } else {
            let half = size / 2;
            self.build(2 * v, from, half);
            self.build(2 * v + 1, from + half, size - half);

            let left = self.heap[2 * v];
            let right = self.heap[2 * v + 1];
            self.heap[v].sum = left.sum.saturating_add(right.sum);
            self.heap[v].max = left.max.max(right.max);
            self.heap[v].min = left.min.min(right.min);
        }
    }

    /// Range maximum query over the inclusive index range `[from, to]`.
    ///
    /// Returns `0` if the range does not overlap any elements (or if all values are `< 0`
    /// in the linear-scan fast path), matching `SegmentTree::rMaxQ`.
    #[must_use]
    pub fn r_max_q(&self, from: i32, to: i32) -> ChartValue {
        if self.array.is_empty() {
            return 0;
        }
        if self.array.len() < MIN_ARRAY_SIZE {
            let mut max = 0;
            let from = from.max(0);
            let to = to.min((self.array.len() - 1) as i32);
            if from <= to {
                for i in from..=to {
                    max = max.max(self.array[i as usize]);
                }
            }
            return max;
        }
        self.r_max_q_node(1, from, to)
    }

    fn r_max_q_node(&self, v: usize, from: i32, to: i32) -> ChartValue {
        let n = self.heap[v];
        if Self::contains(from, to, n.from, n.to) {
            return n.max;
        }
        if Self::intersects(from, to, n.from, n.to) {
            let left_max = self.r_max_q_node(2 * v, from, to);
            let right_max = self.r_max_q_node(2 * v + 1, from, to);
            return left_max.max(right_max);
        }
        0
    }

    /// Range minimum query over the inclusive index range `[from, to]`.
    ///
    /// Returns `ChartValue::MAX` if the range does not overlap any elements,
    /// matching `SegmentTree::rMinQ`.
    #[must_use]
    pub fn r_min_q(&self, from: i32, to: i32) -> ChartValue {
        if self.array.is_empty() {
            return ChartValue::MAX;
        }
        if self.array.len() < MIN_ARRAY_SIZE {
            let mut min = ChartValue::MAX;
            let from = from.max(0);
            let to = to.min((self.array.len() - 1) as i32);
            if from <= to {
                for i in from..=to {
                    min = min.min(self.array[i as usize]);
                }
            }
            return min;
        }
        self.r_min_q_node(1, from, to)
    }

    fn r_min_q_node(&self, v: usize, from: i32, to: i32) -> ChartValue {
        let n = self.heap[v];
        if Self::contains(from, to, n.from, n.to) {
            return n.min;
        }
        if Self::intersects(from, to, n.from, n.to) {
            let left_min = self.r_min_q_node(2 * v, from, to);
            let right_min = self.r_min_q_node(2 * v + 1, from, to);
            return left_min.min(right_min);
        }
        ChartValue::MAX
    }

    #[inline]
    fn contains(from1: i32, to1: i32, from2: i32, to2: i32) -> bool {
        (from2 >= from1) && (to2 <= to1)
    }

    #[inline]
    fn intersects(from1: i32, to1: i32, from2: i32, to2: i32) -> bool {
        ((from1 <= from2) && (to1 >= from2)) || ((from1 >= from2) && (from1 <= to2))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_fast_path_below_30() {
        let data = vec![10, 5, 25, 3, 18, 42, 7];
        let tree = SegmentTree::new(data);
        assert_eq!(tree.r_max_q(0, 6), 42);
        assert_eq!(tree.r_min_q(0, 6), 3);
        assert_eq!(tree.r_max_q(0, 2), 25);
        assert_eq!(tree.r_min_q(0, 2), 5);
        assert_eq!(tree.r_max_q(3, 4), 18);
        assert_eq!(tree.r_min_q(4, 6), 7);
    }

    #[test]
    fn tree_path_matches_brute_force_for_all_ranges() {
        let data: Vec<ChartValue> = (0..64)
            .map(|i| ((i * 37 + 13) % 97) as ChartValue)
            .collect();
        let tree = SegmentTree::new(data.clone());

        for from in 0..data.len() {
            for to in from..data.len() {
                let expected_max = *data[from..=to].iter().max().unwrap_or(&0);
                let expected_min = *data[from..=to].iter().min().unwrap_or(&ChartValue::MAX);
                assert_eq!(tree.r_max_q(from as i32, to as i32), expected_max);
                assert_eq!(tree.r_min_q(from as i32, to as i32), expected_min);
            }
        }
    }

    #[test]
    fn empty_and_out_of_bounds_queries() {
        let empty = SegmentTree::new(Vec::new());
        assert!(empty.is_empty());
        assert_eq!(empty.r_max_q(0, 5), 0);
        assert_eq!(empty.r_min_q(0, 5), ChartValue::MAX);

        let data: Vec<ChartValue> = (1..=40).collect();
        let tree = SegmentTree::new(data);
        assert_eq!(tree.r_max_q(50, 60), 0);
        assert_eq!(tree.r_min_q(50, 60), ChartValue::MAX);
        assert_eq!(tree.r_max_q(-10, 5), 6);
        assert_eq!(tree.r_min_q(35, 100), 36);
    }
}
