-- The tables of a Rookery 0.6 store on PostgreSQL, as its migrations made them.
CREATE TABLE messages(
    seq BIGINT NOT NULL UNIQUE, host TEXT NOT NULL, id BIGINT NOT NULL,
    topic TEXT NOT NULL, body TEXT NOT NULL, keep TEXT NOT NULL,
    from_level TEXT NOT NULL, from_uid BIGINT NOT NULL, session TEXT,
    time TEXT COLLATE "C" NOT NULL, done BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (host, id));
CREATE TABLE cursors(host TEXT PRIMARY KEY, last_id BIGINT NOT NULL);
CREATE TABLE kv(
    namespace TEXT NOT NULL, key TEXT COLLATE "C" NOT NULL, value TEXT NOT NULL,
    revision BIGINT NOT NULL, updated TEXT NOT NULL,
    PRIMARY KEY (namespace, key));
CREATE INDEX messages_topic ON messages(topic, time);
CREATE TABLE jobs(
    id TEXT COLLATE "C" PRIMARY KEY, host TEXT NOT NULL, what TEXT NOT NULL,
    started TEXT COLLATE "C" NOT NULL, ended TEXT, exit_code INTEGER,
    output_path TEXT NOT NULL);
CREATE TABLE state(
    kind TEXT COLLATE "C" NOT NULL, scope TEXT COLLATE "C" NOT NULL,
    name TEXT COLLATE "C" NOT NULL, value BYTEA NOT NULL, sealed BOOLEAN NOT NULL,
    updated TEXT NOT NULL, PRIMARY KEY (kind, scope, name));
ALTER TABLE jobs ADD COLUMN subject TEXT NOT NULL DEFAULT '';
ALTER TABLE jobs ADD COLUMN action TEXT NOT NULL DEFAULT '';
ALTER TABLE jobs ADD COLUMN token TEXT;
CREATE UNIQUE INDEX jobs_token ON jobs(subject, action, token) WHERE token IS NOT NULL;
ALTER TABLE jobs ADD COLUMN stopped_by TEXT;
ALTER TABLE jobs ADD COLUMN arguments TEXT;
ALTER TABLE state ADD COLUMN label TEXT COLLATE "C";
CREATE UNIQUE INDEX state_label ON state(kind, scope, label) WHERE label IS NOT NULL;
CREATE TABLE schema_version(version INTEGER NOT NULL);
INSERT INTO schema_version(version) VALUES (7);
