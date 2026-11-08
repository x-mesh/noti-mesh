CREATE TABLE incidents (
 id text PRIMARY KEY, source text NOT NULL, service text NOT NULL, environment text NOT NULL,
 incident_key text NOT NULL, sequence bigint NOT NULL, severity smallint NOT NULL,
 state text NOT NULL DEFAULT 'open', owner text, count bigint NOT NULL DEFAULT 1,
 created_at timestamptz NOT NULL DEFAULT now(), policy_snapshot jsonb NOT NULL DEFAULT '{}',
 updated_at timestamptz NOT NULL DEFAULT now(),
 UNIQUE(source,service,environment,incident_key)
);
CREATE TABLE events(source text NOT NULL,event_id text NOT NULL,payload jsonb NOT NULL,incident_id text NOT NULL REFERENCES incidents(id),PRIMARY KEY(source,event_id));
CREATE TABLE outbox(id bigserial PRIMARY KEY,incident_id text NOT NULL REFERENCES incidents(id),due_at timestamptz NOT NULL DEFAULT now(),attempts integer NOT NULL DEFAULT 0,last_error text,sent_at timestamptz);
