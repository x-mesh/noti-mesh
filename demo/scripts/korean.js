// Switches the language with the same selector that a user changes.
export default async function (ctx) {
  await ctx.evaluate("admin", () => {
    const select = document.getElementById("lang-select");
    select.value = "ko";
    select.dispatchEvent(new Event("change", { bubbles: true }));
  });
}
