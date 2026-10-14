//! モジュールの中の 1 フレームの時間の目安（FHD・既定の設定）。本体の getpixeldata / putpixeldata は入らない。
//!
//!     cargo run --release --example bench
//!
//! 既定（数 12・間隔 1・減衰 70・色を適用・色 1 赤）で、素材を 4 通り:
//! - matte:  検証パターン_H の 硬マット（余白 5→35）。実機の速さの項目 CEI-03 の素材
//! - opaque: 画面いっぱいの不透明な絵が横に流れる（edge_sweep の FHD の素材に近い）
//! - square: 透明の地に不透明な 400x400 の四角が 1 フレームに 24px 動く（ふだんの使い方）
//! - soft:   画面いっぱいの半透明（α がフレームごとに変わる）。いちばん重い
//!
//! 「定常」は同じオブジェクトを 60 フレーム回した後半 40 フレーム、「1 枚目」は毎回新しいオブジェクト（履歴なし）。
//! 出力のハッシュ（全フレームの出力をつないだ FNV-1a）を出すので、版の間で絵が同じかを比べられる。
//! 間隔 0.5（前後の 2 枚を混ぜる）と グラデーション の場合も 1 つずつ測る。
//! 2 つ目の引数に色空間の番号（v1.3.0。0〜9）を渡すと、グラデーション ON・その色空間で測る（`-- matte 7`）。

use colored_echo_img::store::StateStore;
use colored_echo_img::trail::ProcessParams;
use std::time::Instant;

const W: u32 = 1920;
const H: u32 = 1080;

fn params(frame: i32, interval: f64, use_grad: bool, space: i32) -> ProcessParams {
    ProcessParams {
        frame,
        max_frames: 12,
        interval,
        decay: 0.7,
        use_color: true,
        r1: 1.0,
        g1: 0.0,
        b1: 0.0,
        a1: 1.0,
        use_grad,
        r2: 0.0,
        g2: 0.0,
        b2: 1.0,
        a2: 0.2,
        space,
        hue_long: false,
    }
}

fn picture(kind: &str, f: i32) -> Vec<u8> {
    let (w, h) = (W as usize, H as usize);
    let mut v = vec![0u8; w * h * 4];
    match kind {
        "opaque" => {
            for y in 0..h {
                for x in 0..w {
                    let i = (y * w + x) * 4;
                    let u = x + f as usize * 13;
                    v[i..i + 4].copy_from_slice(&[(u % 256) as u8, (y % 256) as u8, ((u / 7 + y / 3) % 256) as u8, 255]);
                }
            }
        }
        "matte" => {
            // 検証パターン_H の 硬マット（CEI-03 の素材）: 余白 5→35 を 30 フレームで。白の不透明な枠、外と内の穴は透明
            let m = (5.0 + 30.0 * (f % 30) as f64 / 29.0) / 100.0;
            let outer = 0.5 - m * 0.5;
            let inner = outer * 0.5;
            for y in 0..h {
                let pv = ((y as f64 + 0.5) / h as f64 - 0.5).abs();
                for x in 0..w {
                    let pu = ((x as f64 + 0.5) / w as f64 - 0.5).abs();
                    let e = pu.max(pv);
                    if e <= outer && e > inner {
                        let i = (y * w + x) * 4;
                        v[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
                    }
                }
            }
        }
        "square" => {
            let x0 = (100 + f as usize * 24) % (w - 400);
            let y0 = 300;
            for y in y0..y0 + 400 {
                for x in x0..x0 + 400 {
                    let i = (y * w + x) * 4;
                    v[i..i + 4].copy_from_slice(&[230, ((x - x0) / 2) as u8, 40, 255]);
                }
            }
        }
        _ => {
            for y in 0..h {
                for x in 0..w {
                    let i = (y * w + x) * 4;
                    let a = ((x + y + f as usize * 9) % 256) as u8;
                    v[i..i + 4].copy_from_slice(&[(x % 256) as u8, 200, (y % 256) as u8, a]);
                }
            }
        }
    }
    v
}

fn fnv(h: &mut u64, data: &[u8]) {
    for &b in data {
        *h ^= b as u64;
        *h = h.wrapping_mul(0x100000001b3);
    }
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn run(kind: &str, interval: f64, use_grad: bool, space: i32) {
    let pics: Vec<Vec<u8>> = (0..60).map(|f| picture(kind, f)).collect();
    // 定常
    let mut st = StateStore::new();
    let mut times = Vec::new();
    let mut hash = 0xcbf29ce484222325u64;
    for f in 0..60 {
        let mut px = pics[f as usize].clone();
        let t = Instant::now();
        st.process("a", &mut px, W, H, &params(f, interval, use_grad, space), Instant::now());
        let e = t.elapsed().as_secs_f64() * 1000.0;
        if f >= 20 {
            times.push(e);
        }
        fnv(&mut hash, &px);
    }
    // 1 枚目（毎回新しいオブジェクト。前のオブジェクトの履歴は残る）
    let mut st1 = StateStore::new();
    let mut firsts = Vec::new();
    for f in 0..20 {
        let mut px = pics[f as usize].clone();
        let t = Instant::now();
        st1.process(&format!("o{f}"), &mut px, W, H, &params(f, interval, use_grad, space), Instant::now());
        firsts.push(t.elapsed().as_secs_f64() * 1000.0);
        fnv(&mut hash, &px);
    }
    println!(
        "{kind:6} 間隔 {interval:<4} グラデ {} 色空間 {space}: 定常 中央 {:6.2} ms（最大 {:6.2}）/ 1 枚目 中央 {:6.2} ms / ハッシュ {hash:016x}",
        use_grad as u8,
        median(times.clone()),
        times.iter().cloned().fold(0.0, f64::max),
        median(firsts)
    );
}

/// 積む（`trail::ingest`）と合成（`trail::render`）を分けて測る（定常の中央値）。
fn split(kind: &str, use_grad: bool, space: i32) {
    use colored_echo_img::trail::{ingest, render, TrailState};
    let pics: Vec<Vec<u8>> = (0..60).map(|f| picture(kind, f)).collect();
    let mut st = TrailState::new();
    let (mut ti, mut tr) = (Vec::new(), Vec::new());
    for f in 0..60 {
        let mut px = pics[f as usize].clone();
        let p = params(f, 1.0, use_grad, space);
        let t = Instant::now();
        ingest(&mut st, &px, W, H, &p, true);
        let t1 = Instant::now();
        render(&st, &mut px, W, H, &p);
        let t2 = Instant::now();
        if f >= 20 {
            ti.push((t1 - t).as_secs_f64() * 1000.0);
            tr.push((t2 - t1).as_secs_f64() * 1000.0);
        }
    }
    println!("{kind:6} 色空間 {space} 内訳: 積む 中央 {:6.2} ms / 合成 中央 {:6.2} ms", median(ti), median(tr));
}

fn main() {
    // 引数に素材の名前を渡すと、その素材だけを測って内訳も出す（cargo run --release --example bench -- matte）
    // 2 つ目に色空間の番号を渡すと、グラデーション ON・その色空間で測る（cargo run --release --example bench -- matte 7）
    if let Some(kind) = std::env::args().nth(1) {
        match std::env::args().nth(2).and_then(|s| s.parse::<i32>().ok()) {
            Some(space) => {
                run(&kind, 1.0, true, space);
                split(&kind, true, space);
            }
            None => {
                run(&kind, 1.0, false, 0);
                split(&kind, false, 0);
            }
        }
        return;
    }
    for kind in ["matte", "opaque", "square", "soft"] {
        run(kind, 1.0, false, 0);
    }
    run("square", 0.5, false, 0);
    run("soft", 0.5, true, 0);
}
