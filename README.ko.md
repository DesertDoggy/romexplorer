# romexplorer

[English](README.md) · [日本語](README.ja.md) · [Français](README.fr.md) · [Español](README.es.md) · [Italiano](README.it.md) · [Português](README.pt.md) · [Deutsch](README.de.md) · **한국어** · [Polski](README.pl.md)

*이 문서는 번역본입니다. 내용이 다를 경우 영어 README를 기준으로 합니다.*

> **romexplorer는 ROM, BIOS, 키를 배포하거나 다운로드하지 않으며, 불법 복제를 지원하지 않습니다.**
> 직접 소유한 게임을 직접 덤프해서 사용하세요.

ROM 컬렉션 관리 도구입니다. 폴더를 스캔하고, 모든 파일을 해시로 참조 DAT와 대조해 식별합니다.
메타데이터와 아트를 가져오고, 파일을 변환·검증·복사합니다. 브라우저 콘솔과 웹 GUI를 갖춘 서버로,
또는 데스크톱 앱으로 Windows, macOS, Linux에서 실행됩니다. 어떤 환경에서 무엇을 테스트했는지는
[docs/release_notes.ko.md](docs/release_notes.ko.md)를 참고하십시오.

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
- **기기로 복사.** 게임과 아트를 서버나 이 기기에서 폴더나 카드로 복사합니다. 복사하면서
  변환하고, 프론트엔드가 요구하는 이름과 구조로 맞춥니다. 아트는 RetroArch나
  PCSX2 / DuckStation용 구성으로 출력합니다.
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

디스크 이미지를 끌어다 놓으면 CHD나 RVZ가 만들어집니다. `chdlite`(빠름)와 `chdcomp`(최소 용량)는
양방향으로 동작합니다. 압축되지 않은 이미지는 압축하고, `.chd`나 `.rvz`는 추출합니다. `chdread`는
파일의 메타데이터를 보여 주고, `chdhash`는 해시를 계산합니다. 도구마다 하는 일이 하나라서 실행 파일에
파일을 끌어다 놓기만 하면 알맞은 작업이 수행됩니다. `chdlite-gui`는 같은 기능을 창에서 제공합니다.
모든 옵션은 플래그로도 지정할 수 있습니다(`--help`). chdlite는 별도로 릴리스됩니다.
[docs/chdlite_release_notes.md](docs/chdlite_release_notes.md)를 참고하십시오.

### 명령줄 도구(rommusic)

ROM, 디스크 이미지, 덤프한 폴더, 음악 립 세트를 `rommusic`에 끌어다 놓으면 게임의 배경 음악을 FLAC으로
옆 폴더에 저장합니다. 모든 옵션은 플래그로도 지정할 수 있습니다(`--help`). 지원 기종은
[ROM 오디오](#rom-오디오)를 참고하십시오. rommusic은 별도로 릴리스됩니다.
[docs/rommusic_release_notes.md](docs/rommusic_release_notes.md)를 참고하십시오.

### 제공자 키

선택 사항입니다. 관리자로 구성 › 제공자 인증 정보에서 API 키를 입력합니다. 키가 없는 제공자는
사용할 수 없을 뿐입니다.

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

## 스캔과 해시 매치

**파일 스캔**은 폴더를 훑으며 모든 파일의 해시를 계산합니다(CRC32, MD5, SHA-1, SHA-256, XXH3,
RA 해시). 아카이브(CHD, RVZ/WIA/GCZ, zip, 7z)는 안쪽 내용의 해시를 계산합니다. 스캔이 끝나면
체크한 상자대로 같은 폴더의 해시 매치를 대기열에 추가합니다. 상자가 모두 꺼져 있으면 매치는
실행되지 않습니다.

| 상자 | 하는 일 |
|---|---|
| DAT 매치 | 파일의 해시가 일치하는 DAT 릴리스에 각 게임을 연결합니다(CHD는 트랙 단위로도 대조). 해시로 찾지 못한 것은 시리얼로 대조합니다. 오프라인. |
| RetroAchievements | 이미 동기화된 RA 데이터에서 RA 해시로 RetroAchievements 게임에 연결합니다. 오프라인, 항상 켜짐. |
| Hasheous | hasheous.org에서 각 파일을 해시로 검색합니다(키 불필요). 릴리스와 다른 제공자에서의 ID를 돌려줍니다. |
| PlayMatch | 아직 IGDB ID가 없는 파일을 검색합니다(키 불필요, 요청당 100개). 같은 종류의 ID를 돌려줍니다. |

Hasheous와 PlayMatch는 데이터가 로컬에 캐시되어 있지 않을 때만 인터넷에 접속합니다.

**Hasheous와 PlayMatch가 이어 주는 제공자:** IGDB, TheGamesDB, RetroAchievements,
SteamGridDB, ScreenScraper, MobyGames(찾은 ID로 데이터와 아트를 가져옴). 아직 연결되지 않은
타이틀은 IGDB, TheGamesDB, SteamGridDB, MobyGames, EmuReady, ScreenScraper에서 이름으로
검색합니다. 키가 없는 제공자는 건너뜁니다.

## 복사

**게임과 아트워크를 다른 대상으로 복사합니다.** 데스크톱 클라이언트 모드에서는 메인 서버에서도
복사할 수 있습니다. 클라이언트 설정에서 네트워크로 마운트한 디렉터리(SMB, NFS 등)와 서버의
컬렉션 루트를 추가하세요.

**세트, 또는 개별 타이틀·릴리스·크로스플랫·결정판을 복사할 수 있습니다.**

복사 모드(복사할 때 선택):

| 모드 | 용도 | 규칙 |
|---|---|---|
| 이 컬렉션으로 | 서버 → 이 기기의 컬렉션 폴더 | 이 기기에 이미 있는 것은 건너뜀. 복사한 것은 바로 이 기기의 보유로 기록 |
| SD 카드 / USB / 휴대폰 폴더로 | 컬렉션 밖의 아무 폴더 | 다른 곳에 보유하고 있어도 건너뛰지 않음. 기록하지 않음 |
| 이 컬렉션 안에서 | 제자리 변환(ISO → RVZ 등) | 두 사본을 모두 유지 |
| 서버 컬렉션 안에서 | 서버 자체 UI에서 | 모든 파일 검증. 덮어쓰지 않음 |
| 업로드 | 이 기기 → 서버 | 서버에 없는 것만. 덮어쓰지 않음. 서버가 다시 스캔 |

- 복사본은 플랫폼별 하위 폴더에 들어갑니다. 하위 폴더 이름 형식은 복사할 때 고릅니다.
- 공간을 아끼기 위한 압축 보관이나, 특정 기기가 필요로 하는 다른 형식으로의 변환을 복사와
  동시에 할 수 있습니다.
- 대상에 파일이 이미 있으면 건너뜁니다.

**아트워크도 내보낼 수 있습니다.** 복사 대화상자에서 "복사 후 아트 내보내기"를 체크하거나,
**아트 내보내기...** 를 따로 사용합니다. 이미지 파일은 에뮬레이터용 이름과 폴더 구성으로
출력됩니다.

- **RetroArch:** `<system>/Named_Boxarts`, `Named_Titles`, `Named_Snaps`, `Named_Logos`,
  `<rom name>.png`. libretro-thumbnails와 같은 구성입니다.
- **PCSX2 / DuckStation:** covers 폴더 하나에 `<serial>.<ext>`, 또는 게임 파일 이름으로
  출력합니다.

각 슬롯에 어느 제공자의 이미지를 쓸지는 `media_export_priority`를 따릅니다(실물 패키지 스캔
우선).

## 컬렉션 탭

컬렉션은 보유한 모든 게임과, Redump, No-Intro, TOSEC, IGDB 같은 데이터베이스에 있는 다른
게임들의 데이터베이스입니다(DAT / 메타데이터 다운로드 필요).

| 하위 탭 | 한 행의 단위 |
|---|---|
| 타이틀\* | 타이틀은 하나의 플랫폼에 있는 하나의 게임입니다. |
| 릴리스\*\* | 릴리스는 지역·언어·리비전·에디션·체험판 등을 각각 따로 센 게임입니다. |
| 크로스플랫\*\*\* | 크로스플랫은 서로 다른 플랫폼에 있는 같은 게임의 타이틀을 모두 묶은 것입니다. |
| 결정판\*\*\*\* | 결정판은 오리지널, 리마스터, 이식, 리메이크를 포함한 모든 버전(게임 패밀리) 가운데 플레이할 버전을 고르기 위한 것입니다. 한 행이 하나의 게임 패밀리(오리지널과 그 이식, 리메이크, GOTY 에디션 등)입니다. |
| 게임 | 게임은 한 게임의 ROM 한 세트(다 갖춰지지 않은 것 포함)입니다. 같은 게임을 여러 개 보유하면 각각이 하나의 게임으로 셉니다. 한 게임에 여러 ROM(Disc 1, Disc 2 등)이 들어 있을 수 있습니다. |
| ROM | ROM은 게임 미디어의 한 단위입니다. 디스크가 나뉘어 있으면 각각 별도의 ROM입니다. 한 ROM에 여러 파일이 들어 있을 수 있습니다(Disc 1 = cue + track 1.bin + track 2.bin). |
| 파일 | 디스크에 있는 개별 파일입니다. |
| 콘텐츠 | 콘텐츠는 해시로 식별되는 바이트열입니다. 파일 자체이거나 아카이브 안의 항목 하나일 수 있습니다. 콘텐츠 안에 또 콘텐츠가 들어 있을 수도 있습니다. |
| 세트 | 세트는 타이틀 / 릴리스 / 크로스플랫 / 결정판을 골라 순서대로 만든 나만의 목록입니다. |
| 멀티미디어, 문서, 소스 코드, 기타 | 영상·음악 디스크, 매뉴얼·잡지·스캔, 소스 코드, 도구·트레이너·세이브 데이터 |
| 패치 | RetroAchievements 호환 패치(다운로드 가능), DAT에 실린 패치, 내 패치 폴더 |
| BIOS / 펌웨어 | BIOS, 펌웨어, NAND, SD 카드 이미지 등 기기의 시스템 파일입니다. |
| 오디오 | audio 디렉터리에 있는 오디오 파일 컬렉션입니다. 이것들(과 타이틀)에 태그를 달면 타이틀 페이지를 보는 동안 사운드트랙을 재생할 수 있습니다. |

\* 같은 게임이라도 플랫폼이 다르면(Hoge Quest (PS2), Hoge Quest (Dreamcast)) 별도의 타이틀이
됩니다. 리비전(Hoge Quest, Hoge Quest (Rev A)), 지역(Hoge Quest (Japan), Adventure of Hoge
(USA)), 언어(Hoge Quest (ja), Hoge Quest (fr))가 다른 것은 같은 타이틀로 묶입니다.

\*\* 복사할 때 같은 릴리스가 여러 개 있으면, 완전한 것(빠진 파일 없음)이 먼저이고 그다음은
설정의 우선순위(헤더 있음 > 헤더 없음, z64 > n64 등)로 고릅니다. 복사할 때는 지역·언어·발매
형태(일반판 / 염가판 / 특별판) 우선순위에 따라 서버나 클라이언트에 있는 릴리스 하나를 복사합니다.
보유하지 않은 릴리스는 건너뛰고 다음 우선순위로 넘어갑니다.

\*\*\* Hoge Quest라면 Hoge Quest (PS2)와 Hoge Quest (Dreamcast)를 포함합니다.

\*\*\*\* 최신 버전만, 또는 플레이할 수 있는 콘텐츠가 가장 많은 버전만 하고 싶을 때를 위한
것입니다. 결정판은 기본적으로 최신 크로스플랫을 고르지만, 일부 결정판에는 최신 크로스플랫에 이전
버전의 내용이 빠진 경우를 설명하는 등급별 주석이 있습니다(예: Hoge Quest 3D Remake (PC)에는
Hoge Quest (GBA)에 있던 추가 던전이 없음). 이럴 때는 버전을 끌어서 원하는 순서로 바꿀 수
있습니다. 결정판은 현행 플랫폼에만 있을 수도 있으므로, 컬렉션에서 우선순위가 가장 높은
크로스플랫 / 타이틀을 복사할지, 가장 결정판다운 버전이 컬렉션에 없으면 아무것도 복사하지 않을지
(즉, Steam에서 하세요)를 설정할 수 있습니다.

참고: 크로스플랫과 결정판은 IGDB 메타데이터로 구성되므로 이를 다운로드해야 합니다. 관련 데이터를
자동으로 다운로드하도록 설정할 수 있습니다(데이터베이스가 커져 동작이 느려지므로 기본값은 꺼짐).

## ROM 오디오

타이틀 페이지에서 ROM 안의 음악을 파일에서 바로 재생합니다. 압축 해제나 복사는 하지 않습니다. 먼저
오디오 탭에서 "ROM 오디오 색인"를 실행하세요. `rommusic`도 같은 것을 읽습니다.

| 음악 위치 | 기종 | 파일 |
|---|---|---|
| CD 오디오 트랙 | 모든 CD 게임(PC 엔진 CD, 메가 CD, 세가 새턴, 플레이스테이션, 드림캐스트 등) | CHD, cue/bin, gdi |
| 디스크 안의 음악 파일 | 플레이스테이션, 플레이스테이션 2, PSP, 세가 새턴, 드림캐스트, 게임큐브, Wii, Xbox, Xbox 360 | CHD, cue/bin, gdi, ISO, CSO/ZSO, RVZ/WIA/GCZ/WBFS, XISO |
| | Wii U | .wua. .wud/.wux와 다운로드판은 옆에 키가 있으면 읽습니다 |
| | PS3, PS Vita | 복호화된 폴더 덤프 |
| 사운드 아카이브 | 닌텐도 DS(스트림과 시퀀스 음악), 닌텐도 3DS(복호화된 이미지만) | .nds, .3ds/.cci |
| 게임의 사운드 엔진 | 게임보이 어드밴스: 닌텐도 표준 엔진을 쓰는 게임(약 절반) | .gba |
| 음악 립 | 패미컴·디스크 시스템(NSF, NSFE), 슈퍼 패미컴(SPC), 게임보이(GBS), PC 엔진(HES), MSX(KSS), 마스터 시스템·게임기어(SGC, VGM), 메가 드라이브(VGM, GYM), ZX Spectrum·Amstrad CPC(AY), Atari 8비트(SAP), 닌텐도 64(USF), 플레이스테이션(PSF), 플레이스테이션 2(PSF2), 세가 새턴(SSF), 드림캐스트(DSF), 게임보이 어드밴스(GSF), 원더스완(WSR), 네오지오 포켓·버추얼 보이(VGM) | 게임 폴더나 립 라이브러리. zip 그대로도 가능 |

- 립 라이브러리(설정 › 오디오)는 NSF, NSFE, GBS, SPC, HES, KSS, SGC 세트를 내용으로 ROM과
  대조합니다. 이름이 달라도 지역별 ROM 모두에서 재생됩니다.
- 플레이스테이션과 플레이스테이션 2 립에는 컬렉션에 있는 PS2 BIOS를 사용합니다. BIOS가 없으면
  건너뜁니다.
- zip, 7z 안의 디스크 이미지는 아직 읽지 못합니다. 암호화된 이미지도 읽지 못합니다.
- `rommusic`은 슈퍼 패미컴 게임의 타이틀 음악을 립 없이 만들 수 있습니다.

## RetroAchievements 탭

RetroAchievements 계정과 도전 과제가 있는 게임을 마지막 계정 동기화 시점 기준으로 보여 줍니다
(`ra_username`과 `ra_api_key` 필요). 위쪽 계정 영역에 프로필과 진행 상황이 표시됩니다. 게임
페이지에는 도전 과제와 호환 패치가 다운로드 버튼과 함께 나열됩니다.

RA 탭에서 타이틀을 복사하거나 세트에 추가하면 도전 과제가 있는 릴리스**만** 선택됩니다.
(예: Hoge Quest (USA)에는 도전 과제가 있고 Hoge Quest (Japan)에는 없다면, 지역 우선순위가
Japan > USA여도 Hoge Quest (USA)가 복사됩니다.)

| 하위 탭 | 표시 내용 |
|---|---|
| 타이틀 | RetroAchievements 도전 과제가 있는 타이틀. |
| 마스터리 | 하드코어 모드에서 모든 도전 과제를 달성한 타이틀. |
| 완료 | 소프트코어 모드에서 모든 도전 과제를 달성한 타이틀. |
| 클리어 | 클리어했지만 완료하지 않은 게임. |
| 진행 중 | 시작했지만 마스터리 / 완료하지 않은 것(`ra_include_beaten` 설정이 꺼져 있지 않으면 클리어한 것도 포함). |
| 플레이 예정 | 도전 과제가 있지만 아직 시작하지 않은 타이틀. |

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
