-- Emulators the launcher opens roms / games with ("Open with ...").
--
-- `path`: the executable (Windows .exe; a macOS .app bundle, whose binary is read from its
-- Info.plist; a Linux binary / AppImage), or for `kind = 'command'` a command line prefix
-- such as `flatpak run org.libretro.RetroArch`. `args`: the argument template, split into
-- arguments as written; `{rom}` is the file to open and `{core}` a RetroArch core path.
CREATE TABLE emulators (
    id                BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
    name              VARCHAR(128) NOT NULL,
    preset            VARCHAR(64) NULL,
    kind              ENUM('standalone','retroarch','command') NOT NULL DEFAULT 'standalone',
    path              VARCHAR(1024) NOT NULL,
    args              VARCHAR(1024) NOT NULL DEFAULT '{rom}',
    -- RetroArch: where its cores are (default: `cores` next to the executable / the OS default).
    cores_dir         VARCHAR(1024) NULL,
    working_dir       VARCHAR(1024) NULL,
    supports_archives BOOLEAN NOT NULL DEFAULT FALSE,
    enabled           BOOLEAN NOT NULL DEFAULT TRUE,
    created_at        DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Which platforms an emulator opens (with the RetroArch core for each), and which one a
-- platform opens with by default.
CREATE TABLE emulator_platforms (
    emulator_id     BIGINT UNSIGNED NOT NULL,
    app_platform_id SMALLINT UNSIGNED NOT NULL,
    core            VARCHAR(128) NULL,
    is_default      BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (emulator_id, app_platform_id),
    INDEX idx_ep_platform (app_platform_id),
    CONSTRAINT fk_ep_emulator FOREIGN KEY (emulator_id) REFERENCES emulators(id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
