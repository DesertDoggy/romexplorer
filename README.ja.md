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
- **デバイスへのコピー。** ゲームとそのアートを、サーバーまたはこのマシンからフォルダやカードへ
  コピーします。コピーしながら変換し、フロントエンドが求める名前と配置に整えます。アートは
  RetroArch や PCSX2 / DuckStation 用の構成で出力できます。
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

ディスクイメージをドロップすると、CHD か RVZ ができます。`chdlite`（高速）と `chdcomp`（最小サイズ）は
どちら向きにも使えます。非圧縮のイメージは圧縮し、`.chd` や `.rvz` は展開します。`chdread` はメタデータを
表示し、`chdhash` はハッシュを計算します。ツールごとに役割が決まっているので、実行ファイルにファイルを
ドロップするだけで目的の処理が行われます。`chdlite-gui` は同じ機能をウィンドウで使えます。どの
オプションもフラグで指定できます（`--help`）。chdlite は別途リリースしています。
[docs/chdlite_release_notes.ja.md](docs/chdlite_release_notes.ja.md) を参照してください。

### プロバイダーのキー

任意です。管理者として「設定 › プロバイダー認証情報」で API キーを入力します。キーのないプロバイダーは
使えないだけで、他に影響はありません。

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

## スキャンとハッシュマッチ

**ファイルスキャン** はフォルダ内を巡回し、ハッシュを計算します（CRC32、MD5、SHA-1、SHA-256、
XXH3、RA Hash）。アーカイブ（CHD、RVZ/WIA/GCZ、zip、7z）は中身のハッシュを計算します。スキャンが
終わると、チェックしたボックスの内容で同じフォルダのハッシュマッチをキューに追加します。すべての
ボックスがオフの場合、マッチは実行されません。

| ボックス | 内容 |
|---|---|
| DATマッチ | ファイルのハッシュが一致するDATのリリースに各ゲームを紐付けます（CHDはトラック単位でも照合）。ハッシュで一致しなかったものはシリアルで照合します。オフライン。 |
| RetroAchievements | 同期済みのRAデータから、RA Hash で RetroAchievements のゲームに紐付けます。オフライン、常にオン。 |
| Hasheous | hasheous.org で各ファイルをハッシュ検索します（キー不要）。リリースと、他のプロバイダーでのIDを返します。 |
| PlayMatch | まだIGDB IDのないファイルを検索します（キー不要、1リクエスト100件）。同様のIDを返します。 |

Hasheous と PlayMatch によるハッシュマッチは、データがローカルにキャッシュされていない場合に
インターネットに接続します。

**Hasheous と PlayMatch から照会するプロバイダー:** IGDB、TheGamesDB、RetroAchievements、
SteamGridDB、ScreenScraper、MobyGames（見つかったIDでデータとアートを取得）。まだ紐付けのない
タイトルは、IGDB、TheGamesDB、SteamGridDB、MobyGames、EmuReady、ScreenScraper で名前検索します。
キーが未設定のプロバイダーはスキップされます。

## コピー

**ゲームとそのアートワークのコピー：** デスクトップクライアントモードでは、メインサーバーからの
コピーもできます（クライアント設定で、ネットワークマウントしたフォルダ（SMB、NFS など）とサーバーの
コレクションルートフォルダを追加してください）。

**セット、または個別のタイトル／リリース／クロプラタイトル／決定版をコピーできます。**

コピーモード（コピー時に選択）:

| モード | 用途 | ルール |
|---|---|---|
| このコレクションへ | サーバー → このマシンのコレクションフォルダ | このマシンに既にあるものはスキップ。コピーしたものはすぐにこのマシンの所持として記録 |
| SDカード / USB / スマホのフォルダへ | コレクション外の任意のフォルダ | 他の場所で所持していてもスキップしない。記録もしない |
| このコレクション内 | その場で変換（ISO → RVZ など） | 両方のコピーを残す |
| サーバーのコレクション内 | サーバー自身のUIから | 全ファイルを検証。上書きしない |
| アップロード | このマシン → サーバー | サーバーにないものだけ。上書きしない。サーバーが再スキャン |

- コピー先には、プラットフォームごとのサブフォルダが作られます。サブフォルダ名の形式は、
  コピー時に選べます。
- 容量節約のためのアーカイブ化や、特定のデバイス向けの別形式への変換を、コピーと同時に行えます。
- コピー先にファイルが既にある場合は、スキップされます。

**アートワークのエクスポート：** コピーダイアログで「コピー後にアートをエクスポート」にチェックを
入れるか、**アートをエクスポート…** を単独で使います。画像ファイルは、エミュレーターで使える
名前とフォルダ構成で出力されます。

- **RetroArch:** `<system>/Named_Boxarts`、`Named_Titles`、`Named_Snaps`、`Named_Logos`、
  `<rom name>.png`。libretro-thumbnails と同じ構成です。
- **PCSX2 / DuckStation:** covers フォルダ1つに `<serial>.<ext>`、またはゲームファイル名で
  出力します。

各スロットにどのプロバイダーからの画像を使うかは `media_export_priority` に従います（実物の
パッケージスキャンが優先）。

## コレクションタブ

コレクションは、所持しているすべてのゲームと、Redump、No-Intro、TOSEC、IGDB などのデータベースに
存在するゲームのデータベースです（DAT／メタデータのダウンロードが必要）。

| サブタブ | 1行の単位 |
|---|---|
| タイトル\* | タイトルは、プラットフォームを1つに限定した1つのゲームタイトルです。 |
| リリース\*\* | リリースは、地域・言語・リビジョン・エディション・体験版などをそれぞれ別物としてカウントしたものです。 |
| クロプラタイトル\*\*\* | クロプラタイトルは、異なるプラットフォームにある同じゲームタイトルを、すべてまとめたものです。 |
| 決定版\*\*\*\* | 決定版は、オリジナル、リマスター、移植、リメイクを含むすべてのバージョン（ゲームファミリー）から、遊ぶバージョンを選ぶためのものです。1エントリーが1つのゲームファミリー（オリジナルとその移植、リメイク、GOTYエディションなど）です。 |
| ゲーム | ゲームは、1本のゲームのROM一式（揃っていないものも含む）です。同じゲームを複数所持している場合は、それぞれが1ゲームとして数えられます。1つのゲームに複数のROM（Disc 1、Disc 2 など）が含まれることがあります。 |
| ROM | ROMは、ゲームメディアの1単位です。ディスクが分かれていれば別々のROMになります。1つのROMに複数のファイルが含まれることがあります（Disc 1 = cue + track 1.bin + track 2.bin）。 |
| ファイル | ディスク上の個々のファイルです。 |
| コンテンツ | コンテンツは、ハッシュで識別されるバイト列です。ファイルそのもの、またはアーカイブ内の1つの中身がコンテンツになります。コンテンツの中に、さらにコンテンツが含まれることもあります。 |
| セット | セットは、タイトル／リリース／クロプラタイトル／決定版を取捨選択し作成した自分専用のリストです。 |
| マルチメディア、ドキュメント、ソースコード、その他 | 映像・音楽ディスク、マニュアル・雑誌・スキャン、ソースコード、ツール・トレーナー・セーブデータ |
| パッチ | RetroAchievements互換パッチ（ダウンロード可）、DAT掲載のパッチ、自分のパッチフォルダ |
| BIOS / ファームウェア | BIOS、ファームウェア、NAND、SDカードイメージなど、ハードのシステムファイルです。 |
| オーディオ | audio ディレクトリ内の音声ファイルのコレクションです。これら（とタイトル）にタグを付けると、タイトルページの表示中にサウンドトラックを再生できます。 |

\* 同じゲームでも、プラットフォームが違えば（Hoge Quest (PS2)、Hoge Quest (Dreamcast)）別の
タイトルになります。リビジョン違い（Hoge Quest、Hoge Quest (Rev A)）、地域違い
（Hoge Quest (Japan)、Adventure of Hoge (USA)）、言語違い（Hoge Quest (ja)、Hoge Quest (fr)）は、
同じタイトルにまとめられます。

\*\* コピー時に同じリリースが複数見つかった場合は、完全版（欠けているファイルがないもの）、または
設定の優先順位（ヘッダーあり > ヘッダーなし、z64 > n64 など）で選ばれます。コピー時は、地域・言語・
発売形態（通常版／廉価版／特別版）の優先順位に従って、サーバーまたはクライアントにあるリリースを
1つコピーします。所持していないリリースは飛ばして、次の優先順位に進みます。

\*\*\* Hoge Quest なら、Hoge Quest (PS2) と Hoge Quest (Dreamcast) を含みます。

\*\*\*\* 最新版だけ、またはゲームコンテンツが最も多い版だけを遊びたいときのためのものです。決定版は
既定で最新のクロプラタイトルを選びますが、一部の決定版には、最新のクロプラタイトルに以前の版の
内容が欠けているケースを説明する、ランク付きの注記があります（例: Hoge Quest 3D Remake (PC) には、
Hoge Quest (GBA) にあった追加ダンジョンがない）。その場合は、ドラッグして優先順位を好みに合わせて
変えられます。決定版は現行プラットフォームにしかないこともあるため、コレクション内で最優先の
クロプラタイトル／タイトルをコピーするか、最も決定版らしい版がコレクションにない場合は何もコピー
しない（「Steamで買え」）かを設定できます。

注: クロプラタイトルと決定版を構成するには、IGDBメタデータのダウンロードが必要です。関連データを
自動でダウンロードするよう設定できます（データベースが大きくなり動作が遅くなるため、既定はオフ）。

## RetroAchievements タブ

RetroAchievements のアカウントと実績が設定されているゲームを、最後のアカウント同期の時点の内容で
表示します（`ra_username` と `ra_api_key` が必要）。上部のアカウント欄に、プロフィールと進捗が
表示されます。ゲームのページには、実績と互換パッチが一覧表示され、ダウンロードボタンがあります。

RAタブからタイトルをコピーしたりセットに追加したりすると、実績対応のリリース **だけ** が選ばれます。
（例: Hoge Quest (USA) に実績があり Hoge Quest (Japan) にない場合、地域の優先順位が Japan > USA
でも Hoge Quest (USA) がコピーされます。）

| サブタブ | 表示内容 |
|---|---|
| タイトル | RetroAchievements の実績があるタイトル。 |
| マスタリー | ハードコアモードで全実績を解除したタイトル。 |
| コンプリート | ソフトコアモードで全実績を解除したタイトル。 |
| クリア | クリア済みだが、コンプリートしていないゲーム。 |
| プレイ中 | 開始したが、マスタリー／コンプリートしていないもの（設定 `ra_include_beaten` がオフでなければ、クリア済みも含む）。 |
| 未プレイ | 実績があり、まだ始めていないタイトル。 |

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
