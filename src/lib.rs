// pub は examples/bench.rs（release の時間の目安）と examples/colorspace_dump.rs（共有部品との突き合わせ）から呼ぶため
pub mod colorspace;
pub mod janitor;
pub mod store;
pub mod trail;

use aviutl2::{
    module::{ScriptModuleCallHandle, ScriptModuleFunctions},
    AnyResult,
};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use store::StateStore;
use trail::ProcessParams;

#[aviutl2::plugin(ScriptModule)]
struct ColoredEchoImgModule {
    /// オブジェクトごとの履歴。合計の上限と、使われなくなった履歴の破棄は StateStore が持つ
    states: Arc<Mutex<StateStore>>,
    /// 呼ばれなくなった履歴を時間で捨てる（v1.3.1。`Drop` で止めて待つ）
    _janitor: janitor::Janitor,
}

impl aviutl2::module::ScriptModule for ColoredEchoImgModule {
    fn new(_info: aviutl2::AviUtl2Info) -> AnyResult<Self> {
        let init_t0 = std::time::Instant::now();
        let states = Arc::new(Mutex::new(StateStore::new()));
        let this = Self {
            _janitor: janitor::Janitor::start(Arc::clone(&states)),
            states,
        };
        // 初期化にかかった時間をログに 1 行出す（強化デバッグ。仕様 `AI/specifications/20261012_CurveEditor_H_キーフレーム速度_spec.md` の 7.0）
        let new_ms = init_t0.elapsed().as_secs_f64() * 1000.0;
        aviutl2::lprintln!(info, "ColoredEchoImg: 初期化 {new_ms:.1} ms（new {new_ms:.1} ms）");
        Ok(this)
    }

    fn plugin_info(&self) -> aviutl2::module::ScriptModuleTable {
        aviutl2::module::ScriptModuleTable {
            information: format!(
                "colored_echo_img.mod2 frame-history colored echo / v{} / HexBrowns",
                env!("CARGO_PKG_VERSION")
            ),
            functions: Self::functions(),
        }
    }
}

#[aviutl2::module::functions]
impl ColoredEchoImgModule {
    /// reset(id)
    #[direct]
    fn reset(&self, params: &mut ScriptModuleCallHandle) {
        let id = match params.get_param_str(0) {
            Ok(s) if !s.is_empty() => s,
            _ => {
                let _ = params.set_error("reset: id required");
                return;
            }
        };
        match self.states.lock() {
            Ok(mut store) => store.reset(&id, Instant::now()),
            Err(e) => {
                let _ = params.set_error(&format!("{e}"));
            }
        }
    }

    /// version() → "1.3.0"（v1.3.0 から）。
    ///
    /// スクリプト v1.10 は、これが無い（`mod.version == nil`）モジュールを v1.2.0 以前とみなす。古いモジュールは
    /// process の色空間・色相経路の引数を読まない（sRGB で描く）ので、色空間を選んだときにスクリプトが 1 回だけ知らせる。
    #[direct]
    fn version(&self, params: &mut ScriptModuleCallHandle) {
        let _ = params.push_result_str(env!("CARGO_PKG_VERSION"));
    }

    /// process(id, data, w, h, frame, max_frames, interval,
    ///         decay, use_color, r1,g1,b1,a1, use_grad, r2,g2,b2,a2, space, hue_long) → 1（data を書き換えた）/ 0（そのまま）
    ///
    /// space（色空間の番号。共有部品と同じ 0〜9）と hue_long（1 で長経路）は v1.3.0 から。無ければ 0（sRGB・短経路）。
    ///
    /// 0 のとき（1 枚目の不透明な絵・残像が今の不透明な絵に隠れるなど）は、スクリプトが putpixeldata を省く（v1.2.0）。
    /// v1.1.0 までは何も返さなかった（スクリプトは nil を「書き換えた」と読む）。
    #[direct]
    fn process(&self, params: &mut ScriptModuleCallHandle) {
        let id = match params.get_param_str(0) {
            Ok(s) if !s.is_empty() => s,
            _ => {
                let _ = params.set_error("process: id required");
                return;
            }
        };
        // aviutl2 0.42 から Result<*mut u8, _>（0.40 は Option<NonNull<u8>>）。null も弾く
        let ptr = match params.get_param_data::<u8>(1) {
            Ok(p) if !p.is_null() => p,
            _ => {
                let _ = params.set_error("process: pixel pointer required");
                return;
            }
        };
        let w = params.get_param_int(2).unwrap_or(0).max(0) as u32;
        let h = params.get_param_int(3).unwrap_or(0).max(0) as u32;
        let frame = params.get_param_int(4).unwrap_or(0);
        let max_frames = params.get_param_int(5).unwrap_or(0);
        let interval = params.get_param_float(6).unwrap_or(1.0);
        let decay = params.get_param_float(7).unwrap_or(0.5);
        let use_color = params.get_param_int(8).unwrap_or(0) != 0 || params.get_param_boolean(8).unwrap_or(false);
        let r1 = params.get_param_float(9).unwrap_or(1.0) as f32;
        let g1 = params.get_param_float(10).unwrap_or(1.0) as f32;
        let b1 = params.get_param_float(11).unwrap_or(1.0) as f32;
        let a1 = params.get_param_float(12).unwrap_or(1.0) as f32;
        let use_grad = params.get_param_int(13).unwrap_or(0) != 0 || params.get_param_boolean(13).unwrap_or(false);
        let r2 = params.get_param_float(14).unwrap_or(1.0) as f32;
        let g2 = params.get_param_float(15).unwrap_or(1.0) as f32;
        let b2 = params.get_param_float(16).unwrap_or(1.0) as f32;
        let a2 = params.get_param_float(17).unwrap_or(1.0) as f32;
        // v1.3.0: 色空間・色相経路。スクリプト v1.01 までは渡さない（引数が無い・数でない）ので既定の sRGB・短経路になる
        let space = params.get_param_int(18).unwrap_or(colorspace::SRGB);
        let hue_long = params.get_param_int(19).unwrap_or(0) != 0;

        let len = (w as usize).saturating_mul(h as usize).saturating_mul(4);
        if len == 0 {
            let _ = params.set_error("process: empty size");
            return;
        }

        let pp = ProcessParams {
            frame,
            max_frames,
            interval,
            decay,
            use_color,
            r1,
            g1,
            b1,
            a1,
            use_grad,
            r2,
            g2,
            b2,
            a2,
            space,
            hue_long,
        };

        let pixels = unsafe { std::slice::from_raw_parts_mut(ptr, len) };

        match self.states.lock() {
            Ok(mut store) => {
                let changed = store.process(&id, pixels, w, h, &pp, Instant::now());
                params.push_result_int(changed as i32);
            }
            Err(e) => {
                let _ = params.set_error(&format!("{e}"));
            }
        }
    }
}

aviutl2::register_script_module!(ColoredEchoImgModule);
