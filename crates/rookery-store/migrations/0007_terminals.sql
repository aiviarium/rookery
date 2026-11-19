-- Terminals: sessions on a host the server keeps a record of, by the host's id, so they go with it when
-- it is renamed and are forgotten with it, whose they are and how they ended. The screen and the output are the host's; the size and how far the output came are
-- the server's last word on them.
CREATE TABLE rookery.terminals (
    id         text PRIMARY KEY,
    host       text NOT NULL REFERENCES rookery.hosts (id) ON DELETE CASCADE ON UPDATE CASCADE,
    -- The session's id on the host.
    session    text NOT NULL,
    owner      uuid REFERENCES rookery.users (id) ON DELETE SET NULL,
    -- agent, operator or root.
    run_as     text NOT NULL DEFAULT 'agent' CHECK (run_as IN ('agent', 'operator', 'root')),
    title      text NOT NULL DEFAULT '',
    command    jsonb NOT NULL DEFAULT '[]',
    cwd        text,
    state      text NOT NULL DEFAULT 'running' CHECK (state IN ('running', 'exited', 'lost')),
    exit_code  integer,
    cols       integer NOT NULL DEFAULT 80,
    rows       integer NOT NULL DEFAULT 24,
    "offset"   bigint NOT NULL DEFAULT 0,
    last_input jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    ended_at   timestamptz,
    UNIQUE (host, session)
);

CREATE INDEX terminals_owner ON rookery.terminals (owner, created_at DESC);
CREATE INDEX terminals_running ON rookery.terminals (host) WHERE state = 'running';
