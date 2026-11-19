-- Settings and connections of a space: a space's value of a setting sits between a person's own and the
-- global one, and a space's connection is for its members.

ALTER TABLE rookery.settings
    ADD COLUMN space_id uuid REFERENCES rookery.spaces (id) ON DELETE CASCADE,
    DROP CONSTRAINT settings_scope_check,
    DROP CONSTRAINT settings_plugin_key_scope_user_id_key,
    ADD CONSTRAINT settings_scope_check CHECK (scope IN ('default', 'global', 'space', 'user')),
    ADD CONSTRAINT settings_space_check CHECK ((scope = 'space') = (space_id IS NOT NULL)),
    ADD CONSTRAINT settings_key UNIQUE NULLS NOT DISTINCT (plugin, key, scope, user_id, space_id);

ALTER TABLE rookery.connections
    ADD COLUMN space_id uuid REFERENCES rookery.spaces (id) ON DELETE CASCADE,
    DROP CONSTRAINT connections_scope_check,
    DROP CONSTRAINT connections_type_name_scope_user_id_key,
    ADD CONSTRAINT connections_scope_check CHECK (scope IN ('global', 'space', 'user')),
    ADD CONSTRAINT connections_space_check CHECK ((scope = 'space') = (space_id IS NOT NULL)),
    ADD CONSTRAINT connections_key UNIQUE NULLS NOT DISTINCT (type, name, scope, user_id, space_id);

CREATE INDEX settings_space ON rookery.settings (space_id) WHERE space_id IS NOT NULL;
CREATE INDEX connections_space ON rookery.connections (space_id) WHERE space_id IS NOT NULL;
