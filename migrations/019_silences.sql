CREATE TABLE silences (
 id text PRIMARY KEY,
 service text NOT NULL,
 environment text,
 starts_at timestamptz NOT NULL DEFAULT now(),
 ends_at timestamptz NOT NULL,
 reason text NOT NULL,
 created_by text NOT NULL,
 created_at timestamptz NOT NULL DEFAULT now(),
 cancelled_at timestamptz,
 cancelled_by text,
 CHECK (ends_at > starts_at)
);
CREATE INDEX silences_service_active ON silences (service, ends_at) WHERE cancelled_at IS NULL;
-- An incident with a silence_id was held by that silence and has not been announced yet.
ALTER TABLE incidents ADD COLUMN silence_id text REFERENCES silences(id);
CREATE INDEX incidents_held ON incidents (silence_id) WHERE silence_id IS NOT NULL;
ALTER TABLE gateway_messages ADD COLUMN silence_id text REFERENCES silences(id);
