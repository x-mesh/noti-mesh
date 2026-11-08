CREATE INDEX events_incident_sequence ON events (incident_id, ((payload->>'sequence')::bigint) DESC);
CREATE INDEX events_correlation ON events ((payload->>'correlation_id')) WHERE payload->>'correlation_id' IS NOT NULL;
CREATE INDEX outbox_incident ON outbox (incident_id);
CREATE INDEX channel_deliveries_incident ON channel_deliveries (incident_id) WHERE incident_id IS NOT NULL;
CREATE INDEX confirmations_incident ON confirmations (incident_id, created_at DESC);
CREATE INDEX message_confirmations_message ON message_confirmations (message_id, created_at DESC);
CREATE INDEX incidents_summary_due ON incidents (next_summary) WHERE dirty;
CREATE INDEX incidents_escalation_due ON incidents (next_escalation) WHERE state = 'open';
