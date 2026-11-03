## Abstract
RomExplorer のMVPは、ネイティブCHD/RVZ読取からハッシュを生成し、ローカルROM DBを作成し、Playmatchで照合した後にIGDB/ScreenScraperでメタデータとメディアを補完する。共通データモデルをアプリ全体で使い、保存は nullable、表示は N/A に統一する。

## Steps
1. 共通型を src/common に定義し、Platform は IGDB 公式名 + IGDB 数値IDを正準値として扱う。
2. CHD/RVZ の読取結果を共通型へ正規化し、ハッシュ順序を CRC32 -> XXH3-128 -> MD5 -> SHA-1 -> SHA-256 に固定する。
3. セクション順序を Archive Hash -> RAHash -> Tracks に固定し、保存順と表示順を一致させる。
4. 正規化済みデータをローカルROM DBへ保存し、Playmatch入力DTOを共通型から生成する。
5. マッチ結果をキーに IGDB/ScreenScraper補完を実行し、欠損値は保存時Option、表示時N/Aで統一する。
