-- What each provider bulk sync covered, so the console can say how much of a provider is
-- cached: per provider and app platform, the entries the last sync fetched, the provider's
-- own total when it's known (a dump's count, IGDB's games/count, a complete list's length),
-- and whether that sync got the whole list. app_platform_id 0 = the provider as a whole
-- (its overall total; `complete` = a full-database sync ran to the end). Cache tables have
-- no platform column, which is why this is recorded at sync time.
CREATE TABLE provider_sync_state (
    provider        VARCHAR(32) NOT NULL,
    app_platform_id SMALLINT UNSIGNED NOT NULL DEFAULT 0,
    remote_id       VARCHAR(128) NULL,              -- the provider's own platform/console id(s)
    entries         INT UNSIGNED NOT NULL DEFAULT 0,
    remote_total    INT UNSIGNED NULL,
    complete        BOOLEAN NOT NULL DEFAULT FALSE,
    synced_at       DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (provider, app_platform_id),
    CONSTRAINT fk_pss_platform FOREIGN KEY (app_platform_id) REFERENCES platforms(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
