## Plan: 7-Zip-zstd Integration + Streaming Hash Data Path

7-Zip-zstd 側で、既存の zlib-ng dynamic library を `deps/7-Zip-zstd/deps/zlib-ng/{platform}/{arch}/{version}/dynamic`（同階層の `include` も使用）から参照してリンクする。zlib-ng 自体の再ビルドは行わない。加えて、アーカイブ展開時に「内容チャンク」を外部へストリーミング供給し、保存なし（hash-only）および部分取得（必要量到達で早期停止）を可能にする。作業ファイル・補助スクリプト・ログ・生成物・テストは原則 `user` 配下に限定し、例外として「7-Zip-zstd 本体ソースの最小調整」のみ `C/` や `CPP/` を変更対象とする。

**Steps**
1. Phase 1: Scope and path contract
1. 依存ライブラリの参照元を `deps/7-Zip-zstd/deps/zlib-ng/{platform}/{arch}/{version}/dynamic` に固定する。
2. ヘッダ参照は `deps/7-Zip-zstd/deps/zlib-ng/{platform}/{arch}/{version}/include` を優先使用する。
3. 補助スクリプト、仕様メモ、ログ、成果物は `deps/7-Zip-zstd/` 配下のみへ配置する。
4. テストコード、テストデータ、テスト生成物は `deps/7-Zip-zstd/tests` に固定する。
5. 本体コード変更は、zlib 動的リンク対応と展開チャンク供給対応の最小差分に限定する。

2. Phase 2: Dependency discovery and selection
1. `platform/arch/version` の解決ルールを定義する（CLI 引数、環境変数、既定値の順）。
2. 対象 dynamic ディレクトリ内の実体を検出する（macOS: `*.dylib`, Linux: `*.so*`, Windows: `*.dll` + import lib）。
3. 必須ファイル欠落時は即エラーにし、不足パスを明示する。

3. Phase 3: Build wiring for 7-Zip-zstd
1. 7-Zip-zstd のビルドフラグに include path と library path を注入する。
2. 動的リンク用のランタイム探索設定を追加する（macOS `@rpath` / Linux `rpath` / Windows は実行時配置）。
3. 既存の zlib 参照がある場合は、競合しないよう zlib-ng 側への解決順を調整する。

4. Phase 4: Streaming extraction hook (main app minimal adjustment)
1. 展開データの主要経路にチャンク通知フックを追加する。
2. チャンク契約は `data, size, fileIndex, offsetInFile, context` を基本形とする。
3. ファイル境界契約として `OnFileBegin/OnFileEnd` を追加し、外部ハッシャーが file 単位で状態管理できるようにする。
4. 複数ファイルアーカイブでは `fileIndex` ごとに独立ストリームとして扱えることを保証する。
5. 保存なしモード（hash-only）でも同一チャンク通知を利用できるようにする。
6. 7-Zip-zstd 本体ではハッシュ本体実装を持たず、外部プロジェクトへチャンクを渡す責務に限定する。

5. Phase 5: Partial-content and early-stop behavior
1. 先頭 N バイトや `offset-length` 指定で必要範囲のみ取得できる契約を用意する。
2. 必要量到達時の停止を `OnFileBegin` または `OnData` で要求できるようにする。
3. 既存の展開エラー系終了と区別し、ログに `complete/partial/error` を明記する。
4. file 単位選択（indices）と file 内範囲指定を分離し、不要データの流出を抑える。

6. Phase 6: Hash algorithm compatibility contract
1. CRC32 / MD5 / SHA1 / SHA256 / XXH3 はすべてインクリメンタル更新前提で扱う。
2. チャンクサイズは可変でよい前提とし、アルゴリズム側の内部バッファ処理に委ねる。
3. XXH3 は streaming state API を前提にし、state 管理は外部ハッシュ側責務とする。
4. solid archive では対象位置より前段の展開が必要な場合がある点を制約として明記する。
5. ファイルごとのハッシュ確定タイミング（開始・更新・終了）を `fileIndex` 単位で仕様化する。

7. Phase 7: User-scoped wrappers, docs, logs, and tests
1. 実行入口スクリプトを `deps/7-Zip-zstd/scripts` に置き、依存パス解決と実行モード（extract/hash-only/partial）を一本化する。
2. 仕様書を `deps/7-Zip-zstd/docs` に置き、コールバック引数・戻り値・終了理由を定義する。
3. ログを `deps/7-Zip-zstd/logs/build-YYYYMMDD-HHMMSS.log` に保存し、参照した `platform/arch/version` を必ず記録する。
4. テストを `deps/7-Zip-zstd/tests` に集約し、ケース・入力・期待値・結果ログを同一階層で管理する。

8. Phase 8: Validation
1. 指定した `platform/arch/version` の dynamic lib と include が正しく解決されることを確認する。
2. 7-Zip-zstd バイナリが zlib-ng dynamic lib を参照していることを確認する（`otool -L` / `ldd` / `dumpbin` 等）。
3. 保存なし hash-only で内容チャンクを取得できることを確認する。
4. 範囲指定ハッシュ（先頭 N バイト、任意 offset-length）で早期停止できることを確認する。
5. solid / 非 solid / 暗号化アーカイブで挙動差分を確認する。
6. テストコード・テストデータ・テスト出力が `deps/7-Zip-zstd/tests` 配下にあることを確認する。
7. `user` 以外に新規補助ファイルや生成物を作っていないことを確認する。
8. 本体ソース変更が最小差分にとどまっていることを確認する。
9. 複数ファイルアーカイブで `fileIndex` ごとに独立したハッシュ結果が得られることを確認する。
