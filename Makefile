.PHONY: dev dev-mock dev-mock-down dev-down down dev-fail-slack send event scenarios

DEV_PORT ?= 8080
export DEV_PORT

DEV_COMPOSE = docker compose --project-name noti-mesh-slack-test --file compose.dev.yaml --file compose.slack-test.yaml
MOCK_COMPOSE = docker compose --project-name noti-mesh-dev --file compose.dev.yaml
SCENARIO_PORT ?= 18080
# An empty env file keeps real Slack credentials from .env out of the scenario stack.
SCENARIO_COMPOSE = DEV_PORT=$(SCENARIO_PORT) docker compose --env-file /dev/null --project-name noti-mesh-scenarios --file compose.dev.yaml

dev:
	@printf '%s\n' 'noti-mesh development mode (real Slack, AI disabled)' 'Admin: http://localhost:$(DEV_PORT)/admin' 'Admin authentication: disabled for local testing' 'Source token: dev-source-token-not-for-production-0001' 'Press Ctrl+C to stop. Database contents persist.'
	$(DEV_COMPOSE) up --build

dev-mock:
	$(MOCK_COMPOSE) up --build

dev-mock-down:
	$(MOCK_COMPOSE) down

dev-down:
	$(DEV_COMPOSE) down

TEST_PGDATA ?= /tmp/noti-mesh-test-pg

# Stops every noti-* Compose project, leftover noti-* containers started with docker run, and the local test PostgreSQL. Volumes stay.
down:
	@for project in $$(docker compose ls --all --quiet | grep '^noti-'); do docker compose --project-name $$project down; done
	@containers=$$(docker ps --all --quiet --filter name=^noti-); if [ -n "$$containers" ]; then docker rm --force $$containers; fi
	@if [ -f $(TEST_PGDATA)/postmaster.pid ]; then pg_ctl --pgdata $(TEST_PGDATA) stop --mode fast; fi

dev-fail-slack:
	$(MOCK_COMPOSE) exec -T slack python -c 'import urllib.request; urllib.request.urlopen(urllib.request.Request("http://127.0.0.1:8090/__dev/fail-next",data=b"{}",headers={"Content-Type":"application/json"}))'

send:
	@python3 scripts/send_test.py

event:
	@python3 scripts/send_event.py

scenarios:
	$(SCENARIO_COMPOSE) up -d --build
	python3 scripts/dev_scenarios.py --base-url http://127.0.0.1:$(SCENARIO_PORT) --project noti-mesh-scenarios; status=$$?; $(SCENARIO_COMPOSE) down -v; exit $$status
