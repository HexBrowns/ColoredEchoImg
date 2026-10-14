# ColoredEchoImg

`色付き残像(画像履歴).anm2` が呼ぶスクリプトモジュール（`.mod2`）。過去のフレームの絵を覚えておき、色付きの残像として今の絵の下に重ねる。

- 種別: スクリプトモジュール（`.mod2`、aviutl2-rs の `module` 機能）
- SDK: [aviutl2-rs](https://github.com/sevenc-nanashi/aviutl2-rs) 0.47（本体 2.1.10 以上）
- 配置先: `Script/ColoredEchoImg/colored_echo_img.mod2`（スクリプトは `Script/色付き残像(画像履歴).anm2`）
- 仕様: `AI/specifications/20260726_ColoredEchoImg_spec.md` ／ 使い方: `AI/公開情報/ColoredEchoImg/色付き残像(画像履歴).md`
- ライセンス: MIT（`LICENSE`）

## 構成

| パス | 内容 |
|---|---|
| `src/lib.rs` | モジュールの入口。Lua から呼ぶ `process` / `reset` / `version` の引数を読み、`StateStore` へ渡す。`process` は 1 画素でも書き換えたら 1、そのままなら 0 を返す（v1.2.0 から。スクリプト v1.01 は 0 なら putpixeldata を省く）。`process` の末尾の 2 つ（色空間・色相経路）と `version`（`"1.3.0"` を返す）は v1.3.0 から |
| `src/store.rs` | オブジェクトごとの履歴をまとめて持つ。合計の上限（2 GiB）と、使われなくなった履歴の破棄 |
| `src/trail.rs` | 1 オブジェクト分の履歴（`TrailState`）と、履歴から残像を合成する処理。残像ごとの色（`echo_color`） |
| `src/colorspace.rs` | 色1→色2 の補間の色空間と色相経路（v1.3.0）。共有部品 `Script/EffectUtils_ColorSpace.lua` の式の写し（下の「色空間」） |
| `aviutl2.toml` | ビルドと同梱物（`colored_echo_img.mod2` と `色付き残像(画像履歴).anm2`） |
| `assets/色付き残像(画像履歴).anm2` | スクリプトの正本。本番の `Script/` にあるのは配置した写し |
| `examples/colorspace_dump.rs` | 残像ごとの色と、それを描いた画素を書き出す。`AI/scripts/色付き残像/verify_colorspace_ext.py` が共有部品の Lua の値と突き合わせる |
| `examples/bench.rs` | モジュールの中の 1 フレームの時間の目安（FHD・既定の設定。本体の getpixeldata / putpixeldata は入らない）と、出力のハッシュ。素材の名前を渡すとその素材だけ測り、積むと合成の内訳も出す（`cargo run --release --example bench -- matte`）。2 つ目に色空間の番号を渡すとグラデーション ON・その色空間で測る（`-- matte 7`）。P コアと E コアのある CPU では時間が 2 倍ほどぶれるので、比べるときは 1 つの論理 CPU に固定して回す |

画素は `obj.getpixeldata("object", "rgba")` の RGBA8（**ストレートアルファ**）をそのまま受け取り、同じ形式で書き戻す。
スレッドは起こさない（上限と破棄の判定も `process` の呼び出しの中で行う）。

合成は v1.2.0 から、1 画素ずつ最後まで計算する（今の絵が不透明な画素・残像が掛からない所は計算しない、4 成分を SSE2 でまとめて計算する、
1 つ前の画素と入力（今の絵と残像の元の画素）がすべて同じ画素は計算せずに同じ答えを書く。`src/trail.rs` の冒頭）。
1 画素の式と順番は v1.1.0 と同じで、出力は 1 ビットも変わらない。

## 色空間（v1.3.0）

スクリプト v1.10 の「色空間」「色相経路」（グラデーションの 色1→色2 の補間）を受け取る。`process` の末尾の 2 つの引数で、

- 色空間: 共有部品と同じ番号 `0 sRGB / 1 リニア / 2 Oklab / 3 HSV / 4 HSL / 5 Lab / 6 LCh / 7 Oklch / 8 XYZ / 9 HWB`
  （UI に出すのは XYZ を除いた 9 つ。仕様 `AI/specifications/20261013_色空間拡張_spec.md` の 2 章）
- 色相経路: 0 短経路 / 1 長経路（色相のある HSV・HSL・LCh・Oklch・HWB でだけ効く）

既定（0 sRGB）と、色を適用・グラデーションのどちらかが OFF のときは、v1.2.0 と同じ f32 の lerp を通る（出力は 1 ビットも変わらない）。
それ以外では、2 色を先にその色空間へ変換し（`colorspace::Gradient`。1 回の process に 1 回）、残像ごとに 1 回だけ混ぜて sRGB へ戻す。
画素のループには v1.2.0 と同じ定数（色付けの色）として渡すので、色空間を選んでも合成の速さは変わらない。
不透明度1→2 は今までどおり線形に混ぜる（色だけを色空間で混ぜる）。

式は `Script/EffectUtils_ColorSpace.lua`（正本。HLSL の同じ式は `AI/tools/assets/colorspace.hlsl`）を f64 のまま同じ順に写した
（立方根も Lua と同じ `x ^ (1/3)`）。正本を直したら `src/colorspace.rs` も直し、`verify_colorspace_ext.py` を回す。

互換:

- 古いスクリプト（v1.01 まで）は末尾の 2 つを渡さない。引数が無いと既定（sRGB・短経路）になる
- 古いモジュール（v1.2.0 まで）は末尾の 2 つを読まない（sRGB で描く）。スクリプト v1.10 は `mod.version` が無いモジュールで
  sRGB 以外を選んだとき、ログに `[WARN]` を 1 回だけ出して、そのまま描く

## ビルド

```powershell
python AI/tools/au2_build.py ColoredEchoImg               # au2 release → 本番（C:\ProgramData\aviutl2）へ配置
python AI/tools/au2_build.py ColoredEchoImg --no-deploy   # 配置しない（本番との違いだけ出す）
```

ビルドと同梱物は `aviutl2.toml`（[aviutl2-cli](https://github.com/sevenc-nanashi/aviutl2-cli)）が正本。このフォルダで `au2 release` だけを実行すると `release/` に au2pkg ができる。

AviUtl2 が `.mod2` を掴んでいると配置で失敗する。差し替えたら AviUtl2 の再起動が要る。

## テスト

```powershell
cd C:\ProgramData\aviutl2\AI\plugins\ColoredEchoImg
cargo test
cargo run --release --example bench     # 時間の目安（版の間で絵が同じかはハッシュで比べる）
```

窓・補間・シーク（戻る / 先へ飛ぶ / 小さな抜け）・合計の上限・破棄の単体テスト。
v1.1.0 の合成（`trail.rs` の `tests` の `v110`）と新しい合成を乱数の絵で回して 1 ビットも違わないこと（`matches_v110_render`）、
SSE2 とスカラーの合成が同じこと（`simd_matches_scalar`）、丸めの `r8` が `f32::round` と同じこと（`r8_exhaustive` は全 f32、`--ignored`）も見る。
合成の結果を実機の判定と突き合わせる机上検算は `python AI/scripts/色付き残像/verify_cei_judge.py`、
スクリプトの書き戻しを省く条件は `python AI/scripts/色付き残像/verify_cei_lua.py`（Lua の本文を luaJIT で走らせる）。
色空間（v1.3.0）は `python AI/scripts/色付き残像/verify_colorspace_ext.py`（Rust の残像ごとの色が共有部品の Lua と差 1e-6 以内、
render の画素が差 1/255 以内、スクリプトが渡す引数・古いモジュールの警告・UI の互換）と、`cargo test` の
`colorspace_only_affects_gradient_tint`（色を適用・グラデーションのどちらかが OFF なら、どの色空間でも v1.2.0 と 1 ビットも違わない）。
