# romexplorer

[English](README.md) · **日本語** · [Français](README.fr.md) · [Español](README.es.md) · [Italiano](README.it.md) · [Português](README.pt.md) · [Deutsch](README.de.md) · [한국어](README.ko.md) · [Polski](README.pl.md)

*これは英語版 README の翻訳です。内容に違いがある場合は英語版が正となります。*

ROM コレクションの管理ツールです。フォルダをスキャンし、すべてのファイルをハッシュで基準 DAT と
照合します。メタデータやアートを取得し、ファイルの変換・検証・コピーも行います。Windows・macOS・
Linux で動作し、サーバー（ブラウザから使うコンソールと Web GUI 付き）としても、デスクトップアプリ
としても使えます。どの環境で何をテストしたかは [docs/release_notes.ja.md](docs/release_notes.ja.md)
を参照してください。

## できること

- **スキャンと識別。** ディレクトリを走査し、各ファイルのプラットフォームを内容から判別します。
  ハッシュ（CRC32、MD5、SHA-1、SHA-256、XXH3、RetroAchievements）を計算し、まずハッシュで、
  次に名前でカタログと照合します。
- **コンテナの中身を直接読む。** CHD、RVZ/WIA/GCZ、zip、7z はストリームとしてデコードします。
  ディスクイメージを展開しないまま、ハッシュの計算と識別ができます。
- **変換と再パック。** CHD（CD、GD-ROM、DVD）と RVZ、zip・7z の作成と展開、NES/SNES ヘッダーの
  追加・削除に対応します。欠けている `.cue`、`.gdi`、`.m3u` も再構築できます。
- **カタログ。** Redump、MAME-Redump、No-Intro、TOSEC、RetroAchievements、RetroBIOS の DAT を
  同梱の MariaDB に取り込みます。地域ごとのリリースやタイトルもまとめます。
- **メタデータとアート。** IGDB、ScreenScraper、TheGamesDB、SteamGridDB、MobyGames、Hasheous、
  PlayMatch、EmuReady、RetroAchievements に対応します。レート制限はそれぞれ個別に管理します。
- **デバイスへのコピー。** 選んだゲームをフォルダやカードへコピーします。コピーしながら変換し、
  フロントエンドが求める名前と配置に整えます。
- **検証とバックアップ。** 記録済みのハッシュで整合性をチェックします。データベースとカタログは
  zstd でバックアップし、復元もできます。
- **マシン間での共有。** メインサーバーのカタログとアートを、クライアントやピアにミラーできます。
  スマートフォン向けには SQLite でエクスポートします。

すべての機能は REST API（`/api/v1/...`）、ブラウザコンソール（`/`）、Web GUI（`/app/`）から
利用できます。

## 初期設定

パッケージは 3 種類あります。いずれかを選んでください。

| パッケージ | 内容 | セットアップ |
|---|---|---|
| **server** | サーバー、ワーカー、同梱 MariaDB、Web GUI | インストーラーを実行 |
| **desktop** | 専用の MariaDB を持つ GUI アプリ | 起動するだけ |
| **cli** | chdlite: 単体で動く変換・展開・ハッシュツール。データベース不要 | 実行するだけ |

### サーバー

1. パッケージを展開します。
2. 任意: `romexplorer.conf.example` を `romexplorer.conf` にコピーして編集します。値はすべて
   省略できます。パスワードを空にすると自動生成されます。このファイルは初回インストール時にだけ
   読み込まれます。
3. インストーラーを実行します。
   - Linux / macOS: `./install.sh` -- root で実行すると `/opt/romexplorer` にシステムサービスとして
     インストールされます。一般ユーザーで実行すると `~/.local/opt/romexplorer` にユーザーサービス
     としてインストールされます。
   - Windows: 管理者権限の PowerShell で `.\install.ps1` を実行します。
4. 初回起動時にデータベース、そのアカウント、管理者ログインが作成され、コンソールのログイン情報が
   **一度だけ** 表示されます。必ず控えておいてください。
5. `http://127.0.0.1:7337/` を開いてサインインします。

既定では、サーバーは `127.0.0.1:7337`、同梱の MariaDB は `3307` で待ち受けます。LAN に公開するには
`server_bind=0.0.0.0` を設定します。この場合、他のマシンへの通信は TLS になります。証明書はサーバーが
初回起動時に作成します。

インストール後は次の操作ができます。

- 管理者パスワードの変更: コンソールから、または `romexplorer_server reset-admin` で行います。
- インストールの確認: `romexplorer_dbtool check` を実行すると、見つかったネイティブライブラリと
  データベースが一覧表示されます。
- アンインストール: `./install.sh --uninstall`（データは残ります）または `--uninstall --purge`。

### サーバーの更新

新しいリリースのインストーラーを、既存のインストールに対して実行します。インストーラーはデータベースを
検出してサービスを止め、実行ファイル、`lib/`、同梱のアセットを置き換えてから、サービスを再び起動します。
データベース、設定、認証情報、メディアなど、`data/` と `assets/` の下にあるものはそのまま残ります。

- Linux / macOS で最新リリースから直接更新する場合:
  `curl -fsSL https://raw.githubusercontent.com/DesertDoggy/romexplorer/main/scripts/install.sh | bash -s -- --release latest`
- 新しいパッケージを展開してその `./install.sh` を実行するか、インストール済みのものにパッケージを
  指定します:
  `./install.sh --from romexplorer-server-<version>-<os>-<arch>.tar.gz`
- Windows: 新しいパッケージを展開し、管理者権限の PowerShell でその `.\install.ps1` を実行するか、
  `.\install.ps1 -From <package>.zip` を実行します。

初回インストールと同じユーザー（root か一般ユーザーか）で実行してください。`--prefix` / `-Prefix` を
指定していた場合は、同じ値を渡します。romexplorer はアルファ版のため、リリース間でデータベースの
スキーマが変わることがあります。先にバックアップを取ってください（設定 › バックアップ）。

### デスクトップ

展開して `romexplorer` を実行します（macOS では `romexplorer.app`。公証を受けていないため、初回は
右クリック > 開く で起動します）。初回起動時に専用のデータベースが作成・起動されます。

### コマンドラインツール（chdlite）

ディスクイメージをドロップすると、CHD か RVZ ができます。`chdlite`（高速）、`chdcomp`（最小サイズ）、
`chdread`（展開）、`chdhash`（ハッシュ）はモードごとに分かれたツールです。実行ファイルにファイルを
ドロップするだけで、目的の処理が行われます。`chdlite-gui` は同じ機能をウィンドウで使えます。どの
オプションもフラグで指定できます（`--help`）。chdlite は別途リリースしています。
[docs/chdlite_release_notes.ja.md](docs/chdlite_release_notes.ja.md) を参照してください。

### プロバイダーのキー

任意です。管理者として「設定 › プロバイダー認証情報」で API キーを入力します。キーはサーバーに暗号化して
保存され、二度と表示されません。キーのないプロバイダーは使えないだけで、他に影響はありません。

### 最初の使い方

1. ディレクトリを追加し、どのプラットフォームのものかを指定します。自動判別に任せることもできます。
2. DAT 同期を実行し、照合に使うカタログを用意します。
3. そのディレクトリでファイルスキャンを実行します。
4. ハッシュ照合を実行し、続いてプロバイダーへの問い合わせでメタデータとアートを取得します。

これらはいずれもジョブです。詳しくは下記を参照してください。

### ソースから

```bash
git submodule update --init --recursive
bash scripts/build_native_deps.sh        # every native library, into app_root_ex/lib/<os>/<arch>/
bash scripts/build_gui_web.sh            # the web GUI (needs `dx`)
cargo run --bin romexplorer_server
bash scripts/build_release.sh            # or: all packages for this host, into release/<version>/
```

`build_native_deps.sh --check` は、ビルドせずに不足しているツールを報告します。macOS では
Homebrew の `bash`、`bison`、`ninja` が必要です。

## ジョブ

時間のかかる処理はすべてバックグラウンドの **ジョブ** として動くため、ブラウザを閉じても続きます。
各ジョブは進捗を表示し、キャンセルできます。スケジュールに登録すれば（「スケジュールジョブ」タブ）、
分・時間・日・週・月単位で繰り返し実行できます。主なジョブは次のとおりです。

- **DAT 同期** -- 基準 DAT（Redump、No-Intro、TOSEC など）をダウンロードして取り込みます。
- **プロバイダーのメタデータとメディアのダウンロード** -- IGDB、ScreenScraper、SteamGridDB などから
  ゲーム情報とアートを取得します。
- **ファイルスキャン** -- ディレクトリを走査してファイルのハッシュを計算し、カタログと照合します。
- **整合性チェック** -- 保有しているファイルを読み直し、記録済みのハッシュと比べます。
- **シート / プレイリストの検証と作成** -- `.cue`、`.gdi`、`.m3u` ファイルの参照先が今も有効かを
  確かめ、欠けているものを再構築します。

## サードパーティのコード

romexplorer は以下のプロジェクトをもとに作られています。ほとんどはライブラリとして使い、`external/`
の下にサブモジュールとして固定しています。「パッチ」は、ビルド時にそのプロジェクト自体のコードを数行
変更していることを表します。「ラッパー」は、コードには手を加えず、周りに独自の小さなライブラリを追加
していることを表します。どちらも `deps/<name>/` に置き、サブモジュールの中には置きません。すべての
プロジェクトに感謝します。

| プロジェクト | 用途 | 当方の変更 |
|---|---|---|
| [7-Zip-zstd](https://github.com/mcmilk/7-Zip-zstd) | zip / 7z | パッチ、ラッパー |
| [chdman-simd](https://github.com/grouik1er-coder/chdman-simd) | CHD の作成・展開 | パッチ、ラッパー |
| [MAME](https://github.com/mamedev/mame) | chdman-simd のビルド元ソース | なし |
| [libchdr](https://github.com/rtissera/libchdr) | LZMA デコーダー、CHD リーダーの参考 | なし |
| [Dolphin](https://github.com/dolphin-emu/dolphin) | GameCube / Wii（RVZ、WIA、GCZ） | ラッパー |
| [Cemu](https://github.com/cemu-project/Cemu) | Wii U .wua | ラッパー |
| [ZArchive](https://github.com/Exzap/ZArchive) | .wua / .zar の書き込み | なし |
| [zstd](https://github.com/facebook/zstd) | 圧縮 | なし |
| [fmt](https://github.com/fmtlib/fmt) | Cemu のコードが使用 | なし |
| [extract-xiso](https://github.com/XboxDev/extract-xiso) | Xbox XISO | ラッパー |
| [Beetle PSX](https://github.com/libretro/beetle-psx-libretro) | PS1 EBOOT.PBP | ラッパー |
| [RALibretro](https://github.com/RetroAchievements/RALibretro) | RetroAchievements のハッシュ | パッチ、ラッパー |
| [MariaDB Server](https://github.com/MariaDB/server) | 同梱のデータベース | ビルドファイルにパッチ |
| [MariaDB Connector/C](https://github.com/mariadb-corporation/mariadb-connector-c) | データベースクライアント | なし |
| [zlib-ng](https://github.com/zlib-ng/zlib-ng) | Deflate | なし |
| [OpenSSL](https://github.com/openssl/openssl) | TLS | なし |
| [libxaac](https://github.com/ittiam-systems/libxaac) | AAC オーディオ | ラッパー |
| [PlayMatch](https://github.com/RetroRealm/playmatch) | プロバイダークライアント、名前照合の移植 | コードを取り込んで改変 |
| [RomM](https://github.com/rommapp/romm) | 参考、プラットフォームアイコン | コードとしては不使用 |
| [RAChecker](https://github.com/x3kim/RAChecker) | RetroAchievements クライアントの参考 | コードとしては不使用 |
| [psx-covers](https://github.com/xlenore/psx-covers)、[ps2-covers](https://github.com/xlenore/ps2-covers)（xlenore） | PS1 / PS2 のカバー画像（シリアルで取得） | 使うときにダウンロード、同梱なし |

ビルドの詳細、固定バージョン、パッチは [deps/README.md](deps/README.md) にあります。各プロジェクトの
ライセンスは、バイナリとともに `lib/LICENSES/` に同梱しています。
