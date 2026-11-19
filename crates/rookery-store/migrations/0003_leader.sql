-- The leader's epoch: one row, incremented each time a server takes the leader lock. A leader's
-- writes to jobs and leases name its epoch, and rookery.fence refuses them once another server leads.
CREATE TABLE rookery.leader (
    id    boolean PRIMARY KEY DEFAULT true CHECK (id),
    epoch bigint NOT NULL
);

-- True when `want` is the epoch now, or NULL for a store that leads nothing (tests, tools). It holds a
-- share lock on the row until the transaction ends, so a takeover waits for a write that passed it.
CREATE FUNCTION rookery.fence(want bigint) RETURNS boolean LANGUAGE plpgsql VOLATILE AS $$
BEGIN
    IF want IS NULL THEN
        RETURN true;
    END IF;
    PERFORM 1 FROM rookery.leader WHERE epoch = want FOR SHARE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'this server does not lead any more: another took the leader lock'
            USING ERRCODE = 'RK001';
    END IF;
    RETURN true;
END
$$;

-- Claims whose time ran out, for the leader to reclaim.
CREATE INDEX jobs_claimed ON rookery.jobs (claimed_until) WHERE state = 'running';
