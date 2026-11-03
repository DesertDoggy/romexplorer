-- Whether an emulator opens a multi-disc game's `.m3u` itself. A game with a playlist is
-- started through it only when the chosen emulator does; otherwise through its first disc.
-- TRUE for existing rows: that is what every launch did before this column.
ALTER TABLE emulators ADD COLUMN supports_playlists BOOLEAN NOT NULL DEFAULT TRUE AFTER supports_archives;
