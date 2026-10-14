//! 色1→色2 の補間（v1.3.0）の値を書き出す。`AI/scripts/色付き残像/verify_colorspace_ext.py` が
//! 共有部品の Lua（`Script/EffectUtils_ColorSpace.lua` の `mix`）の値と突き合わせる。
//!
//!     cargo run --release --example colorspace_dump
//!
//! 1 行に 1 件、空白区切り:
//!
//!     mix <色1 hex> <色2 hex> <色空間> <長経路 0/1> <age> <r> <g> <b>
//!     px  <色1 hex> <色2 hex> <色空間> <長経路 0/1> <残像の番号 i> <age> <R8> <G8> <B8> <A8>
//!
//! - `mix`: `trail::echo_color`（`render` が残像ごとに 1 回呼ぶ）の色。色はスクリプトと同じく 8bit ÷ 255 を f32 で渡す
//! - `px`:  `trail::render` の出力の画素。履歴は不透明な白 1 画素（明るさ 1）、今の絵は透明、減衰 1・不透明度 1 なので、
//!   残像 1 枚だけが残る数（数 = 残像の番号 i、それより新しい残像は履歴に無い）で描いた画素がその残像の色そのもの
//!
//! 値は 17 桁で書く（Lua に同じ値を渡せるように）。

use colored_echo_img::colorspace::Gradient;
use colored_echo_img::trail::{echo_color, gradient_of, ingest, render, ProcessParams, TrailState};

fn unit(c: u32) -> (f32, f32, f32) {
    let k = |s: u32| ((c >> s) & 255) as f64 / 255.0;
    (k(16) as f32, k(8) as f32, k(0) as f32)
}

fn params(c1: u32, c2: u32, space: i32, hue_long: bool, max_frames: i32) -> ProcessParams {
    let (r1, g1, b1) = unit(c1);
    let (r2, g2, b2) = unit(c2);
    ProcessParams {
        frame: 0,
        max_frames,
        interval: 1.0,
        decay: 1.0,
        use_color: true,
        r1,
        g1,
        b1,
        a1: 1.0,
        use_grad: true,
        r2,
        g2,
        b2,
        a2: 1.0,
        space,
        hue_long,
    }
}

fn main() {
    // 赤→青（色相が 120° 離れる）・補色・無彩色を含む組・暗い色・sRGB の端に近い色
    let pairs: [(u32, u32); 10] = [
        (0xff0000, 0x0000ff),
        (0xff0000, 0x00ffff),
        (0xffff00, 0x8000ff),
        (0x00ff00, 0xff00ff),
        (0xffffff, 0xff0000),
        (0x000000, 0x00a0ff),
        (0x808080, 0x20c040),
        (0x203040, 0xffe0a0),
        (0x0a0014, 0x140a00),
        (0xfe0101, 0x01fe01),
    ];
    let spaces = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
    let n_echo = 12; // 既定の数
    let age_denom = (n_echo - 1) as f32;
    for (c1, c2) in pairs {
        for space in spaces {
            for hue_long in [false, true] {
                let p = params(c1, c2, space, hue_long, n_echo);
                let grad: Option<Gradient> = gradient_of(&p);
                for i in 1..=n_echo {
                    let age = (i - 1) as f32 / age_denom;
                    let (r, g, b, _) = echo_color(&p, grad.as_ref(), age);
                    println!(
                        "mix {c1:06x} {c2:06x} {space} {} {:.17e} {:.17e} {:.17e} {:.17e}",
                        hue_long as u8, age as f64, r as f64, g as f64, b as f64
                    );
                }
                // render を通した画素: 残像 i 枚目だけを描く（数 = i。履歴は i フレーム前の 1 枚だけ）。
                // 数が変わると age の分母（数 − 1）も変わるので、数 n_echo の分母に合わせた age は mix の行で見る。
                // ここでは数 = n_echo のまま、i フレーム前だけを履歴に置く
                for i in [1, n_echo / 2, n_echo] {
                    let mut st = TrailState::new();
                    let white = [255u8, 255, 255, 255];
                    let mut q = p.clone_with_frame(-i);
                    ingest(&mut st, &white, 1, 1, &q, true);
                    q = p.clone_with_frame(0);
                    let mut cur = [0u8, 0, 0, 0];
                    ingest(&mut st, &cur, 1, 1, &q, true);
                    render(&st, &mut cur, 1, 1, &q);
                    let age = (i - 1) as f32 / age_denom;
                    println!(
                        "px {c1:06x} {c2:06x} {space} {} {i} {:.17e} {} {} {} {}",
                        hue_long as u8, age as f64, cur[0], cur[1], cur[2], cur[3]
                    );
                }
            }
        }
    }
}

trait WithFrame {
    fn clone_with_frame(&self, frame: i32) -> ProcessParams;
}

impl WithFrame for ProcessParams {
    fn clone_with_frame(&self, frame: i32) -> ProcessParams {
        ProcessParams { frame, ..*self }
    }
}
