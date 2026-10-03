# ColoredEchoImg

`色付き残像(画像履歴).anm2` が呼ぶスクリプトモジュール（`.mod2`）。過去のフレームの絵を覚えておき、色付きの残像として今の絵の下に重ねる。

- 種別: スクリプトモジュール（`.mod2`、aviutl2-rs の `module` 機能）
- SDK: [aviutl2-rs](https://github.com/sevenc-nanashi/aviutl2-rs) 0.47（本体 2.1.10 以上）
- 配置先: `Script/ColoredEchoImg/colored_echo_img.mod2`（スクリプトは `Script/色付き残像(画像履歴).anm2`）
- 仕様: `AI/specifications/20260726_ColoredEchoImg_spec.md` ／ 使い方: `自作スクリプトマニュアル/色付き残像(画像履歴).md`
- ライセンス: MIT（`LICENSE`）

## 構成

| パス | 内容 |
|---|---|
| `src/lib.rs` | モジュールの入口。Lua から呼ぶ `process` / `reset` の引数を読み、`StateStore` へ渡す |
| `src/store.rs` | オブジェクトごとの履歴をまとめて持つ。合計の上限（2 GiB）と、使われなくなった履歴の破棄 |
| `src/trail.rs` | 1 オブジェクト分の履歴（`TrailState`）と、履歴から残像を合成する処理 |
| `build.ps1` | release ビルド → `colored_echo_img.mod2` → `Script/ColoredEchoImg/` へ配置 |

画素は `obj.getpixeldata("object", "rgba")` の RGBA8（**ストレートアルファ**）をそのまま受け取り、同じ形式で書き戻す。
スレッドは起こさない（上限と破棄の判定も `process` の呼び出しの中で行う）。

## ビルド

```powershell
& "C:\ProgramData\aviutl2\AI\plugins\ColoredEchoImg\build.ps1"            # ビルドして配置
& "C:\ProgramData\aviutl2\AI\plugins\ColoredEchoImg\build.ps1" -NoDeploy  # 配置しない
```

AviUtl2 が `.mod2` を掴んでいると配置で失敗する。差し替えたら AviUtl2 の再起動が要る。

## テスト

```powershell
cd C:\ProgramData\aviutl2\AI\plugins\ColoredEchoImg
cargo test
```

窓・補間・シーク（戻る / 先へ飛ぶ / 小さな抜け）・合計の上限・破棄の単体テスト。
合成の結果を実機の判定と突き合わせる机上検算は `python AI/scripts/色付き残像/verify_cei_judge.py`。
