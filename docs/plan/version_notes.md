# Version notes (Collection › Definitive)

The source for `app_root_ex/assets/definitive_notes.json`: games whose latest version may have
less content than an earlier one. Each entry's text goes into the file once its IGDB id (the
base game's) is filled in and the text is approved. This page names real games because the
notes are IGDB data; the code and the JSON file stay free of names (the file holds ids and
text only).

Add ideas and plans per series under its heading as they come.

## Rules

- **S**: strongly recommend the older version.
- **A**: which one is better depends on preference.
- **B**: names an older version you can play instead and what it has that the latest lacks.
  The latest is still the better pick.
- **None**: no note; the game is not in the file.
- Pixel art over 3D always counts as a reason, but on its own it gives B, and never more than A.
- It is not only nostalgia: when the newer version is an early-3D remake (roughly the PS1 / PS2
  era, low-poly or low-resolution 3D), its graphics were limited by the hardware of its day and
  can look lower in quality today than refined 2D pixel art. Then 2D against early 3D can be A
  (preference), not just B.
- Japanese wording: objective terms for graphics -- 高画質, 高解像度, HD 化 -- never きれい / 綺麗,
  which reads as personal taste.
- **Approved** marks a note the user approved. All S / A / B notes were approved on 2026-11-03 and are in the JSON file, except Dragon Quest X (Latest unsure).
- **Latest** is what the Definitive tab picks: the newest official version.
- `recommend` is the line in bold on the family page (which version to play instead);
  `note` is why. Both have English and Japanese text.

## How the ids were found

From the user's IGDB index (`igdb_games`, `igdb_game_relations`), 2026-11-03. A relation row
`(game_id, related_id, parent)` means `game_id`'s parent is `related_id`; the base game also
lists its own versions as `(base, version, port|remake|remaster|bundle)`. "IGDB id" is the
family's root: the game at the top of the parent chain. Fan hacks (`mod`), DLC, editions and
store bundles of extras are left out of the version lists. Platforms are as IGDB lists them;
"mobile" is IGDB's legacy mobile-phone platform (Japanese feature phones).

## Final Fantasy

### Final Fantasy
- IGDB id: 385
- Versions: Famicom/NES (1987, 385; also Wii, Wii U, 3DS Virtual Console); MSX2 (1989, 159296); WonderSwan Color (2000, 121688); PS1 Origins (2002, 380632); GBA Dawn of Souls (2004, 271227); 20th Anniversary Edition (PSP 2007, later iOS, Android, 3DS, Windows Phone; 397); Pixel Remaster (PC, Android, iOS, PS4, Switch, Xbox Series; 2021, 158980)
- Rank: S
- Latest: Pixel Remaster (2021)
- Recommend (en): PSP Anniversary Edition.
- Recommend (ja): PSP 版がおすすめです。
- Note (en): The Pixel Remaster has none of their bonus dungeons: Soul of Chaos (GBA, PSP) and Labyrinth of Time (PSP). The PSP version also has redrawn 2D art.
- Note (ja): ピクセルリマスターには、GBA 版・PSP 版の追加ダンジョン「ソウル・オブ・カオス」と、PSP 版の「時の迷宮」がありません。PSP 版はグラフィックも描き直されています。
- The older smartphone version (2010, from the PSP one) has both dungeons too, but was delisted when the Pixel Remaster came out (unsure: whether the FF I/II smartphone versions were delisted, as the V/VI ones were).

### Final Fantasy II
- IGDB id: 16474
- Versions: Famicom (1988, 16474; also Wii, Wii U, 3DS Virtual Console); WonderSwan Color (2001, 145817); PS1 Origins (2002, 380633); GBA Dawn of Souls (2004, 271819); 20th Anniversary Edition (PSP, 2007, 121717); Pixel Remaster (PC, Android, iOS, PS4, Switch, Xbox Series; 2021, 158981)
- Rank: S
- Latest: Pixel Remaster (2021)
- Recommend (en): The PSP Anniversary Edition or the GBA version.
- Recommend (ja): PSP 版か GBA 版がおすすめです。
- Note (en): The Pixel Remaster lacks the Soul of Rebirth bonus scenario (GBA, PSP) and the Arcane Labyrinth (PSP).
- Note (ja): ピクセルリマスターには、GBA 版・PSP 版の追加シナリオ「ソウル・オブ・リバース」と、PSP 版の「秘紋の迷宮」がありません。

### Final Fantasy III
- IGDB id: 77234
- Versions: Famicom (1990, 77234; also Wii, Wii U, 3DS Virtual Console); DS 3D remake (2006, 393); smartphone port of the 3D remake (Android, iOS, Ouya, Windows Phone; 2011, 340017); Pixel Remaster (PC, Android, iOS, PS4, Switch, Xbox Series; 2021, 158982). The 3D remake's PSP (2012) and Steam (2014) releases have no entry of their own in the index.
- Rank: A
- Latest: Pixel Remaster (2021)
- Recommend (en): The DS 3D remake (also on PSP, mobile and Steam) is worth considering.
- Recommend (ja): DS 版の 3D リメイク（PSP 版・スマホ版・Steam 版もあります）も選択肢です。
- Note (en): The 3D remake gives the party named characters with their own story scenes, rebalances the jobs and adds bonus content. The Pixel Remaster keeps the Famicom original's pixel look while adding QoL features, so it comes down to preference.
- Note (ja): 3D リメイクでは仲間に名前と個別のストーリーがあり、ジョブのバランスも調整され、追加要素もあります。ピクセルリマスターはファミコン版のドット絵の雰囲気のまま、遊びやすくなっています。どちらを選ぶかは好みです。
- Careful: IGDB id 426, also titled "Final Fantasy III", is the US SNES release of VI (below).

### Final Fantasy IV
- IGDB id: 16587
- Versions: Super Famicom (1991, 16587; also Wii, Wii U, New 3DS Virtual Console); Easy Type (Super Famicom, 1991, 205703); US SNES "Final Fantasy II" (1991, 387); PS1 (1997, 282725; in Final Fantasy Collection, Anthology, Chronicles); WonderSwan Color (2002, 283313); GBA Final Fantasy IV Advance (2005, 406); DS 3D remake (2007, 399); 3D remake on PC, Android, iOS (2012, 380634); PSP (2011, 283093, in The Complete Collection 21779); Pixel Remaster (PC, Android, iOS, PS4, Switch, Xbox Series; 2021, 158983)
- Rank: S
- Latest: Pixel Remaster (2021)
- Recommend (en): Play the PSP Complete Collection or the GBA version.
- Recommend (ja): PSP 版（コンプリートコレクション）か GBA 版がおすすめです。
- Note (en): The Pixel Remaster lacks the Lunar Ruins bonus dungeon and the late-game party changes (GBA, PSP). The PSP version has updated pixel graphics and includes the sequel The After Years and the Interlude chapter.
- Note (ja): ピクセルリマスターには、GBA 版・PSP 版の追加ダンジョン「月の遺跡」と終盤のパーティー入れ替えがありません。PSP 版は高解像度で描き直したドット絵で、続編の「ジ・アフター -月の帰還-」と「インタールード」も入っています。

### Final Fantasy V
- IGDB id: 419
- Versions: Super Famicom (1992, 419; also Wii Virtual Console); PS1 (1998, 288349; in Anthology, Final Fantasy Collection); GBA Final Fantasy V Advance (2006, 20597); smartphone/PC version based on the GBA one (PC, Android, iOS; 2013, 146069); Pixel Remaster (PC, Android, iOS, PS4, Switch, Xbox Series; 2021, 158984)
- Rank: S
- Latest: Pixel Remaster (2021)
- Recommend (en): Play the GBA version, or the older mobile / Steam version if you already own it.
- Recommend (ja): GBA 版がおすすめです。以前のスマホ版・Steam 版を持っていれば、そちらでもかまいません。
- Note (en): The Pixel Remaster lacks four jobs (Gladiator, Cannoneer, Oracle, Necromancer) and the Sealed Temple and Cloister of the Dead bonus dungeons. The older mobile / Steam version has them too, but is no longer sold.
- Note (ja): ピクセルリマスターには、GBA 版の追加ジョブ 4 種（剣闘士・砲撃士・予言士・ネクロマンサー）と追加ダンジョン 2 つがありません。以前のスマホ版・Steam 版にもありますが、販売は終了しています。

### Final Fantasy VI
- IGDB id: 426 (titled "Final Fantasy III" in IGDB, after the US SNES release)
- Versions: Super Famicom / SNES (1994, 426; also Wii, Wii U, New 3DS Virtual Console); PS1 (1999, 288348; in Anthology, Final Fantasy Collection); GBA Final Fantasy VI Advance (2006, 20655); smartphone/PC version based on the GBA one (PC, Android, iOS; 2014, 146218); Pixel Remaster (PC, Android, iOS, PS4, Switch, Xbox Series; 2022, 158985)
- Rank: S
- Latest: Pixel Remaster (2022)
- Recommend (en): Play the GBA version, or the older mobile / Steam version if you already own it.
- Recommend (ja): GBA 版がおすすめです。以前のスマホ版・Steam 版を持っていれば、そちらでもかまいません。
- Note (en): The Pixel Remaster lacks the Dragon's Den and Soul Shrine bonus dungeons and four espers (Leviathan, Gilgamesh, Cactuar, Diabolos). The older mobile / Steam version has them too, but is no longer sold.
- Note (ja): ピクセルリマスターには、追加ダンジョン「竜の巣」「魂の祠」と、追加の幻獣 4 体（リヴァイアサン・ギルガメッシュ・サボテンダー・ディアボロス）がありません。以前のスマホ版・Steam 版にもありますが、販売は終了しています。
- Fixed: Latest was given as 2021; IGDB has the VI Pixel Remaster as 2022.

### Final Fantasy VII
- IGDB id: 393025 (the Japanese PS1 original). IGDB types the usual entry, 427 (PS1, PS3, PSP), as a remake of it, so 393025 is the family root. The JSON file has the note under both ids, so it shows whichever the family page's base game is.
- Versions: PS1 Japan (1997, 393025); PS1 / PS3 / PSP (1997, 427); International (PS1, PS3, PSP; 1997, 146620); PC (1998, 207021); PC re-release (2012, 392808); Remake (PC, PS4, PS5, Xbox Series, Switch 2; 2020, 11169; Intergrade 2021, 144024); Rebirth (PC, PS5, Xbox Series, Switch 2; 2024, 133236); Revelation (PC, PS5, Xbox Series, Switch 2; 2027, 404721)
- Rank: B
- **Approved** (2026-11-03), in definitive_notes.json.
- Latest: Rebirth (2024), part 2 of the 3-part remake. Part 3, Revelation, is due in spring 2027.
- The older version named is the remaster of the 1997 game (PC, PS4, Switch, Xbox One, smartphones), not the Remake trilogy. It is based on the International version; in Japan that content was only in the International edition.
- Recommend (en): The remaster of the original (PC, PS4, Switch, Xbox One, smartphones) is also an option.
- Recommend (ja): オリジナル版のリマスター（PC・PS4・Switch・Xbox One・スマートフォン）も選択肢です。
- Note (en): The remaster is based on the International version, so it has the content added there. It tells the whole story in one game; the remake splits it into three, and the third is not out yet. It also has turn-based command battles instead of the remake's action combat. The remake has far higher-quality graphics and retells some events.
- Note (ja): このリマスターはインターナショナル版のリマスターでもあります。日本ではインターナショナル版でしか遊べなかった追加要素も入っています。1 本で物語を最後まで遊べます。リメイクは 3 部作に分かれていて、第 3 部はまだ発売されていません。戦闘はリメイクのアクションではなく、コマンド式です。グラフィックはリメイクのほうがはるかに高画質で、一部の展開も変わっています。
- Once Revelation is out (and becomes Latest), "the third is not out yet" goes; the rest still holds.

### Final Fantasy VIII
- IGDB id: 425
- Versions: PS1 (1999, 425; also PS3, PSP); PC (2000, 203524); PC re-release (2013, 203538); Remastered (PC, PS4, Xbox One, Switch; 2019, 119374); Remastered on Android, iOS (2021, 340338)
- Rank: B
- Latest: Remastered (2019; mobile 2021)
- Recommend (en): The original PC version (2000) is also an option.
- Recommend (ja): 2000 年の PC 版も選択肢です。
- Note (en): It includes Chocobo World, the PocketStation minigame, which the Remastered leaves out. The Remastered gives its items to Rinoa's ability instead and has sharper characters, speed-up and battle toggles.
- Note (ja): ポケットステーション用のミニゲーム「チョコボワールド」が入っています。リマスター版にはありませんが、その報酬アイテムはリノアのアビリティで手に入ります。リマスター版はキャラクターが高精細になり、倍速などの機能も付いています。
- The 2013 Steam release is not named: whether it still has Chocobo World is unverified.

### Final Fantasy IX
- IGDB id: 421
- Versions: PS1 (2000, 421; also PS3, PSP); Android, iOS (2016, 88970); PC (2016, 340349); PS4, Xbox One, Switch (2017, 205673)
- Rank: None. The later ports keep all the content and add speed-up and other boosts.

### Final Fantasy X
- IGDB id: 418
- Versions: PS2 (2001, 418); International (PS2, 2002, 146774); HD Remaster (PC, PS3, Vita, PS4, Xbox One, Switch; 2013, 21899; in X/X-2 HD Remaster 14144)
- Rank: None. The HD Remaster is the International version, with all its additions.

### Final Fantasy X-2
- IGDB id: 413
- Versions: PS2 (2003, 413); International (PS2, 2004, 247548); International + Last Mission (PS2, 2004, 146852); HD Remaster (PC, PS3, Vita, PS4, Xbox One, Switch; 2013, 21880)
- Rank: None. The HD Remaster is the International version with Last Mission.
- In the index the HD Remaster hangs off the International + Last Mission bundle, not off 413, so it may not show up in this family.

### Final Fantasy XI
- IGDB id: 411
- Versions: PC, PS2, Xbox 360 (2002, 411) with its expansions and Ultimate Collection bundles; Final Fantasy XI R (PC, mobile; no date, 101214)
- Rank: None. An online game; the PS2 and Xbox 360 servers are closed, and the PC version is still running.

### Final Fantasy XII
- IGDB id: 398
- Versions: PS2 (2006, 398); International Zodiac Job System (PS2, 2007, Japan only, 129890); The Zodiac Age (PC, PS4, Xbox One, Switch; 2017, 19455)
- Rank: B
- Latest: The Zodiac Age (2017)
- Recommend (en): The original PS2 release (2006) is also an option.
- Recommend (ja): PS2 のオリジナル版（2006 年）も選択肢です。
- Note (en): The original has one shared License Board where every character can learn everything. The Zodiac Age replaces it with 12 job boards but adds Trial Mode, speed-up and better balance.
- Note (ja): オリジナル版はライセンスボードが 1 つで、誰でもすべてを覚えられます。ザ ゾディアック エイジでは 12 種のジョブ別ボードに変わりましたが、トライアルモードやハイスピードモードが加わり、バランスも良くなっています。
- The job boards first came with the Japan-only International Zodiac Job System (2007); The Zodiac Age is built on it.

### Final Fantasy XIII and later
- XIII (389): one version (PC, PS3, Xbox 360). None.
- XIV (386, the 2013 relaunch; the 2010 original is 14729): online, the latest keeps everything; the 2010 original is no longer playable. None.
- XV (359): Royal Edition (PS4, Xbox One; 2018, 80877) and Windows Edition (PC, 2018, 55034) have everything; Pocket Edition (2018, 55035; HD 108156) is a cut-down retelling (unsure: whether the Definitive tab could pick the Pocket Edition, which IGDB types as a remake from the same year). None.
- XVI (31551): Complete Edition (2024, 318557) is the base game plus its DLC. None.

## Dragon Quest

IGDB lists I-IV and VII under their US titles (Dragon Warrior ...).

### Dragon Quest
- IGDB id: 205229 (Dragon Warrior)
- Versions: Famicom / NES / MSX / MSX2 (1986, 205229); Super Famicom (1993, 239185; in Dragon Quest I.II); mobile, later Android, iOS, 3DS, PS4, Switch (2004, 1812; in Dragon Quest 1, 2, 3 Collection); HD-2D Remake (PC, Switch, PS5, Xbox Series, Switch 2; 2025, 375616; in Dragon Quest I & II HD-2D Remake 306144). The GBC version (Dragon Warrior I & II) has no entry of its own here.
- Rank: None. The HD-2D Remake cuts nothing found, adds story and keeps pixel sprites.

### Dragon Quest II
- IGDB id: 205592 (Dragon Warrior II)
- Versions: Famicom / NES / MSX / MSX2 (1987, 205592); Super Famicom (1993, 239186); GBC (1999, 239184; in Dragon Warrior I & II); mobile, later Android, iOS, 3DS, PS4, Switch (2005, 1813); smartphone (Android, iOS; 2014, 287153); HD-2D Remake (PC, Switch, PS5, Xbox Series, Switch 2; 2025, 375617)
- Rank: None. As for I: the HD-2D Remake adds a new character and story and cuts nothing found.

### Dragon Quest III
- IGDB id: 205589 (Dragon Warrior III)
- Versions: Famicom / NES (1988, 205589); Super Famicom (1996, 144651); GBC (2000, 205600); mobile, later Android, iOS, 3DS, PS4, Switch (2009, 1814); HD-2D Remake (PC, Switch, PS5, Xbox Series, Switch 2; 2024, 149980; in the HD-2D Erdrick Trilogy Collection 376240)
- Rank: B
- Latest: HD-2D Remake (2024)
- Recommend (en): The Super Famicom or Game Boy Color version is also an option.
- Recommend (ja): スーパーファミコン版かゲームボーイカラー版も選択肢です。
- Note (en): Both have the Treasures n' Trapdoors board game (sugoroku), which the HD-2D remake leaves out. The Game Boy Color version has more content, including monster medals and an extra dungeon. The Super Famicom version has the higher-quality graphics.
- Note (ja): どちらにも HD-2D 版にはない「すごろく場」があります。ゲームボーイカラー版にはモンスターメダルや追加ダンジョンなど、より多くの要素があります。グラフィックはスーパーファミコン版のほうが高画質です。
- The smartphone / PS4 / 3DS / Switch version has no sugoroku either; its prizes are moved elsewhere. The GBC extra dungeon is the Ice Cave, reached through the monster medals.

### Dragon Quest IV
- IGDB id: 16584 (Dragon Warrior IV)
- Versions: Famicom / NES (1990, 16584); PS1 (2001, Japan only, 145467); DS (2007, Japan 271211; Chapters of the Chosen 1815); smartphone (Android, iOS; 2014, 216304)
- Rank: None. Latest is the smartphone port of the DS version; it keeps the PS1 / DS additions and, unlike the US/EU DS release, has party chat.

### Dragon Quest V
- IGDB id: 205595
- Versions: Super Famicom (1992, 205595); PS2 (2004, Japan only, 144540); DS, later Android, iOS (2008, 16585)
- Rank: None. The DS / smartphone version keeps the PS2 additions and adds a third bride (Debora); the smartphone release has party chat.

### Dragon Quest VI
- IGDB id: 145497
- Versions: Super Famicom (1995, 145497); DS, later Android, iOS (2010, 1817)
- Rank: B
- Latest: DS remake (2010), also on mobile
- Recommend (en): The Super Famicom original (1995) is also an option.
- Recommend (ja): スーパーファミコン版（1995 年）も選択肢です。
- Note (en): The original lets monsters join the party after battles (the DS lets only a few slimes join), and it has the original pixel art.
- Note (ja): スーパーファミコン版では戦闘後にモンスターが仲間になります（DS 版で仲間になるのは一部のスライムだけです）。グラフィックもオリジナルのドット絵です。

### Dragon Quest VII
- IGDB id: 1818 (Dragon Warrior VII)
- Versions: PS1 (2000, 1818); 3DS Fragments of the Forgotten Past (2013, 23913); Reimagined (PC, Switch, PS5, Xbox Series, Switch 2; 2026, 366884)
- Rank: B
- Latest: Reimagined (2026)
- Recommend (en): The 3DS (2013) or PS1 (2000) version is also an option.
- Recommend (ja): 3DS 版（2013 年）か PS1 版（2000 年）も選択肢です。
- Note (en): Reimagined cuts three story episodes, plus the casino, Monster Park and the Immigrant Town. It is otherwise mostly an upgrade, with new episodes and much faster pacing.
- Note (ja): Reimagined では 3 つのエピソードと、カジノ・モンスターパーク・移民の町が削られています。それ以外はほぼ上位版で、新しいエピソードが加わり、テンポも大きく良くなっています。

### Dragon Quest VIII
- IGDB id: 1819
- Versions: PS2 (2004, 1819); Android, iOS (2013, 205649); 3DS (2015, 145528)
- Rank: B
- Latest: 3DS (2015)
- Recommend (en): Outside Japan, the PS2 version (2005, the US/EU release) is also an option.
- Recommend (ja): 海外版では、PS2 版（2005 年の北米・欧州版）も選択肢です。
- Note (en): Non-Japan releases only. The US/EU PS2 release has an orchestral soundtrack, but the US/EU 3DS release uses synth music. The 3DS adds two party members and new scenes.
- Note (ja): 海外版のみの話です。北米・欧州の PS2 版はオーケストラ音源ですが、北米・欧州の 3DS 版はシンセ音源です。3DS 版は仲間 2 人と新しいイベントが追加されています。
- Dropped as unverified: which music the Japanese 3DS version has.

### Dragon Quest IX
- IGDB id: 1820
- Versions: DS (2009, 1820) only.
- Rank: None.

### Dragon Quest X
- IGDB id: 80597
- Versions: online (Wii, Wii U, 3DS, PC, Android, PS4, Switch; 2012, 80597) with All In One Package bundles up to Versions 1-7 (2024, 276515); Offline (PC, Android, iOS, PS4, Switch, PS5; 2022, 149989)
- Rank: B (unsure: Latest). Not in definitive_notes.json until the Definitive tab is checked.
- Latest: Offline (2022) if the Definitive tab passes over the online game's bundles; if it picks the All In One Package 1-7 (2024), this is None.
- Recommend (en): The online version (Japan only, monthly fee) is also an option.
- Recommend (ja): オンライン版（月額制）も選択肢です。
- Note (en): Offline covers only the Version 1 story. The online game is still running and continues the story through Version 7.
- Note (ja): オフライン版で遊べるのは「バージョン 1」の物語だけです。オンライン版はサービスが続いていて、物語はバージョン 7 まで進んでいます。

### Dragon Quest XI
- IGDB id: 11667
- Versions: PS4, PC (2017, 11667); 3DS (2017, 115277); XI S Definitive Edition (PC, PS4, Xbox One, Switch, Stadia, Switch 2; 2019, 110069)
- Rank: None. XI S has the 3DS version's 2D mode and adds more.

## SaGa

### SaGa (Final Fantasy Legend)
- IGDB id: 396 (The Final Fantasy Legend)
- Versions: Game Boy (1989, 396); WonderSwan Color remake Makai Toushi SaGa (2002, also Japanese mobile phones; 84142); Collection of SaGa (PC, Android, iOS, Switch; 2020, 137130)
- Rank: B
- Latest: Collection of SaGa (2020), the Game Boy version
- Recommend (en): The WonderSwan Color remake (2002, Japan only) is also an option.
- Recommend (ja): ワンダースワンカラー版「魔界塔士 サ・ガ」（2002 年）も選択肢です。
- Note (en): It has color graphics. The Game Boy original in the Collection is monochrome.
- Note (ja): カラーで遊べます。コレクション版はゲームボーイ版そのままのモノクロです。

### SaGa 2 (Final Fantasy Legend II)
- IGDB id: 403 (Final Fantasy Legend II)
- Versions: Game Boy (1990, 403); DS remake Goddess of Destiny (2009, 62328); Collection of SaGa (PC, Android, iOS, Switch; 2020, 137130)
- Rank: S
- Latest: Collection of SaGa (2020), the Game Boy version
- Recommend (en): Play the DS remake, Goddess of Destiny (2009, Japan only).
- Recommend (ja): DS 版「サガ2秘宝伝説 GODDESS OF DESTINY」（2009 年）がおすすめです。
- Note (en): A full remake with added story scenes and characters and a reworked system. The Collection is only the Game Boy version.
- Note (ja): イベントやキャラクターが追加され、システムも作り直されたフルリメイクです。コレクション版はゲームボーイ版そのままです。

### SaGa 3 (Final Fantasy Legend III)
- IGDB id: 410 (Final Fantasy Legend III)
- Versions: Game Boy (1991, 410); DS remake Shadow or Light (2011, 66404); Collection of SaGa (PC, Android, iOS, Switch; 2020, 137130)
- Rank: S
- Latest: Collection of SaGa (2020), the Game Boy version
- Recommend (en): Play the DS remake, Shadow or Light (2011, Japan only).
- Recommend (ja): DS 版「サガ3時空の覇者 Shadow or Light」（2011 年）がおすすめです。
- Note (en): A full remake with a remade story and systems. The Collection is only the Game Boy version.
- Note (ja): 物語とシステムを作り直したフルリメイクです。コレクション版はゲームボーイ版そのままです。

### Romancing SaGa
- IGDB id: 11311
- Versions: Super Famicom (1992, 11311; the WonderSwan Color version, mobile and Wii Virtual Console are folded into this entry); PS2 remake Minstrel Song (2005, 141296); Minstrel Song Remastered (PC, Android, iOS, PS4, Switch, PS5; 2022, 203315)
- Rank: A
- **Approved** (2026-11-03), in definitive_notes.json.
- Latest: Minstrel Song Remastered (2022)
- Names only the WonderSwan Color version (an upgrade of the Super Famicom original). A, not B: the latest is not fully high-definition, and its gameplay is substantially reworked, so the choice is preference.
- Recommend (en): The WonderSwan Color version (2001, Japan only), an upgraded version of the Super Famicom original, is also an option.
- Recommend (ja): スーパーファミコン版を強化したワンダースワンカラー版（2001 年）も選択肢です。
- Note (en): It keeps the original's pixel art and gameplay, adds a new scenario and lets you collect all ten Destiny Stones. The latest version is not fully high-definition, and its gameplay was substantially reworked, so which to play depends on preference.
- Note (ja): オリジナルのドット絵と遊び方のまま、新しいシナリオが加わり、10 個すべてのデスティニーストーンを集められます。最新版は完全な HD ではなく、ゲームの内容も大きく作り直されているので、どちらを選ぶかは好みによります。

### Romancing SaGa 2
- IGDB id: 11312
- Versions: Super Famicom (1993, 11312; the 2016 remaster on PC, Android, iOS, Vita, PS4, Xbox One, Switch is folded into this entry); Revenge of the Seven (PC, PS4, Switch, PS5, Xbox Series, Switch 2; 2024, 306154)
- Rank: B
- **Approved** (2026-11-03), in definitive_notes.json.
- Latest: Revenge of the Seven (2024), the 3D remake.
- Names only the remaster (2016; 2017 on some platforms), a complete upgrade of the Super Famicom original.
- Recommend (en): The remaster (2016), a complete upgrade of the original game, is also an option.
- Recommend (ja): オリジナル版を全面的に強化したリマスター版（2016 年）も選択肢です。
- Note (en): It keeps the pixel art and the original's more detailed systems, such as eight separate defence types, and has its own extra dungeon. The 3D remake has a different extra dungeon instead.
- Note (ja): ドット絵。防御の種類が 8 つに分かれているなど、オリジナルの細かいシステムが残っています。独自の追加ダンジョンもあります（3D リメイクの追加ダンジョンは別のものです）。

### Romancing SaGa 3
- IGDB id: 11313
- Versions: Super Famicom (1995, 11313; also Wii Virtual Console); remaster (PC, Android, iOS, Vita, PS4, Xbox One, Switch; 2019, 109592); Destinies United (PC, Switch, PS5, Xbox Series, Switch 2; 2027, 415836)
- Rank: B
- **Approved** (2026-11-03), in definitive_notes.json.
- Latest: Destinies United (2027), the 3D remake; not out yet, so until then the 2019 remaster is Latest and the note does not apply.
- Names only the remaster (2019), a complete upgrade of the Super Famicom original. Commander Mode's removal is from the producer's interview (AUTOMATON, in Sources). Unsure: whether anything else the remaster has is missing; the remake's "every character can be recruited" is an addition, so it is left out of the note.
- Recommend (en): The remaster (2019), a complete upgrade of the original game, is also an option.
- Recommend (ja): オリジナル版を全面的に強化したリマスター版（2019 年）も選択肢です。
- Note (en): It keeps the pixel art and Commander Mode, where the leader commands from the back row. The 3D remake removes Commander Mode.
- Note (ja): ドット絵。主人公が後列から指揮する「コマンダーモード」があります。3D リメイクではコマンダーモードがなくなりました。

### SaGa Frontier
- IGDB id: 11310
- Versions: PS1 (1997, 11310; also PS3, PSP); Remastered (PC, Android, iOS, PS4, Switch; 2021, 141281)
- Rank: None. The remaster adds content (Fuse's scenario among it) and cuts nothing found.

### SaGa Frontier 2
- IGDB id: 2353
- Versions: PS1 (1999, 2353; also PS3, PSP); Remastered (PC, Android, iOS, PS4, Switch, PS5; 2025, 294870)
- Rank: None. The remaster adds content and cuts nothing found.

### Later entries
- Unlimited SaGa (11314): one version (PS2). None.
- SaGa Scarlet Grace (23981, PC, Vita 2016): Ambitions (PC, Android, iOS, PS4, Switch; 2018, 119373) is clearly better. None.
- SaGa Emerald Beyond (266681): one version. None.

## Sources

- [Romancing SaGa (Wikipedia)](https://en.wikipedia.org/wiki/Romancing_SaGa)
- [Romancing SaGa WonderSwan Color: Destiny Stones and new scenario (RPGamer, 2001)](https://archive.rpgamer.com/news/Q4-2001/110501b.html)
- [Romancing SaGa 3: Destinies United removes Commander Mode (AUTOMATON)](https://automaton-media.com/en/news/romancing-saga-3-destinies-united-removes-originals-commander-mode-you-can-safely-choose-sarah-as-your-protagonist-producer-jokes/)
- [Romancing SaGa 2: Revenge of the Seven review: defence types reduced (Final Weapon)](https://finalweapon.net/2024/10/24/romancing-saga-2-revenge-of-the-seven-review-a-crowning-achievement/)
- [Dragon Quest VII Reimagined: episodes and features omitted (RPG Site)](https://www.rpgsite.net/news/18576-dragon-quest-vii-reimagined-will-have-several-episodes-features-omitted-replaced)
- [Dragon Quest III HD-2D Remake lacks Treasures n' Trapdoors (RPG Site)](https://www.rpgsite.net/news/16386-dragon-quest-iii-hd-2d-remake-tgs-2024-panel-confirms-lack-treasures-n-trapdoors-board-game)
- [Dragon Warrior III GBC additions (TCRF)](https://tcrf.net/Dragon_Warrior_III_(Game_Boy_Color))
- [Dragon Quest VI (Wikipedia)](https://en.wikipedia.org/wiki/Dragon_Quest_VI)
- [Party Chat (Dragon Quest Wiki)](https://dragon-quest.org/wiki/Party_chat)
- [Dragon Quest VIII 3DS: voice acting, no orchestrated music (Nintendo Everything)](https://nintendoeverything.com/dragon-quest-viii-3ds-will-have-voice-acting-no-orchestrated-music/)
- [Final Fantasy VII Remake Part 3 is Revelation, spring 2027 (RPG Site)](https://rpgsite.net/news/20542-final-fantasy-vii-remake-part-3-revelation-ff7r-part-3)
- [Final Fantasy VIII Remastered drops Chocobo World (Nova Crystallis)](https://novacrystallis.com/2019/06/yoshinori-kitase-talks-final-fantasy-viii-remastered-famitsu-interview/)
- [Final Fantasy V and VI delisted from Steam (RPG Site)](https://www.rpgsite.net/news/11420-final-fantasy-v-and-final-fantasy-vi-are-being-delisted-from-steam-on-july-27)
- [Final Fantasy Pixel Remaster (Wikipedia)](https://en.wikipedia.org/wiki/Final_Fantasy_Pixel_Remaster)
- [SaGa Frontier 2 Remastered now available (RPG Site)](https://www.rpgsite.net/news/17077-saga-frontier-2-remastered-now-available-nintendo-direct-trailer-gameplay-remaster)
