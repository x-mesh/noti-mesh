// The monitoring system reports that checkout recovered.
export default async function () {
  const response = await fetch("http://127.0.0.1:18080/v1/events", {
    method: "POST",
    headers: { Authorization: "Bearer dev-source-token-not-for-production-0001", "Content-Type": "application/json" },
    body: JSON.stringify({
      event_id: "checkout-error-rate-2", service: "checkout", environment: "development",
      incident_key: "checkout-error-rate", sequence: 2, severity: 1, recovered: true,
      occurred_at: new Date().toISOString(), summary: "Checkout API error rate is back to normal",
    }),
  });
  if (!response.ok) throw new Error(`recover: HTTP ${response.status}`);
}
