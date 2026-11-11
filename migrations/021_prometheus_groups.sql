-- Alerts with the same group labels share one incident; the group row is locked before its alerts so joins serialize.
CREATE TABLE prometheus_groups (
 source text NOT NULL,
 group_hash text NOT NULL,
 generation integer NOT NULL DEFAULT 0,
 incident_key text NOT NULL DEFAULT '',
 sequence bigint NOT NULL DEFAULT 0,
 open boolean NOT NULL DEFAULT false,
 PRIMARY KEY (source, group_hash)
);
-- Alerts stored before grouping keep a NULL group and resolve their own incident.
ALTER TABLE prometheus_alerts ADD COLUMN group_hash text, ADD COLUMN group_generation integer;
CREATE INDEX prometheus_alerts_active_group ON prometheus_alerts (source, group_hash, group_generation) WHERE resolved_at IS NULL;
