-- Accounts for network access: who may use a main server or a share, and what they may do.
--
-- Auth applies only to network listeners. The desktop app's in-process backend never signs in
-- (it is the machine's own admin), so a standalone or client install needs no account at all.
-- Until an admin account with a password exists, the setup token in `data/server_token` acts
-- as admin; creating one retires it.
--
-- Named accounts, no shared passphrase: each person signs in as themselves, and each sign-in
-- mints a token for that one device, so a lost phone is revoked without touching anyone else.

-- `username_key` is the lowercased name, so "Bob" and "bob" are one account, while
-- `username` keeps the spelling it was created with. `password_hash` is an argon2id PHC
-- string; NULL means the account cannot sign in (a restore onto a fresh database brings
-- accounts back this way, because backups never carry hashes -- see `api::secrets`).
--
-- `can_upload` is the one extra permission a regular user can have: registering new files it
-- copied onto the server's share. It never allows changing what is already there.
CREATE TABLE users (
    id             INT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
    username       VARCHAR(64) NOT NULL,
    username_key   VARCHAR(64) NOT NULL,
    password_hash  VARCHAR(255) NULL,
    role           ENUM('admin','user') NOT NULL DEFAULT 'user',
    can_upload     BOOLEAN NOT NULL DEFAULT FALSE,
    enabled        BOOLEAN NOT NULL DEFAULT TRUE,
    deleted        BOOLEAN NOT NULL DEFAULT FALSE,
    created_at     DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at     DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
    last_login_at  DATETIME NULL,
    UNIQUE KEY uq_users_name (username_key)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- One row per signed-in device. Only the SHA-256 of the token is kept, so a copy of this table
-- signs nobody in. No expiry, by choice: this is a LAN app, and revoking is per device or per
-- user. `revoked` rather than a hard delete keeps "which devices has this account used" visible.
CREATE TABLE user_tokens (
    id            BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
    user_id       INT UNSIGNED NOT NULL,
    token_sha256  CHAR(64) NOT NULL,
    device        VARCHAR(128) NOT NULL DEFAULT '',
    created_at    DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    last_used_at  DATETIME NULL,
    revoked       BOOLEAN NOT NULL DEFAULT FALSE,
    UNIQUE KEY uq_user_tokens_sha (token_sha256),
    KEY ix_user_tokens_user (user_id, revoked),
    CONSTRAINT fk_user_tokens_user FOREIGN KEY (user_id) REFERENCES users(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- The SHA-256 of the TLS certificate a peer presented the first time this machine reached it.
-- Later connections must present the same one (`server::tls::PinnedVerifier`): a self-signed
-- certificate on a DHCP address has no CA or stable name to check instead.
ALTER TABLE remote_sources ADD COLUMN tls_fingerprint CHAR(64) NULL AFTER token;
