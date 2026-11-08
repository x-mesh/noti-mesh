CREATE TABLE message_duplicates (
    source text NOT NULL,
    event_id text NOT NULL,
    message_id text NOT NULL REFERENCES gateway_messages(id),
    payload jsonb NOT NULL,
    received_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (source, event_id)
);
CREATE INDEX message_duplicates_message_id_idx ON message_duplicates (message_id);
CREATE INDEX gateway_messages_source_created_idx ON gateway_messages (source, created_at DESC);
