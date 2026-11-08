ALTER TABLE channel_deliveries DROP CONSTRAINT channel_deliveries_state_check;
ALTER TABLE channel_deliveries ADD CONSTRAINT channel_deliveries_state_check
 CHECK (state IN ('standby','pending','sent','cancelled','failed'));
ALTER TABLE outbox ADD COLUMN failed_at timestamptz;
ALTER TABLE gateway_messages ADD COLUMN failed_at timestamptz;
CREATE TABLE delivery_retry_audit (
 id bigserial PRIMARY KEY,
 target_kind text NOT NULL CHECK (target_kind IN ('channel','slack_incident','slack_message')),
 target_id text NOT NULL,
 actor_kind text NOT NULL CHECK (actor_kind IN ('shared_admin','named_admin','development')),
 actor text,
 created_at timestamptz NOT NULL DEFAULT now()
);
