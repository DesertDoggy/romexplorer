use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Envelope every TheGamesDB v1 response wraps the payload in. We only read the
/// fields we need: the rate-limit metadata and the typed `data` payload.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TgdbEnvelope<T> {
	pub code: i32,
	#[serde(default)]
	pub status: Option<String>,
	#[serde(default)]
	pub remaining_monthly_allowance: Option<i32>,
	#[serde(default)]
	pub extra_allowance: Option<i32>,
	/// romexplorer addition: the paging links of list endpoints (`Games/ByPlatformID`).
	#[serde(default)]
	pub pages: Option<TgdbPages>,
	pub data: T,
}

/// romexplorer addition: `pages` of a paginated response; `next` is null on the last page.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TgdbPages {
	#[serde(default)]
	pub previous: Option<String>,
	#[serde(default)]
	pub current: Option<String>,
	#[serde(default)]
	pub next: Option<String>,
}

/// romexplorer addition: `/v1/Platforms` (keyed by id as a string).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TgdbPlatformsData {
	#[serde(default)]
	pub count: i32,
	#[serde(default)]
	pub platforms: HashMap<String, TgdbPlatform>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TgdbPlatform {
	pub id: i64,
	#[serde(default)]
	pub name: Option<String>,
	#[serde(default)]
	pub alias: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TgdbSearchData {
	#[serde(default)]
	pub count: i32,
	#[serde(default)]
	pub games: Vec<TgdbApiGame>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TgdbGameByIdData {
	#[serde(default)]
	pub count: i32,
	#[serde(default)]
	pub games: Vec<TgdbApiGame>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TgdbApiGame {
	pub id: i64,
	#[serde(default)]
	pub game_title: Option<String>,
	#[serde(default)]
	pub alternates: Option<Vec<String>>,
	/// romexplorer addition: present in list responses and the JSON dump.
	#[serde(default)]
	pub release_date: Option<String>,
	#[serde(default)]
	pub platform: Option<i64>,
	/// romexplorer addition: present in the JSON dump (and the API's `Game` schema). Each
	/// regional release is its own TheGamesDB game; 0 or absent means not set.
	#[serde(default)]
	pub region_id: Option<i64>,
	#[serde(default)]
	pub country_id: Option<i64>,
}

/// romexplorer addition: one entry of `/v1/Regions` or `/v1/Countries`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TgdbNamed {
	pub id: i64,
	#[serde(default)]
	pub name: Option<String>,
}

/// romexplorer addition: `/v1/Regions`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TgdbRegionsData {
	#[serde(default, deserialize_with = "map_or_empty")]
	pub regions: HashMap<String, TgdbNamed>,
}

/// romexplorer addition: `/v1/Countries`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TgdbCountriesData {
	#[serde(default, deserialize_with = "map_or_empty")]
	pub countries: HashMap<String, TgdbNamed>,
}

/// romexplorer addition: `/v1/Games/Images`. `images` is keyed by game id; `base_url` gives
/// the prefix per size, and `original` is the full-size scan.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TgdbImagesData {
	#[serde(default)]
	pub count: i32,
	#[serde(default)]
	pub base_url: TgdbImageBaseUrl,
	#[serde(default, deserialize_with = "map_or_empty")]
	pub images: HashMap<String, Vec<TgdbImage>>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TgdbImageBaseUrl {
	#[serde(default)]
	pub original: String,
	#[serde(default)]
	pub large: Option<String>,
	#[serde(default)]
	pub medium: Option<String>,
}

/// One image: `kind` is `boxart`, `titlescreen`, `screenshot`, `clearlogo`, `fanart` or
/// `banner`; `side` is `front`/`back` for box art.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct TgdbImage {
	pub id: i64,
	#[serde(rename = "type")]
	pub kind: String,
	#[serde(default)]
	pub side: Option<String>,
	pub filename: String,
	#[serde(default)]
	pub resolution: Option<String>,
}

/// A JSON object keyed by id -- which TheGamesDB's PHP backend sends as `[]` when it is empty.
fn map_or_empty<'de, D, V>(deserializer: D) -> Result<HashMap<String, V>, D::Error>
where
	D: serde::Deserializer<'de>,
	V: Deserialize<'de>,
{
	#[derive(Deserialize)]
	#[serde(untagged)]
	enum MapOrList<V> {
		Map(HashMap<String, V>),
		List(Vec<serde_json::Value>),
	}
	Ok(match Option::<MapOrList<V>>::deserialize(deserializer)? {
		Some(MapOrList::Map(map)) => map,
		_ => HashMap::new(),
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_dump_row_keeps_its_region_and_country() {
		let game: TgdbApiGame = serde_json::from_str(r#"{"id":126005,"game_title":"Hoge Quest","platform":4,"region_id":6,"country_id":28,"alternates":null,"uids":null,"hashes":null}"#).unwrap();
		assert_eq!((game.region_id, game.country_id), (Some(6), Some(28)));
		let bare: TgdbApiGame = serde_json::from_str(r#"{"id":2,"game_title":"Hoge Quest 2"}"#).unwrap();
		assert_eq!((bare.region_id, bare.country_id), (None, None));
	}

	#[test]
	fn an_images_page_parses_and_an_empty_one_too() {
		let data: TgdbImagesData = serde_json::from_str(
			r#"{"count":3,"base_url":{"original":"https://cdn.thegamesdb.net/images/original/","small":"x","thumb":"y"},
			   "images":{"126005":[{"id":1,"type":"boxart","side":"front","filename":"boxart/front/126005-1.jpg","resolution":"1000x1000"},
			                       {"id":2,"type":"boxart","side":"back","filename":"boxart/back/126005-1.jpg","resolution":null},
			                       {"id":3,"type":"clearlogo","side":null,"filename":"clearlogo/126005.png","resolution":null}]}}"#,
		)
		.unwrap();
		assert_eq!(data.base_url.original, "https://cdn.thegamesdb.net/images/original/");
		let images = &data.images["126005"];
		assert_eq!(images.len(), 3);
		assert_eq!((images[0].kind.as_str(), images[0].side.as_deref()), ("boxart", Some("front")));
		let empty: TgdbImagesData = serde_json::from_str(r#"{"count":0,"base_url":{"original":"u"},"images":[]}"#).unwrap();
		assert!(empty.images.is_empty());
		let countries: TgdbCountriesData = serde_json::from_str(r#"{"countries":{"28":{"id":28,"name":"Japan"}}}"#).unwrap();
		assert_eq!(countries.countries["28"].name.as_deref(), Some("Japan"));
	}
}
