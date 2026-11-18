# rommusic

ROM이나 디스크 이미지를 `rommusic`에 끌어다 놓으면 게임 음악을 FLAC으로 저장합니다.
romexplorer 앱이 ROM 음악을 재생할 때와 같은 읽기·재생 구조를 쓰며, 따로 릴리스합니다(버전 0.2.1).

[English](README.md) · [日本語](README.ja.md) · [Français](README.fr.md) · [Español](README.es.md) · [Italiano](README.it.md) · [Português](README.pt.md) · [Deutsch](README.de.md) · **한국어** · [Polski](README.pl.md)

**다운로드:** [Rom Music 릴리스](https://github.com/DesertDoggy/romexplorer/releases?q=m0) · **변경 사항:** [릴리스 노트](../docs/rommusic_release_notes.md) · [romexplorer](../README.ko.md)의 일부

> **rommusic은 ROM, BIOS, 키를 배포하거나 다운로드하지 않으며 불법 복제를 지원하지 않습니다.**
> 직접 소유하고 직접 덤프한 게임으로 사용하세요.

## 창

`rommusic-gui`(macOS에서는 `rommusic.app`, Linux에서는 rommusic AppImage)를 엽니다.

1. 파일이나 폴더를 고르거나 창에 끌어다 놓습니다. ROM, 디스크 이미지, 덤프한 게임 폴더를 쓸 수 있습니다.
2. **FLAC**, **Original audio**(원래 형식), **Both**(둘 다) 중에서 고릅니다.
3. 출력 폴더를 고릅니다. 비워 두면 각 파일 옆에 저장합니다.
4. **Start**를 누릅니다.

징글·효과음·동영상 소리, 곡 수 제한, 샘플링 레이트와 비트 수, 슈퍼 패미컴 실행(게임을 돌리는 시간과
그동안 스타트 버튼을 누를지)은 **Options**에 있습니다. 설정은 다음에도 유지됩니다.

**Android** rommusic 앱(Android 9 이상)도 같은 화면입니다. 처음 실행할 때 ROM을 읽고 음악을
저장하기 위해 모든 파일 접근 권한을 요청합니다. 파일과 폴더는 앱 안의 브라우저에서 고르며, 화면을
꺼도 저장은 계속됩니다.

**macOS·Linux** `rommusic.AppImage --install`로 데스크톱 메뉴에 추가할 수 있습니다. macOS 앱은
공증을 받지 않았으므로 처음에는 오른쪽 클릭 > 열기로 실행합니다.

## 지원 게임

| 기종 | 파일 | 음악 |
|---|---|---|
| CD 게임: 플레이스테이션, 세가 새턴, 메가 CD, PC 엔진 CD, 네오지오 CD, 드림캐스트 등 | CHD, CUE + BIN, GDI + BIN, ISO | CD 오디오 트랙, 디스크 안의 음악 파일(ADX, XA, AT3 등. CPK, AFS 아카이브 안도 가능) |
| 플레이스테이션 2, PSP, 게임큐브, Wii, Xbox, Xbox 360 등 | CHD, ISO, RVZ, WIA, GCZ, WBFS, CSO, ZSO, XISO, 덤프한 게임 폴더 | 디스크 안의 음악 파일(ADX, AT3, ATRAC3, WMA, XMA, BRSTM, HCA 등. CPK, AFS, ARC, SZS 아카이브 안도 가능) |
| Wii U | WUA. WUD, WUX, 타이틀 폴더는 옆에 키가 있으면 가능 | 디스크 안의 음악 파일(BFSTM 등) |
| Nintendo DS | NDS | SDAT 사운드 아카이브(STRM 스트림, SSEQ 시퀀스) |
| Nintendo 3DS | 3DS, CCI(복호화된 것만) | 사운드 아카이브 안의 음악 파일(BCSTM 등) |
| 패미컴(NES), 디스크 시스템, 게임보이, 게임보이 컬러, 게임보이 어드밴스, 버추얼 보이, SG-1000, 마스터 시스템, 게임 기어, 메가 드라이브, PC 엔진, 슈퍼그래픽스, MSX, 원더스완, 원더스완 컬러, 네오지오 포켓, 네오지오 포켓 컬러 | ROM(zip, 7z 안도 가능) | BGM 바이너리 코드 |
| 슈퍼 패미컴(SNES) | ROM(zip, 7z 안도 가능) | BGM 바이너리 코드 |
| Nintendo 64 | ROM(zip, 7z 안도 가능) | BGM 바이너리 코드 |
| PC-98 | 플로피 이미지(FDI, HDM, FDD 등. zip, 7z 안도 가능) | 플로피 안의 PMD, FMP 음악 파일 |

지원 기종이라도 기술적인 이유로 지원하지 않는 게임이 있습니다. zip, 7z 안의 디스크 이미지, 암호화된
이미지, PS3, Vita, 다운로드판 패키지는 읽지 못합니다.

## 출력

각 파일 옆(또는 출력 폴더)에 `<이름> (music)` 폴더를 만듭니다. 이전에 저장한 파일은 덮어씁니다.

| 출력 | 내용 |
|---|---|
| **FLAC** (기본값) | `01 - <곡 이름>.flac` 등: 배경 음악입니다. 반복되는 곡은 반복 구간을 두 번 재생하고 페이드아웃합니다. `index.txt`에 곡 목록을 씁니다. |
| **Original audio** | 게임에 저장된 형식 그대로의 음악 파일을 `original` 폴더에 저장합니다. CD 오디오 트랙과 카트리지 음악에는 없으므로 FLAC을 쓰세요. |
| **Both** | FLAC과 원래 형식 파일 모두. |

원래 형식 파일에는 게임의 프로그램과 데이터가 들어 있습니다. ROM과 함께 보관하고 다른 사람에게
주지 마세요.

## 끌어다 놓기

파일이나 폴더를 `rommusic.exe`(Linux, macOS에서는 `rommusic`)에 끌어다 놓습니다. 기본 설정으로
FLAC을 저장하며, 창에서 아무것도 바꾸지 않고 **Start**를 누른 것과 같습니다.

## 명령줄

명령줄 도구는 `rommusic`입니다(앱 안에서는 `rommusic.app/Contents/MacOS/rommusic` 또는
`rommusic.AppImage rommusic ...`). 창의 모든 설정을 옵션으로 지정할 수 있습니다.

```
rommusic [옵션] <ROM, 디스크 이미지 또는 폴더>...
```

예:

```
rommusic "Hoge Quest (Japan).chd"
rommusic --all --max 20 "Hoge Quest (Japan).chd"
rommusic --out D:\music "Hoge Quest (Japan).nes"
rommusic --hires "Hoge Quest (Japan).cue"
rommusic --format both "Hoge Quest (Japan).sfc"
```

| 옵션 | 설명 |
|---|---|
| `--all` | 징글, 효과음, 동영상 소리도 저장합니다. 기본값은 배경 음악만입니다. |
| `--max N` | 파일 하나당 최대 N곡. |
| `--out DIR` | `<이름> (music)` 폴더를 각 파일 옆이 아니라 `DIR`에 만듭니다. |
| `--rate native` | 기본값: 디코딩한 레이트 그대로. |
| `--cd` / `--rate cd` | 44.1 kHz, 16비트. |
| `--dvd` / `--rate dvd` | 48 kHz, 24비트. |
| `--hires` / `--rate hires` | 96 kHz, 24비트. |
| `--rate <Hz>` | 8000~192000의 임의 레이트. |
| `--bits 16\|24` | 샘플당 비트 수(기본 16, `--dvd`와 `--hires`에서는 24). |
| `--format flac` | 기본값: FLAC만. |
| `--format original` | 게임에 저장된 형식 그대로 `original` 폴더에 저장하고 FLAC은 만들지 않습니다. |
| `--format both` | FLAC과 원래 형식 파일 모두. |
| `--seconds N` | SNES: 음악을 가져오기 전에 게임을 돌리는 초 수(기본 20). |
| `--start` | SNES: 돌리는 동안 가끔 스타트 버튼을 누릅니다. 타이틀 화면 뒤에 음악이 시작되는 게임용입니다. |
| `--list` | 곡 목록만 보여 주고 아무것도 저장하지 않습니다. 반복되는 곡은 한 번 분량과 "loop"를 표시하며, 파일에서는 반복 구간을 두 번 재생하고 페이드아웃합니다. |
| `--verbose` | 파일이나 곡을 건너뛴 이유를 표시합니다. |
| `--no-pause` | 마지막에 Enter를 기다리지 않습니다. Enter를 기다리는 것은 끌어다 놓았을 때 열리는 창뿐입니다. |
| `-h`, `--help` / `-V`, `--version` | 도움말과 버전. |

`native`가 아닌 레이트는 리샘플링합니다. 리샘플링으로 음질이 좋아지지는 않습니다. 모든 파일을 같은
레이트로 맞추고 싶거나 37.8 kHz 같은 드문 레이트를 잘 처리하지 못하는 플레이어용입니다. 리샘플링한
파일은 주석 태그에 그렇게 기록합니다.

## 포함된 라이브러리

| 라이브러리 | 라이선스 |
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
| [98fmplayer](https://github.com/myon98/98fmplayer) | [BSD-2-Clause](https://github.com/myon98/98fmplayer) |
| [vgmstream](https://github.com/vgmstream/vgmstream) | [ISC](https://github.com/vgmstream/vgmstream/blob/master/COPYING) |
| [FFmpeg](https://github.com/FFmpeg/FFmpeg) | [LGPL-2.1+](https://github.com/FFmpeg/FFmpeg/blob/master/LICENSE.md) |
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

## 라이선스

romexplorer와 같은 [AGPL-3.0](../LICENSE)입니다.
