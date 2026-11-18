# chdlite

디스크 이미지(CUE + BIN, GDI + BIN, ISO)를 chdlite에 끌어다 놓으면 CHD나 RVZ가 되고, `.chd`나 `.rvz`를
끌어다 놓으면 CUE + BIN, GDI + BIN, ISO로 되돌립니다. 메타데이터 읽기와 플랫폼 판별, CHD·RVZ·CSO·PBP
내용의 해시 계산, NES·N64·SNES 덤프의 형식 변환, 3DS 타이틀 복호화도 할 수 있습니다. 데이터베이스나
설정은 필요 없습니다. romexplorer의 코어를 쓰며, 따로 릴리스합니다(버전 0.3.4).

[English](README.md) · [日本語](README.ja.md) · [Français](README.fr.md) · [Español](README.es.md) · [Italiano](README.it.md) · [Português](README.pt.md) · [Deutsch](README.de.md) · **한국어** · [Polski](README.pl.md)

**다운로드:** [CHD Lite 릴리스](https://github.com/DesertDoggy/romexplorer/releases?q=c0) · **변경 사항:** [릴리스 노트](../docs/chdlite_release_notes.md) · [romexplorer](../README.ko.md)의 일부

> **chdlite는 ROM, BIOS, 키를 배포하거나 다운로드하지 않으며 불법 복제를 지원하지 않습니다.**
> 직접 소유하고 직접 덤프한 게임으로 사용하세요.

## 창

`chdlite-gui`(macOS에서는 `chdlite.app`, Linux에서는 chdlite AppImage)를 엽니다. 모드는 네 가지입니다.

| 모드 | 설명 |
|---|---|
| **Lite** | 빠르게 압축합니다. `.chd`, `.rvz`는 CUE + BIN, GDI + BIN, ISO로 풉니다. |
| **Comp** | 시간을 들여 가장 작게 압축합니다. `.chd`, `.rvz`는 같은 방식으로 풉니다. |
| **Read** | 메타데이터를 읽고 플랫폼을 판별합니다. |
| **Hash** | 파일 내용의 해시를 계산합니다. |

모드를 고르고 파일이나 폴더를 창에 끌어다 놓습니다(골라도 됩니다). 스레드 수와 동시 변환 파일 수, 출력
폴더와 기존 파일 덮어쓰기, Lite 프리셋과 Comp 코덱, RVZ 코덱·레벨·블록 크기, 해시, cue 형식과 GD-ROM
시트, 카트리지 덤프와 3DS 타이틀의 출력 형식, 손실 출력(WUA, ZAR, XISO) 전에 확인할지는 **Settings**에
있습니다. 설정은 다음에도 유지되며, 명령줄에서도 같은 설정을 씁니다.

**Android** chdlite 앱도 같은 화면입니다. 파일과 폴더는 앱 안의 브라우저에서 고릅니다.

**macOS·Linux** macOS 앱은 공증을 받지 않았으므로 처음에는 오른쪽 클릭 > 열기로 실행합니다.

## 변환

출력 형식은 이미지에 맞춰 자동으로 고릅니다.

**압축**

| 입력 | 출력 |
|---|---|
| CD: CUE + BIN, TOC + BIN, 또는 BIN/IMG 트랙 하나 | CD CHD |
| GD-ROM(드림캐스트): GDI + BIN/RAW | GD-ROM CHD |
| DVD(PS2, PSP 등): ISO | DVD CHD |
| PSP: CSO | DVD CHD |
| PS1: PBP | CD CHD |
| 게임큐브, Wii: ISO, GCM | RVZ |
| Wii U: WUD, WUX | WUA |
| Xbox 360: ISO | ZAR |
| Xbox: ISO | XISO |

**풀기**

| 입력 | 출력 |
|---|---|
| CD CHD | CUE + BIN(트랙마다 BIN 하나) |
| GD-ROM CHD | GDI + BIN(`--gdrom-sheet cue`면 CUE + BIN) |
| DVD CHD | ISO |
| RVZ | ISO |
| CSO | ISO |
| PBP | CUE + BIN |

WUA, ZAR, XISO는 게임 파일은 남지만 디스크 자체는 남지 않으므로 원래 이미지로 되돌릴 수 없습니다.
입력은 무손실 형식만 받으며 WUA, ZAR, XISO는 받지 않습니다. Wii U 디스크는 옆에 키가 있어야 합니다.

**카트리지 덤프**(`.nes`, `.unh`, `.z64`, `.v64`, `.n64`, `.sfc`, `.smc`, `.swc`)는 압축하지 않습니다.
기종마다 설정한 형식으로 씁니다. NES는 헤더 있음·없음(또는 `.unh`), N64는 세 가지 바이트 순서, SNES는
카피어 헤더 있음·없음입니다. NES 헤더의 유무는 해시로 확인하므로 알려진 게임만 변환합니다.

**3DS 타이틀**(`.cia`, `.3ds`, `.cci`, `.cxi`, `.3dsx`와 압축된 `.zcci`, `.zcia`, `.zcxi`, `.z3dsx`)은
복호화합니다. 게임은 `.cci`/`.3ds`, `.cxi` 또는 압축된 `.zcci`/`.zcxi`가 되고, 업데이트·DLC·체험판은
`.cia`(압축하면 `.zcia`)로 남습니다. chdlite에는 키가 들어 있지 않습니다.

**해시** SHA-1, MD5, CRC32, SHA-256, XXH3와 RetroAchievements 해시를 지원합니다. RetroAchievements
해시는 동봉된 목록으로 그 게임에 도전 과제가 있는지도 표시합니다.

## Read

Read는 디스크 자체의 데이터로 플랫폼을 판별하고, 게임의 시리얼과 타이틀을 읽습니다. 그 밖에 다음을
표시합니다.

| 파일 | 표시 내용 |
|---|---|
| CHD | CHD 버전과 종류(CD, GD-ROM, DVD, 하드 디스크), 플랫폼·시리얼·타이틀, 원래 크기·파일 크기·압축률, 헌크 크기와 개수, 유닛 크기, 코덱, 전체와 데이터의 SHA-1, 부모 CHD의 SHA-1(있는 경우), 트랙별 종류와 길이 |
| RVZ | 게임큐브인지 Wii인지, 게임 ID와 타이틀, 디스크 크기·파일 크기·압축률, 블록 크기 |
| CSO | CSO 버전과 블록, 플랫폼·시리얼·타이틀, ISO 크기·파일 크기·압축률 |
| PBP | 디스크 수, 플랫폼·시리얼·타이틀, 디스크별 트랙과 크기 |
| CUE + BIN, GDI + BIN, ISO | 플랫폼·시리얼·타이틀, 출력 형식 |
| Wii U WUD, WUX | 디스크 크기, 타이틀 ID와 버전 |
| 3DS 타이틀 | 컨테이너(CIA, 카트리지 이미지, NCCH, 3DSX), 압축 여부, 타이틀 종류, 암호화 여부, 출력 형식 |
| 카트리지 덤프 | 기종과 크기, 현재 형식(헤더, 바이트 순서), 출력 형식 |

## 끌어다 놓기

도구마다 역할이 정해져 있어서, 실행 파일에 파일을 끌어다 놓기만 하면 저장된 설정으로 그 작업을 합니다.

| 도구 | 끌어다 놓았을 때 동작 |
|---|---|
| `chdlite` | 빠르게 압축합니다. `.chd`, `.rvz`는 CUE + BIN, GDI + BIN, ISO로 풉니다. |
| `chdcomp` | 가장 작게 압축합니다. `.chd`, `.rvz`는 CUE + BIN, GDI + BIN, ISO로 풉니다. |
| `chdread` | 메타데이터를 읽고 플랫폼을 판별합니다. |
| `chdhash` | `.chd`, `.rvz`, `.cso`, `.pbp`의 내용 또는 파일 자체의 해시를 계산합니다. |

## 명령줄

모든 도구가 모든 명령과 옵션을 쓸 수 있습니다. 앱 안에서는 `chdlite.app/Contents/MacOS/`에 있으며,
AppImage에서는 `chdlite.AppImage chdlite ...`로 실행합니다.

```
chdlite [명령] <파일|폴더>... [옵션]
```

| 명령 | 설명 |
|---|---|
| `auto` | `.chd`, `.rvz`는 풀고 나머지는 빠른 프리셋으로 만듭니다. |
| `create` | 빠른 프리셋으로 만듭니다. |
| `comp` | 가장 작게 만듭니다. |
| `extract` | `.chd`, `.rvz`, `.cso`, `.pbp`를 CUE + BIN, GDI + BIN, ISO로, 압축된 3DS 타이틀을 원래 형식으로 풉니다. |
| `read` | 메타데이터를 읽고 플랫폼을 판별합니다. |
| `hash` | 내용의 해시를 계산합니다. |

| 옵션 | 설명 |
|---|---|
| `-o`, `--output <dir>` | 각 파일 옆이 아니라 여기에 씁니다. |
| `-f`, `--force` | 이미 있는 출력을 덮어씁니다. |
| `-np`, `--threads <n>` | 전체 스레드 수(기본: 물리 코어 수 − 4). |
| `-jf`, `--concurrent-files <n>` | 동시에 변환할 파일 수. |
| `--preset pc\|mobile` | `create`가 쓰는 빠른 프리셋. |
| `--comp best\|chdman\|custom` | `comp`의 동작. |
| `-c`, `--compression <list>` | CHD 코덱(최대 4개). |
| `-hs`, `--hunksize <bytes>` | CHD 헌크 크기. |
| `--rvz-codec`, `--rvz-level`, `--rvz-block`, `--rvz-scrub` | RVZ 설정. `--rvz-scrub`은 불필요한 데이터를 지우며 되돌릴 수 없습니다. |
| `--hash <list>` | `sha1,md5,crc32,sha256,xxh3,rahash`(기본 `sha1,rahash`). |
| `--cue-style <style>` | 푼 cue 시트의 형식: `redump`, `redump_catalog`, `chdman`. |
| `--gdrom-sheet gdi\|cue` | 푼 GD-ROM의 시트. |
| `--nes`, `--n64`, `--snes`, `--3ds <format>` | 카트리지 덤프와 3DS 타이틀의 출력 형식. |
| `-h`, `--help` / `-V`, `--version` | 도움말과 버전. |

기본값은 `chdlite.toml`에서 읽습니다. 프로그램 옆이나 설정 폴더에 둡니다(위치는 `--help`에 표시).

## 포함된 라이브러리

| 라이브러리 | 라이선스 |
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

## 라이선스

romexplorer와 같은 [AGPL-3.0](../LICENSE)입니다.
