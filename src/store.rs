//! 複数オブジェクトの履歴をまとめて持ち、メモリの上限と使われなくなった履歴の破棄を受け持つ。
//!
//! - **上限**: 全オブジェクトの履歴の合計を `DEFAULT_BUDGET_BYTES` までにする。超えたら、
//!   いちばん多く持っているオブジェクトの**いちばん古いフレーム**から捨てる。
//!   古い残像ほど減衰（減衰^i）で薄いので、見た目への影響がいちばん小さい。
//!   間引き（1 枚おきに捨てる）は残像がフレームごとに出たり消えたりして目立つので採らない。
//! - **破棄**: `STALE_CALLS` 回のあいだ呼ばれず、かつ `STALE_AFTER` 以上たった履歴を捨てる。
//!   回数だけだと、オブジェクトが多いときに表示中の履歴まで捨てうる。時間だけだと、
//!   編集の手を止めていた（何も描いていない）後の再生で表示中の履歴を捨てる。呼ばれたときは両方を条件にする。
//! - **見回り**（v1.3.1。`janitor.rs`）: 呼ばれたときにしか見ない作りでは、オブジェクトを全部消すと履歴が本体を閉じるまで残る。
//!   見回りのスレッドが、呼ばれた回数によらず `STALE_AFTER` 呼ばれていない履歴を捨てる（`purge_idle`）。止めて `STALE_AFTER` 以上たってから
//!   値を変えると、その 1 枚は残像の無い絵になる（再生し直せば戻る）。TimeDisplace_H v1.0.4 と同じ決め方（ユーザーの決定、2026-10-14）

use crate::trail::{self, ProcessParams, TrailState};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// 全オブジェクトの履歴の合計の上限（2 GiB）。
/// 1080p（1 枚 約 8.3 MB）なら 258 枚。既定（数 12・間隔 1）は 1 オブジェクト 15 枚（約 124 MB）なので届かない。
pub const DEFAULT_BUDGET_BYTES: usize = 2 << 30;

/// この回数のあいだ（他のオブジェクトの分も数える）呼ばれなかった履歴は捨てる候補。
pub const STALE_CALLS: u64 = 300;

/// 最後に呼ばれてからこれだけたった履歴は捨てる候補。
pub const STALE_AFTER: Duration = Duration::from_secs(10);

struct Slot {
    state: TrailState,
    last_tick: u64,
    last_used: Instant,
}

pub struct StateStore {
    slots: HashMap<String, Slot>,
    /// `process` が呼ばれた回数。
    tick: u64,
    budget: usize,
    stale_calls: u64,
    stale_after: Duration,
}

impl Default for StateStore {
    fn default() -> Self {
        Self::new()
    }
}

impl StateStore {
    pub fn new() -> Self {
        Self::with_limits(DEFAULT_BUDGET_BYTES, STALE_CALLS, STALE_AFTER)
    }

    pub fn with_limits(budget: usize, stale_calls: u64, stale_after: Duration) -> Self {
        Self {
            slots: HashMap::new(),
            tick: 0,
            budget,
            stale_calls,
            stale_after,
        }
    }

    /// `id` の履歴を空にする（無ければ作る）。
    pub fn reset(&mut self, id: &str, now: Instant) {
        let tick = self.tick;
        let slot = self.slot_mut(id, now);
        slot.state.reset();
        slot.last_tick = tick;
    }

    /// 今のフレームを `id` の履歴に積み、上限を守ってから、残像を合成して `pixels` に書き戻す。
    /// 1 画素でも書き換えたら true（`trail::render`）。
    pub fn process(
        &mut self,
        id: &str,
        pixels: &mut [u8],
        w: u32,
        h: u32,
        p: &ProcessParams,
        now: Instant,
    ) -> bool {
        self.tick = self.tick.wrapping_add(1);
        let tick = self.tick;
        self.purge_stale(id, now);

        // 1 枚で上限を超えるなら履歴を持たない（残像なしで今のフレームだけ出す）
        let can_store = trail::frame_bytes(w, h).is_some_and(|n| n <= self.budget);
        {
            let slot = self.slot_mut(id, now);
            slot.last_tick = tick;
            trail::ingest(&mut slot.state, pixels, w, h, p, can_store);
        }
        self.enforce_budget(id);
        match self.slots.get(id) {
            Some(slot) => trail::render(&slot.state, pixels, w, h, p),
            None => false,
        }
    }

    fn slot_mut(&mut self, id: &str, now: Instant) -> &mut Slot {
        let tick = self.tick;
        let slot = self.slots.entry(id.to_string()).or_insert_with(|| Slot {
            state: TrailState::new(),
            last_tick: tick,
            last_used: now,
        });
        slot.last_used = now;
        slot
    }

    /// 長く呼ばれていない履歴を捨てる（`keep_id` は今使うので残す）。
    fn purge_stale(&mut self, keep_id: &str, now: Instant) {
        let tick = self.tick;
        let stale_calls = self.stale_calls;
        let stale_after = self.stale_after;
        self.slots.retain(|id, s| {
            if id == keep_id {
                return true;
            }
            let idle_calls = tick.wrapping_sub(s.last_tick);
            // duration_since は now が前でも 0 を返す（panic しない）
            let idle_time = now.duration_since(s.last_used);
            !(idle_calls > stale_calls && idle_time > stale_after)
        });
    }

    /// 見回り（`janitor.rs`）から呼ぶ: `stale_after` 以上呼ばれていない履歴を、呼ばれた回数によらず捨てる。
    /// 戻り値は (捨てた履歴の数, 捨てたバイト数)。
    pub fn purge_idle(&mut self, now: Instant) -> (usize, usize) {
        let after = self.stale_after;
        let (mut count, mut bytes) = (0, 0);
        self.slots.retain(|_, s| {
            let keep = now.saturating_duration_since(s.last_used) <= after;
            if !keep {
                count += 1;
                bytes += s.state.bytes();
            }
            keep
        });
        (count, bytes)
    }

    /// 合計が上限を超えていれば、いちばん多く持つ履歴の古いフレームから捨てる。
    /// 今のオブジェクト（`current`）の最新フレームは捨てない。
    fn enforce_budget(&mut self, current: &str) {
        let mut total = self.total_bytes();
        while total > self.budget {
            let victim = self
                .slots
                .iter()
                .filter(|(id, s)| {
                    let min_frames = if id.as_str() == current { 1 } else { 0 };
                    s.state.frame_count() > min_frames
                })
                .max_by(|(ida, a), (idb, b)| {
                    a.state
                        .bytes()
                        .cmp(&b.state.bytes())
                        // 同じなら今のオブジェクト以外を先に削る
                        .then_with(|| (idb.as_str() == current).cmp(&(ida.as_str() == current)))
                })
                .map(|(id, _)| id.clone());
            let Some(victim) = victim else {
                break;
            };
            let freed = self
                .slots
                .get_mut(&victim)
                .map_or(0, |s| s.state.drop_oldest());
            if freed == 0 {
                break;
            }
            total -= freed;
        }
    }

    pub fn total_bytes(&self) -> usize {
        self.slots.values().map(|s| s.state.bytes()).sum()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.slots.len()
    }

    #[cfg(test)]
    fn contains(&self, id: &str) -> bool {
        self.slots.contains_key(id)
    }

    #[cfg(test)]
    fn state(&self, id: &str) -> Option<&TrailState> {
        self.slots.get(id).map(|s| &s.state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trail::{frame_bytes, keep_window, test_params};

    fn px(v: u8) -> Vec<u8> {
        vec![v, 0, 0, 255]
    }

    #[test]
    fn default_settings_at_1080p_fit_in_budget() {
        // 既定（数 12・間隔 1）の 1080p は上限に届かない = 見た目は変わらない
        let frames = keep_window(1.0, 12) as usize + 1;
        let bytes = frames * frame_bytes(1920, 1080).unwrap();
        assert_eq!(frames, 15);
        assert!(bytes * 8 < DEFAULT_BUDGET_BYTES, "{bytes}");
        // 数 120・間隔 1 も 1 オブジェクトなら収まる
        let frames_120 = keep_window(1.0, 120) as usize + 1;
        assert!(frames_120 * frame_bytes(1920, 1080).unwrap() <= DEFAULT_BUDGET_BYTES);
        // 最大（数 120・間隔 50）は窓が 6002 枚で、上限で止まる
        assert_eq!(keep_window(50.0, 120), 6002);
    }

    #[test]
    fn budget_drops_oldest_frames_first() {
        // 1 枚 4 バイト、上限 10 枚
        let mut st = StateStore::with_limits(40, STALE_CALLS, STALE_AFTER);
        let now = Instant::now();
        for f in 0..100 {
            let mut cur = px(f as u8);
            st.process("a", &mut cur, 1, 1, &test_params(f, 120, 1.0), now);
            assert!(st.total_bytes() <= 40);
        }
        let s = st.state("a").unwrap();
        assert_eq!(s.frame_count(), 10);
        // 新しい方の 10 枚が残る（古い方から捨てた）
        assert_eq!(s.oldest_frame(), Some(90));
        assert_eq!(s.newest_frame(), Some(99));
    }

    #[test]
    fn budget_keeps_drawing_recent_echoes() {
        let mut st = StateStore::with_limits(40, STALE_CALLS, STALE_AFTER);
        let now = Instant::now();
        // 2x1: 物体が 1 フレームごとに左右を行き来する
        let mut last = Vec::new();
        for f in 0..50 {
            let mut cur = if f % 2 == 0 {
                vec![255u8, 255, 255, 255, 0, 0, 0, 0]
            } else {
                vec![0u8, 0, 0, 0, 255, 255, 255, 255]
            };
            st.process("a", &mut cur, 2, 1, &test_params(f, 120, 1.0), now);
            last = cur;
        }
        // f=49 は右に物体。左に 1 フレーム前の残像（上限で古い分が欠けても新しい残像は出る）
        assert!(last[3] > 0);
        assert_eq!(last[7], 255);
    }

    /// 誰も呼ばなくなっても（オブジェクトを全部消した）、見回りが時間だけで捨てる
    #[test]
    fn idle_state_is_purged_without_calls() {
        let mut st = StateStore::with_limits(DEFAULT_BUDGET_BYTES, STALE_CALLS, Duration::from_secs(10));
        let t0 = Instant::now();
        for (i, id) in ["a", "b", "c"].iter().enumerate() {
            for f in 0..4 {
                let mut cur = vec![9u8, 0, 0, 255, 0, 0, 0, 0];
                st.process(id, &mut cur, 2, 1, &test_params(f, 12, 1.0), t0 + Duration::from_secs(i as u64));
            }
        }
        let before = st.total_bytes();
        assert!(before > 0);
        // 最後に呼ばれてから 10 秒以内は残す（"b" は t0 + 1 秒、"c" は t0 + 2 秒）
        assert_eq!(st.purge_idle(t0 + Duration::from_secs(11)), (1, before / 3));
        assert!(!st.contains("a") && st.contains("b") && st.contains("c"));
        let (n, freed) = st.purge_idle(t0 + Duration::from_secs(13));
        assert_eq!((n, freed), (2, before - before / 3));
        assert_eq!((st.total_bytes(), st.len()), (0, 0));
        assert_eq!(st.purge_idle(t0 + Duration::from_secs(60)), (0, 0));
    }

    #[test]
    fn frame_larger_than_budget_is_not_stored() {
        let mut st = StateStore::with_limits(4, STALE_CALLS, STALE_AFTER);
        let now = Instant::now();
        let mut cur = vec![9u8, 0, 0, 255, 9, 0, 0, 255];
        st.process("a", &mut cur, 2, 1, &test_params(0, 12, 1.0), now);
        assert_eq!(st.total_bytes(), 0);
        assert_eq!(cur, vec![9u8, 0, 0, 255, 9, 0, 0, 255]);
    }

    #[test]
    fn budget_is_shared_between_objects() {
        // 上限 20 枚。2 つとも窓が大きい → それぞれ 10 枚前後で釣り合う
        let mut st = StateStore::with_limits(80, STALE_CALLS, STALE_AFTER);
        let now = Instant::now();
        for f in 0..200 {
            for id in ["a", "b"] {
                let mut cur = px(1);
                st.process(id, &mut cur, 1, 1, &test_params(f, 120, 1.0), now);
                assert!(st.total_bytes() <= 80);
            }
        }
        let a = st.state("a").unwrap().frame_count();
        let b = st.state("b").unwrap().frame_count();
        assert_eq!(a + b, 20);
        assert!(a.abs_diff(b) <= 1, "a={a} b={b}");
    }

    #[test]
    fn small_object_is_not_trimmed_by_big_one() {
        // 小さい窓（既定相当）のオブジェクトは、大きい窓のオブジェクトに削られない
        let mut st = StateStore::with_limits(4 * 40, STALE_CALLS, STALE_AFTER);
        let now = Instant::now();
        for f in 0..200 {
            let mut a = px(1);
            st.process("small", &mut a, 1, 1, &test_params(f, 12, 1.0), now);
            let mut b = px(1);
            st.process("big", &mut b, 1, 1, &test_params(f, 120, 1.0), now);
        }
        assert_eq!(st.state("small").unwrap().frame_count(), 15);
        assert_eq!(st.state("big").unwrap().frame_count(), 25);
    }

    #[test]
    fn stale_state_is_purged_after_calls_and_time() {
        let mut st = StateStore::with_limits(DEFAULT_BUDGET_BYTES, 10, Duration::from_secs(5));
        let t0 = Instant::now();
        let mut cur = px(1);
        st.process("gone", &mut cur, 1, 1, &test_params(0, 12, 1.0), t0);
        // 呼ばれた回数は超えたが、時間はたっていない → 残す
        for f in 0..20 {
            let mut c = px(1);
            st.process("alive", &mut c, 1, 1, &test_params(f, 12, 1.0), t0 + Duration::from_secs(1));
        }
        assert!(st.contains("gone"));
        // 回数も時間も超えた → 捨てる
        let mut c = px(1);
        st.process("alive", &mut c, 1, 1, &test_params(20, 12, 1.0), t0 + Duration::from_secs(6));
        assert!(!st.contains("gone"));
        assert!(st.contains("alive"));
        assert_eq!(st.len(), 1);
    }

    #[test]
    fn idle_time_alone_does_not_purge() {
        // 編集の手を止めていた（何も描いていない）後の再生で、表示中の履歴を捨てない
        let mut st = StateStore::with_limits(DEFAULT_BUDGET_BYTES, 10, Duration::from_secs(5));
        let t0 = Instant::now();
        for f in 0..5 {
            for id in ["a", "b"] {
                let mut c = px(1);
                st.process(id, &mut c, 1, 1, &test_params(f, 12, 1.0), t0);
            }
        }
        let later = t0 + Duration::from_secs(600);
        let mut c = px(1);
        st.process("a", &mut c, 1, 1, &test_params(5, 12, 1.0), later);
        assert!(st.contains("b"));
        assert_eq!(st.state("b").unwrap().frame_count(), 5);
    }

    #[test]
    fn reset_clears_one_state() {
        let mut st = StateStore::new();
        let now = Instant::now();
        for f in 0..5 {
            for id in ["a", "b"] {
                let mut c = px(1);
                st.process(id, &mut c, 1, 1, &test_params(f, 12, 1.0), now);
            }
        }
        st.reset("a", now);
        assert_eq!(st.state("a").unwrap().frame_count(), 0);
        assert_eq!(st.state("b").unwrap().frame_count(), 5);
        assert_eq!(st.total_bytes(), 5 * 4);
    }
}
