//! Frame-history colored echo core.
//!
//! 1 オブジェクト分の履歴（`TrailState`）と、履歴から残像を合成する処理。
//! 複数オブジェクトの履歴をまとめて持ち、メモリの上限と破棄を受け持つのは `store.rs`。

use std::collections::VecDeque;

/// 抜けたフレームを前後の補間で埋める最大の間隔（フレーム）。
/// 3 なら 2 フレームまでの抜け（プレビューのコマ落ち程度）を埋める。
/// それより広い抜け（先へ大きく飛んだ）は埋めず、その残像は描かない。
pub const MAX_BRIDGE: i32 = 3;

struct FrameBuf {
    frame: i32,
    pixels: Vec<u8>,
}

pub struct TrailState {
    width: u32,
    height: u32,
    last_frame: Option<i32>,
    /// frame の昇順。frame が戻ったら reset するので、常に昇順に積まれる。
    frames: VecDeque<FrameBuf>,
    /// `frames` が持つ画素のバイト数の合計。
    bytes: usize,
}

impl Default for TrailState {
    fn default() -> Self {
        Self::new()
    }
}

impl TrailState {
    pub fn new() -> Self {
        Self {
            width: 0,
            height: 0,
            last_frame: None,
            frames: VecDeque::new(),
            bytes: 0,
        }
    }

    pub fn reset(&mut self) {
        self.width = 0;
        self.height = 0;
        self.last_frame = None;
        self.frames.clear();
        self.frames.shrink_to_fit();
        self.bytes = 0;
    }

    /// 履歴が持つ画素のバイト数。
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    #[cfg(test)]
    pub fn oldest_frame(&self) -> Option<i32> {
        self.frames.front().map(|f| f.frame)
    }

    #[cfg(test)]
    pub fn newest_frame(&self) -> Option<i32> {
        self.frames.back().map(|f| f.frame)
    }

    /// いちばん古いフレームを 1 枚捨て、空いたバイト数を返す（無ければ 0）。
    pub fn drop_oldest(&mut self) -> usize {
        match self.frames.pop_front() {
            Some(f) => {
                let n = f.pixels.len();
                self.bytes -= n;
                n
            }
            None => 0,
        }
    }

    fn prune(&mut self, keep_from: i32) {
        while self.frames.front().is_some_and(|f| f.frame < keep_from) {
            self.drop_oldest();
        }
    }

    fn store(&mut self, frame: i32, pixels: &[u8], w: u32, h: u32) {
        // 大きさが変わった・前へ戻った（シークで戻った）ら履歴は使えない
        let size_changed = w != self.width || h != self.height;
        let went_back = self.last_frame.is_some_and(|last| frame < last);
        if size_changed || went_back {
            self.reset();
            self.width = w;
            self.height = h;
        }
        self.last_frame = Some(frame);

        if let Some(back) = self.frames.back_mut() {
            if back.frame == frame {
                // 同じフレームの描き直し: 中身を差し替える（大きさは同じ）
                back.pixels.clear();
                back.pixels.extend_from_slice(pixels);
                return;
            }
        }
        self.frames.push_back(FrameBuf {
            frame,
            pixels: pixels.to_vec(),
        });
        self.bytes += pixels.len();
    }

    /// `target` 以下でいちばん新しいフレームと、`target` 以上でいちばん古いフレーム。
    fn neighbors(&self, target: f64) -> (Option<&FrameBuf>, Option<&FrameBuf>) {
        let upper_i = self.frames.partition_point(|f| (f.frame as f64) < target);
        let upper = self.frames.get(upper_i);
        let lower = match upper {
            Some(u) if u.frame as f64 == target => Some(u),
            _ => upper_i.checked_sub(1).and_then(|i| self.frames.get(i)),
        };
        (lower, upper)
    }

    /// Sample at `target` (may be fractional). Linear RGBA lerp between the neighbors.
    ///
    /// 抜けが無ければ floor / ceil の 2 枚の補間で、従来と同じ結果になる。
    /// 抜けがあれば、前後の間隔が `MAX_BRIDGE` 以内のときだけ前後の 2 枚で補間する。
    /// 履歴より古い位置は描かない（false）。
    fn sample_into(&self, target: f64, out: &mut [u8]) -> bool {
        if !target.is_finite() {
            return false;
        }
        let (lower, upper) = self.neighbors(target);
        let Some(a) = lower else {
            return false;
        };
        if a.frame as f64 == target {
            out.copy_from_slice(&a.pixels);
            return true;
        }
        if let Some(b) = upper {
            if b.frame - a.frame <= MAX_BRIDGE {
                let span = (b.frame - a.frame) as f64;
                let t = ((target - a.frame as f64) / span).clamp(0.0, 1.0) as f32;
                lerp_into(&a.pixels, &b.pixels, t, out);
                return true;
            }
        }
        // 従来の挙動: floor のフレームがあって ceil が無ければ floor をそのまま使う
        if a.frame == target.floor() as i32 {
            out.copy_from_slice(&a.pixels);
            return true;
        }
        false
    }
}

fn lerp_into(a: &[u8], b: &[u8], t: f32, out: &mut [u8]) {
    let inv = 1.0 - t;
    for i in 0..out.len() {
        out[i] = (a[i] as f32 * inv + b[i] as f32 * t).round() as u8;
    }
}

#[inline]
fn luma(r: f32, g: f32, b: f32) -> f32 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// Luminance-preserving tint into `px` (PMA-ish 8bit: treat as straight for simplicity of echo look).
fn apply_tint(px: &mut [u8], tr: f32, tg: f32, tb: f32, alpha_scale: f32) {
    for c in px.chunks_exact_mut(4) {
        let a = c[3] as f32 / 255.0;
        if a < 1e-5 {
            c[0] = 0;
            c[1] = 0;
            c[2] = 0;
            c[3] = 0;
            continue;
        }
        let r = c[0] as f32 / 255.0;
        let g = c[1] as f32 / 255.0;
        let b = c[2] as f32 / 255.0;
        // getpixeldata is typically straight RGBA8 from host; keep luma of RGB
        let y = luma(r, g, b);
        let na = (a * alpha_scale).clamp(0.0, 1.0);
        c[0] = (tr * y * 255.0).round().clamp(0.0, 255.0) as u8;
        c[1] = (tg * y * 255.0).round().clamp(0.0, 255.0) as u8;
        c[2] = (tb * y * 255.0).round().clamp(0.0, 255.0) as u8;
        c[3] = (na * 255.0).round().clamp(0.0, 255.0) as u8;
    }
}

fn scale_alpha(px: &mut [u8], alpha_scale: f32) {
    for c in px.chunks_exact_mut(4) {
        let a = ((c[3] as f32 / 255.0) * alpha_scale).clamp(0.0, 1.0);
        c[3] = (a * 255.0).round() as u8;
        if c[3] == 0 {
            c[0] = 0;
            c[1] = 0;
            c[2] = 0;
        }
    }
}

/// Source-over composite `src` onto `dst` (both RGBA8).
fn over(dst: &mut [u8], src: &[u8]) {
    for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
        let sa = s[3] as f32 / 255.0;
        if sa <= 0.0 {
            continue;
        }
        let da = d[3] as f32 / 255.0;
        let out_a = sa + da * (1.0 - sa);
        if out_a <= 1e-5 {
            d[0] = 0;
            d[1] = 0;
            d[2] = 0;
            d[3] = 0;
            continue;
        }
        for k in 0..3 {
            let sc = s[k] as f32 / 255.0;
            let dc = d[k] as f32 / 255.0;
            let oc = (sc * sa + dc * da * (1.0 - sa)) / out_a;
            d[k] = (oc * 255.0).round().clamp(0.0, 255.0) as u8;
        }
        d[3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
    }
}

pub struct ProcessParams {
    pub frame: i32,
    pub max_frames: i32,
    pub interval: f64,
    pub decay: f64,
    pub use_color: bool,
    pub r1: f32,
    pub g1: f32,
    pub b1: f32,
    pub a1: f32,
    pub use_grad: bool,
    pub r2: f32,
    pub g2: f32,
    pub b2: f32,
    pub a2: f32,
}

/// 数・間隔・減衰を使える値に揃える。
fn normalized(p: &ProcessParams) -> (i32, f64, f64) {
    let max_frames = p.max_frames.max(0);
    let interval = if !p.interval.is_finite() || p.interval.abs() < 1e-9 {
        1.0
    } else {
        p.interval.abs()
    };
    let decay = if p.decay.is_finite() {
        p.decay.clamp(0.0, 1.0)
    } else {
        0.0
    };
    (max_frames, interval, decay)
}

/// 残像に要るだけの過去のフレーム数（窓）。この窓より古いフレームは捨てる。
/// 既定（数 12・間隔 1）なら 14 で、今のフレームを含めて 15 枚を持つ（従来と同じ）。
pub fn keep_window(interval: f64, max_frames: i32) -> i32 {
    // f64 -> i32 の as は飽和する（NaN は 0）
    ((interval * max_frames.max(0) as f64).ceil() as i32).saturating_add(2)
}

/// 画素 1 枚のバイト数（w × h × 4）。溢れるなら None。
pub fn frame_bytes(w: u32, h: u32) -> Option<usize> {
    (w as usize).checked_mul(h as usize)?.checked_mul(4)
}

/// 今のフレームを履歴に積み、窓より古いフレームを捨てる。
/// `can_store` が false（1 枚でメモリの上限を超える）なら履歴を空にして積まない。
pub fn ingest(state: &mut TrailState, pixels: &[u8], w: u32, h: u32, p: &ProcessParams, can_store: bool) {
    let Some(len) = frame_bytes(w, h) else {
        return;
    };
    if len == 0 || pixels.len() < len {
        return;
    }
    if !can_store {
        state.reset();
        return;
    }
    let (max_frames, interval, _) = normalized(p);
    state.store(p.frame, &pixels[..len], w, h);
    let keep = keep_window(interval, max_frames);
    state.prune(p.frame.saturating_sub(keep));
}

/// In-place: `pixels` is current frame; becomes composite (echoes + current).
/// 履歴に無い位置の残像は描かない（描ける分だけ描く）。
pub fn render(state: &TrailState, pixels: &mut [u8], w: u32, h: u32, p: &ProcessParams) {
    let Some(len) = frame_bytes(w, h) else {
        return;
    };
    if len == 0 || pixels.len() < len {
        return;
    }
    let (max_frames, interval, decay) = normalized(p);
    if max_frames <= 0 {
        return;
    }
    let px = &mut pixels[..len];
    let current = px.to_vec();

    let mut out = vec![0u8; len];
    let mut scratch = vec![0u8; len];
    let age_denom = (max_frames - 1).max(1) as f32;
    // 履歴が別の大きさのもの（積めなかった）なら残像は描かない
    let can_sample = state.width == w && state.height == h;

    for i in (1..=max_frames).rev() {
        if !can_sample {
            break;
        }
        let frames_back = interval * i as f64;
        let target = p.frame as f64 - frames_back;
        if !state.sample_into(target, &mut scratch) {
            continue;
        }
        let age = (i - 1) as f32 / age_denom;
        let (tr, tg, tb, ta) = if p.use_grad {
            (
                p.r1 + (p.r2 - p.r1) * age,
                p.g1 + (p.g2 - p.g1) * age,
                p.b1 + (p.b2 - p.b1) * age,
                p.a1 + (p.a2 - p.a1) * age,
            )
        } else {
            (p.r1, p.g1, p.b1, p.a1)
        };
        let fade = decay.powi(i) as f32 * ta.clamp(0.0, 1.0);
        if p.use_color {
            apply_tint(&mut scratch, tr, tg, tb, fade);
        } else {
            scale_alpha(&mut scratch, fade);
        }
        over(&mut out, &scratch);
    }

    // Current frame on top (untinted)
    over(&mut out, &current);
    px.copy_from_slice(&out);
}

/// 1 オブジェクト分の処理（上限なし）。上限つきは `StateStore::process`。
#[cfg(test)]
pub fn process(state: &mut TrailState, pixels: &mut [u8], w: u32, h: u32, p: &ProcessParams) {
    ingest(state, pixels, w, h, p, true);
    render(state, pixels, w, h, p);
}

#[cfg(test)]
pub(crate) fn test_params(frame: i32, max_frames: i32, interval: f64) -> ProcessParams {
    ProcessParams {
        frame,
        max_frames,
        interval,
        decay: 0.7,
        use_color: false,
        r1: 1.0,
        g1: 0.0,
        b1: 0.0,
        a1: 1.0,
        use_grad: false,
        r2: 0.0,
        g2: 0.0,
        b2: 1.0,
        a2: 0.2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1x1 の不透明画素。中身で何フレーム目かを見分ける。
    fn px(v: u8) -> Vec<u8> {
        vec![v, 0, 0, 255]
    }

    #[test]
    fn lerp_midpoint() {
        let mut st = TrailState::new();
        let a = vec![0u8, 0, 0, 255, 0, 0, 0, 0];
        let b = vec![255u8, 0, 0, 255, 0, 0, 0, 0];
        st.store(0, &a, 2, 1);
        st.store(1, &b, 2, 1);
        let mut out = vec![0u8; 8];
        assert!(st.sample_into(0.5, &mut out));
        assert_eq!(out[0], 128);
        assert_eq!(out[3], 255);
    }

    #[test]
    fn exact_and_fractional_sampling_without_gaps() {
        let mut st = TrailState::new();
        for f in 0..5 {
            st.store(f, &px(f as u8 * 40), 1, 1);
        }
        let mut out = vec![0u8; 4];
        assert!(st.sample_into(2.0, &mut out));
        assert_eq!(out[0], 80);
        // floor=3, ceil=4: 120 * 0.75 + 160 * 0.25 = 130
        assert!(st.sample_into(3.25, &mut out));
        assert_eq!(out[0], 130);
        // 履歴より古い位置は描かない
        assert!(!st.sample_into(-1.0, &mut out));
        assert!(!st.sample_into(-0.5, &mut out));
    }

    #[test]
    fn window_prune_keeps_default_count() {
        // 既定（数 12・間隔 1）: 今のフレームを含めて 15 枚（従来と同じ）
        assert_eq!(keep_window(1.0, 12), 14);
        let mut st = TrailState::new();
        for f in 0..100 {
            let p = test_params(f, 12, 1.0);
            ingest(&mut st, &px(1), 1, 1, &p, true);
        }
        assert_eq!(st.frame_count(), 15);
        assert_eq!(st.oldest_frame(), Some(85));
        assert_eq!(st.newest_frame(), Some(99));
        assert_eq!(st.bytes(), 15 * 4);
    }

    #[test]
    fn keep_window_saturates() {
        assert_eq!(keep_window(f64::MAX, i32::MAX), i32::MAX);
        assert_eq!(keep_window(f64::NAN, 10), 2);
    }

    #[test]
    fn going_back_resets_history() {
        let mut st = TrailState::new();
        for f in 10..20 {
            ingest(&mut st, &px(1), 1, 1, &test_params(f, 12, 1.0), true);
        }
        ingest(&mut st, &px(2), 1, 1, &test_params(5, 12, 1.0), true);
        assert_eq!(st.frame_count(), 1);
        assert_eq!(st.oldest_frame(), Some(5));
    }

    #[test]
    fn large_forward_jump_drops_old_history() {
        let mut st = TrailState::new();
        for f in 0..20 {
            ingest(&mut st, &px(1), 1, 1, &test_params(f, 12, 1.0), true);
        }
        // 窓（14）より大きく先へ飛ぶ: 前の履歴は全部捨てる（メモリも返る）
        let mut cur = px(200);
        process(&mut st, &mut cur, 1, 1, &test_params(500, 12, 1.0));
        assert_eq!(st.frame_count(), 1);
        assert_eq!(st.bytes(), 4);
        // 残像は描けないので、今のフレームだけ
        assert_eq!(cur, px(200));
    }

    #[test]
    fn small_gap_is_bridged_large_gap_is_not() {
        let mut st = TrailState::new();
        st.store(0, &px(0), 1, 1);
        st.store(3, &px(90), 1, 1); // 1, 2 が抜け（間隔 3 = MAX_BRIDGE）
        st.store(20, &px(200), 1, 1); // 4..19 が抜け
        let mut out = vec![0u8; 4];
        assert!(st.sample_into(1.0, &mut out));
        assert_eq!(out[0], 30);
        assert!(st.sample_into(2.0, &mut out));
        assert_eq!(out[0], 60);
        // 広い抜けの中は描かない
        assert!(!st.sample_into(10.0, &mut out));
        assert!(!st.sample_into(19.0, &mut out));
        // 抜けの手前のフレームそのものは使える
        assert!(st.sample_into(3.0, &mut out));
        assert_eq!(out[0], 90);
    }

    #[test]
    fn partial_history_draws_what_it_can() {
        // 途中から始めたとき: たまった分だけ残像が出る
        let mut st = TrailState::new();
        let p0 = test_params(100, 12, 1.0);
        let mut c0 = vec![0u8, 0, 0, 0, 255, 255, 255, 255];
        process(&mut st, &mut c0, 2, 1, &p0);
        // 左の画素は透明のまま（残像なし）
        assert_eq!(&c0[0..4], &[0, 0, 0, 0]);

        // 次のフレーム: 物体が右から左へ動いた。左に今の絵、右に 1 フレーム前の残像
        let p1 = test_params(101, 12, 1.0);
        let mut c1 = vec![255u8, 255, 255, 255, 0, 0, 0, 0];
        process(&mut st, &mut c1, 2, 1, &p1);
        assert_eq!(&c1[0..4], &[255, 255, 255, 255]);
        // 右: 1 フレーム前（不透明）× 減衰 0.7^1 ≒ 178.5
        assert!((178..=179).contains(&c1[7]), "alpha = {}", c1[7]);
    }

    #[test]
    fn cannot_store_clears_history_and_keeps_current() {
        let mut st = TrailState::new();
        for f in 0..5 {
            ingest(&mut st, &px(1), 1, 1, &test_params(f, 12, 1.0), true);
        }
        let p = test_params(5, 12, 1.0);
        ingest(&mut st, &px(9), 1, 1, &p, false);
        assert_eq!(st.frame_count(), 0);
        assert_eq!(st.bytes(), 0);
        let mut cur = px(9);
        render(&st, &mut cur, 1, 1, &p);
        assert_eq!(cur, px(9));
    }
}
