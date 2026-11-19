-- The plugins installed on the server: the package and version of each, where it came from, and the
-- digest of its files. Hatchery installs them into the server's plugin folder; this is the record of
-- what is there, written after every change and when the server starts leading.
CREATE TABLE rookery.plugins (
    id           text PRIMARY KEY,
    package      text NOT NULL,
    version      text NOT NULL,
    origin       text NOT NULL,
    source       text NOT NULL DEFAULT '',
    digest       text NOT NULL,
    client_hash  text,
    problem      text,
    installed_at timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now()
);
