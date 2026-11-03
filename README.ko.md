# romexplorer

[English](README.md) · [日本語](README.ja.md) · [Français](README.fr.md) · [Español](README.es.md) · [Italiano](README.it.md) · [Português](README.pt.md) · [Deutsch](README.de.md) · **한국어** · [Polski](README.pl.md)

*이 문서는 번역본입니다. 내용이 다를 경우 영어 README를 기준으로 합니다.*

ROM 컬렉션 관리 도구입니다. 폴더를 스캔하고, 모든 파일을 해시로 참조 DAT와 대조해 식별합니다.
메타데이터와 아트를 가져오고, 파일을 변환·검증·복사합니다. 브라우저 콘솔과 웹 GUI를 갖춘 서버로,
또는 데스크톱 앱으로 Windows, macOS, Linux에서 실행됩니다. 어떤 환경에서 무엇을 테스트했는지는
[docs/release_notes.md](docs/release_notes.md)를 참고하십시오.

## 주요 기능

- **스캔과 식별.** 디렉터리를 탐색하고, 각 파일의 플랫폼을 내용으로 판별합니다. 해시(CRC32, MD5,
  SHA-1, SHA-256, XXH3, RetroAchievements)를 계산한 뒤, 먼저 해시로, 다음으로 이름으로 카탈로그와
  대조합니다.
- **컨테이너 내부 읽기.** CHD, RVZ/WIA/GCZ, zip, 7z는 스트림으로 디코딩합니다. 따라서 디스크
  이미지를 먼저 디스크에 풀지 않고도 해시를 계산하고 식별할 수 있습니다.
- **변환과 재압축.** CHD(CD, GD-ROM, DVD)와 RVZ의 생성·추출, zip과 7z, NES/SNES 헤더 추가·제거를
  지원합니다. 빠진 `.cue`, `.gdi`, `.m3u`도 다시 만들 수 있습니다.
- **카탈로그.** Redump, MAME-Redump, No-Intro, TOSEC, RetroAchievements, RetroBIOS DAT를 내장
  MariaDB로 가져오고, 지역별 릴리스와 타이틀을 통합합니다.
- **메타데이터와 아트.** IGDB, ScreenScraper, TheGamesDB, SteamGridDB, MobyGames, Hasheous,
  PlayMatch, EmuReady, RetroAchievements를 지원하며, 각각 요청 제한을 따로 관리합니다.
- **기기로 복사.** 선택한 게임을 폴더나 카드로 복사합니다. 복사하면서 변환하고, 프론트엔드가
  요구하는 이름과 구조로 맞춥니다.
- **검증과 백업.** 기록된 해시로 무결성을 검사합니다. 데이터베이스와 카탈로그를 zstd로 백업하고
  복원할 수 있습니다.
- **기기 간 공유.** 메인 서버의 카탈로그와 아트를 클라이언트와 피어가 미러링할 수 있습니다.
  SQLite 내보내기는 휴대폰용으로 사용합니다.

모든 기능은 REST API(`/api/v1/...`), 브라우저 콘솔(`/`), 웹 GUI(`/app/`)에서 사용할 수 있습니다.

## 초기 설정

패키지는 세 가지입니다. 하나를 선택하십시오.

| 패키지 | 구성 | 설치 |
|---|---|---|
| **server** | 서버, 워커, 내장 MariaDB, 웹 GUI | 설치 프로그램 실행 |
| **desktop** | 자체 MariaDB를 갖춘 GUI 앱 | 실행만 하면 됨 |
| **cli** | chdlite: 독립 실행형 변환 / 추출 / 해시 도구, 데이터베이스 불필요 | 실행만 하면 됨 |

### 서버

1. 패키지의 압축을 풉니다.
2. 선택 사항: `romexplorer.conf.example`을 `romexplorer.conf`로 복사해 편집합니다. 모든 값은
   생략할 수 있으며, 비밀번호를 비워 두면 자동으로 생성됩니다. 이 파일은 최초 설치 때만 읽습니다.
3. 설치 프로그램을 실행합니다.
   - Linux / macOS: `./install.sh` -- root로 실행하면 `/opt/romexplorer`에 시스템 서비스로,
     일반 사용자로 실행하면 `~/.local/opt/romexplorer`에 사용자 서비스로 설치됩니다.
   - Windows: 관리자 권한 PowerShell에서 `.\install.ps1`을 실행합니다.
4. 처음 시작할 때 데이터베이스, 데이터베이스 계정, 관리자 로그인이 만들어지고, 콘솔 로그인 정보가
   **한 번만** 표시됩니다. 반드시 보관해 두십시오.
5. `http://127.0.0.1:7337/`을 열고 로그인합니다.

기본값: 서버는 `127.0.0.1:7337`, 내장 MariaDB는 `3307`에서 대기합니다. LAN에 서비스하려면
`server_bind=0.0.0.0`을 설정합니다. 이 경우 다른 기기와는 TLS로 통신하며, 인증서는 서버가 처음
시작할 때 만듭니다.

설치 후에는 다음 작업을 할 수 있습니다.

- 관리자 비밀번호는 콘솔에서, 또는 `romexplorer_server reset-admin`으로 변경합니다.
- `romexplorer_dbtool check`로 설치 상태를 확인합니다. 찾은 네이티브 라이브러리와 데이터베이스를
  보여 줍니다.
- `./install.sh --uninstall`(데이터 유지) 또는 `--uninstall --purge`로 제거합니다.

### 서버 업데이트

새 릴리스의 설치 프로그램을 기존 설치 위에 실행합니다. 설치 프로그램은 데이터베이스를 감지하고,
서비스를 중지한 뒤 실행 파일, `lib/`, 함께 제공되는 에셋을 교체하고, 서비스를 다시 시작합니다.
데이터베이스, 설정, 인증 정보, 미디어 등 `data/`와 `assets/` 아래의 모든 것은 그대로 유지됩니다.

- Linux / macOS에서 최신 릴리스로 바로 업데이트:
  `curl -fsSL https://raw.githubusercontent.com/DesertDoggy/romexplorer/main/scripts/install.sh | bash -s -- --release latest`
- 또는 새 패키지의 압축을 풀고 그 안의 `./install.sh`를 실행하거나, 이미 설치된 설치 프로그램에
  패키지를 지정합니다.
  `./install.sh --from romexplorer-server-<version>-<os>-<arch>.tar.gz`
- Windows: 새 패키지의 압축을 풀고 관리자 권한 PowerShell에서 그 안의 `.\install.ps1`을
  실행하거나, `.\install.ps1 -From <package>.zip`을 실행합니다.

최초 설치와 같은 사용자(root 여부 포함)로 실행하십시오. `--prefix` / `-Prefix`를 지정했다면 같은
값을 넘깁니다. romexplorer는 알파 단계이므로 릴리스 사이에 데이터베이스 스키마가 바뀔 수
있습니다. 먼저 백업하십시오(구성 › 백업).

### 데스크톱

압축을 풀고 `romexplorer`를 실행합니다(macOS에서는 `romexplorer.app`. 공증을 받지 않았으므로 처음
실행할 때는 마우스 오른쪽 버튼 클릭 > 열기를 사용합니다). 처음 실행할 때 자체 데이터베이스를 만들고
시작합니다.

### 명령줄 도구(chdlite)

디스크 이미지를 끌어다 놓으면 CHD나 RVZ가 만들어집니다. `chdlite`(빠름), `chdcomp`(최소 용량),
`chdread`(추출), `chdhash`(해시)는 모드별로 나뉜 도구입니다. 실행 파일에 파일을 끌어다 놓기만 하면
알맞은 작업이 수행됩니다. `chdlite-gui`는 같은 기능을 창에서 제공합니다. 모든 옵션은 플래그로도
지정할 수 있습니다(`--help`). chdlite는 별도로 릴리스됩니다.
[docs/chdlite_release_notes.md](docs/chdlite_release_notes.md)를 참고하십시오.

### 제공자 키

선택 사항입니다. 관리자로 구성 › 제공자 인증 정보에서 API 키를 입력합니다. 키는 서버에
암호화되어 저장되며 다시 표시되지 않습니다. 키가 없는 제공자는 사용할 수 없을 뿐입니다.

### 처음 사용하기

1. 디렉터리를 추가하고 어떤 플랫폼인지 지정합니다. 자동 감지에 맡길 수도 있습니다.
2. DAT 동기화를 실행해 대조에 쓸 카탈로그를 준비합니다.
3. 해당 디렉터리에서 파일 스캔을 실행합니다.
4. 해시 대조를 실행한 다음, 제공자 조회로 메타데이터와 아트를 가져옵니다.

각 단계는 모두 작업입니다. 아래를 참고하십시오.

### 소스에서 빌드

```bash
git submodule update --init --recursive
bash scripts/build_native_deps.sh        # every native library, into app_root_ex/lib/<os>/<arch>/
bash scripts/build_gui_web.sh            # the web GUI (needs `dx`)
cargo run --bin romexplorer_server
bash scripts/build_release.sh            # or: all packages for this host, into release/<version>/
```

`build_native_deps.sh --check`는 빌드하지 않고 빠진 도구만 알려 줍니다. macOS에서는 Homebrew의
`bash`, `bison`, `ninja`가 필요합니다.

## 작업

시간이 걸리는 처리는 모두 백그라운드 **작업**으로 실행되므로, 브라우저를 닫아도 계속됩니다. 각
작업은 진행 상황을 표시하고, 취소할 수 있으며, 예약해 두면(예약 작업 탭) 몇 분·시간·일·주·월
간격으로 반복 실행할 수 있습니다. 주요 작업은 다음과 같습니다.

- **DAT 동기화** -- 참조 DAT(Redump, No-Intro, TOSEC 등)를 다운로드해 가져옵니다.
- **제공자 메타데이터 및 미디어 다운로드** -- IGDB, ScreenScraper, SteamGridDB 등에서 게임
  정보와 아트를 가져옵니다.
- **파일 스캔** -- 디렉터리를 탐색해 파일의 해시를 계산하고 카탈로그와 대조합니다.
- **무결성 검사** -- 보유한 파일을 다시 읽어 기록된 해시와 비교합니다.
- **시트 / 재생 목록 검사 및 생성** -- `.cue`, `.gdi`, `.m3u` 파일이 가리키는 파일이 여전히
  유효한지 확인하고, 빠진 파일은 다시 만듭니다.

## 서드파티 코드

romexplorer는 다음 프로젝트를 바탕으로 만들어졌습니다. 대부분 라이브러리로 사용하며, `external/`
아래에 서브모듈로 고정해 둡니다. '패치'는 빌드할 때 해당 프로젝트 자체의 코드를 몇 줄 수정한다는
뜻입니다. '래퍼'는 코드를 수정하지 않고 그 주위에 자체 소형 라이브러리를 덧붙인다는 뜻입니다. 둘
다 `deps/<name>/`에 두며, 서브모듈 안에는 두지 않습니다. 모든 프로젝트에 감사드립니다.

| 프로젝트 | 용도 | 변경 사항 |
|---|---|---|
| [7-Zip-zstd](https://github.com/mcmilk/7-Zip-zstd) | zip / 7z | 패치, 래퍼 |
| [chdman-simd](https://github.com/grouik1er-coder/chdman-simd) | CHD 생성 / 추출 | 패치, 래퍼 |
| [MAME](https://github.com/mamedev/mame) | chdman-simd를 빌드하는 원본 소스 | 없음 |
| [libchdr](https://github.com/rtissera/libchdr) | LZMA 디코더, CHD 리더의 참고 자료 | 없음 |
| [Dolphin](https://github.com/dolphin-emu/dolphin) | GameCube / Wii(RVZ, WIA, GCZ) | 래퍼 |
| [Cemu](https://github.com/cemu-project/Cemu) | Wii U .wua | 래퍼 |
| [ZArchive](https://github.com/Exzap/ZArchive) | .wua / .zar 쓰기 | 없음 |
| [zstd](https://github.com/facebook/zstd) | 압축 | 없음 |
| [fmt](https://github.com/fmtlib/fmt) | Cemu 코드에서 사용 | 없음 |
| [extract-xiso](https://github.com/XboxDev/extract-xiso) | Xbox XISO | 래퍼 |
| [Beetle PSX](https://github.com/libretro/beetle-psx-libretro) | PS1 EBOOT.PBP | 래퍼 |
| [RALibretro](https://github.com/RetroAchievements/RALibretro) | RetroAchievements 해시 | 패치, 래퍼 |
| [MariaDB Server](https://github.com/MariaDB/server) | 내장 데이터베이스 | 빌드 파일 패치 |
| [MariaDB Connector/C](https://github.com/mariadb-corporation/mariadb-connector-c) | 데이터베이스 클라이언트 | 없음 |
| [zlib-ng](https://github.com/zlib-ng/zlib-ng) | Deflate | 없음 |
| [OpenSSL](https://github.com/openssl/openssl) | TLS | 없음 |
| [libxaac](https://github.com/ittiam-systems/libxaac) | AAC 오디오 | 래퍼 |
| [PlayMatch](https://github.com/RetroRealm/playmatch) | 제공자 클라이언트, 이름 대조 기능 이식 | 코드를 가져와 수정 |
| [RomM](https://github.com/rommapp/romm) | 참고 자료, 플랫폼 아이콘 | 코드로는 사용하지 않음 |
| [RAChecker](https://github.com/x3kim/RAChecker) | RetroAchievements 클라이언트의 참고 자료 | 코드로는 사용하지 않음 |
| [psx-covers](https://github.com/xlenore/psx-covers), [ps2-covers](https://github.com/xlenore/ps2-covers) (xlenore) | PS1 / PS2 커버 이미지(시리얼로 조회) | 사용할 때 다운로드, 동봉하지 않음 |

빌드 상세, 고정 버전, 패치는 [deps/README.md](deps/README.md)에 있습니다. 각 프로젝트의 라이선스는
바이너리와 함께 `lib/LICENSES/`에 포함되어 있습니다.
