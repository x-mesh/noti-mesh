ALTER TABLE client_tokens ADD COLUMN rotated_at timestamptz;
CREATE TABLE client_token_audit (
 id bigserial PRIMARY KEY,
 token_id text NOT NULL REFERENCES client_tokens(id),
 action text NOT NULL CHECK (action IN ('created', 'rotated', 'revoked')),
 actor_kind text NOT NULL CHECK (actor_kind IN ('shared_admin', 'development', 'unknown')),
 created_at timestamptz NOT NULL DEFAULT now()
);
INSERT INTO client_token_audit(token_id,action,actor_kind,created_at)
 SELECT id,'created','unknown',created_at FROM client_tokens;
INSERT INTO client_token_audit(token_id,action,actor_kind,created_at)
 SELECT id,'revoked','unknown',revoked_at FROM client_tokens WHERE revoked_at IS NOT NULL;
