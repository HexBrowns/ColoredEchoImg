//! 色1→色2 の補間の色空間と色相経路（v1.3.0〜）
//!
//! 式は共有部品の写し。**正本は `Script/EffectUtils_ColorSpace.lua`**（HLSL の同じ式は `AI/tools/assets/colorspace.hlsl`。
//! 式の正しさは CSS Color 4 の値などで `AI/scripts/色空間拡張/verify_colorspace.py` が見る）。
//! 正本を直したら、ここも同じに直す。正本との一致は `AI/scripts/色付き残像/verify_colorspace_ext.py` が
//! `examples/colorspace_dump.rs` の出力と Lua の値を突き合わせて確かめる。
//!
//! Lua（LuaJIT）と同じ f64 で、同じ順に計算する。立方根も Lua と同じ `x ^ (1/3)`（`powf`）で求める（`cbrt` とは末尾の桁が違う）。
//! 補間に要る分（変換・逆変換・補間）だけを写す。色相回転・鮮やかさ・色差は写さない。
//!
//! 色空間の番号（全スクリプト共通）: `0 sRGB / 1 リニア / 2 Oklab / 3 HSV / 4 HSL / 5 Lab / 6 LCh / 7 Oklch / 8 XYZ / 9 HWB`。
//! 色相のある色空間（3 / 4 / 6 / 7 / 9）は 1 番目に色相（0〜1 周）を置く。Lab は D65 の白。

const TAU: f64 = std::f64::consts::TAU;

pub const SRGB: i32 = 0;

/// Lua の `if v < 0 … elseif v > 1 …` と同じ（NaN はそのまま返る）
fn clamp01(v: f64) -> f64 {
    v.clamp(0.0, 1.0)
}

fn frac(x: f64) -> f64 {
    x - x.floor()
}

/// Lua の `x ^ (1/3)`（負の数は符号を外して戻す）
fn cbrt(x: f64) -> f64 {
    if x < 0.0 {
        -(-x).powf(1.0 / 3.0)
    } else {
        x.powf(1.0 / 3.0)
    }
}

fn lin1(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c.abs() + 0.055) / 1.055).powf(2.4)
    }
}

fn srgb1(c: f64) -> f64 {
    if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.abs().powf(1.0 / 2.4) - 0.055
    }
}

type V3 = [f64; 3];

fn to_linear([r, g, b]: V3) -> V3 {
    [lin1(r), lin1(g), lin1(b)]
}

/// リニア → sRGB。範囲外は切り詰める
fn to_srgb([r, g, b]: V3) -> V3 {
    [srgb1(clamp01(r)), srgb1(clamp01(g)), srgb1(clamp01(b))]
}

fn linear_to_oklab([r, g, b]: V3) -> V3 {
    let l = cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
    let m = cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
    let s = cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}

fn oklab_to_linear([ll, a, b]: V3) -> V3 {
    let l = ll + 0.3963377774 * a + 0.2158037573 * b;
    let m = ll - 0.1055613458 * a - 0.0638541728 * b;
    let s = ll - 0.0894841775 * a - 1.2914855480 * b;
    let (l, m, s) = (l * l * l, m * m * m, s * s * s);
    [
        4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
        -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
        -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s,
    ]
}

fn linear_to_xyz([r, g, b]: V3) -> V3 {
    [
        0.4124564 * r + 0.3575761 * g + 0.1804375 * b,
        0.2126729 * r + 0.7151522 * g + 0.0721750 * b,
        0.0193339 * r + 0.1191920 * g + 0.9503041 * b,
    ]
}

fn xyz_to_linear([x, y, z]: V3) -> V3 {
    [
        3.2404542 * x - 1.5371385 * y - 0.4985314 * z,
        -0.9692660 * x + 1.8760108 * y + 0.0415560 * z,
        0.0556434 * x - 0.2040259 * y + 1.0572252 * z,
    ]
}

/// CIE 15: ε = 216/24389、κ = 24389/27
fn lab_f(t: f64) -> f64 {
    if t > 0.008856452 {
        cbrt(t)
    } else {
        (903.2963 * t + 16.0) / 116.0
    }
}

fn lab_finv(f: f64) -> f64 {
    let f3 = f * f * f;
    if f3 > 0.008856452 {
        f3
    } else {
        (116.0 * f - 16.0) / 903.2963
    }
}

fn xyz_to_lab([x, y, z]: V3) -> V3 {
    let (fx, fy, fz) = (lab_f(x / 0.95047), lab_f(y), lab_f(z / 1.08883));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

fn lab_to_xyz([l, a, b]: V3) -> V3 {
    let fy = (l + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;
    [lab_finv(fx) * 0.95047, lab_finv(fy), lab_finv(fz) * 1.08883]
}

/// (L, a, b) → (色相 0〜1 周, C, L)
fn lab_to_polar([l, a, b]: V3) -> V3 {
    [frac(b.atan2(a) / TAU + 1.0), (a * a + b * b).sqrt(), l]
}

fn polar_to_lab([h, c, l]: V3) -> V3 {
    [l, c * (h * TAU).cos(), c * (h * TAU).sin()]
}

fn hue_of(r: f64, g: f64, b: f64, mx: f64, mn: f64) -> f64 {
    let d = mx - mn;
    let mut h = 0.0;
    if d > 0.000001 {
        h = if mx == r {
            (g - b) / d
        } else if mx == g {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
    }
    frac(h / 6.0 + 1.0)
}

fn hue_rgb(h: f64) -> V3 {
    let ch = |o: f64| clamp01((frac(h + o) * 6.0 - 3.0).abs() - 1.0);
    [ch(1.0), ch(2.0 / 3.0), ch(1.0 / 3.0)]
}

fn max3(r: f64, g: f64, b: f64) -> f64 {
    // Lua の math.max(r, g, b) と同じ順に比べる
    let mut m = r;
    if g > m {
        m = g;
    }
    if b > m {
        m = b;
    }
    m
}

fn min3(r: f64, g: f64, b: f64) -> f64 {
    let mut m = r;
    if g < m {
        m = g;
    }
    if b < m {
        m = b;
    }
    m
}

fn rgb_to_hsv([r, g, b]: V3) -> V3 {
    let (mx, mn) = (max3(r, g, b), min3(r, g, b));
    let s = if mx > 0.000001 { (mx - mn) / mx } else { 0.0 };
    [hue_of(r, g, b, mx, mn), s, mx]
}

fn hsv_to_rgb([h, s, v]: V3) -> V3 {
    let [hr, hg, hb] = hue_rgb(h);
    let s = clamp01(s);
    [v * (1.0 + (hr - 1.0) * s), v * (1.0 + (hg - 1.0) * s), v * (1.0 + (hb - 1.0) * s)]
}

fn rgb_to_hsl([r, g, b]: V3) -> V3 {
    let (mx, mn) = (max3(r, g, b), min3(r, g, b));
    let l = (mx + mn) * 0.5;
    let den = 1.0 - (2.0 * l - 1.0).abs();
    let s = if den > 0.000001 { (mx - mn) / den } else { 0.0 };
    [hue_of(r, g, b, mx, mn), s, l]
}

fn hsl_to_rgb([h, s, l]: V3) -> V3 {
    let chroma = (1.0 - (2.0 * l - 1.0).abs()) * clamp01(s);
    let [hr, hg, hb] = hue_rgb(h);
    [(hr - 0.5) * chroma + l, (hg - 0.5) * chroma + l, (hb - 0.5) * chroma + l]
}

fn rgb_to_hwb([r, g, b]: V3) -> V3 {
    let (mx, mn) = (max3(r, g, b), min3(r, g, b));
    [hue_of(r, g, b, mx, mn), mn, 1.0 - mx]
}

fn hwb_to_rgb([h, w, b]: V3) -> V3 {
    let (mut w, mut b) = (w.max(0.0), b.max(0.0));
    let sum = w + b;
    if sum > 1.0 {
        w /= sum;
        b /= sum;
    }
    let [hr, hg, hb] = hue_rgb(h);
    let k = 1.0 - w - b;
    [hr * k + w, hg * k + w, hb * k + w]
}

fn in_gamut([r, g, b]: V3) -> bool {
    let e = max3(r.abs(), g.abs(), b.abs()) * 0.000002 + 0.000000001;
    r >= -e && g >= -e && b >= -e && r <= 1.0 + e && g <= 1.0 + e && b <= 1.0 + e
}

/// Oklch (h, C, L) をリニアへ。sRGB に無い色は、明るさと色相を保ったまま C を下げて収める（二分探索 16 回）
fn oklch_to_linear_fit([h, c, l]: V3) -> V3 {
    let mut rgb = oklab_to_linear(polar_to_lab([h, c, l]));
    if !in_gamut(rgb) {
        let (mut lo, mut hi) = (0.0, c.max(0.0));
        for _ in 0..16 {
            let mid = (lo + hi) * 0.5;
            if in_gamut(oklab_to_linear(polar_to_lab([h, mid, l]))) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        rgb = oklab_to_linear(polar_to_lab([h, lo, l]));
    }
    rgb
}

/// 色空間の番号を、Lua の `space > 8.5` などの比べ方と同じ区分に揃える（整数で渡る。範囲の外は端へ）
fn norm(space: i32) -> i32 {
    space.clamp(0, 9)
}

/// sRGB（0〜1）→ 色空間の値
pub fn to_space(c: V3, space: i32) -> V3 {
    match norm(space) {
        9 => rgb_to_hwb(c),
        8 => linear_to_xyz(to_linear(c)),
        7 => lab_to_polar(linear_to_oklab(to_linear(c))),
        6 => lab_to_polar(xyz_to_lab(linear_to_xyz(to_linear(c)))),
        5 => xyz_to_lab(linear_to_xyz(to_linear(c))),
        4 => rgb_to_hsl(c),
        3 => rgb_to_hsv(c),
        2 => linear_to_oklab(to_linear(c)),
        1 => to_linear(c),
        _ => c,
    }
}

/// 色空間の値 → sRGB（0〜1）。Oklch だけは C を下げて sRGB に収め、ほかは切り詰める
pub fn from_space(v: V3, space: i32) -> V3 {
    let [r, g, b] = match norm(space) {
        9 => hwb_to_rgb(v),
        8 => return to_srgb(xyz_to_linear(v)),
        7 => return to_srgb(oklch_to_linear_fit(v)),
        6 => return to_srgb(xyz_to_linear(lab_to_xyz(polar_to_lab(v)))),
        5 => return to_srgb(xyz_to_linear(lab_to_xyz(v))),
        4 => hsl_to_rgb(v),
        3 => hsv_to_rgb(v),
        2 => return to_srgb(oklab_to_linear(v)),
        1 => return to_srgb(v),
        _ => v,
    };
    [clamp01(r), clamp01(g), clamp01(b)]
}

fn is_polar(space: i32) -> bool {
    matches!(norm(space), 3 | 4 | 6 | 7 | 9)
}

/// 色相を持たない（無彩色の）値か
fn no_hue(y: f64, z: f64, space: i32) -> bool {
    match norm(space) {
        9 => y + z > 0.9999,
        7 => y < 0.0002,
        6 => y < 0.05,
        _ => y < 0.0001,
    }
}

/// 色空間の値どうしの補間（共有部品の `mix_values`）。a0 / a1 は不透明度で、乗算済みのまま混ぜる。
/// 色相のある色空間では色相だけは乗算せず、無彩色の側は相手の色相に合わせる。`hue_long` で長い方を回る。
fn mix_values(v0: V3, a0: f64, v1: V3, a1: f64, u: f64, space: i32, hue_long: bool) -> (V3, f64) {
    let [mut x0, y0, z0] = v0;
    let [mut x1, y1, z1] = v1;
    let a = a0 + (a1 - a0) * u;
    if is_polar(space) {
        if no_hue(y0, z0, space) {
            x0 = x1;
        } else if no_hue(y1, z1, space) {
            x1 = x0;
        }
        let mut dh = x1 - x0;
        if hue_long {
            if dh > 0.0 && dh < 0.5 {
                dh -= 1.0;
            } else if dh <= 0.0 && dh > -0.5 {
                dh += 1.0;
            }
        } else if dh > 0.5 {
            dh -= 1.0;
        } else if dh < -0.5 {
            dh += 1.0;
        }
        let (y, z) = if a > 0.000001 {
            ((y0 * a0 + (y1 * a1 - y0 * a0) * u) / a, (z0 * a0 + (z1 * a1 - z0 * a0) * u) / a)
        } else {
            (y0 + (y1 - y0) * u, z0 + (z1 - z0) * u)
        };
        return ([frac(x0 + dh * u + 1.0), y, z], a);
    }
    if a > 0.000001 {
        return (
            [
                (x0 * a0 + (x1 * a1 - x0 * a0) * u) / a,
                (y0 * a0 + (y1 * a1 - y0 * a0) * u) / a,
                (z0 * a0 + (z1 * a1 - z0 * a0) * u) / a,
            ],
            a,
        );
    }
    ([x0 + (x1 - x0) * u, y0 + (y1 - y0) * u, z0 + (z1 - z0) * u], a)
}

/// 2 色（色1・色2）を先に色空間へ変換しておいたもの。残像ごとに `at` で引く（変換は 1 回だけ）
#[derive(Clone, Copy, Debug)]
pub struct Gradient {
    v0: V3,
    v1: V3,
    space: i32,
    hue_long: bool,
}

impl Gradient {
    /// c0 / c1 はストレートの sRGB（0〜1）。2 色とも不透明として混ぜる（不透明度1・2 は別に線形で混ぜる）
    pub fn new(c0: V3, c1: V3, space: i32, hue_long: bool) -> Self {
        Self { v0: to_space(c0, space), v1: to_space(c1, space), space, hue_long }
    }

    /// 位置 u（0 で色1、1 で色2）の色（sRGB 0〜1）。共有部品の `mix(r0, g0, b0, 1, r1, g1, b1, 1, u, space, hue_long)` と同じ
    pub fn at(&self, u: f64) -> V3 {
        let (v, _) = mix_values(self.v0, 1.0, self.v1, 1.0, u, self.space, self.hue_long);
        from_space(v, self.space)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: V3, b: V3, tol: f64) -> bool {
        a.iter().zip(b.iter()).all(|(x, y)| (x - y).abs() <= tol)
    }

    #[test]
    fn oklab_of_red_matches_css_color_4() {
        let [l, a, b] = linear_to_oklab(to_linear([1.0, 0.0, 0.0]));
        assert!((l - 0.62796).abs() < 2e-5 && (a - 0.22486).abs() < 2e-5 && (b - 0.12585).abs() < 2e-5, "{l} {a} {b}");
        let [h, c, l] = to_space([1.0, 0.0, 0.0], 7);
        assert!((l - 0.62796).abs() < 2e-5 && (c - 0.25768).abs() < 2e-5 && (h * 360.0 - 29.2339).abs() < 1e-3, "{h} {c} {l}");
    }

    #[test]
    fn lab_uses_d65_white() {
        let [l, a, b] = to_space([1.0, 1.0, 1.0], 5);
        assert!((l - 100.0).abs() < 1e-3 && a.abs() < 1e-3 && b.abs() < 1e-3, "{l} {a} {b}");
        let [l, a, b] = to_space([1.0, 0.0, 0.0], 5);
        assert!((l - 53.24).abs() < 0.01 && (a - 80.09).abs() < 0.01 && (b - 67.20).abs() < 0.01, "{l} {a} {b}");
    }

    #[test]
    fn round_trip_every_space() {
        let cols = [[0.2, 0.4, 0.6], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.5, 0.5, 0.5], [0.9, 0.8, 0.1]];
        for space in 0..=9 {
            for c in cols {
                // XYZ の行列どうしが厳密な逆でないので、Lab・LCh・XYZ では 2e-6 ほどずれる（共有部品も同じ。8bit で 0.01 階調未満）
                let back = from_space(to_space(c, space), space);
                assert!(close(back, c, 1e-5), "space {space} {c:?} -> {back:?}");
            }
        }
    }

    #[test]
    fn ends_are_the_two_colors() {
        let (red, blue) = ([1.0, 0.0, 0.0], [0.0, 0.0, 1.0]);
        for space in 0..=9 {
            for long in [false, true] {
                let g = Gradient::new(red, blue, space, long);
                assert!(close(g.at(0.0), red, 1e-5), "space {space} long {long}: {:?}", g.at(0.0));
                assert!(close(g.at(1.0), blue, 1e-5), "space {space} long {long}: {:?}", g.at(1.0));
            }
        }
    }

    #[test]
    fn hue_paths_go_opposite_ways() {
        // 赤（0°）→ 青（240°）: HSV の短経路はマゼンタ（300°）を、長経路は緑（120°）を通る
        let g = Gradient::new([1.0, 0.0, 0.0], [0.0, 0.0, 1.0], 3, false);
        assert!(close(g.at(0.5), [1.0, 0.0, 1.0], 1e-9), "{:?}", g.at(0.5));
        let g = Gradient::new([1.0, 0.0, 0.0], [0.0, 0.0, 1.0], 3, true);
        assert!(close(g.at(0.5), [0.0, 1.0, 0.0], 1e-9), "{:?}", g.at(0.5));
        // 色相の無い色空間では経路を見ない
        let a = Gradient::new([1.0, 0.0, 0.0], [0.0, 0.0, 1.0], 2, false).at(0.3);
        let b = Gradient::new([1.0, 0.0, 0.0], [0.0, 0.0, 1.0], 2, true).at(0.3);
        assert_eq!(a, b);
    }

    #[test]
    fn gray_takes_the_other_hue() {
        // 白 → 赤 を Oklch で: 白は無彩色なので赤の色相のまま C だけが増える（途中で別の色相を通らない）
        let g = Gradient::new([1.0, 1.0, 1.0], [1.0, 0.0, 0.0], 7, false);
        let red_h = to_space([1.0, 0.0, 0.0], 7)[0];
        for k in 1..10 {
            let h = to_space(g.at(k as f64 / 10.0), 7)[0];
            assert!((h - red_h).abs() < 0.01, "{k}: {h} / {red_h}");
        }
    }

    #[test]
    fn out_of_range_space_numbers_clamp_like_lua() {
        let c = [0.3, 0.6, 0.9];
        assert_eq!(to_space(c, -3), c);
        assert_eq!(to_space(c, 12), to_space(c, 9));
    }
}
