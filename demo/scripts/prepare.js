// The demo shows the English UI in the light theme. This runs on an empty page of the same origin.
export default async function (ctx) {
  await ctx.evaluate("admin", () => {
    localStorage.setItem("noti-mesh-lang", "en");
    localStorage.setItem("noti-mesh-theme", "light");
  });
}
