# Version sets and minor versions (Titles › "one of each version group", "hide minor versions")

The source for `app_root_ex/assets/version_groups.json`, which holds IGDB ids and dump hashes
only. This page names the games. Code: `src/database/versions.rs`.

## Rules

- **Version set**: two or more equal versions of one game sold side by side, differing in which
  monsters, units or route you get. Third versions are members of the set (Yellow, Crystal,
  Emerald, Platinum, Sky, 真打, スキヤキ ...), not editions of it. Remakes form sets of their own.
- "One of each version group" lists one version of each set: one held here, else the user's pick
  ("show this one"), else a stable random one; 🎲 picks another.
- Versions of one set never share a Definitive family, whatever IGDB links (IGDB links each
  third version to one member: Yellow→Red, Crystal→Silver, ...). Remakes stay with their own
  original (FireRed with Red, LeafGreen with Blue, Omega Ruby with Ruby).
- A version IGDB has no entry of its own for is listed by its No-Intro dump (`sha1:`). It counts
  once the title holding it is that version alone (split off by the user; a split is sticky:
  `title_merge_blocks`). Several ids can be one version (`[igdb id, "sha1:…"]`) when IGDB and the
  DAT have it as two titles here.
- **Minor version** ("hide minor versions"): hidden where a fuller edition covers every platform
  and region it has (International, Director's Cut, Final Mix, Maniax, Royal, Golden, FES,
  Vengeance, 二周目はじめました, Enhanced / Complete / Definitive / GOTY Edition, Final Cut, Redux).
  Reduced versions (Easy Type, Lite, Kids, demo, trial, 体験版) lose to the regular game. From IGDB
  editions, from names within one family, from `supersedes` below, and the user's corrections.
  Never between two versions of one set. Not on the Definitive tab, whose families already hold
  every version.

## Sets (IGDB ids, 2026-11-03)

### Pokémon
- Red / Blue / Yellow + JP 青: 1561, 1511, 1512, sha1 0da501e3… (JP Ao; IGDB's Blue 1511 is JP Green)
- Gold / Silver / Crystal: 1558, 1513, 1514
- Ruby / Sapphire / Emerald: 1557, 1515, 1517
- Diamond / Pearl / Platinum: 1560, 1518, 1519
- FireRed / LeafGreen: 1559, 1516
- HeartGold / SoulSilver: 1556, 1520
- Black / White: 1521, 1522
- Black 2 / White 2: 8284, 8353
- X / Y: 2286, 2287
- Omega Ruby / Alpha Sapphire: 6897, 6898
- Sun / Moon / Ultra Sun / Ultra Moon (one set of four, the user's choice): 18100, 18099, 36792, 36793
- Let's Go Pikachu / Eevee: 25877, 102873
- Sword / Shield: 37382, 115653
- Brilliant Diamond / Shining Pearl: 144051, 144052
- Scarlet / Violet: 191931, 191930
- Mystery Dungeon Rescue Team Red / Blue: 2319, 2320
- Mystery Dungeon Explorers of Time / Darkness / Sky: 2321, 2322, 2323
- Fushigi no Dungeon WiiWare 炎 / 嵐 / 光の冒険団: 103511, 103512, 103513

### Dragon Quest Monsters
- DQM2 ルカの旅立ち / イルの冒険 (GB): 24819, 24820 (24820 known only as a parent id)
- DQM2 (PS1): 222275, 222276

### Devil Children (checked against the user's No-Intro DATs)
- GB 赤の書 / 黒の書 / 白の書: [71306 + sha1 b15cbd01…], [71694 + sha1 b6177175…], sha1 0ad803e3… (白 is not in IGDB; the DAT titles have no IGDB link)
- GBA 光の書 / 闇の書 (DemiKids Light / Dark): 49185, 19884. The user's GBA DAT has a second 闇の書 (Japan) release (and its beta) inside the 光 title.
- GBA 炎の書 / 氷の書: [65864 + sha1 8c29d692…], [92475 + sha1 8470a93b…]. In the user's catalogue both are one title (IGDB 250196) until one is split off.

### Yo-kai Watch
- 2 元祖 / 本家 / 真打: 11658, 24470, 27129
- 3 スシ / テンプラ / スキヤキ: 136889, 222295, 113248
- Busters 赤猫団 / 白犬隊 / 月兎組: 11659, 54561, 54562 (IGDB types 月兎組 as an expansion)
- Busters 2 ソード / マグナム: 54564, 136935

### Medarot / Medabots (カブト / クワガタ)
- 1: 55145, 55144 · Perfect Edition: 37571, 37570 · 2: 55139, 76574 · 3: 55140, 76575 · 4: 91532, 204235
- Cardrobottle: 66551, 228618 · Navi: 49697, 49696 · G: 188653, 79757
- Medabots Metabee / Rokusho (GBA): 7583, 49235 · AX: 49415, 49234 · Shingata: 49823, 49822
- DS: 67687, 204249 (duplicate 229104 left out) · Dual: 61977, 203434 · 7: 138121, 83882 · 8: 138117, 78206 · 9: 138065, 136909 · Girls Mission: 136901, 222459
- 5 and 6 are not in IGDB.

### Fire Emblem Fates
- 白夜王国 / 暗夜王国: 24220, 21863. 透魔王国 (18038) is DLC and stays out.
- `supersedes`: the Special Edition (10229, all three routes) is the fuller version of both on 3DS.

### Mega Man
- Battle Network 3 White / Blue: 1757, 1758 (JP Black has no entry)
- Battle Network 4 Red Sun / Blue Moon: 1759, 1760
- Battle Network 5 Team Protoman / Team Colonel: 1761, 1762
- Rockman EXE 6 グレイガ / ファルザー (JP): 352770, 352771; Battle Network 6 Gregar / Falzar: 1765, 1764
- Star Force Pegasus / Leo / Dragon: 1781, 1782, 1783
- Star Force 2 Zerker x Ninja / Zerker x Saurian: 1785, 1784
- Star Force 3 Black Ace / Red Joker: 1786, 1787

### Inazuma Eleven
- 2 Blizzard / Firestorm: 47716, 47835 · 3 Bomb Blast / Lightning Bolt / Team Ogre: 47676, 47675, 47674
- GO Light / Shadow: 47673, 9685 · Chrono Stones Thunderflash / Wildfire: 23220, 9693 · Galaxy Big Bang / Supernova: 62739, 134112 (duplicate 136053 left out)

### Others
- Digimon World DS Dawn / Dusk: 21521, 78285 · Digimon Story Super Xros Wars Blue / Red: 112400, 112401 · Digimon Anode / Cathode Tamer: 37517, 37516
- Telefang Power / Speed: 27493, 27492 · Telefang 2 Power / Speed: 27494, 27495
- Robopon 2 Cross / Ring: 6561, 6560 (Robopon 1 Star / Moon are not in IGDB)
- Yu-Gi-Oh! Duel Monsters 4 Yugi / Jonouchi: 50002, 334709 (Kaiba deck not in IGDB)
- Bomberman Max: 49839, 49838 · Bomberman Max 2: 6331, 6333 · Beyblade 2002: 49489, 49490 · Ou Dorobou Jing Devil / Angel: 146341, 228630 · Card Fighters' Clash Capcom / SNK: 28398, 28399 · Dive Alert: 69072, 69075 · Shaman King Legacy of the Spirits: 6588, 6589
