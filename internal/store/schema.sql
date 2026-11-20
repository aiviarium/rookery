-- The core schema.
--
-- Until the first tagged release this file is the whole core schema: a change
-- edits it, and the server records its SHA-256 in rookery.meta. A database whose
-- digest differs is refused at start-up, and `rookery server reset` drops the
-- schemas and applies this file again. At the first tag this file becomes
-- migration 1 and numbered migrations start from there.

CREATE SCHEMA rookery;

COMMENT ON SCHEMA rookery IS 'The Rookery core: people, access, hosts, jobs, data.';

CREATE TABLE rookery.meta (
	key        text        PRIMARY KEY,
	value      bytea       NOT NULL,
	updated_at timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE rookery.meta IS 'Values per database: the schema digest today, more later.';
