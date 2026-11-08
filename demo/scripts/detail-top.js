// Scrolls the incident detail back to the assignment form.
export default async function (ctx) {
  await ctx.evaluate("admin", () => {
    document.querySelector("#assign-form")?.scrollIntoView({ block: "center" });
  });
}
