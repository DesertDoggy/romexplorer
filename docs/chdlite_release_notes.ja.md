# chdlite 0.3.4

chdlite は romexplorer とは別にリリースされます。このノートは chdlite のみが対象です。

## 変更点

- **RetroAchievements。** リスト（assets/radb.bin）を同梱。新しいリストに差し替え可能。

# chdlite 0.3.3

## 変更点

- **3DS。** .cci・.3ds・.cxi・.zcci・.zcxi 出力に対応。
- **NES・N64・SNES のダンプ。** NES のヘッダー付き（.nes）・ヘッダーなし（.nes）・.unh、N64 の .z64・.v64・.n64、SNES の .sfc・.smc 出力に対応。
- NES のヘッダー変換はデータベースによるハッシュ照合のため対応ゲームのみ。
- **Mac アプリと Linux の AppImage 版を追加。**
- **RetroAchievements。** Hash で RA ハッシュに対応。

# chdlite 0.3.2

Android 版のみのリリースです。

## バグ修正

- Android アプリでファイルへのアクセスを許可できない問題を修正。

# chdlite 0.3.1

## バグ修正

- Windows arm64 版と Linux arm64 版で、Wii U・Xbox・Xbox 360 のイメージの変換と PBP の読み込みができない問題を修正。

# chdlite 0.3.0

## 変更点

- **Rust ベースに変更。**
- **RVZ 対応。**
- **Wii U と Xbox。** WUA・XISO・ZAR 出力に対応。
- **入力は可逆な形式のみ。**
- **出力形式の自動選択。** CHD・RVZ は ISO で出力、ISO はプラットフォームに合わせて CHD・RVZ・XISO・ZAR を自動で選択。
- **1 つのウィンドウに 4 つのモード。** Lite（高速）、Comp（最小サイズ）、Read（メタデータ読み取り）、Hash を切り替えてドラッグ＆ドロップする UI に変更。
- **Android に対応。**
- Windows・Linux の ARM64 に対応。

## バグ修正

- Ctrl-C やクラッシュ、ウィンドウを閉じたことで変換が途中で止まると、壊れたファイルが残り、次の実行で完了扱いになる問題を修正。
- 特定のサイズの PS2 / PSP の ISO が音楽 CD と判定される問題を修正。
- 短いジョブが続く間はキャンセルが効かない問題を修正。
- 読めない設定ファイルが既定値で上書きされる問題を修正（`chdlite.toml.bak` として保存）。
- `-hs` が CD と DVD に同じハンクサイズを設定する問題を修正。

## 今回のリリースに含まれないもの

- GCZ と WIA の出力。
- chdman 互換のコマンド名（`createcd`、`extractcd` など）と、ハードディスク / raw の CHD。
- 単一 `.bin` での展開オプション（CD は常にトラックごとに 1 ファイル）。
