// A monitoring system reports a critical error rate for checkout.
export default async function () {
  const response = await fetch("http://127.0.0.1:18080/v1/events", {
    method: "POST",
    headers: { Authorization: "Bearer dev-source-token-not-for-production-0001", "Content-Type": "application/json" },
    body: JSON.stringify({
      event_id: "checkout-error-rate-1", service: "checkout", environment: "development",
      incident_key: "checkout-error-rate", sequence: 1, severity: 1, recovered: false,
      occurred_at: new Date().toISOString(), error_code: "HTTP_5XX",
      summary: "Checkout API returns 5xx for 12% of requests", source_url: "https://grafana.example.com/d/checkout",
    }),
  });
  if (!response.ok) throw new Error(`alert: HTTP ${response.status}`);
}
