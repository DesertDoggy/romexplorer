## Plan: User-Scoped Multi-Target Release Builder

/user 配下のみへ生成物を出す単一 sh スクリプトで、zlib-ng を性能重視・zlib 互換設定で macOS arm64 / iOS arm64 / Android arm64 / Linux x64 / Windows x64 向けに static+dynamic 同時ビルドする。言語は C23 優先（ツールチェーン非対応時は C11 へ自動フォールバック）とし、x64 では AVX2/AVX512 系を有効化、ARM では ARM 最適化群を有効化する。version は repo 由来値優先、なければ datestamp を使う。

**Steps**
1. Phase 1: Script contract and strict user-scope paths
1. deps/zlib-ng/scripts/build-zlib-ng-release.sh を唯一の実行入口として設計する。
2. 生成先・中間物をすべて /Users/max/GitHub/zlib-ng/user 以下に固定する。
3. 中間ビルドは deps/zlib-ng/release/_build/{platform}/{arch}、成果物は deps/zlib-ng/release/{platform}/{arch}/{version}/{static|dynamic} を強制する。
4. ログ出力先を deps/zlib-ng/logs に固定し、実行ごとに build-YYYYMMDD-HHMMSS.log を作成する。
5. --platform all|mac|ios|android|linux|windows、--clean、--version、--c-standard auto|23|11 を提供する。
6. CLI 通知規約を固定する: フォールバック時は必ず [FALLBACK]、異常終了時は必ず [ERROR]、通常進捗は [INFO] プレフィックスで表示し、理由と次アクション（再試行内容や不足環境変数名）を1行で出す。標準出力と標準エラーの詳細は同時に deps/zlib-ng/logs へ保存する。

2. Phase 2: Baseline CMake profile (performance + compatibility)
1. 全ターゲット共通 CMake 定義を固定する。
2. 有効化: ZLIB_COMPAT=ON, ZLIB_ALIASES=ON, WITH_GZFILEOP=ON, WITH_OPTIM=ON, WITH_CRC32_CHORBA=ON。
3. 無効化: WITH_REDUCED_MEM=OFF, BUILD_TESTING=OFF, WITH_GTEST=OFF, WITH_BENCHMARKS=OFF。
4. BUILD_SHARED_LIBS は未指定にし、zlib-ng の CMake 実装どおり static+shared を同時生成する。
5. CMAKE_BUILD_TYPE=Release を強制する。

3. Phase 3: Language and ISA tuning rules
1. 言語は C のみ（C++ には切り替えない）。
2. CMAKE_C_STANDARD=23 をまず適用し、configure 失敗時のみ CMAKE_C_STANDARD=11 で再試行する。
3. x64 向けは WITH_AVX2=ON, WITH_AVX512=ON, WITH_AVX512VNNI=ON, WITH_VPCLMULQDQ=ON を明示指定する。
4. ARM 向けは WITH_ARMV8=ON, WITH_NEON=ON を明示指定する。

4. Phase 4: Per-platform configure strategy
1. macOS arm64: -DCMAKE_OSX_ARCHITECTURES=arm64。
2. iOS arm64: 環境変数経由（例: IOS_TOOLCHAIN_FILE, CMAKE_OSX_SYSROOT=iphoneos, CMAKE_OSX_ARCHITECTURES=arm64）。未設定は即エラー。
3. Android arm64: ANDROID_NDK_HOME 必須、$ANDROID_NDK_HOME/build/cmake/android.toolchain.cmake, ANDROID_ABI=arm64-v8a を使用。
4. Linux x64: native x64 か cross x86_64 toolchain を使用（x86 は対象外）。
5. Windows x64: cmake/toolchain-llvm-mingw-x86_64.cmake を既定使用。Windows でも sh 実行環境（Git Bash/MSYS2/WSL）前提。

5. Phase 5: Version resolution and metadata
1. version は repo 内定義（zlib-ng.h.in の ZLIBNG_VERSION、または CMake から得るバージョン）を優先採用する。
2. 取得失敗時のみ YYYYMMDD を使う。
3. 各成果物ディレクトリに build-info.txt を置き、commit SHA・cmake version・有効フラグ・C 標準・ターゲット triplet を記録する。

6. Phase 6: Artifact collection logic
1. static: *.a と（Windows の場合）*.lib を static 配下へ。
2. dynamic: *.so*、*.dylib、*.dll と import lib を dynamic 配下へ。
3. ヘッダ（zlib.h, zconf.h 等）を各 version 直下に include として配置する。

7. Phase 7: Validation
1. --platform mac|linux|windows のスモークで static+dynamic 両方を確認。
2. --platform ios|android は必要 env 不足時の明示エラーと、設定済み時の configure 通過を確認。
3. /user 以外に生成物が出ていないことを確認。
4. C23 成功/失敗時フォールバック挙動をログで確認。
5. 実行ごとに deps/zlib-ng/logs/build-YYYYMMDD-HHMMSS.log が作成され、詳細出力（stdout/stderr）が保存されることを確認。
6. フォールバック発生ケースで [FALLBACK]、異常ケースで [ERROR]、通常進捗で [INFO] が CLI と deps/zlib-ng/logs の両方で確認できることを確認。
