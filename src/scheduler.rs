//! 壁紙ローテーションキュー。
//!
//! Sequential / Random の 2 モードで画像を順に選択し、`prev()` で履歴を遡る。
//! ランダムモードでは全画像を一巡してから再シャッフルし、連続重複を防止する。

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};

use crate::config::Order;

/// 「前の壁紙」で戻れる最大履歴数。
const HISTORY_LIMIT: usize = 50;

/// 壁紙のローテーションキューを管理する。
///
/// 内部状態:
/// - `queue`: まだ表示していない画像のキュー（前端が「次」）
/// - `history`: 表示済み画像のスタック（後端が「直前」）
/// - `current`: 現在表示中の画像
pub struct Scheduler {
    /// ソース画像リスト（shuffle / refill の原本）
    images: Vec<PathBuf>,
    /// `images` の重複チェック用セット（O(1) ルックアップ）
    image_set: HashSet<PathBuf>,
    order: Order,
    /// 未表示の画像キュー。空になったら `refill()` する。
    queue: VecDeque<PathBuf>,
    /// 表示済み履歴（後端が直近）。上限 `HISTORY_LIMIT`。
    history: VecDeque<PathBuf>,
    /// 現在表示中の画像。起動直後は `None`。
    current: Option<PathBuf>,
    /// `true` ならタイマー割り込みでの自動切り替えを停止する。
    paused: bool,
}

impl Scheduler {
    /// 新しいスケジューラを生成する。
    ///
    /// `order` が `Random` の場合はキューを Fisher-Yates シャッフルする。
    pub fn new(images: Vec<PathBuf>, order: Order) -> Self {
        let queue = Self::build_queue(&images, order, None);
        let image_set = images.iter().cloned().collect();
        Self {
            images,
            image_set,
            order,
            queue,
            history: VecDeque::new(),
            current: None,
            paused: false,
        }
    }

    /// 次の壁紙を選択して返す。
    ///
    /// - 現在の画像を履歴に積む（HISTORY_LIMIT を超えたら最古を捨てる）
    /// - キューが空なら refill して再度ポップする
    /// - 画像が 0 枚なら `None` を返す
    pub fn next(&mut self) -> Option<PathBuf> {
        if self.images.is_empty() {
            return None;
        }

        // 現在の画像を履歴に積む
        if let Some(cur) = self.current.take() {
            if self.history.len() >= HISTORY_LIMIT {
                self.history.pop_front();
            }
            self.history.push_back(cur);
        }

        // キューが空なら補充
        if self.queue.is_empty() {
            self.refill();
        }

        self.current = self.queue.pop_front();
        self.current.clone()
    }

    /// 直前の壁紙に戻る。
    ///
    /// 履歴がなければ `None` を返す（最古の壁紙より前には戻れない）。
    /// 現在の画像はキューの先頭に差し戻されるので、`next()` で再び取得できる。
    pub fn prev(&mut self) -> Option<PathBuf> {
        let prev = self.history.pop_back()?;

        // 現在の画像をキューに差し戻す
        if let Some(cur) = self.current.take() {
            self.queue.push_front(cur);
        }

        self.current = Some(prev.clone());
        Some(prev)
    }

    /// 次に表示される画像をチラ見せする（状態を変えない）。
    ///
    /// - キューが空でない場合はその先頭を返す（prefetch で使用）
    /// - キューが空の場合は `None`（refill 前のタイミング）
    pub fn peek_next(&self) -> Option<&PathBuf> {
        self.queue.front()
    }

    /// 現在表示中の画像パスを返す。
    pub fn current(&self) -> Option<&PathBuf> {
        self.current.as_ref()
    }

    /// タイマー自動切り替えを一時停止する。
    pub fn pause(&mut self) {
        self.paused = true;
    }

    /// タイマー自動切り替えを再開する。
    pub fn resume(&mut self) {
        self.paused = false;
    }

    /// 一時停止中かどうかを返す。
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn image_count(&self) -> usize {
        self.images.len()
    }

    /// 画像を動的に追加する（ディレクトリ監視で使用）。
    ///
    /// すでにリストに存在する場合は何もしない。
    /// ランダムモードではキューにも追加する（一巡の途中でも拾われるように）。
    pub fn add_image(&mut self, path: PathBuf) {
        if !self.image_set.insert(path.clone()) {
            return; // すでに存在する
        }
        self.images.push(path.clone());
        // キューの末尾にも追加して、現在の一巡に含める
        self.queue.push_back(path);
    }

    /// 画像を動的に削除する（ディレクトリ監視で使用）。
    ///
    /// リスト・キュー・現在画像から除去する。
    /// 現在表示中の画像が削除された場合は `current` を `None` にし、
    /// 次の `next()` 呼び出しでキューから新しい画像を選択する。
    pub fn remove_image(&mut self, path: &Path) {
        self.images.retain(|p| p != path);
        self.image_set.remove(path);
        self.queue.retain(|p| p != path);
        if self.current.as_deref() == Some(path) {
            self.current = None;
        }
        self.history.retain(|p| p != path);
    }

    // ---- private --------------------------------------------------------

    /// キューを補充する。
    /// - Sequential: ソース順に再追加
    /// - Random: Fisher-Yates でシャッフルして追加（直前の current と先頭が重ならないよう配慮）
    fn refill(&mut self) {
        let avoid_first = self.current.as_ref();
        self.queue = Self::build_queue(&self.images, self.order, avoid_first);
    }

    fn build_queue(
        images: &[PathBuf],
        order: Order,
        avoid_first: Option<&PathBuf>,
    ) -> VecDeque<PathBuf> {
        match order {
            Order::Sequential => images.iter().cloned().collect(),
            Order::Random => {
                let mut v = images.to_vec();
                fisher_yates(&mut v);
                // 直前に表示していた画像が先頭に来てしまったら 1 つずらす
                if v.len() > 1 {
                    if let Some(avoid) = avoid_first {
                        if v.first() == Some(avoid) {
                            v.rotate_left(1);
                        }
                    }
                }
                v.into()
            }
        }
    }
}

/// Fisher-Yates シャッフル（設計書 §5a "全画像一巡"）。
fn fisher_yates<T>(slice: &mut [T]) {
    let mut rng = rand::rng();
    use rand::Rng;
    for i in (1..slice.len()).rev() {
        let j = rng.random_range(0..=i);
        slice.swap(i, j);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(n: usize) -> Vec<PathBuf> {
        (0..n)
            .map(|i| PathBuf::from(format!("/tmp/img{:03}.jpg", i)))
            .collect()
    }

    #[test]
    fn sequential_visits_all_and_wraps() {
        let mut s = Scheduler::new(paths(3), Order::Sequential);
        let p0 = s.next().unwrap();
        let p1 = s.next().unwrap();
        let p2 = s.next().unwrap();
        // wrap
        let p3 = s.next().unwrap();
        assert_eq!(p0, p3, "should cycle back to the first image");
        assert_ne!(p0, p1);
        assert_ne!(p1, p2);
    }

    #[test]
    fn random_one_cycle_visits_each_exactly_once() {
        let n = 10;
        let mut s = Scheduler::new(paths(n), Order::Random);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..n {
            seen.insert(s.next().unwrap());
        }
        assert_eq!(seen.len(), n, "each image should appear exactly once per cycle");
    }

    #[test]
    fn prev_returns_to_previous_image() {
        let mut s = Scheduler::new(paths(5), Order::Sequential);
        let first = s.next().unwrap();
        let second = s.next().unwrap();
        assert_ne!(first, second);

        let back = s.prev().unwrap();
        assert_eq!(back, first, "prev() should return to the first image");
        assert_eq!(s.current(), Some(&first));
    }

    #[test]
    fn prev_then_next_returns_to_same_position() {
        let mut s = Scheduler::new(paths(5), Order::Sequential);
        let _a = s.next().unwrap();
        let b = s.next().unwrap();

        s.prev(); // back to a
        let b_again = s.next().unwrap(); // should see b again
        assert_eq!(b_again, b);
    }

    #[test]
    fn prev_at_start_returns_none() {
        let mut s = Scheduler::new(paths(3), Order::Sequential);
        // no history yet
        assert!(s.prev().is_none());
        s.next();
        // history has 0 entries (first next() calls next() with no current)
        // Actually after first next(), history is empty (current was None → not pushed)
        assert!(s.prev().is_none());
    }

    #[test]
    fn pause_and_resume() {
        let mut s = Scheduler::new(paths(3), Order::Sequential);
        assert!(!s.is_paused());
        s.pause();
        assert!(s.is_paused());
        s.resume();
        assert!(!s.is_paused());
    }

    #[test]
    fn peek_next_does_not_advance() {
        let mut s = Scheduler::new(paths(3), Order::Sequential);
        s.next(); // warm up
        let peeked = s.peek_next().cloned();
        let actual = s.next();
        assert_eq!(peeked, actual, "peek_next should not advance the queue");
    }

    #[test]
    fn empty_scheduler_returns_none() {
        let mut s = Scheduler::new(vec![], Order::Random);
        assert!(s.next().is_none());
        assert!(s.prev().is_none());
        assert!(s.peek_next().is_none());
    }
}
