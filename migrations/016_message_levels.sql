ALTER TABLE gateway_messages ADD COLUMN level text NOT NULL DEFAULT 'info'
 CHECK (level IN ('crit','error','warn','info'));
CREATE INDEX events_received ON events (received_at);
CREATE INDEX gateway_messages_created ON gateway_messages (created_at);
CREATE INDEX message_duplicates_received ON message_duplicates (received_at);
