# chdlite

ディスクイメージ（CUE + BIN・GDI + BIN・ISO）をドロップすると CHD か RVZ になり、`.chd`・`.rvz` をドロップすると CUE + BIN・GDI + BIN・ISO に戻ります。
メタデータの読み取りとプラットフォームの判別、中身のハッシュ計算、NES・N64・SNES のダンプの形式変換、3DS タイトルの復号もできます。
データベースや設定は要りません。romexplorer のコアを使い、romexplorer とは別にリリースします（バージョン 0.3.4）。

[English](README.md) · **日本語** · [Français](README.fr.md) · [Español](README.es.md) · [Italiano](README.it.md) · [Português](README.pt.md) · [Deutsch](README.de.md) · [한국어](README.ko.md) · [Polski](README.pl.md)

**ダウンロード:** [CHD Lite のリリース](https://github.com/DesertDoggy/romexplorer/releases?q=c0) · **変更点:** [リリースノート](../docs/chdlite_release_notes.ja.md) · [romexplorer](../README.ja.md) の一部です

> **chdlite は ROM・BIOS・鍵の配布もダウンロードもしません。違法コピーを支援するものではありません。**
> 自分で持っているゲームを自分で吸い出してお使いください。

## ウィンドウ版

`chdlite-gui`（macOS では `chdlite.app`、Linux では chdlite の AppImage）を開きます。モードは 4 つです。

| モード | 内容 |
|---|---|
| **Lite** | 高速に圧縮します。`.chd`・`.rvz` は CUE + BIN・GDI + BIN・ISO に展開します。 |
| **Comp** | 時間をかけて、いちばん小さくなるように圧縮します。`.chd`・`.rvz` は同じように展開します。 |
| **Read** | メタデータを読み取り、プラットフォームを判別します。 |
| **Hash** | ファイルの中身のハッシュを計算します。 |

モードを選び、ファイルかフォルダーをウィンドウにドロップします（選んでも構いません）。スレッド数と同時に変換する
ファイル数、出力先と既存ファイルの置き換え、Lite のプリセットと Comp のコーデック、RVZ のコーデック・レベル・
ブロックサイズ、ハッシュ、cue の書き方と GD-ROM のシート、カートリッジのダンプと 3DS タイトルの出力形式、
非可逆な出力（WUA・ZAR・XISO）の前に確認するかは **Settings** にあります。設定は次回も残り、コマンドラインでも
同じ設定を使います。

**Android**　chdlite アプリも同じ画面です。ファイルとフォルダーはアプリ内のブラウザーで選びます。

**macOS・Linux**　macOS 版は公証していないので、最初は右クリック ›「開く」で起動します。

## 変換

出力形式はイメージに合わせて自動で選びます。

**圧縮**

| 入力 | 出力 |
|---|---|
| CD：CUE + BIN、TOC + BIN、または BIN・IMG のトラック 1 つ | CD の CHD |
| GD-ROM（ドリームキャスト）：GDI + BIN/RAW | GD-ROM の CHD |
| DVD（PS2・PSP など）：ISO | DVD の CHD |
| PSP：CSO | DVD の CHD |
| PS1：PBP | CD の CHD |
| ゲームキューブ・Wii：ISO、GCM | RVZ |
| Wii U：WUD、WUX | WUA |
| Xbox 360：ISO | ZAR |
| Xbox：ISO | XISO |

**展開**

| 入力 | 出力 |
|---|---|
| CD の CHD | CUE + BIN（トラックごとに BIN 1 つ） |
| GD-ROM の CHD | GDI + BIN（`--gdrom-sheet cue` なら CUE + BIN） |
| DVD の CHD | ISO |
| RVZ | ISO |
| CSO | ISO |
| PBP | CUE + BIN |

WUA・ZAR・XISO はゲームのファイルは残しますが、ディスクそのものは残らないため、元のイメージには戻せません。
入力は可逆な形式だけで、WUA・ZAR・XISO は受け付けません。Wii U のディスクには、隣に鍵が必要です。

**カートリッジのダンプ**（`.nes`・`.unh`・`.z64`・`.v64`・`.n64`・`.sfc`・`.smc`・`.swc`）は圧縮しません。
機種ごとに設定した形式で書き出します。NES はヘッダーあり・なし（または `.unh`）、N64 は 3 種類のバイト順、
SNES はコピーヤーヘッダーあり・なしです。NES のヘッダーの有無はハッシュで調べるため、対応ゲームのみ変換します。

**3DS タイトル**（`.cia`・`.3ds`・`.cci`・`.cxi`・`.3dsx` と、圧縮した `.zcci`・`.zcia`・`.zcxi`・`.z3dsx`）は復号します。
ゲームは `.cci`/`.3ds`・`.cxi`、または圧縮した `.zcci`・`.zcxi` になります。更新データ・DLC・体験版は `.cia`
（圧縮すると `.zcia`）のままです。chdlite に鍵は付いていません。

**ハッシュ**　SHA-1・MD5・CRC32・SHA-256・XXH3 と RetroAchievements のハッシュに対応します。RetroAchievements の
ハッシュでは、同梱のリストでそのゲームに実績があるかどうかも表示します。

## Read

Read は、ディスク自身のデータからプラットフォームを判別し、ゲームのシリアルとタイトルを読み取ります。
ほかに次の内容を表示します。

| ファイル | 表示内容 |
|---|---|
| CHD | CHD のバージョンと種類（CD・GD-ROM・DVD・ハードディスク）、プラットフォーム・シリアル・タイトル、元のサイズ・ファイルサイズ・圧縮率、ハンクサイズと数、ユニットサイズ、コーデック、全体とデータの SHA-1、親 CHD の SHA-1（ある場合）、トラックごとの種類と長さ |
| RVZ | ゲームキューブか Wii か、ゲーム ID とタイトル、ディスクサイズ・ファイルサイズ・圧縮率、ブロックサイズ |
| CSO | CSO のバージョンとブロック、プラットフォーム・シリアル・タイトル、ISO のサイズ・ファイルサイズ・圧縮率 |
| PBP | ディスクの枚数、プラットフォーム・シリアル・タイトル、ディスクごとのトラックとサイズ |
| CUE + BIN・GDI + BIN・ISO | プラットフォーム・シリアル・タイトル、変換後の形式 |
| Wii U の WUD・WUX | ディスクサイズ、タイトル ID とバージョン |
| 3DS タイトル | コンテナ（CIA・カートリッジイメージ・NCCH・3DSX）、圧縮の有無、タイトルの種類、暗号化の有無、変換後の形式 |
| カートリッジのダンプ | 機種とサイズ、今の形式（ヘッダー・バイト順）、変換後の形式 |

## ドラッグ＆ドロップ

ツールごとに役割が決まっているので、実行ファイルにファイルをドロップするだけで、保存した設定で目的の処理が行われます。

| ツール | ドロップ時の動作 |
|---|---|
| `chdlite` | 高速に圧縮します。`.chd`・`.rvz` は CUE + BIN・GDI + BIN・ISO に展開します。 |
| `chdcomp` | いちばん小さくなるように圧縮します。`.chd`・`.rvz` は CUE + BIN・GDI + BIN・ISO に展開します。 |
| `chdread` | メタデータを読み取り、プラットフォームを判別します。 |
| `chdhash` | `.chd`・`.rvz`・`.cso`・`.pbp` の中身、またはファイルそのもののハッシュを計算します。 |

## コマンドライン

どのツールもすべてのコマンドとオプションを使えます。アプリの中では `chdlite.app/Contents/MacOS/` にあり、
AppImage では `chdlite.AppImage chdlite ...` で使えます。

```
chdlite [コマンド] <ファイル|フォルダー>... [オプション]
```

| コマンド | 内容 |
|---|---|
| `auto` | `.chd`・`.rvz` は展開し、それ以外は高速プリセットで作成します。 |
| `create` | 高速プリセットで作成します。 |
| `comp` | いちばん小さくなるように作成します。 |
| `extract` | `.chd`・`.rvz`・`.cso`・`.pbp` を CUE + BIN・GDI + BIN・ISO に、圧縮した 3DS タイトルを元の形式に展開します。 |
| `read` | メタデータを読み取り、プラットフォームを判別します。 |
| `hash` | 中身のハッシュを計算します。 |

| オプション | 内容 |
|---|---|
| `-o`、`--output <dir>` | 元のファイルの隣ではなく、ここに書き出します。 |
| `-f`、`--force` | すでにある出力を置き換えます。 |
| `-np`、`--threads <n>` | 全体のスレッド数です（既定は物理コア数 − 4）。 |
| `-jf`、`--concurrent-files <n>` | 同時に変換するファイル数です。 |
| `--preset pc\|mobile` | `create` が使う高速プリセットです。 |
| `--comp best\|chdman\|custom` | `comp` の動作です。 |
| `-c`、`--compression <list>` | CHD のコーデックです（4 つまで）。 |
| `-hs`、`--hunksize <bytes>` | CHD のハンクサイズです。 |
| `--rvz-codec`・`--rvz-level`・`--rvz-block`・`--rvz-scrub` | RVZ の設定です。`--rvz-scrub` は不要なデータを削除し、元に戻せません。 |
| `--hash <list>` | `sha1,md5,crc32,sha256,xxh3,rahash`（既定は `sha1,rahash`）。 |
| `--cue-style <style>` | 展開した cue シートの書き方です。`redump`・`redump_catalog`・`chdman`。 |
| `--gdrom-sheet gdi\|cue` | 展開した GD-ROM のシートです。 |
| `--nes`・`--n64`・`--snes`・`--3ds <format>` | カートリッジのダンプと 3DS タイトルの出力形式です。 |
| `-h`、`--help` / `-V`、`--version` | ヘルプとバージョンです。 |

既定値は `chdlite.toml` から読みます。プログラムの隣か設定フォルダーに置きます（場所は `--help` に表示されます）。

## 同梱ライブラリ

| ライブラリ | ライセンス |
|---|---|
| [chdman-simd](https://github.com/grouik1er-coder/chdman-simd) | [BSD-2-Clause](../app_root_ex/lib/LICENSES/chdman-simd.LICENSE) |
| [MAME](https://github.com/mamedev/mame) | [GPL-2.0+](../app_root_ex/lib/LICENSES/mame.LICENSE) |
| [libchdr](https://github.com/rtissera/libchdr) | [BSD-3-Clause](../app_root_ex/lib/LICENSES/libchdr.LICENSE) |
| [Dolphin](https://github.com/dolphin-emu/dolphin) | [GPL-2.0+](../app_root_ex/lib/LICENSES/dolphin.LICENSE) |
| [Cemu](https://github.com/cemu-project/Cemu) | [MPL-2.0](../app_root_ex/lib/LICENSES/cemu.LICENSE) |
| [ZArchive](https://github.com/Exzap/ZArchive) | [MIT-0](../app_root_ex/lib/LICENSES/ZArchive.LICENSE) |
| [zstd](https://github.com/facebook/zstd) | [BSD-3-Clause](../app_root_ex/lib/LICENSES/zstd.LICENSE) |
| [fmt](https://github.com/fmtlib/fmt) | [MIT](../app_root_ex/lib/LICENSES/fmt.LICENSE) |
| [extract-xiso](https://github.com/XboxDev/extract-xiso) | [BSD](../app_root_ex/lib/LICENSES/extract-xiso.LICENSE) |
| [Beetle PSX](https://github.com/libretro/beetle-psx-libretro) | [GPL-2.0](../app_root_ex/lib/LICENSES/beetle-psx-libretro.LICENSE) |
| [RALibretro](https://github.com/RetroAchievements/RALibretro) | [GPL-3.0](../app_root_ex/lib/LICENSES/RALibretro.LICENSE) |
| [zlib-ng](https://github.com/zlib-ng/zlib-ng) | [zlib](../app_root_ex/lib/LICENSES/zlib-ng.LICENSE) |
| [OpenSSL](https://github.com/openssl/openssl) | [Apache-2.0](../app_root_ex/lib/LICENSES/openssl.LICENSE) |

## ライセンス

romexplorer と同じ [AGPL-3.0](../LICENSE) です。
