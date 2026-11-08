// Fills an empty mock stack with a few incidents and messages so that the demo starts from a working state.
// Usage: node demo/seed.mjs [base URL]. The mock stack accepts the development source token and admin requests.
const base = process.argv[2] ?? "http://127.0.0.1:18080";
const auth = { Authorization: "Bearer dev-source-token-not-for-production-0001", "Content-Type": "application/json" };
const ago = (minutes) => new Date(Date.now() - minutes * 60_000).toISOString();
const post = async (path, body, headers = auth) => {
  const response = await fetch(base + path, { method: "POST", headers, body: JSON.stringify(body) });
  if (!response.ok) throw new Error(`${path}: HTTP ${response.status} ${await response.text()}`);
  return response.json();
};
const event = (key, sequence, severity, summary, minutes, recovered = false) =>
  post("/v1/events", {
    event_id: `${key}-${sequence}`, service: key.split("-")[0], environment: "development", incident_key: key,
    sequence, severity, recovered, occurred_at: ago(minutes), summary,
  });

const payments = await event("payments-card-latency", 1, 2, "Card authorization latency is above 2 s", 42);
await post(`/v1/admin/incidents/${payments.incident_id}/ack`, { actor: "dev-operator", sequence: 1 }, { "Content-Type": "application/json" });
await event("search-index-lag", 1, 3, "Search index is 6 minutes behind", 18);
await event("auth-login-errors", 1, 2, "Login error rate is 4%", 95);
await event("auth-login-errors", 2, 2, "Login error rate is back to normal", 70, true);
await post("/v1/messages", { event_id: "backup-done", service: "checkout", environment: "development", title: "Nightly backup completed", body: "All 12 volumes were backed up." });
await post("/v1/messages", { event_id: "cert-expiry", service: "checkout", environment: "development", title: "TLS certificate expires in 7 days", body: "api.example.com", level: "warn" });
console.log("seeded");
