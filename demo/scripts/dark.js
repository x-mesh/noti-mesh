// Switches the theme with the same selector that a user changes.
export default async function (ctx) {
  await ctx.evaluate("admin", () => {
    const select = document.getElementById("theme-select");
    select.value = "dark";
    select.dispatchEvent(new Event("change", { bubbles: true }));
  });
}
