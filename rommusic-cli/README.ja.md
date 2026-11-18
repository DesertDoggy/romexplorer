# rommusic

ROM・ディスクイメージを `rommusic` にドロップすると、ゲームの音楽を FLAC で書き出します。
romexplorer アプリが ROM の音楽を流すときと同じ読み込み・再生の仕組みを使います。
romexplorer とは別にリリースします（バージョン 0.2.1）。

[English](README.md) · **日本語** · [Français](README.fr.md) · [Español](README.es.md) · [Italiano](README.it.md) · [Português](README.pt.md) · [Deutsch](README.de.md) · [한국어](README.ko.md) · [Polski](README.pl.md)

**ダウンロード:** [Rom Music のリリース](https://github.com/DesertDoggy/romexplorer/releases?q=m0) · **変更点:** [リリースノート](../docs/rommusic_release_notes.ja.md) · [romexplorer](../README.ja.md) の一部です

> **rommusic は ROM・BIOS・鍵の配布もダウンロードもしません。違法コピーを支援するものではありません。**
> 自分で持っているゲームを自分で吸い出してお使いください。

## ウィンドウ版

`rommusic-gui`（macOS では `rommusic.app`、Linux では rommusic の AppImage）を開きます。

1. ファイルかフォルダーを選びます。ウィンドウにドロップしても選べます。ROM・ディスクイメージ・ダンプしたゲームのフォルダーに対応します。
2. **FLAC**・**Original audio**（元の形式）・**Both**（両方）を選びます。
3. 出力先を選びます。空なら、それぞれの隣に書き出します。
4. **Start** を押します。

ジングル・効果音・ムービーの音声、曲数の上限、サンプリングレートとビット数、スーパーファミコンの実行（動かす秒数と、その間にスタートボタンを押すか）は **Options** にあります。設定は次回も残ります。

**Android**　rommusic アプリ（Android 9 以降）も同じ画面です。最初の起動で「すべてのファイルへのアクセス」を求めます。ROM を読んで音楽を書き出すためです。ファイルとフォルダーはアプリ内のブラウザーで選びます。画面を消しても書き出しは続きます。

**macOS・Linux**　`rommusic.AppImage --install` でデスクトップのメニューに追加できます。macOS 版は公証していないので、最初は右クリック ›「開く」で起動します。

## 対応ゲーム

| 機種 | ファイル | 音楽 |
|---|---|---|
| CD のゲーム：プレイステーション・サターン・メガ CD・PC エンジン CD・ネオジオ CD・ドリームキャストなど | CHD、CUE + BIN、GDI + BIN、ISO | CD の音声トラック、ディスク内の音楽ファイル（ADX・XA・AT3 など。CPK・AFS のアーカイブ内も可） |
| プレイステーション2・PSP・ゲームキューブ・Wii・Xbox・Xbox 360 など | CHD、ISO、RVZ、WIA、GCZ、WBFS、CSO、ZSO、XISO、ダンプしたゲームのフォルダー | ディスク内の音楽ファイル（ADX・AT3・ATRAC3・WMA・XMA・BRSTM・HCA など。CPK・AFS・ARC・SZS のアーカイブ内も可） |
| Wii U | WUA。WUD・WUX・タイトルフォルダーは隣に鍵があれば可 | ディスク内の音楽ファイル（BFSTM など） |
| ニンテンドーDS | NDS | SDAT のサウンドアーカイブ（STRM のストリーム、SSEQ のシーケンス） |
| ニンテンドー3DS | 3DS、CCI（復号済みのみ） | サウンドアーカイブ内の音楽ファイル（BCSTM など） |
| ファミコン・ディスクシステム、ゲームボーイ・ゲームボーイカラー、ゲームボーイアドバンス、バーチャルボーイ、SG-1000・マスターシステム・ゲームギア・メガドライブ、PC エンジン・スーパーグラフィックス、MSX、ワンダースワン・ワンダースワンカラー、ネオジオポケット・ネオジオポケットカラー | ROM（zip・7z 内も可） | BGM のバイナリコード |
| スーパーファミコン | ROM（zip・7z 内も可） | BGM のバイナリコード |
| NINTENDO64 | ROM（zip・7z 内も可） | BGM のバイナリコード |
| PC-98 | フロッピーイメージ（FDI・HDM・FDD など。zip・7z 内も可） | フロッピー内の PMD・FMP の音楽ファイル |

対応機種でも、技術的な理由で対応していないゲームがあります。
zip・7z に入ったディスクイメージ、暗号化されたイメージ、PS3・Vita・ダウンロード版のパッケージは読めません。

## 出力

それぞれの隣（または出力先）に `<名前> (music)` フォルダーを作ります。前回書き出したファイルは置き換えます。

| 出力 | 内容 |
|---|---|
| **FLAC**（既定） | `01 - <曲名>.flac` など：BGM です。ループする曲はループを 2 回流してフェードアウトします。`index.txt` に曲の一覧を書きます。 |
| **Original audio**（元の形式） | ゲームに入っている形式のままの音楽ファイルを `original` フォルダーに書き出します。CD の音声トラックとカートリッジの音楽にはないので、FLAC を使ってください。 |
| **Both**（両方） | FLAC と元の形式の両方です。 |

元の形式のファイルにはゲームのプログラムやデータが入っています。ROM と一緒に保管し、人に渡さないでください。

## ドラッグ＆ドロップ

ファイルやフォルダーを `rommusic.exe`（Linux・macOS では `rommusic`）にドロップします。
既定の設定で FLAC を書き出します。ウィンドウ版で何も変えずに **Start** を押したときと同じです。

## コマンドライン

コマンドライン版は `rommusic` です（アプリの中では `rommusic.app/Contents/MacOS/rommusic`、
または `rommusic.AppImage rommusic ...`）。ウィンドウ版の設定はすべてフラグで指定できます。

```
rommusic [オプション] <ROM・ディスクイメージ・フォルダー>...
```

例：

```
rommusic "Hoge Quest (Japan).chd"
rommusic --all --max 20 "Hoge Quest (Japan).chd"
rommusic --out D:\music "Hoge Quest (Japan).nes"
rommusic --hires "Hoge Quest (Japan).cue"
rommusic --format both "Hoge Quest (Japan).sfc"
```

| オプション | 内容 |
|---|---|
| `--all` | ジングル・効果音・ムービーの音声も書き出します。既定では BGM だけです。 |
| `--max N` | 1 つにつき最大 N 曲までにします。 |
| `--out DIR` | `<名前> (music)` フォルダーを、元のファイルの隣ではなく `DIR` に作ります。 |
| `--rate native` | 既定です。デコードしたときのレートのままにします。 |
| `--cd` / `--rate cd` | 44.1 kHz・16 ビットにします。 |
| `--dvd` / `--rate dvd` | 48 kHz・24 ビットにします。 |
| `--hires` / `--rate hires` | 96 kHz・24 ビットにします。 |
| `--rate <Hz>` | 8000〜192000 の任意のレートにします。 |
| `--bits 16\|24` | ビット数です（既定は 16。`--dvd` と `--hires` では 24）。 |
| `--format flac` | 既定です。FLAC だけを書き出します。 |
| `--format original` | ゲームに入っている形式のまま `original` フォルダーに書き出します。FLAC は作りません。 |
| `--format both` | FLAC と元の形式の両方を書き出します。 |
| `--seconds N` | スーファミ：音楽を取り出すまでにゲームを動かす秒数です（既定 20）。 |
| `--start` | スーファミ：動かしている間、ときどきスタートボタンを押します。タイトル画面のあとで曲が始まるゲーム向けです。 |
| `--list` | 曲の一覧を表示するだけで、何も書き出しません。ループする曲は 1 回分の長さと「loop」を表示します。ファイルではループを 2 回流してフェードアウトします。 |
| `--verbose` | ファイルや曲を飛ばした理由を表示します。 |
| `--no-pause` | 最後に Enter を待ちません。Enter を待つのは、ドロップしたときに開くウィンドウだけです。 |
| `-h`、`--help` / `-V`、`--version` | ヘルプとバージョンです。 |

`native` 以外のレートではリサンプリングします。音が細かくなるわけではありません。
レートをそろえたいときや、37.8 kHz のような珍しいレートが苦手なプレーヤー向けです。
リサンプリングしたファイルは、コメントタグにそう記録します。

## 同梱ライブラリ

| ライブラリ | ライセンス |
|---|---|
| [game-music-emu](https://github.com/libgme/game-music-emu) | [LGPL-2.1+](../app_root_ex/lib/LICENSES/gme.LICENSE) |
| [Game_Music_Emu (kode54)](https://github.com/kode54/Game_Music_Emu) | [LGPL-2.1+](../app_root_ex/lib/LICENSES/gme-kode54.LICENSE) |
| [agbplay](https://github.com/ipatix/agbplay) | [LGPL-3.0](../app_root_ex/lib/LICENSES/agbplay.LICENSE) |
| [SSEQPlayer](https://github.com/kode54/SSEQPlayer) | [WTFPL](../app_root_ex/lib/LICENSES/SSEQPlayer.LICENSE) |
| [lazyusf2](https://gitlab.com/kode54/lazyusf2) | [GPL-2.0+](../app_root_ex/lib/LICENSES/lazyusf2.LICENSE) |
| [Highly Experimental](https://gitlab.com/kode54/highly_experimental) | [GPL-3.0](../app_root_ex/lib/LICENSES/highly_experimental.LICENSE) |
| [Highly Theoretical](https://gitlab.com/kode54/highly_theoretical) | [GPL-3.0](../app_root_ex/lib/LICENSES/highly_theoretical.LICENSE) |
| [mGBA](https://github.com/mgba-emu/mgba) | [MPL-2.0](../app_root_ex/lib/LICENSES/mgba.LICENSE) |
| [Beetle WonderSwan](https://github.com/libretro/beetle-wswan-libretro) | [GPL-2.0+](../app_root_ex/lib/LICENSES/beetle-wswan.LICENSE) |
| [libvgm](https://github.com/ValleyBell/libvgm) | [BSD-3-Clause, GPL-2.0+](../app_root_ex/lib/LICENSES/libvgm.LICENSE) |
| [VGMTrans](https://github.com/vgmtrans/vgmtrans) | [zlib](../app_root_ex/lib/LICENSES/vgmtrans.LICENSE) |
| [TinySoundFont](https://github.com/schellingb/TinySoundFont) | [MIT](../app_root_ex/lib/LICENSES/TinySoundFont.LICENSE) |
| [98fmplayer](https://github.com/myon98/98fmplayer) | [BSD-2-Clause](../app_root_ex/lib/LICENSES/98fmplayer.LICENSE) |
| [vgmstream](https://github.com/vgmstream/vgmstream) | [ISC](../app_root_ex/lib/LICENSES/vgmstream.LICENSE) |
| [FFmpeg](https://github.com/FFmpeg/FFmpeg) | [LGPL-2.1+](../app_root_ex/lib/LICENSES/ffmpeg.LICENSE) |
| [fmt](https://github.com/fmtlib/fmt) | [MIT](../app_root_ex/lib/LICENSES/fmt.LICENSE) |
| [spdlog](https://github.com/gabime/spdlog) | [MIT](../app_root_ex/lib/LICENSES/spdlog.LICENSE) |
| [unarr](https://github.com/selmf/unarr) | [LGPL-3.0](../app_root_ex/lib/LICENSES/unarr.LICENSE) |
| [zlib](https://github.com/madler/zlib) | [zlib](../app_root_ex/lib/LICENSES/zlib.LICENSE) |
| [chdman-simd](https://github.com/grouik1er-coder/chdman-simd) | [BSD-2-Clause](../app_root_ex/lib/LICENSES/chdman-simd.LICENSE) |
| [MAME](https://github.com/mamedev/mame) | [GPL-2.0+](../app_root_ex/lib/LICENSES/mame.LICENSE) |
| [libchdr](https://github.com/rtissera/libchdr) | [BSD-3-Clause](../app_root_ex/lib/LICENSES/libchdr.LICENSE) |
| [Dolphin](https://github.com/dolphin-emu/dolphin) | [GPL-2.0+](../app_root_ex/lib/LICENSES/dolphin.LICENSE) |
| [Cemu](https://github.com/cemu-project/Cemu) | [MPL-2.0](../app_root_ex/lib/LICENSES/cemu.LICENSE) |
| [ZArchive](https://github.com/Exzap/ZArchive) | [MIT-0](../app_root_ex/lib/LICENSES/ZArchive.LICENSE) |
| [zstd](https://github.com/facebook/zstd) | [BSD-3-Clause](../app_root_ex/lib/LICENSES/zstd.LICENSE) |
| [extract-xiso](https://github.com/XboxDev/extract-xiso) | [BSD](../app_root_ex/lib/LICENSES/extract-xiso.LICENSE) |
| [7-Zip-zstd](https://github.com/mcmilk/7-Zip-zstd) | [LGPL-2.1+](../app_root_ex/lib/LICENSES/7-Zip-zstd.LICENSE) |
| [zlib-ng](https://github.com/zlib-ng/zlib-ng) | [zlib](../app_root_ex/lib/LICENSES/zlib-ng.LICENSE) |

## ライセンス

romexplorer と同じ [AGPL-3.0](../LICENSE) です。
