mod store;
mod trail;

use aviutl2::{
    module::{ScriptModuleCallHandle, ScriptModuleFunctions},
    AnyResult,
};
use std::sync::Mutex;
use std::time::Instant;
use store::StateStore;
use trail::ProcessParams;

#[aviutl2::plugin(ScriptModule)]
struct ColoredEchoImgModule {
    /// オブジェクトごとの履歴。合計の上限と、使われなくなった履歴の破棄は StateStore が持つ
    states: Mutex<StateStore>,
}

impl aviutl2::module::ScriptModule for ColoredEchoImgModule {
    fn new(_info: aviutl2::AviUtl2Info) -> AnyResult<Self> {
        Ok(Self {
            states: Mutex::new(StateStore::new()),
        })
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

    /// process(id, data, w, h, frame, max_frames, interval,
    ///         decay, use_color, r1,g1,b1,a1, use_grad, r2,g2,b2,a2)
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
        };

        let pixels = unsafe { std::slice::from_raw_parts_mut(ptr, len) };

        match self.states.lock() {
            Ok(mut store) => store.process(&id, pixels, w, h, &pp, Instant::now()),
            Err(e) => {
                let _ = params.set_error(&format!("{e}"));
            }
        }
    }
}

aviutl2::register_script_module!(ColoredEchoImgModule);
