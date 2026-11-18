# chdlite

Trascina un'immagine disco (CUE + BIN, GDI + BIN, ISO) su chdlite e diventa un CHD o un RVZ;
trascina un `.chd` o `.rvz` e riottieni il CUE + BIN, GDI + BIN o ISO. Legge anche i metadati e
rileva la piattaforma, calcola l'hash del contenuto di CHD, RVZ, CSO e PBP, mette i dump NES, N64 e
SNES nel formato scelto e decifra i titoli 3DS. Non servono database né configurazione. Usa il core
di romexplorer ed è rilasciato separatamente (versione 0.3.4).

[English](README.md) · [日本語](README.ja.md) · [Français](README.fr.md) · [Español](README.es.md) · **Italiano** · [Português](README.pt.md) · [Deutsch](README.de.md) · [한국어](README.ko.md) · [Polski](README.pl.md)

**Download:** [release di CHD Lite](https://github.com/DesertDoggy/romexplorer/releases?q=c0) · **Modifiche:** [note di rilascio](../docs/chdlite_release_notes.md) · Parte di [romexplorer](../README.it.md)

> **chdlite non distribuisce né scarica ROM, BIOS o chiavi, e non sostiene la pirateria.** Usalo
> con giochi che possiedi e che hai estratto tu stesso.

## Finestra

Apri `chdlite-gui` (su macOS `chdlite.app`, su Linux l'AppImage di chdlite). Ha quattro modalità:

| Modalità | Descrizione |
|---|---|
| **Lite** | Comprime velocemente. Un `.chd` o `.rvz` viene estratto in CUE + BIN, GDI + BIN o ISO. |
| **Comp** | Comprime lentamente, per il file più piccolo. Un `.chd` o `.rvz` viene estratto allo stesso modo. |
| **Read** | Legge i metadati e rileva la piattaforma. |
| **Hash** | Calcola l'hash del contenuto di un file. |

Scegli una modalità, poi trascina file o cartelle sulla finestra (o sceglili). **Settings** contiene
il resto: thread e file contemporanei, cartella di destinazione e sostituzione degli output
esistenti, preset Lite e codec Comp, codec, livello e dimensione blocco RVZ, hash, stile cue e foglio
GD-ROM, formato dei dump di cartuccia e dei titoli 3DS, e conferma prima di un output con perdita
(WUA, ZAR, XISO). La finestra li ricorda, e la riga di comando usa le stesse impostazioni.

**Android.** L'app chdlite è la stessa finestra. File e cartelle si scelgono nel suo browser.

**macOS e Linux.** L'app Mac non è notarizzata: al primo avvio fai clic destro > Apri.

## Conversione

Il formato di output viene scelto in base all'immagine.

**Compressione**

| Input | Output |
|---|---|
| CD: CUE + BIN, TOC + BIN, o una traccia BIN/IMG singola | CHD CD |
| GD-ROM (Dreamcast): GDI + BIN/RAW | CHD GD-ROM |
| DVD (PS2, PSP e altri): ISO | CHD DVD |
| PSP: CSO | CHD DVD |
| PS1: PBP | CHD CD |
| GameCube, Wii: ISO, GCM | RVZ |
| Wii U: WUD, WUX | WUA |
| Xbox 360: ISO | ZAR |
| Xbox: ISO | XISO |

**Estrazione**

| Input | Output |
|---|---|
| CHD CD | CUE + BIN (un BIN per traccia) |
| CHD GD-ROM | GDI + BIN, o CUE + BIN con `--gdrom-sheet cue` |
| CHD DVD | ISO |
| RVZ | ISO |
| CSO | ISO |
| PBP | CUE + BIN |

WUA, ZAR e XISO mantengono i file del gioco ma non il disco stesso, quindi non possono tornare
all'immagine originale. Si accettano solo input senza perdita: non WUA, ZAR o XISO. Un disco Wii U
richiede la sua chiave accanto.

**Dump di cartuccia** (`.nes`, `.unh`, `.z64`, `.v64`, `.n64`, `.sfc`, `.smc`, `.swc`): non vengono
compressi. Vengono scritti nel formato impostato per la piattaforma: NES con o senza header (o come
`.unh`), N64 in uno dei tre ordini di byte, SNES con o senza header del copier. Se un dump NES ha
bisogno dell'header si verifica tramite hash, quindi vengono convertiti solo giochi noti.

**Titoli 3DS** (`.cia`, `.3ds`, `.cci`, `.cxi`, `.3dsx` e i compressi `.zcci`, `.zcia`, `.zcxi`,
`.z3dsx`): vengono decifrati. Un gioco diventa `.cci`/`.3ds`, `.cxi`, o compresso `.zcci`/`.zcxi`;
aggiornamenti, DLC e demo restano `.cia` (`.zcia` compresso). chdlite non include chiavi.

**Hash.** SHA-1, MD5, CRC32, SHA-256, XXH3 e l'hash di RetroAchievements, che indica anche se
RetroAchievements ha obiettivi per il gioco (dalla lista inclusa).

## Read

Read rileva la piattaforma dai dati del disco e ne legge il seriale e il titolo del gioco. Mostra
inoltre:

| File | Mostrato |
|---|---|
| CHD | Versione e tipo di CHD (CD, GD-ROM, DVD, disco rigido); piattaforma, seriale e titolo; dimensione completa, dimensione del file e rapporto di compressione; dimensione e numero di hunk; dimensione unità; codec; SHA-1 totale e dei dati; SHA-1 del genitore (se presente); tipo e durata di ogni traccia |
| RVZ | GameCube o Wii; ID e titolo del gioco; dimensione del disco, dimensione del file e rapporto; dimensione blocco |
| CSO | Versione e blocchi del CSO; piattaforma, seriale e titolo; dimensione ISO, dimensione del file e rapporto |
| PBP | Numero di dischi; piattaforma, seriale e titolo; tracce e dimensione di ogni disco |
| CUE + BIN, GDI + BIN, ISO | Piattaforma, seriale e titolo; formato di output |
| Wii U WUD, WUX | Dimensione del disco; ID e versione del titolo |
| Titoli 3DS | Contenitore (CIA, immagine cartuccia, NCCH, 3DSX); compresso o no; tipo di titolo; cifrato o no; formato di output |
| Dump di cartuccia | Piattaforma e dimensione; formato attuale (header, ordine dei byte); formato di output |

## Trascina e rilascia

Ogni strumento ha un solo compito: trascinare file sul suo eseguibile lo esegue, con le
impostazioni salvate.

| Strumento | Azione al rilascio |
|---|---|
| `chdlite` | Comprime velocemente. Un `.chd` o `.rvz` viene estratto in CUE + BIN, GDI + BIN o ISO. |
| `chdcomp` | Comprime per il file più piccolo. Un `.chd` o `.rvz` viene estratto in CUE + BIN, GDI + BIN o ISO. |
| `chdread` | Legge i metadati e rileva la piattaforma. |
| `chdhash` | Calcola l'hash del contenuto di un `.chd`, `.rvz`, `.cso` o `.pbp`, o del file stesso. |

## Riga di comando

Ogni strumento accetta tutti i comandi e le opzioni. Nelle app gli strumenti sono in
`chdlite.app/Contents/MacOS/`, o si avviano con `chdlite.AppImage chdlite ...`.

```
chdlite [comando] <file|cartella>... [opzioni]
```

| Comando | Descrizione |
|---|---|
| `auto` | I `.chd`/`.rvz` vengono estratti, il resto viene creato con il preset veloce. |
| `create` | Crea con il preset veloce. |
| `comp` | Crea il file più piccolo. |
| `extract` | Estrae `.chd`, `.rvz`, `.cso` e `.pbp` in CUE + BIN, GDI + BIN o ISO, e i titoli 3DS compressi. |
| `read` | Legge i metadati e rileva la piattaforma. |
| `hash` | Calcola l'hash del contenuto. |

| Opzione | Descrizione |
|---|---|
| `-o`, `--output <dir>` | Scrive qui invece che accanto a ogni file. |
| `-f`, `--force` | Sostituisce gli output già esistenti. |
| `-np`, `--threads <n>` | Thread in totale (predefinito: core fisici meno 4). |
| `-jf`, `--concurrent-files <n>` | File convertiti contemporaneamente. |
| `--preset pc\|mobile` | Preset veloce usato da `create`. |
| `--comp best\|chdman\|custom` | Modalità di `comp`. |
| `-c`, `--compression <list>` | Codec CHD, fino a 4. |
| `-hs`, `--hunksize <bytes>` | Dimensione hunk CHD. |
| `--rvz-codec`, `--rvz-level`, `--rvz-block`, `--rvz-scrub` | Impostazioni RVZ. `--rvz-scrub` elimina i dati inutili e non si può annullare. |
| `--hash <list>` | `sha1,md5,crc32,sha256,xxh3,rahash` (predefinito `sha1,rahash`). |
| `--cue-style <style>` | Stile del foglio cue estratto: `redump`, `redump_catalog` o `chdman`. |
| `--gdrom-sheet gdi\|cue` | Foglio di un GD-ROM estratto. |
| `--nes`, `--n64`, `--snes`, `--3ds <format>` | Formato in cui vengono scritti i dump di cartuccia e i titoli 3DS. |
| `-h`, `--help` / `-V`, `--version` | Aiuto e versione. |

I valori predefiniti vengono da `chdlite.toml`, accanto al programma o nella cartella delle
impostazioni (`--help` indica dove).

## Librerie incluse

| Libreria | Licenza |
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

## Licenza

[AGPL-3.0](../LICENSE), come romexplorer.
