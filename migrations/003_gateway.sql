CREATE TABLE client_tokens (
 id text PRIMARY KEY,
 name text NOT NULL,
 service text NOT NULL,
 token_hash text NOT NULL UNIQUE,
 created_at timestamptz NOT NULL DEFAULT now(),
 last_used_at timestamptz,
 revoked_at timestamptz
);
CREATE TABLE confirmations (
 id text PRIMARY KEY,
 incident_id text NOT NULL REFERENCES incidents(id),
 actor text NOT NULL,
 channel text NOT NULL,
 message_ts text NOT NULL,
 event_ts text NOT NULL,
 message_snapshot jsonb NOT NULL,
 created_at timestamptz NOT NULL DEFAULT now()
);
ALTER TABLE outbox ADD COLUMN provider_channel text;
ALTER TABLE outbox ADD COLUMN provider_message_ts text;
CREATE TABLE gateway_messages (
 id text PRIMARY KEY,
 source text NOT NULL,
 event_id text NOT NULL,
 payload jsonb NOT NULL,
 target text NOT NULL,
 channel text NOT NULL,
 created_at timestamptz NOT NULL DEFAULT now(),
 due_at timestamptz NOT NULL DEFAULT now(),
 sent_at timestamptz,
 provider_message_ts text,
 attempts integer NOT NULL DEFAULT 0,
 last_error text,
 lease_until timestamptz,
 lease_token text,
 UNIQUE(source,event_id)
);
CREATE TABLE message_confirmations (
 id text PRIMARY KEY,
 message_id text NOT NULL REFERENCES gateway_messages(id),
 actor text NOT NULL,
 channel text NOT NULL,
 message_ts text NOT NULL,
 event_ts text NOT NULL,
 message_snapshot jsonb NOT NULL,
 created_at timestamptz NOT NULL DEFAULT now()
);
