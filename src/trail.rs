//! Frame-history colored echo core.
//!
//! 1 オブジェクト分の履歴（`TrailState`）と、履歴から残像を合成する処理。
//! 複数オブジェクトの履歴をまとめて持ち、メモリの上限と破棄を受け持つのは `store.rs`。
//!
//! 合成（`render`）は v1.2.0 から 1 画素ずつ最後まで計算する（残像 1 枚ごとに画面全体の写し・色付け・重ねを
//! 3 回ずつ回していたのをやめた）。1 画素の計算の式と順番は v1.1.0 と同じなので、出力は 1 ビットも変わらない
//! （下の `tests::matches_v110_render` が v1.1.0 の実装と突き合わせる）。速くなった所:
//! - 今の絵が不透明な画素は、上に今の絵をそのまま重ねるので答えは今の絵（`over` の sa = 1）。残像を計算しない
//! - 残像の画素が透明なら重ねても変わらない（`over` の sa = 0）ので、色付けと重ねを飛ばす。履歴の絵ごとに 64 画素のまとまりで「α が 0 でない画素があるか」を
//!   積むときに数えておき（`summarize`）、無いまとまりはその残像を読まない。どの残像も掛からないまとまりは、今の絵だけで答えを出す（`bare_chunk`）
//! - 1 画素も書き換えなかったら false を返す（スクリプト v1.01 は putpixeldata を省く）
//! - 4 成分を SSE2（x86_64 では必ずある）でまとめて計算する（`simd`。成分ごとの IEEE の演算は同じ値になる）。
//!   `f32::round` は既定のビルドで関数の呼び出しになるので、同じ値を返す `r8` で丸める。スカラー版（`composite_scalar`）は
//!   0〜255 を 255 で割った値を表から引く（同じ割り算の結果なので値は同じ）
//! - 64 画素ずつ、残像 1 枚ごとに進める（1 画素の中は前の結果を待つが、隣の画素どうしは CPU が重ねて計算できる）
//! - 8 MB（FHD）の作業用の入れ物を毎フレーム 3 枚確保しない。履歴に積む入れ物は、窓から外れた絵の入れ物を使い回す
//! - 同じ入力の画素はまとめて 1 回だけ計算する（SSE2 の道だけ。`simd::composite_sse2` の `same`）。1 画素の答えは今の絵と
//!   残像の元の画素で決まるので、1 つ前の画素と入力がすべて同じなら答えも同じ。残像の元の絵の「1 つ前の画素と同じか」は
//!   積むときに数えておく（`Summary::eqm`）ので、合成では履歴の画素を読まずに見分けられる。単色の図形・文字・透明な地のように
//!   同じ画素が続く絵で効く（強化デバッグ 7.5 の CEI-03、硬マットの枠: モジュールの中で 22 ms → 約 4.5 ms。`examples/bench.rs` の matte）。
//!   写真のように隣と違う画素が続く絵では速くならない（比べる手間だけ増える。まとまりごとに打ち切るので小さい）
//! - α を見る所（不透明か・残像が無いまとまりで書き換えうるか）を SSE2 で 4 画素ずつ見る（`simd::alpha_masks`）
//!
//! v1.3.0 で、色1→色2 の補間に色空間と色相経路を足した（`colorspace.rs`、`echo_color`）。色は残像ごとに 1 回だけ求め、
//! 画素のループには v1.2.0 と同じ定数（`Look::Tint`）として渡すので、合成の速さは色空間によらない。
//! 既定（sRGB）は v1.2.0 と同じ f32 の式を通る（`tests::colorspace_only_affects_gradient_tint`・`matches_v110_render`）。

use std::collections::VecDeque;

use crate::colorspace::{self, Gradient};

/// 抜けたフレームを前後の補間で埋める最大の間隔（フレーム）。
/// 3 なら 2 フレームまでの抜け（プレビューのコマ落ち程度）を埋める。
/// それより広い抜け（先へ大きく飛んだ）は埋めず、その残像は描かない。
pub const MAX_BRIDGE: i32 = 3;

struct FrameBuf {
    frame: i32,
    pixels: Vec<u8>,
    /// まとまりごとの印（`summarize`）。上限の計算には数えない（FHD で 260 KB）
    sum: Summary,
}

/// 透明かどうかを数えるまとまりの画素数（`render` が一度に進める画素の数と同じ）。
const CHUNK: usize = 64;

/// 1 枚の絵の、`CHUNK` 画素のまとまりごとの印（積むときに 1 回だけ数える）。
#[derive(Default)]
struct Summary {
    /// α が 0 でない画素があるか（まとまり b をビット b に置く）
    occ: Vec<u64>,
    /// まとまり b の画素 j が 1 つ前の画素と同じ 4 バイトなら、要素 b のビット j が立つ。
    /// ビット 0 は、1 つ前のまとまりの最後の画素と比べる（まとまり 0 では立たない）。
    /// `render` が残像の画素を読まずに「同じ入力の画素」を見分けるのに使う（単色の図形・透明な地）
    eqm: Vec<u64>,
}

impl Summary {
    #[inline(always)]
    fn occupied(&self, b: usize) -> bool {
        self.occ.get(b / 64).is_some_and(|w| (w >> (b % 64)) & 1 != 0)
    }
}

/// `px`（RGBA8）のまとまりごとの印を `out` に書く。
fn summarize(px: &[u8], out: &mut Summary) {
    let blocks = (px.len() / 4).div_ceil(CHUNK);
    out.occ.clear();
    out.occ.resize(blocks.div_ceil(64), 0);
    out.eqm.clear();
    for b in 0..blocks {
        let base = b * CHUNK * 4;
        let end = (base + CHUNK * 4).min(px.len());
        #[cfg(target_arch = "x86_64")]
        let fast = (end - base == CHUNK * 4 && base >= 4).then(|| simd::full_chunk(px, base));
        #[cfg(not(target_arch = "x86_64"))]
        let fast: Option<(u64, bool)> = None;
        let (m, any_alpha) = fast.unwrap_or_else(|| summarize_slow(px, base, end));
        if any_alpha {
            out.occ[b / 64] |= 1u64 << (b % 64);
        }
        out.eqm.push(m);
    }
}

/// `summarize` の 1 まとまり分（`base`〜`end` バイト）を 1 画素ずつ（最初と最後の半端なまとまり用。`simd::full_chunk` と同じ答え）。
fn summarize_slow(px: &[u8], base: usize, end: usize) -> (u64, bool) {
    let (mut a, mut m) = (0u8, 0u64);
    for (j, o) in (base..end).step_by(4).enumerate() {
        a |= px[o + 3];
        if o >= 4 && px[o..o + 4] == px[o - 4..o] {
            m |= 1 << j;
        }
    }
    (m, a != 0)
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

/// 残像 1 枚の読み方（どの絵を、どれだけ混ぜて読むか）。画素はまだ読まない。
#[derive(Clone, Copy)]
enum Src<'a> {
    /// 1 枚をそのまま
    One(&'a [u8]),
    /// 2 枚を割合 t（b の割合）で混ぜる
    Two(&'a [u8], &'a [u8], f32),
}

impl Src<'_> {
    /// 位置 o（バイト）の 1 画素。`Two` は v1.1.0 の `lerp_into` と同じ式。
    #[inline(always)]
    fn at(&self, o: usize) -> [u8; 4] {
        match *self {
            Src::One(p) => [p[o], p[o + 1], p[o + 2], p[o + 3]],
            Src::Two(a, b, t) => {
                let inv = 1.0 - t;
                let f = |k: usize| r8(a[o + k] as f32 * inv + b[o + k] as f32 * t);
                [f(0), f(1), f(2), f(3)]
            }
        }
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

    /// `keep_from` より古いフレームを捨て、捨てた中から `len` バイトの入れ物を 1 つ返す（次に積む絵に使い回す）。
    fn prune_take(&mut self, keep_from: i32, len: usize) -> Option<FrameBuf> {
        let mut spare = None;
        while self.frames.front().is_some_and(|f| f.frame < keep_from) {
            if let Some(f) = self.frames.pop_front() {
                self.bytes -= f.pixels.len();
                if spare.is_none() && f.pixels.len() == len {
                    spare = Some(f);
                }
            }
        }
        spare
    }

    /// 今のフレームを積み、`keep_from` より古いフレームを捨てる。
    /// 積んでから捨てる（v1.1.0）のと同じ結果になる（積む絵は `keep_from` 以上なので捨てられない）。
    /// 先に捨てて、その入れ物に今の絵を写す: 8 MB（FHD）を毎フレーム確保し直すと、新しい領域への最初の書き込みで
    /// ページの割り当てが起きて遅い（TimeDisplace_H の `history.rs` と同じ）。
    fn store_window(&mut self, frame: i32, pixels: &[u8], w: u32, h: u32, keep_from: i32) {
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
                summarize(&back.pixels, &mut back.sum);
                self.prune(keep_from);
                return;
            }
        }
        let mut buf = match self.prune_take(keep_from, pixels.len()) {
            Some(mut f) => {
                f.pixels.copy_from_slice(pixels);
                f.frame = frame;
                f
            }
            None => FrameBuf { frame, pixels: pixels.to_vec(), sum: Summary::default() },
        };
        summarize(&buf.pixels, &mut buf.sum);
        self.bytes += buf.pixels.len();
        self.frames.push_back(buf);
    }

    #[cfg(test)]
    fn store(&mut self, frame: i32, pixels: &[u8], w: u32, h: u32) {
        self.store_window(frame, pixels, w, h, i32::MIN);
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

    /// `target`（小数でもよい）の読み方。前後の 2 枚の RGBA を線形に混ぜる。
    ///
    /// 抜けが無ければ floor / ceil の 2 枚の補間で、従来と同じ結果になる。
    /// 抜けがあれば、前後の間隔が `MAX_BRIDGE` 以内のときだけ前後の 2 枚で補間する。
    /// 履歴より古い位置は描かない（None）。読む絵の、まとまりごとの印（`summarize`）も返す（1 枚なら同じものを 2 つ）。
    fn sample_src(&self, target: f64) -> Option<(Src<'_>, [&Summary; 2])> {
        if !target.is_finite() {
            return None;
        }
        let (lower, upper) = self.neighbors(target);
        let a = lower?;
        if a.frame as f64 == target {
            return Some((Src::One(&a.pixels), [&a.sum, &a.sum]));
        }
        if let Some(b) = upper {
            if b.frame - a.frame <= MAX_BRIDGE {
                let span = (b.frame - a.frame) as f64;
                let t = ((target - a.frame as f64) / span).clamp(0.0, 1.0) as f32;
                return Some((Src::Two(&a.pixels, &b.pixels, t), [&a.sum, &b.sum]));
            }
        }
        // 従来の挙動: floor のフレームがあって ceil が無ければ floor をそのまま使う
        if a.frame == target.floor() as i32 {
            return Some((Src::One(&a.pixels), [&a.sum, &a.sum]));
        }
        None
    }

    /// `target` の絵を `out` に写す（テストで読み方を確かめる）。
    #[cfg(test)]
    fn sample_into(&self, target: f64, out: &mut [u8]) -> bool {
        match self.sample_src(target) {
            Some((src, _)) => {
                for o in (0..out.len()).step_by(4) {
                    out[o..o + 4].copy_from_slice(&src.at(o));
                }
                true
            }
            None => false,
        }
    }
}

/// `x.round().clamp(0.0, 255.0) as u8` と同じ値（`tests::r8_matches_round`）。
/// `f32::round` は既定のビルド（SSE4.1 なし）で `roundf` の呼び出しになり、1 画素に十数回呼ぶと遅い。
/// 0〜255 に詰めてから、整数部（切り捨て）と小数部（引き算は丸めなしで求まる）に分けて四捨五入する。
/// NaN は 0（`as` の変換と同じ）。0〜255 の外は端に詰まる（v1.1.0 の `as u8` の飽和・clamp と同じ）。
#[inline(always)]
fn r8(x: f32) -> u8 {
    let x = x.clamp(0.0, 255.0);
    let i = x as u32;
    let f = x - i as f32;
    (i + (f >= 0.5) as u32) as u8
}

/// 0〜255 を 255 で割った値（`v as f32 / 255.0` と同じ値）。
type Unit = [f32; 256];

fn unit_table() -> Unit {
    std::array::from_fn(|i| i as f32 / 255.0)
}

#[inline]
fn luma(r: f32, g: f32, b: f32) -> f32 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// Luminance-preserving tint（1 画素。v1.1.0 の `apply_tint` と同じ式）。
#[inline(always)]
fn tint_px(c: [u8; 4], u: &Unit, tr: f32, tg: f32, tb: f32, alpha_scale: f32) -> [u8; 4] {
    let a = u[c[3] as usize];
    if a < 1e-5 {
        return [0, 0, 0, 0];
    }
    let r = u[c[0] as usize];
    let g = u[c[1] as usize];
    let b = u[c[2] as usize];
    // getpixeldata is typically straight RGBA8 from host; keep luma of RGB
    let y = luma(r, g, b);
    let na = (a * alpha_scale).clamp(0.0, 1.0);
    [r8(tr * y * 255.0), r8(tg * y * 255.0), r8(tb * y * 255.0), r8(na * 255.0)]
}

/// α だけを薄める（1 画素。v1.1.0 の `scale_alpha` と同じ式）。
#[inline(always)]
fn scale_px(c: [u8; 4], u: &Unit, alpha_scale: f32) -> [u8; 4] {
    let a = (u[c[3] as usize] * alpha_scale).clamp(0.0, 1.0);
    let a8 = r8(a * 255.0);
    if a8 == 0 {
        [0, 0, 0, 0]
    } else {
        [c[0], c[1], c[2], a8]
    }
}

/// Source-over composite `s` onto `d`（1 画素、どちらも RGBA8 ストレート。v1.1.0 の `over` と同じ式）。
#[inline(always)]
fn over_px(d: &mut [u8; 4], s: [u8; 4], u: &Unit) {
    let sa = u[s[3] as usize];
    if sa <= 0.0 {
        return;
    }
    let da = u[d[3] as usize];
    let out_a = sa + da * (1.0 - sa);
    if out_a <= 1e-5 {
        *d = [0, 0, 0, 0];
        return;
    }
    for k in 0..3 {
        let sc = u[s[k] as usize];
        let dc = u[d[k] as usize];
        let oc = (sc * sa + dc * da * (1.0 - sa)) / out_a;
        d[k] = r8(oc * 255.0);
    }
    d[3] = r8(out_a * 255.0);
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
    /// 色1→色2 を補間する色空間（v1.3.0。番号は共有部品と同じ。0 sRGB は v1.2.0 までと同じ f32 の lerp）
    pub space: i32,
    /// 色相経路（v1.3.0）。真で長い方を回る。色相のある色空間でだけ効く
    pub hue_long: bool,
}

/// 色1→色2 の補間（v1.3.0）。sRGB 以外の色空間のときだけ、2 色を先に色空間へ変換して持つ（`render` で 1 回だけ作る）
pub fn gradient_of(p: &ProcessParams) -> Option<Gradient> {
    (p.use_color && p.use_grad && p.space != colorspace::SRGB).then(|| {
        let c = |r: f32, g: f32, b: f32| [r as f64, g as f64, b as f64];
        Gradient::new(c(p.r1, p.g1, p.b1), c(p.r2, p.g2, p.b2), p.space, p.hue_long)
    })
}

/// 残像 1 枚の色付けの色と不透明度（age: 0 で色1、1 で色2）。
/// `grad` が None（sRGB・グラデーションなし・色を適用しない）なら v1.2.0 と同じ式。
/// 色空間を選んだときは色だけを `Gradient::at` で求め、不透明度は今までどおり線形に混ぜる。
pub fn echo_color(p: &ProcessParams, grad: Option<&Gradient>, age: f32) -> (f32, f32, f32, f32) {
    if !p.use_grad {
        return (p.r1, p.g1, p.b1, p.a1);
    }
    let ta = p.a1 + (p.a2 - p.a1) * age;
    match grad {
        Some(g) => {
            let [r, gg, b] = g.at(age as f64);
            (r as f32, gg as f32, b as f32, ta)
        }
        None => (p.r1 + (p.r2 - p.r1) * age, p.g1 + (p.g2 - p.g1) * age, p.b1 + (p.b2 - p.b1) * age, ta),
    }
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
    let keep = keep_window(interval, max_frames);
    state.store_window(p.frame, &pixels[..len], w, h, p.frame.saturating_sub(keep));
}

/// 残像 1 枚の見え方（色付けか α を薄めるだけか）。
#[derive(Clone, Copy)]
enum Look {
    Tint { r: f32, g: f32, b: f32, fade: f32 },
    Fade(f32),
}

struct Layer<'a> {
    src: Src<'a>,
    look: Look,
    /// 読む絵の、まとまりごとの印（`summarize`。`src` の 1 枚目・2 枚目。1 枚なら同じもの）
    sum: [&'a Summary; 2],
}

impl Layer<'_> {
    /// まとまり b に、この残像の α が 0 でない画素がありうるか（2 枚とも透明なら混ぜても透明）。
    #[inline(always)]
    fn covers(&self, b: usize) -> bool {
        self.sum[0].occupied(b) || self.sum[1].occupied(b)
    }
}

/// In-place: `pixels` is current frame; becomes composite (echoes + current).
/// 履歴に無い位置の残像は描かない（描ける分だけ描く）。
///
/// 1 画素ごとに、古い残像から順に重ね、最後に今の絵を重ねる（v1.1.0 と同じ順番・同じ式）。
/// 1 画素でも書き換えたら true（false なら `pixels` は渡されたまま。スクリプトは書き戻さなくてよい）。
pub fn render(state: &TrailState, pixels: &mut [u8], w: u32, h: u32, p: &ProcessParams) -> bool {
    let Some(len) = frame_bytes(w, h) else {
        return false;
    };
    if len == 0 || pixels.len() < len {
        return false;
    }
    let (max_frames, interval, decay) = normalized(p);
    if max_frames <= 0 {
        return false;
    }
    let px = &mut pixels[..len];

    // 履歴が別の大きさのもの（積めなかった）なら残像は描かない
    let can_sample = state.width == w && state.height == h;
    let age_denom = (max_frames - 1).max(1) as f32;
    let mut layers: Vec<Layer> = Vec::new();
    // 色空間の変換は 2 色ぶん 1 回だけ。残像ごとの色もここで 1 回だけ求め、画素のループには定数として渡す（v1.3.0）
    let grad = gradient_of(p);
    if can_sample {
        for i in (1..=max_frames).rev() {
            let frames_back = interval * i as f64;
            let target = p.frame as f64 - frames_back;
            let Some((src, sum)) = state.sample_src(target) else {
                continue;
            };
            let age = (i - 1) as f32 / age_denom;
            let (tr, tg, tb, ta) = echo_color(p, grad.as_ref(), age);
            let fade = decay.powi(i) as f32 * ta.clamp(0.0, 1.0);
            if fade == 0.0 {
                // α が 0 になるので重ねても変わらない（色付けでも α を薄めるだけでも）
                continue;
            }
            let look = if p.use_color {
                Look::Tint { r: tr, g: tg, b: tb, fade }
            } else {
                Look::Fade(fade)
            };
            layers.push(Layer { src, look, sum });
        }
    }

    #[cfg(target_arch = "x86_64")]
    return simd::composite(px, &layers);
    #[cfg(not(target_arch = "x86_64"))]
    return composite_scalar(px, &layers);
}

/// 残像がどれも掛からないまとまり（`Layer::covers` が偽）: 答えは「何も無い所に今の絵を重ねたもの」（`over_px` の d = 0）。
/// α が 255 ならそのまま、0 なら透明（[0,0,0,0]。色だけ残っていれば消す）、その間は `over_px` で計算する。
/// 透明な地に小さな物が動く使い方では画面の大半がここを通る。値が変わったら true。
fn bare_chunk(px: &mut [u8], u: &Unit) -> bool {
    let mut changed = false;
    for c in px.chunks_exact_mut(4) {
        match c[3] {
            255 => {}
            0 => {
                if c[0] | c[1] | c[2] != 0 {
                    c[..3].fill(0);
                    changed = true;
                }
            }
            _ => {
                let cur = [c[0], c[1], c[2], c[3]];
                let mut d = [0u8; 4];
                over_px(&mut d, cur, u);
                if d != cur {
                    c.copy_from_slice(&d);
                    changed = true;
                }
            }
        }
    }
    changed
}

/// 残像を重ねる（スカラー。x86_64 では `simd::composite` が同じ計算をする。テストで突き合わせる）。
///
/// `CHUNK` 画素ずつ、残像 1 枚ごとに全部の画素を進める。1 画素の中では残像を古い順に重ねるので
/// 前の結果を待つ（割り算と丸めの待ち時間が残像の数だけ続く）が、隣の画素どうしは独立なので CPU が重ねて計算できる。
/// 1 画素でも値が変わったら true。
#[cfg_attr(target_arch = "x86_64", allow(dead_code))]
fn composite_scalar(px: &mut [u8], layers: &[Layer]) -> bool {
    let u = unit_table();
    let mut changed = false;
    let n = px.len() / 4;
    let mut d = [[0u8; 4]; CHUNK];
    let mut idx = [0usize; CHUNK];
    let mut start = 0;
    let mut block = 0;
    while start < n {
        let end = (start + CHUNK).min(n);
        if !layers.iter().any(|l| l.covers(block)) {
            changed |= bare_chunk(&mut px[start * 4..end * 4], &u);
            start = end;
            block += 1;
            continue;
        }
        // 今の絵が不透明な画素は、最後に今の絵を重ねると答えが今の絵そのもの（`tests::opaque_current_wins`）なので計算しない
        let mut m = 0;
        for p in start..end {
            if px[p * 4 + 3] != 255 {
                idx[m] = p * 4;
                m += 1;
            }
        }
        start = end;
        let b = block;
        block += 1;
        if m == 0 {
            continue;
        }
        let (d, idx) = (&mut d[..m], &idx[..m]);
        d.fill([0, 0, 0, 0]);
        for l in layers {
            if !l.covers(b) {
                continue;
            }
            for (dj, &o) in d.iter_mut().zip(idx) {
                let s = l.src.at(o);
                if s[3] == 0 {
                    // 透明な残像は、色付けしても α を薄めても透明のままで、重ねても変わらない
                    continue;
                }
                let e = match l.look {
                    Look::Tint { r, g, b, fade } => tint_px(s, &u, r, g, b, fade),
                    Look::Fade(fade) => scale_px(s, &u, fade),
                };
                over_px(dj, e, &u);
            }
        }
        // Current frame on top (untinted)
        for (dj, &o) in d.iter_mut().zip(idx) {
            let cur = [px[o], px[o + 1], px[o + 2], px[o + 3]];
            over_px(dj, cur, &u);
            changed |= *dj != cur;
            px[o..o + 4].copy_from_slice(dj);
        }
    }
    changed
}

/// SSE2（x86_64 の既定の命令セットに必ずある）で 1 画素の 4 成分をまとめて計算する。
/// 式と演算の順番はスカラーの `tint_px`・`scale_px`・`over_px`・`r8` と同じ。成分ごとの IEEE の演算（割り算も）は
/// SSE でもスカラーでも同じ値になるので、出力も同じ（`tests::simd_matches_scalar`）。
/// 値は「0〜255 の整数を持つ f32 の 4 成分（RGBA）」で受け渡す。
#[cfg(target_arch = "x86_64")]
mod simd {
    use super::*;
    use std::arch::x86_64::*;

    /// 4 バイト（RGBA）を f32 の 4 成分へ。
    #[inline(always)]
    unsafe fn load(p: &[u8], o: usize) -> __m128 {
        let v = u32::from_le_bytes(p[o..o + 4].try_into().unwrap());
        let z = _mm_setzero_si128();
        _mm_cvtepi32_ps(_mm_unpacklo_epi16(_mm_unpacklo_epi8(_mm_cvtsi32_si128(v as i32), z), z))
    }

    /// 0〜255 の整数を持つ 4 成分を 4 バイトへ。
    #[inline(always)]
    unsafe fn pack(v: __m128) -> [u8; 4] {
        let i = _mm_cvttps_epi32(v);
        let b = _mm_packus_epi16(_mm_packs_epi32(i, i), _mm_setzero_si128());
        (_mm_cvtsi128_si32(b) as u32).to_le_bytes()
    }

    /// 0〜255 の整数を持つ 4 成分を 4 バイトへ書く。前と値が変わったら true。
    #[inline(always)]
    unsafe fn store(p: &mut [u8], o: usize, v: __m128) -> bool {
        let new = pack(v);
        let changed = p[o..o + 4] != new;
        p[o..o + 4].copy_from_slice(&new);
        changed
    }

    /// `super::r8` を成分ごとに（整数の値を持つ f32 で返す）。
    /// MAXPS は片方が NaN なら 2 つ目（0）を返すので、NaN は 0 になる（`r8` と同じ）。
    #[inline(always)]
    unsafe fn r8(x: __m128) -> __m128 {
        let x = _mm_min_ps(_mm_max_ps(x, _mm_setzero_ps()), _mm_set1_ps(255.0));
        let i = _mm_cvttps_epi32(x);
        let f = _mm_sub_ps(x, _mm_cvtepi32_ps(i));
        let up = _mm_cmpge_ps(f, _mm_set1_ps(0.5));
        _mm_cvtepi32_ps(_mm_sub_epi32(i, _mm_castps_si128(up)))
    }

    #[inline(always)]
    unsafe fn lane3(v: __m128) -> f32 {
        _mm_cvtss_f32(_mm_shuffle_ps::<0xFF>(v, v))
    }

    /// RGB は v、A は a の A。
    #[inline(always)]
    unsafe fn alpha_from(v: __m128, a: __m128) -> __m128 {
        let m = _mm_castsi128_ps(_mm_set_epi32(-1, 0, 0, 0));
        _mm_or_ps(_mm_andnot_ps(m, v), _mm_and_ps(m, a))
    }

    /// RGB は v、A は a。
    #[inline(always)]
    unsafe fn with_alpha(v: __m128, a: f32) -> __m128 {
        alpha_from(v, _mm_set1_ps(a))
    }

    /// `Src::at` と同じ（2 枚なら a × (1 − t) + b × t を丸める）。
    #[inline(always)]
    unsafe fn sample(src: &Src, o: usize) -> __m128 {
        match *src {
            Src::One(p) => load(p, o),
            Src::Two(a, b, t) => {
                let inv = 1.0 - t;
                r8(_mm_add_ps(_mm_mul_ps(load(a, o), _mm_set1_ps(inv)), _mm_mul_ps(load(b, o), _mm_set1_ps(t))))
            }
        }
    }

    /// `tint_px`（α が 0 でない画素だけを渡す。0 のときの早い道は通らない）。
    #[inline(always)]
    /// `t` は色付けの色（RGB の 3 成分。A は使わない）、`fade` は α に掛ける値を 4 成分に並べたもの。
    unsafe fn tint(s: __m128, t: __m128, fade: __m128) -> __m128 {
        let sn = _mm_div_ps(s, _mm_set1_ps(255.0));
        // y = (0.2126 × r + 0.7152 × g) + 0.0722 × b（luma と同じ順に足す）
        let p = _mm_mul_ps(sn, _mm_set_ps(0.0, 0.0722, 0.7152, 0.2126));
        let y = _mm_add_ss(_mm_add_ss(p, _mm_shuffle_ps::<0x55>(p, p)), _mm_shuffle_ps::<0xAA>(p, p));
        let y = _mm_shuffle_ps::<0x00>(y, y);
        // na = (a × fade) を 0〜1 に詰める（MAXPS は NaN なら 0 を返す。clamp は NaN を残すが、どちらも r8 で 0 になる）
        let na = _mm_min_ps(_mm_max_ps(_mm_mul_ps(sn, fade), _mm_setzero_ps()), _mm_set1_ps(1.0));
        // RGB は (t × y) × 255、A は na × 255
        r8(_mm_mul_ps(alpha_from(_mm_mul_ps(t, y), na), _mm_set1_ps(255.0)))
    }

    /// `over_px`（e の α が 0 でないときだけ呼ぶ。そのとき out_a ≥ sa ≥ 1/255 なので 1e-5 以下の道は通らない）。
    #[inline(always)]
    unsafe fn over(d: __m128, e: __m128) -> __m128 {
        let k = _mm_set1_ps(255.0);
        let en = _mm_div_ps(e, k);
        let dn = _mm_div_ps(d, k);
        // sa・da を 4 成分に並べて、スカラーと同じ式を 4 成分まとめて計算する
        let sa = _mm_shuffle_ps::<0xFF>(en, en);
        let da = _mm_shuffle_ps::<0xFF>(dn, dn);
        let one_m = _mm_sub_ps(_mm_set1_ps(1.0), sa);
        let out_a = _mm_add_ps(sa, _mm_mul_ps(da, one_m));
        // sc × sa + (dc × da) × (1 − sa)
        let num = _mm_add_ps(_mm_mul_ps(en, sa), _mm_mul_ps(_mm_mul_ps(dn, da), one_m));
        // RGB は oc × 255、A は out_a × 255
        r8(_mm_mul_ps(alpha_from(_mm_div_ps(num, out_a), out_a), k))
    }

    /// `super::bare_chunk` と同じ（半透明の画素の重ねを SSE2 で）。
    /// `base` バイト目から `cnt` 画素のまとまり。64 画素そろっていれば、書き換えうる画素（α が 255 でも、4 バイトとも 0 でもない）だけを見る。
    unsafe fn bare(px: &mut [u8], base: usize, cnt: usize) -> bool {
        let mut changed = false;
        if cnt == CHUNK {
            let mut bits = alpha_masks(px, base).1;
            while bits != 0 {
                changed |= bare_px(px, base + bits.trailing_zeros() as usize * 4);
                bits &= bits - 1;
            }
        } else {
            for j in 0..cnt {
                changed |= bare_px(px, base + j * 4);
            }
        }
        changed
    }

    #[inline(always)]
    unsafe fn bare_px(px: &mut [u8], o: usize) -> bool {
        match px[o + 3] {
            255 => false,
            0 => {
                if px[o] | px[o + 1] | px[o + 2] != 0 {
                    px[o..o + 3].fill(0);
                    true
                } else {
                    false
                }
            }
            _ => store(px, o, over(_mm_setzero_ps(), load(px, o))),
        }
    }

    /// 64 画素そろったまとまり（`base` バイト目から）の、α が 255 でない画素のビットと、
    /// α が 255 でも 4 バイトとも 0 でもない画素（残像が無くても答えが今の絵と違いうる画素）のビット。
    #[inline(always)]
    fn alpha_masks(p: &[u8], base: usize) -> (u64, u64) {
        let s = &p[base..base + CHUNK * 4];
        // SAFETY: 読むのは s の中だけ。命令は SSE2 だけ（x86_64 の既定）
        unsafe {
            let am = _mm_set1_epi32(0xff00_0000u32 as i32);
            let z = _mm_setzero_si128();
            let (mut todo, mut touch) = (0u64, 0u64);
            for g in 0..CHUNK / 4 {
                let v = _mm_loadu_si128(s.as_ptr().add(g * 16) as *const __m128i);
                let opaque = _mm_cmpeq_epi32(_mm_and_si128(v, am), am);
                let keep = _mm_or_si128(opaque, _mm_cmpeq_epi32(v, z));
                todo |= ((!_mm_movemask_ps(_mm_castsi128_ps(opaque)) & 0xf) as u64) << (g * 4);
                touch |= ((!_mm_movemask_ps(_mm_castsi128_ps(keep)) & 0xf) as u64) << (g * 4);
            }
            (todo, touch)
        }
    }

    /// `composite_scalar` と同じ計算（1 画素でも値が変わったら true）。
    pub(super) fn composite(px: &mut [u8], layers: &[Layer]) -> bool {
        // SAFETY: 使う命令は SSE / SSE2 だけで、x86_64 の既定の命令セットに含まれる
        unsafe { composite_sse2(px, layers) }
    }

    /// 64 画素そろったまとまり（`base` バイト目から。`base` ≥ 4）を 4 画素ずつ読み、
    /// 「1 つ前の画素と 4 バイトとも同じ」のビット（ビット 0 は前のまとまりの最後と比べる）と、α が 0 でない画素があるかを返す。
    #[inline(always)]
    pub(super) fn full_chunk(p: &[u8], base: usize) -> (u64, bool) {
        let s = &p[base - 4..base + CHUNK * 4];
        // SAFETY: 読むのは s の中だけ（g ≤ 15 で 4 + 15 × 16 + 16 = s.len()）。命令は SSE2 だけ（x86_64 の既定）
        unsafe {
            let mut m = 0u64;
            let mut acc = _mm_setzero_si128();
            for g in 0..CHUNK / 4 {
                let cur = _mm_loadu_si128(s.as_ptr().add(4 + g * 16) as *const __m128i);
                let prev = _mm_loadu_si128(s.as_ptr().add(g * 16) as *const __m128i);
                let eq = _mm_movemask_ps(_mm_castsi128_ps(_mm_cmpeq_epi32(cur, prev))) as u64;
                m |= eq << (g * 4);
                acc = _mm_or_si128(acc, cur);
            }
            let alpha = _mm_and_si128(acc, _mm_set1_epi32(0xff00_0000u32 as i32));
            let any_alpha = _mm_movemask_epi8(_mm_cmpeq_epi32(alpha, _mm_setzero_si128())) != 0xffff;
            (m, any_alpha)
        }
    }

    /// まとまり（`base` バイト目から `cnt` 画素）の各画素が、1 つ前の画素と 4 バイトとも同じなら、そのビットを立てる
    /// （ビット j = 画素 j。ビット 0 は立てない）。64 画素そろったまとまりは SSE2 で 4 画素ずつ比べる。
    #[inline(always)]
    fn eq_prev(p: &[u8], base: usize, cnt: usize) -> u64 {
        if cnt == CHUNK && base >= 4 {
            full_chunk(p, base).0 & !1
        } else {
            let mut m = 0u64;
            for j in 1..cnt {
                let o = base + j * 4;
                if p[o..o + 4] == p[o - 4..o] {
                    m |= 1 << j;
                }
            }
            m
        }
    }

    /// `eq_prev` を、残像の元の 1 枚（印 `s`）のまとまり b について。積むときに数えた印を引くだけで、画素は読まない。
    #[inline(always)]
    fn eq_prev_frame(s: &Summary, b: usize) -> u64 {
        s.eqm[b] & !1
    }

    /// 残像の元の 1 枚（印 `s`）で、まとまり b の最初の画素が 1 つ前の画素（まとまり b − 1 の最後）と同じ 4 バイトか。
    #[inline(always)]
    fn first_eq_prev(s: &Summary, b: usize) -> bool {
        s.eqm[b] & 1 != 0
    }

    unsafe fn composite_sse2(px: &mut [u8], layers: &[Layer]) -> bool {
        let u = unit_table();
        let mut changed = false;
        let n = px.len() / 4;
        let mut d = [_mm_setzero_ps(); CHUNK];
        // 計算する画素の位置（バイト）
        let mut uniq = [0usize; CHUNK];
        let mut start = 0;
        let mut block = 0;
        // 1 つ前のまとまりの最後の画素の、書き換える前の今の絵
        let mut prev_cur = [0u8; 4];
        while start < n {
            let end = (start + CHUNK).min(n);
            let tail = (end - 1) * 4;
            let prev = std::mem::replace(&mut prev_cur, [px[tail], px[tail + 1], px[tail + 2], px[tail + 3]]);
            let (base, cnt) = (start * 4, end - start);
            if !layers.iter().any(|l| l.covers(block)) {
                changed |= bare(px, base, cnt);
                start = end;
                block += 1;
                continue;
            }
            // 今の絵が不透明でない画素（ビット j = 画素 start + j）。不透明な画素は答えが今の絵そのもの（`tests::opaque_current_wins`）
            let todo = if cnt == CHUNK {
                alpha_masks(px, base).0
            } else {
                (0..cnt).fold(0u64, |t, j| t | ((px[base + j * 4 + 3] != 255) as u64) << j)
            };
            start = end;
            let b = block;
            block += 1;
            if todo == 0 {
                continue;
            }
            // 同じ入力の画素をまとめる（単色の図形・文字のように同じ画素が続く絵では大半がここで済む）。
            // 1 画素の答えは、今の絵と、残像の元の画素（`Src` が読む 1〜2 枚の画素）と、残像ごとの定数だけで決まる。
            // 定数はまとまりの中で同じなので、1 つ前の画素と元の画素がすべて同じなら答えも同じ（`tests::matches_v110_render_flat`）。
            // 今の絵が同じなら 1 つ前の画素も不透明でなく、計算する側に入っている。
            // 掛からない残像（`covers` が偽）はまとまりの中がすべて透明で、比べなくても答えに効かない
            let mut same = eq_prev(px, base, cnt) & todo;
            for l in layers {
                if same == 0 {
                    break;
                }
                if !l.covers(b) {
                    continue;
                }
                same &= match l.src {
                    Src::One(_) => eq_prev_frame(l.sum[0], b),
                    Src::Two(..) => eq_prev_frame(l.sum[0], b) & eq_prev_frame(l.sum[1], b),
                };
            }
            // まとまりの最初の画素は、1 つ前のまとまりの最後の画素（答えはもう px に書いてある）と比べる。
            // 掛かる残像がまとまりごとに違うので、ここではすべての残像を見る。2 つのまとまりのどちらにも掛からない残像は、
            // 2 つの画素で透明で、どちらの答えにも効かない（`bare_chunk` の答えも同じ式）。掛かる残像は元の画素を比べる
            let mut word = [0u8; 4];
            if todo & 1 != 0
                && base >= 4
                && px[base..base + 4] == prev
                && layers.iter().all(|l| {
                    !(l.covers(b) || l.covers(b - 1))
                        || match l.src {
                            Src::One(_) => first_eq_prev(l.sum[0], b),
                            Src::Two(..) => first_eq_prev(l.sum[0], b) && first_eq_prev(l.sum[1], b),
                        }
                })
            {
                same |= 1;
                word.copy_from_slice(&px[base - 4..base]);
            }
            let calc = todo & !same;
            let mut k = 0;
            let mut bits = calc;
            while bits != 0 {
                uniq[k] = base + bits.trailing_zeros() as usize * 4;
                k += 1;
                bits &= bits - 1;
            }
            let (d, uidx) = (&mut d[..k], &uniq[..k]);
            d.fill(_mm_setzero_ps());
            for l in layers {
                if !l.covers(b) {
                    continue;
                }
                match l.look {
                    Look::Tint { r, g, b, fade } => {
                        let (t, fade) = (_mm_set_ps(0.0, b, g, r), _mm_set1_ps(fade));
                        for (dj, &o) in d.iter_mut().zip(uidx) {
                            let s = sample(&l.src, o);
                            if lane3(s) == 0.0 {
                                continue;
                            }
                            let e = tint(s, t, fade);
                            if lane3(e) == 0.0 {
                                // α が 0 の残像は重ねても変わらない（over_px の sa <= 0）
                                continue;
                            }
                            *dj = over(*dj, e);
                        }
                    }
                    Look::Fade(fade) => {
                        for (dj, &o) in d.iter_mut().zip(uidx) {
                            let s = sample(&l.src, o);
                            let a = lane3(s);
                            if a == 0.0 {
                                continue;
                            }
                            // scale_px と同じ式（α だけを薄める。0 になったら重ねても変わらない）
                            let a8 = super::r8((u[a as usize] * fade).clamp(0.0, 1.0) * 255.0);
                            if a8 == 0 {
                                continue;
                            }
                            *dj = over(*dj, with_alpha(s, a8 as f32));
                        }
                    }
                }
            }
            for (dj, &o) in d.iter_mut().zip(uidx) {
                let cur = load(px, o);
                if lane3(cur) != 0.0 {
                    *dj = over(*dj, cur);
                }
            }
            // 書き戻しは今の絵を読み終えた後。same の画素は 1 つ前の画素（これも todo に入っている）と同じ答え
            let mut kk = 0;
            let mut bits = todo;
            while bits != 0 {
                let j = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                if (calc >> j) & 1 != 0 {
                    word = pack(d[kk]);
                    kk += 1;
                }
                let o = base + j * 4;
                if px[o..o + 4] != word {
                    px[o..o + 4].copy_from_slice(&word);
                    changed = true;
                }
            }
        }
        changed
    }
}

/// 1 オブジェクト分の処理（上限なし）。上限つきは `StateStore::process`。
#[cfg(test)]
pub fn process(state: &mut TrailState, pixels: &mut [u8], w: u32, h: u32, p: &ProcessParams) -> bool {
    ingest(state, pixels, w, h, p, true);
    render(state, pixels, w, h, p)
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
        space: 0,
        hue_long: false,
    }
}

/// v1.1.0 の合成（画面全体を残像ごとに写し・色付け・重ねる）。新しい `render` と 1 ビットも違わないことを確かめる相手。
#[cfg(test)]
mod v110 {
    use super::*;

    fn lerp_into(a: &[u8], b: &[u8], t: f32, out: &mut [u8]) {
        let inv = 1.0 - t;
        for i in 0..out.len() {
            out[i] = (a[i] as f32 * inv + b[i] as f32 * t).round() as u8;
        }
    }

    fn sample_into(state: &TrailState, target: f64, out: &mut [u8]) -> bool {
        if !target.is_finite() {
            return false;
        }
        let (lower, upper) = state.neighbors(target);
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
        if a.frame == target.floor() as i32 {
            out.copy_from_slice(&a.pixels);
            return true;
        }
        false
    }

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

    pub(super) fn over(dst: &mut [u8], src: &[u8]) {
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

    pub(super) fn render(state: &TrailState, pixels: &mut [u8], w: u32, h: u32, p: &ProcessParams) {
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
        let can_sample = state.width == w && state.height == h;
        for i in (1..=max_frames).rev() {
            if !can_sample {
                break;
            }
            let frames_back = interval * i as f64;
            let target = p.frame as f64 - frames_back;
            if !sample_into(state, target, &mut scratch) {
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
        over(&mut out, &current);
        px.copy_from_slice(&out);
    }

    /// v1.1.0 の履歴の積み方（積んでから捨てる）。
    pub(super) fn ingest(state: &mut TrailState, pixels: &[u8], w: u32, h: u32, p: &ProcessParams) {
        let len = frame_bytes(w, h).unwrap();
        let (max_frames, interval, _) = normalized(p);
        state.store_window(p.frame, &pixels[..len], w, h, i32::MIN);
        let keep = keep_window(interval, max_frames);
        state.prune(p.frame.saturating_sub(keep));
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

    #[test]
    fn unchanged_when_nothing_to_draw() {
        // 1 枚目（履歴なし）の不透明な絵・残像が今の不透明な絵に隠れる: 書き換えない（スクリプトは putpixeldata を省く）
        let mut st = TrailState::new();
        let mut c0 = vec![10u8, 20, 30, 255, 40, 50, 60, 255];
        assert!(!process(&mut st, &mut c0, 2, 1, &test_params(0, 12, 1.0)));
        let mut c1 = vec![11u8, 21, 31, 255, 41, 51, 61, 255];
        assert!(!process(&mut st, &mut c1, 2, 1, &test_params(1, 12, 1.0)));
        assert_eq!(c1, vec![11u8, 21, 31, 255, 41, 51, 61, 255]);
        // 今の絵が透明な所に残像が出る: 書き換える
        let mut c2 = vec![0u8, 0, 0, 0, 41, 51, 61, 255];
        assert!(process(&mut st, &mut c2, 2, 1, &test_params(2, 12, 1.0)));
        // 透明（色だけ残る）の画素は透明に揃う（v1.1.0 と同じ）ので、書き換えたことになる
        let mut st2 = TrailState::new();
        let mut c3 = vec![9u8, 9, 9, 0];
        assert!(process(&mut st2, &mut c3, 1, 1, &test_params(0, 12, 1.0)));
        assert_eq!(c3, vec![0u8, 0, 0, 0]);
    }

    #[test]
    fn opaque_current_wins() {
        // 不透明な今の絵を最後に重ねると、下に何があっても今の絵そのもの（render が残像を計算しない根拠）
        let downs: [[u8; 4]; 5] = [[0, 0, 0, 0], [255, 255, 255, 255], [10, 200, 30, 1], [77, 3, 250, 128], [255, 0, 255, 254]];
        for v in 0..=255u8 {
            for (k, d0) in downs.iter().enumerate() {
                let cur = [v, v.wrapping_mul(7), v.wrapping_add(k as u8 * 50), 255];
                let mut d = d0.to_vec();
                v110::over(&mut d, &cur);
                assert_eq!(d, cur.to_vec(), "v={v} d={d0:?}");
            }
        }
    }

    #[test]
    fn r8_matches_round() {
        // 0〜255 の 0.5 の境目の前後と、範囲の外・NaN・無限大
        let mut xs = vec![f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.0, -0.5, -0.49999997, -1.5, 255.5, 256.0, 1e30, -1e30];
        for k in 0..=256 {
            let b = k as f32 + 0.5;
            let mut x = b;
            let mut y = b;
            for _ in 0..64 {
                x = f32::from_bits(x.to_bits() + 1);
                y = f32::from_bits(y.to_bits() - 1);
                xs.push(x);
                xs.push(y);
            }
            xs.push(b);
            xs.push(k as f32);
        }
        for x in xs {
            assert_eq!(r8(x), x.round().clamp(0.0, 255.0) as u8, "x = {x:e}");
            if (0.0..=256.0).contains(&x) {
                assert_eq!(r8(x), x.round() as u8, "x = {x:e}");
            }
        }
    }

    /// すべての f32（約 43 億）で r8 と round を比べる。release で十数秒。
    ///     cargo test --release --lib r8_exhaustive -- --ignored
    #[test]
    #[ignore]
    fn r8_exhaustive() {
        for bits in 0..=u32::MAX {
            let x = f32::from_bits(bits);
            assert_eq!(r8(x), x.round().clamp(0.0, 255.0) as u8, "bits = {bits:#x}");
        }
    }

    #[test]
    fn summary_marks_blocks() {
        // 200 画素（まとまり 64・64・64・8）。2 つ目と 4 つ目だけに α のある画素
        let mut v = vec![0u8; 200 * 4];
        v[(70 * 4) + 3] = 1;
        v[(199 * 4) + 3] = 255;
        v[10 * 4] = 200; // 色だけ（α 0）は数えない
        let mut s = Summary::default();
        summarize(&v, &mut s);
        assert_eq!(s.occ, vec![0b1010]);
        assert!(!s.occupied(0) && s.occupied(1) && !s.occupied(2) && s.occupied(3) && !s.occupied(64));
        // 1 つ前の画素と同じか（ビット 0 は前のまとまりの最後と比べる。まとまり 0 では立たない）
        assert_eq!(s.eqm, vec![!0u64 & !1 & !(1 << 10) & !(1 << 11), !0u64 & !(1 << 6) & !(1 << 7), !0u64, 0x7f]);
        // 少ない色が続く絵で、素直に数えたものと同じ
        let mut rng = Rng(0x77);
        for _ in 0..50 {
            let n = 1 + (rng.next() % 700) as usize;
            let f = flat_frame(&mut rng, n);
            summarize(&f, &mut s);
            let px: Vec<&[u8]> = f.chunks_exact(4).collect();
            for (b, &m) in s.eqm.iter().enumerate() {
                let any = px[b * CHUNK..((b + 1) * CHUNK).min(n)].iter().any(|c| c[3] != 0);
                assert_eq!(s.occupied(b), any, "n={n} b={b}");
                for j in 0..CHUNK {
                    let i = b * CHUNK + j;
                    let want = i < n && i > 0 && px[i] == px[i - 1];
                    assert_eq!((m >> j) & 1 != 0, want, "n={n} b={b} j={j}");
                }
            }
            assert_eq!(s.eqm.len(), n.div_ceil(CHUNK));
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn simd_matches_scalar() {
        // 同じ残像の並びを、スカラーと SSE2 で重ねて 1 ビットも違わないこと（ふだんの値・端の値・NaN・無限大）
        let mut rng = Rng(0x2545f4914f6cdd1d);
        let n = 64 * 5 + 17;
        for case in 0..300 {
            let frames: Vec<Vec<u8>> = (0..6)
                .map(|_| if case % 2 == 0 { random_frame(&mut rng, n) } else { sparse_frame(&mut rng, n) })
                .collect();
            let sums: Vec<Summary> = frames
                .iter()
                .map(|f| {
                    let mut s = Summary::default();
                    summarize(f, &mut s);
                    s
                })
                .collect();
            let special = [0.0f32, 1.0, -0.5, 2.0, f32::NAN, f32::INFINITY, 1e-7, 0.999_999_9];
            let val = |rng: &mut Rng| -> f32 {
                if rng.next() % 6 == 0 {
                    special[(rng.next() % special.len() as u64) as usize]
                } else {
                    rng.unit() as f32 * 1.3
                }
            };
            let mut layers = Vec::new();
            for _ in 0..(1 + rng.next() % 12) {
                let a = (rng.next() % 6) as usize;
                let b = (rng.next() % 6) as usize;
                let (src, sum) = if rng.next() % 2 == 0 {
                    (Src::One(&frames[a]), [&sums[a], &sums[a]])
                } else {
                    (Src::Two(&frames[a], &frames[b], (rng.unit() as f32).clamp(0.0, 1.0)), [&sums[a], &sums[b]])
                };
                let look = if rng.next() % 2 == 0 {
                    Look::Tint { r: val(&mut rng), g: val(&mut rng), b: val(&mut rng), fade: val(&mut rng) }
                } else {
                    Look::Fade(val(&mut rng))
                };
                layers.push(Layer { src, look, sum });
            }
            let cur = if case % 3 == 0 { sparse_frame(&mut rng, n) } else { random_frame(&mut rng, n) };
            let mut a = cur.clone();
            let ca = composite_scalar(&mut a, &layers);
            let mut b = cur.clone();
            let cb = simd::composite(&mut b, &layers);
            assert_eq!(a, b, "case {case}");
            assert_eq!((ca, cb), (a != cur, b != cur), "case {case}: 変わったかの答え");
        }
    }

    #[test]
    fn reuses_pruned_buffer() {
        let mut st = TrailState::new();
        for f in 0..20 {
            ingest(&mut st, &[f as u8; 16], 2, 2, &test_params(f, 3, 1.0), true);
        }
        // 窓 5（3 + 2）で 6 枚。中身は新しい絵
        assert_eq!(st.frame_count(), 6);
        assert_eq!(st.bytes(), 6 * 16);
        assert_eq!(st.frames.back().unwrap().pixels, vec![19u8; 16]);
        assert_eq!(st.frames.front().unwrap().pixels, vec![14u8; 16]);
    }

    /// 小さな乱数（xorshift）。
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn byte(&mut self) -> u8 {
            (self.next() >> 24) as u8
        }
        fn unit(&mut self) -> f64 {
            (self.next() >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    /// 64 画素のまとまりの半分ほどが透明（色だけ残る）で、残りは `random_frame` と同じ絵。
    /// 残像の掛からないまとまり（`bare_chunk`）と、掛かるまとまりの両方を通す。
    fn sparse_frame(rng: &mut Rng, n: usize) -> Vec<u8> {
        let mut v = random_frame(rng, n);
        for block in v.chunks_mut(CHUNK * 4) {
            if rng.next() % 2 == 0 {
                for c in block.chunks_exact_mut(4) {
                    c[3] = 0;
                }
            }
        }
        v
    }

    /// 透明・半透明・不透明が混ざる絵（α は 0 と 255 が多め）。
    fn random_frame(rng: &mut Rng, n: usize) -> Vec<u8> {
        let mut v = vec![0u8; n * 4];
        for c in v.chunks_exact_mut(4) {
            let a = match rng.next() % 4 {
                0 => 0,
                1 => 255,
                _ => rng.byte(),
            };
            c.copy_from_slice(&[rng.byte(), rng.byte(), rng.byte(), a]);
        }
        v
    }

    /// 少ない色（透明・不透明・半透明の 4〜5 色）が 1〜300 画素ずつ続く絵（単色の図形や文字に近い）。
    /// 同じ入力の画素をまとめる道（`simd::composite_sse2` の `same`）を通す。
    fn flat_frame(rng: &mut Rng, n: usize) -> Vec<u8> {
        let mut palette = vec![[0u8, 0, 0, 0], [255, 255, 255, 255], [rng.byte(), rng.byte(), rng.byte(), 255], [rng.byte(), rng.byte(), rng.byte(), rng.byte()]];
        if rng.next() % 3 == 0 {
            // 色だけ残る透明
            palette.push([rng.byte(), rng.byte(), rng.byte(), 0]);
        }
        let mut v = Vec::with_capacity(n * 4);
        while v.len() < n * 4 {
            let c = palette[(rng.next() % palette.len() as u64) as usize];
            // 長い続き（まとまり 64 画素がすべて同じ = `Summary::uniform`）も混ぜる
            let run = 1 + (rng.next() % if rng.next() % 3 == 0 { 300 } else { 80 }) as usize;
            for _ in 0..run {
                if v.len() < n * 4 {
                    v.extend_from_slice(&c);
                }
            }
        }
        v
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn simd_matches_scalar_flat() {
        // 同じ画素が続く絵で、まとめる道（SSE2 だけにある）がスカラー（まとめない）と 1 ビットも違わないこと
        let mut rng = Rng(0x1234_5678_9abc_def1);
        let n = 64 * 6 + 5;
        let mut merged = 0usize;
        let mut uniform_blocks = 0usize;
        for case in 0..300 {
            let frames: Vec<Vec<u8>> = (0..6).map(|_| flat_frame(&mut rng, n)).collect();
            let sums: Vec<Summary> = frames
                .iter()
                .map(|f| {
                    let mut s = Summary::default();
                    summarize(f, &mut s);
                    s
                })
                .collect();
            uniform_blocks += sums.iter().map(|s| s.eqm.iter().filter(|&&m| m | 1 == !0).count()).sum::<usize>();
            let mut layers = Vec::new();
            for _ in 0..(1 + rng.next() % 12) {
                let a = (rng.next() % 6) as usize;
                let b = (rng.next() % 6) as usize;
                let (src, sum) = if rng.next() % 2 == 0 {
                    (Src::One(&frames[a]), [&sums[a], &sums[a]])
                } else {
                    (Src::Two(&frames[a], &frames[b], (rng.unit() as f32).clamp(0.0, 1.0)), [&sums[a], &sums[b]])
                };
                let look = if rng.next() % 2 == 0 {
                    Look::Tint { r: rng.unit() as f32 * 1.2, g: rng.unit() as f32, b: rng.unit() as f32, fade: rng.unit() as f32 }
                } else {
                    Look::Fade(rng.unit() as f32 * 1.1)
                };
                layers.push(Layer { src, look, sum });
            }
            let cur = flat_frame(&mut rng, n);
            merged += cur.chunks_exact(4).zip(cur.chunks_exact(4).skip(1)).filter(|(x, y)| x == y).count();
            let mut a = cur.clone();
            let ca = composite_scalar(&mut a, &layers);
            let mut b = cur.clone();
            let cb = simd::composite(&mut b, &layers);
            assert_eq!(a, b, "case {case}");
            assert_eq!((ca, cb), (a != cur, b != cur), "case {case}: 変わったかの答え");
        }
        // 同じ画素が続く所が十分にある（まとめる道を通っている）。すべて同じ画素のまとまりも 1 割を超える
        assert!(merged > 300 * n / 2, "{merged}");
        assert!(uniform_blocks > 300 * 6 * 7 / 10, "{uniform_blocks}");
    }

    #[test]
    fn matches_v110_render_flat() {
        // 同じ画素が続く絵（単色の図形）で、v1.1.0 と出力・履歴が 1 ビットも違わないこと
        check_v110(0x51ed_270b_27a3_e1c9, 200, |rng, n, _| flat_frame(rng, n));
    }

    #[test]
    fn matches_v110_render() {
        // v1.1.0 の積み方・合成と、新しい積み方・合成を同じ入力で回し、出力と履歴が 1 ビットも違わないこと
        check_v110(0x9e3779b97f4a7c15, 400, |rng, n, case| if case % 2 == 0 { random_frame(rng, n) } else { sparse_frame(rng, n) });
    }

    fn check_v110(seed: u64, ncases: usize, gen: fn(&mut Rng, usize, usize) -> Vec<u8>) {
        let mut rng = Rng(seed);
        let mut cases = 0;
        for case in 0..ncases {
            // 小さい絵（まとまり 1 つ）と、まとまりが 8 つの絵（半分ほどのまとまりが透明）
            let (w, h) = if case % 2 == 0 { (7u32, 5u32) } else { (150u32, 3u32) };
            let n = (w * h) as usize;
            let mut p = test_params(0, 1 + (rng.next() % 14) as i32, 1.0);
            p.interval = match case % 5 {
                0 => 1.0,
                1 => 0.5,
                2 => 0.01 + rng.unit() * 3.0,
                3 => 2.0,
                _ => 0.25,
            };
            p.decay = match case % 7 {
                0 => 0.0,
                1 => 1.0,
                _ => rng.unit(),
            };
            p.use_color = rng.next() % 2 == 0;
            p.use_grad = rng.next() % 2 == 0;
            p.r1 = rng.unit() as f32 * 1.2;
            p.g1 = rng.unit() as f32;
            p.b1 = rng.unit() as f32;
            p.a1 = rng.unit() as f32 * 1.1;
            p.r2 = rng.unit() as f32;
            p.g2 = rng.unit() as f32;
            p.b2 = rng.unit() as f32;
            p.a2 = if case % 11 == 0 { f32::NAN } else { rng.unit() as f32 };
            // 色空間は既定（sRGB）。色相経路は sRGB では効かないので、長経路を混ぜても v1.1.0 と同じ（乱数の並びは変えない）
            p.space = 0;
            p.hue_long = case % 3 == 0;
            let mut old = TrailState::new();
            let mut new = TrailState::new();
            let mut frame = (rng.next() % 50) as i32;
            for _ in 0..30 {
                // ふだんは 1 ずつ進む。ときどき抜ける・同じフレームを描き直す・戻る
                frame += match rng.next() % 12 {
                    0 => 0,
                    1 => 2 + (rng.next() % 4) as i32,
                    2 => -3,
                    _ => 1,
                };
                p.frame = frame;
                let cur = gen(&mut rng, n, case);
                let mut a = cur.clone();
                v110::ingest(&mut old, &a, w, h, &p);
                v110::render(&old, &mut a, w, h, &p);
                let mut b = cur.clone();
                ingest(&mut new, &b, w, h, &p, true);
                let changed = render(&new, &mut b, w, h, &p);
                assert_eq!(a, b, "case {case} frame {frame}");
                assert_eq!(changed, b != cur, "case {case} frame {frame}: 変わったかの答え");
                assert_eq!(old.frame_count(), new.frame_count());
                assert_eq!(old.bytes(), new.bytes());
                cases += 1;
            }
        }
        assert_eq!(cases, ncases * 30);
    }

    /// 色空間（v1.3.0）が効くのは「色を適用」と「グラデーション」が両方 ON のときだけ。
    /// どちらかが OFF なら、どの色空間・色相経路でも sRGB（v1.2.0）と 1 ビットも違わない
    #[test]
    fn colorspace_only_affects_gradient_tint() {
        let mut rng = Rng(0x0dd_c010);
        let (w, h) = (150u32, 3u32);
        let n = (w * h) as usize;
        let frames: Vec<Vec<u8>> = (0..20).map(|k| if k % 2 == 0 { random_frame(&mut rng, n) } else { sparse_frame(&mut rng, n) }).collect();
        let run = |p: &ProcessParams| -> Vec<Vec<u8>> {
            let mut st = TrailState::new();
            let mut outs = Vec::new();
            for (f, cur) in frames.iter().enumerate() {
                let q = ProcessParams { frame: f as i32, ..*p };
                let mut b = cur.clone();
                ingest(&mut st, &b, w, h, &q, true);
                render(&st, &mut b, w, h, &q);
                outs.push(b);
            }
            outs
        };
        let mut base = test_params(0, 8, 1.0);
        base.r1 = 1.0;
        base.g1 = 0.0;
        base.b1 = 0.0;
        base.r2 = 0.0;
        base.g2 = 0.0;
        base.b2 = 1.0;
        let mut changed = 0;
        for (use_color, use_grad) in [(false, false), (true, false), (false, true), (true, true)] {
            let p0 = ProcessParams { use_color, use_grad, ..base };
            let want = run(&p0);
            for space in [1, 2, 3, 4, 5, 6, 7, 8, 9] {
                for hue_long in [false, true] {
                    let got = run(&ProcessParams { space, hue_long, ..p0 });
                    if use_color && use_grad {
                        changed += (got != want) as usize;
                    } else {
                        assert!(got == want, "色 {use_color} グラデ {use_grad} 色空間 {space} 長経路 {hue_long}");
                    }
                }
            }
        }
        // 赤→青 なので、どの色空間でも中間の残像の色は sRGB の lerp と違う（効いていることの確かめ）
        assert_eq!(changed, 18);
    }

    /// 残像ごとの色: sRGB（None）は v1.2.0 の f32 の式。色空間では両端（age 0・1）が色1・色2 のまま
    #[test]
    fn echo_color_ends_and_default() {
        let mut p = test_params(0, 12, 1.0);
        p.use_color = true;
        p.use_grad = true;
        (p.r1, p.g1, p.b1, p.a1) = (0.8, 0.1, 0.3, 1.0);
        (p.r2, p.g2, p.b2, p.a2) = (0.1, 0.7, 0.9, 0.2);
        for age in [0.0f32, 0.25, 0.5, 1.0] {
            let want = (p.r1 + (p.r2 - p.r1) * age, p.g1 + (p.g2 - p.g1) * age, p.b1 + (p.b2 - p.b1) * age, p.a1 + (p.a2 - p.a1) * age);
            assert_eq!(echo_color(&p, None, age), want);
        }
        assert!(gradient_of(&p).is_none(), "sRGB では色空間の道を通らない");
        for space in 1..=9 {
            p.space = space;
            let g = gradient_of(&p).expect("色空間の道");
            for (age, want) in [(0.0f32, (p.r1, p.g1, p.b1, p.a1)), (1.0, (p.r2, p.g2, p.b2, p.a2))] {
                let got = echo_color(&p, Some(&g), age);
                let d = [(got.0 - want.0).abs(), (got.1 - want.1).abs(), (got.2 - want.2).abs(), (got.3 - want.3).abs()];
                assert!(d.iter().all(|&x| x < 1e-5), "space {space} age {age}: {got:?} / {want:?}");
            }
        }
    }
}
