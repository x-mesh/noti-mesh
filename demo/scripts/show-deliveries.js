// Scrolls the incident detail to its delivery records.
export default async function (ctx) {
  await ctx.evaluate("admin", () => {
    const heading = [...document.querySelectorAll("#detail-dialog h3")].find((h) => /Telegram/.test(h.textContent));
    heading?.scrollIntoView({ block: "center" });
  });
}
