-- Prometheus resends each active alert with a refreshed ends_at; a row whose ends_at passes without a resend is resolved.
CREATE TABLE prometheus_alerts (
 source text NOT NULL,
 service text NOT NULL,
 fingerprint text NOT NULL,
 starts_at timestamptz NOT NULL,
 generation integer NOT NULL DEFAULT 0,
 ends_at timestamptz NOT NULL,
 labels jsonb NOT NULL,
 annotations jsonb NOT NULL,
 generator_url text,
 resolved_at timestamptz,
 updated_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY (source, fingerprint, starts_at)
);
CREATE INDEX prometheus_alerts_expiring ON prometheus_alerts (ends_at) WHERE resolved_at IS NULL;
