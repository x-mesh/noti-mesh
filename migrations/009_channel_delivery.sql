CREATE TABLE channel_deliveries (
 id bigserial PRIMARY KEY,
 incident_id text REFERENCES incidents(id),
 message_id text REFERENCES gateway_messages(id),
 parent_outbox_id bigint REFERENCES outbox(id),
 channel text NOT NULL CHECK (channel IN ('email','telegram')),
 destination text NOT NULL,
 mode text NOT NULL CHECK (mode IN ('fallback','parallel','unacknowledged')),
 state text NOT NULL CHECK (state IN ('standby','pending','sent','cancelled')),
 due_at timestamptz NOT NULL DEFAULT now(),
 attempts integer NOT NULL DEFAULT 0,
 last_error text,
 provider_message_id text,
 incident_state text,
 incident_priority smallint,
 content_snapshot jsonb,
 sent_at timestamptz,
 lease_until timestamptz,
 lease_token text,
 created_at timestamptz NOT NULL DEFAULT now(),
 CHECK ((incident_id IS NOT NULL AND message_id IS NULL AND parent_outbox_id IS NOT NULL) OR
        (incident_id IS NULL AND message_id IS NOT NULL AND parent_outbox_id IS NULL))
);
CREATE INDEX channel_deliveries_due ON channel_deliveries(due_at) WHERE state='pending';
CREATE INDEX channel_deliveries_outbox ON channel_deliveries(parent_outbox_id);
CREATE INDEX channel_deliveries_message ON channel_deliveries(message_id);
ALTER TABLE outbox ADD COLUMN slack_disabled boolean NOT NULL DEFAULT false;
ALTER TABLE gateway_messages ADD COLUMN slack_disabled boolean NOT NULL DEFAULT false;
CREATE TABLE worker_heartbeats (
 worker text PRIMARY KEY,
 last_tick timestamptz NOT NULL
);
