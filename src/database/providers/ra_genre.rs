//! Raw provider genre text -> one of RetroAchievements' 19 major genres
//! (https://docs.retroachievements.org/guidelines/content/genre-definitions.html). RA's
//! own field is fine-grained free text ("2D Platforming", "Sports - Golf", "Turn-Based
//! RPG"); IGDB/MobyGames have their own fixed lists. This is a native port of the
//! classification the user wrote for RAChecker (x3kim/RAChecker, MIT,
//! `server/src/genres.js`, GENRE_MAP_VERSION 3): normalize the token, then, in priority,
//! an exact major-genre name, an exact subgenre/alias name, a keyword rule; "Other" only
//! when nothing more specific classifies. Bump [`GENRE_MAP_VERSION`] when the mapping
//! changes so stored parents can be recomputed.

pub const GENRE_MAP_VERSION: u32 = 3;

pub const MAJOR_GENRES: &[&str] = &[
    "Action-Adventure",
    "Action",
    "Adventure",
    "Arcade",
    "Board and Card",
    "Educational",
    "Fighting",
    "Horror",
    "Literature",
    "Platforming",
    "Puzzle",
    "Racing",
    "Rhythm",
    "Role-Playing Game",
    "Shooter",
    "Simulation",
    "Sports",
    "Strategy",
    "Other",
];

/// Exact subgenre names and alternative spellings -> major genre (compared by [`genre_key`]).
const SUBGENRES: &[(&str, &[&str])] = &[
    ("Action-Adventure", &["Metroidvania", "Open World", "Sandbox", "Zelda-like", "Immersive Sim", "Survival", "Action Adventure", "Adventure Action"]),
    ("Action", &["Beat 'em Up", "Brawler", "Hack and Slash", "Character Action", "Stealth", "Ninja"]),
    ("Adventure", &["Visual Novel", "Point and Click", "Point-and-Click Adventure", "Graphic Adventure", "Text Adventure", "Interactive Fiction", "Interactive Movie", "Escape Room", "Mystery"]),
    ("Arcade", &["Breakout", "Brick Breakers", "Block Breaker", "Maze", "Maze Chase", "Pinball", "Endless Runner", "Runner", "Whack-a-Mole", "Single Screen", "Minigames", "Simon Says"]),
    ("Board and Card", &["Board Game", "Card Game", "Collectible Card Game", "Chess", "Shogi", "Shougi", "Go", "Mahjong", "Mahjong Solitaire", "Solitaire", "Casino", "Gambling", "Pachinko", "Slots", "Trivia", "Quiz", "Game Show", "Tabletop", "Rock-Paper-Scissors", "Card & Board Game", "Quiz/Trivia"]),
    ("Educational", &["Edutainment", "Typing", "Math", "Language Learning", "Learning"]),
    ("Fighting", &["Versus Fighting", "Fighter"]),
    ("Horror", &["Survival Horror", "Psychological Horror"]),
    ("Literature", &["Book", "Gamebook", "Digital Comic", "Reading"]),
    ("Platforming", &["Platform", "Platformer", "Collect-a-thon"]),
    ("Puzzle", &["Match 3", "Match Three", "Tile Matching", "Nonogram", "Picross", "Sokoban", "Logic", "Sudoku", "Hidden Object", "Marble Popper", "Word"]),
    ("Racing", &["Rally", "Motorcycle", "Vehicular Combat", "Vehicle Combat"]),
    ("Rhythm", &["Music", "Dance", "Karaoke"]),
    ("Role-Playing Game", &["RPG", "JRPG", "WRPG", "CRPG", "ARPG", "SRPG", "MMORPG", "Dungeon Crawl", "Dungeon Crawler", "Roguelike", "Roguelite", "Monster Collecting", "Personalike", "Role-playing (RPG)"]),
    ("Shooter", &["Shoot 'Em Up", "Shmup", "Run and Gun", "FPS", "TPS", "Light Gun", "Light-Phaser Game", "Bullet Hell", "Danmaku", "Space Combat"]),
    ("Simulation", &["Farming", "Tycoon", "Business", "City Builder", "Construction and Management", "Cooking", "Fishing", "Virtual Pet", "Photography", "Simulator"]),
    ("Sports", &["Soccer", "Football", "Basketball", "Baseball", "Golf", "Tennis", "Boxing", "Wrestling", "Hockey", "Bowling", "Skateboarding", "Snowboarding", "Olympic", "Volleyball", "Cricket", "Rugby", "Sumo", "Darts", "Pool", "Billiards", "Track & Field", "Dodgeball", "Hunting", "Sport"]),
    ("Strategy", &["RTS", "TBS", "Tactics", "4X", "Tower Defense", "Wargame", "Artillery", "Auto Battler", "MOBA", "Real Time Strategy (RTS)", "Turn-based strategy (TBS)", "Tactical"]),
    ("Other", &["Compilation", "Party", "Minigame Collection", "Utility", "Tool", "Creative", "Demo", "Test Kit", "Multi-genre", "Misc", "Miscellaneous", "Incremental", "Video"]),
];

/// Fallback for compound names ("2.5D Platforming", "Sports - Horse Racing", "Combat Flight
/// Simulation"). Order is the priority: the first needle found in the normalized token
/// decides, so "Platform Fighting" lands in Fighting and "Tactical Shooter" in Shooter.
const KEYWORD_RULES: &[(&[&str], &str)] = &[
    (&["fighting", "fighter", "martialarts"], "Fighting"),
    (&["shooter", "shooting", "shootem", "shmup", "rungun"], "Shooter"),
    (&["rpg", "roleplaying", "dungeoncrawl"], "Role-Playing Game"),
    (&["platform"], "Platforming"),
    (&["puzzle", "nonogram", "jigsaw", "sokoban", "sudoku", "match3"], "Puzzle"),
    (&["sports", "athletic"], "Sports"),
    (&["racing", "driving", "vehicular", "vehiclecombat", "kart", "motocross"], "Racing"),
    (&["simulation", "simulator"], "Simulation"),
    (&["horror"], "Horror"),
    (&["strategy", "tactic", "towerdefense", "wargame"], "Strategy"),
    (&["card", "mahjong", "solitaire", "chess", "trivia", "gameshow", "quiz", "board", "casino", "gambling"], "Board and Card"),
    (&["rhythm", "music", "dance", "karaoke"], "Rhythm"),
    (&["educational", "edutainment"], "Educational"),
    (&["metroidvania", "openworld"], "Action-Adventure"),
    (&["adventure"], "Adventure"),
    (&["maze", "brickbreak", "breakout", "pinball", "minigame", "arcade"], "Arcade"),
    (&["beatem", "brawler", "hackandslash", "stealth", "sidescrolling"], "Action"),
    (&["action"], "Action"),
];

/// Genre values that aren't genres at all (a provider's marketing/scope tags) and are
/// dropped rather than stored.
const NOT_A_GENRE: &[&str] = &["indie"];

/// Normalizes a genre string the way RAChecker's `key()` does: lowercase, "&" -> "and",
/// alphanumerics only.
pub fn genre_key(input: &str) -> String {
    input.to_ascii_lowercase().replace('&', " and ").chars().filter(|c| c.is_ascii_alphanumeric()).collect()
}

/// A value that carries no genre information (see [`NOT_A_GENRE`]).
pub fn is_not_a_genre(input: &str) -> bool {
    let key = genre_key(input);
    NOT_A_GENRE.contains(&key.as_str())
}

fn major_by_key(key: &str) -> Option<&'static str> {
    MAJOR_GENRES.iter().copied().find(|m| genre_key(m) == key)
}

fn subgenre_by_key(key: &str) -> Option<&'static str> {
    SUBGENRES.iter().find_map(|(major, subs)| subs.iter().any(|s| genre_key(s) == key).then_some(*major))
}

fn from_keywords(key: &str) -> Option<&'static str> {
    if key.contains("action") && key.contains("adventure") {
        return Some("Action-Adventure");
    }
    KEYWORD_RULES.iter().find_map(|(needles, major)| needles.iter().any(|n| key.contains(n)).then_some(*major))
}

fn split_keys(raw: &str) -> Vec<String> {
    // RA separates alternatives with commas, slashes and pipes alike.
    raw.split([',', '/', '|']).map(genre_key).filter(|k| !k.is_empty()).collect()
}

/// The major genre for a raw genre string ("2D Platforming, Collect-a-thon"), exactly
/// RAChecker's `majorGenre`: `None` when there is nothing to classify, "Other" when nothing
/// matches. "Compilation" & friends only decide when nothing more specific is present
/// ("Compilation, Turn-Based RPG" is an RPG).
pub fn major_genre(raw: &str) -> Option<&'static str> {
    let keys = split_keys(raw);
    if keys.is_empty() {
        return None;
    }
    let mut fallback: Option<&'static str> = None;
    for k in &keys {
        if let Some(hit) = major_by_key(k) {
            if hit != "Other" {
                return Some(hit);
            }
            fallback = Some(hit);
        }
    }
    for k in &keys {
        if let Some(hit) = subgenre_by_key(k) {
            if hit != "Other" {
                return Some(hit);
            }
            fallback = Some(hit);
        }
    }
    for k in &keys {
        if let Some(hit) = from_keywords(k) {
            return Some(hit);
        }
    }
    Some(fallback.unwrap_or("Other"))
}

/// One major genre for one component (no compound splitting), same three tiers.
fn classify_component(component: &str) -> &'static str {
    let key = genre_key(component);
    if key.is_empty() {
        return "Other";
    }
    major_by_key(&key).or_else(|| subgenre_by_key(&key)).or_else(|| from_keywords(&key)).unwrap_or("Other")
}

/// Every major genre a compound genre string maps to (one per component, split on `,`
/// `/` `|`), deduplicated, in order of appearance; "Other" only when nothing better
/// classified. Empty input -> empty.
pub fn normalize_ra_genres(raw: &str) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for component in raw.split([',', '/', '|']) {
        let component = component.trim();
        if component.is_empty() || is_not_a_genre(component) {
            continue;
        }
        let major = classify_component(component);
        if !out.contains(&major) {
            out.push(major);
        }
    }
    if out.len() > 1 {
        out.retain(|g| *g != "Other");
    }
    out
}

/// The single primary major genre (RAChecker's `majorGenre`), or "Other".
pub fn normalize_ra_genre(raw: &str) -> &'static str {
    major_genre(raw).unwrap_or("Other")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_rachecker_classification() {
        assert_eq!(normalize_ra_genre("2D Platforming"), "Platforming");
        assert_eq!(normalize_ra_genre("Sports - Golf"), "Sports");
        assert_eq!(normalize_ra_genre("Shoot 'Em Up"), "Shooter");
        assert_eq!(normalize_ra_genre("shoot-em-up"), "Shooter", "spellings normalize to one key");
        assert_eq!(normalize_ra_genre("Action RPG / Roguelike"), "Role-Playing Game");
        assert_eq!(normalize_ra_genre("Compilation, Turn-Based RPG"), "Role-Playing Game", "Other only when nothing better");
        assert_eq!(normalize_ra_genre("Compilation"), "Other");
        assert_eq!(normalize_ra_genre("Platform Fighting"), "Fighting", "keyword priority");
        assert_eq!(normalize_ra_genre("Tactical Shooter"), "Shooter");
        assert_eq!(normalize_ra_genre("Survival Horror"), "Horror");
        assert_eq!(normalize_ra_genre("Visual Novel"), "Adventure");
        assert_eq!(normalize_ra_genre("Something Unclassifiable"), "Other");
        assert_eq!(major_genre(""), None);
        assert_eq!(normalize_ra_genres("Puzzle, Platform Fighting"), vec!["Puzzle", "Fighting"]);
        assert!(normalize_ra_genres("").is_empty());
    }

    #[test]
    fn igdb_and_mobygames_names_land_on_majors() {
        for (raw, major) in [
            ("Point-and-click", "Adventure"),
            ("Real Time Strategy (RTS)", "Strategy"),
            ("Turn-based strategy (TBS)", "Strategy"),
            ("Role-playing (RPG)", "Role-Playing Game"),
            ("Hack and slash/Beat 'em up", "Action"),
            ("Quiz/Trivia", "Board and Card"),
            ("Card & Board Game", "Board and Card"),
            ("Simulator", "Simulation"),
            ("Sport", "Sports"),
            ("Tactical", "Strategy"),
            ("MOBA", "Strategy"),
            ("Pinball", "Arcade"),
            ("Music", "Rhythm"),
            ("Platform", "Platforming"),
            ("Strategy / tactics", "Strategy"),
            ("Racing / driving", "Racing"),
        ] {
            assert_eq!(normalize_ra_genre(raw), major, "{raw}");
        }
        assert!(is_not_a_genre("Indie"));
        assert!(normalize_ra_genres("Indie").is_empty());
    }
}
